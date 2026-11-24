use std::sync::Arc;
use std::time::Duration;

use nekorisu_archive::Archive;
use nekorisu_cloud::{CloudService, FileSigner, Publisher, ServiceOptions, Signer, Store, Tls, serve as serve_grpc};
use nekorisu_server::auth::Auth;
use nekorisu_server::settings::Settings;
use nekorisu_server::state::AppState;
use nekorisu_server::{api, db, router, storage};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version" | "-V") => {
            println!("nekorisu-server {} ({})", env!("CARGO_PKG_VERSION"), api::COMMIT);
            return std::process::ExitCode::SUCCESS;
        }
        Some("--help" | "-h") => {
            println!("nekorisu-server: the nekorisu panel, MCP endpoint and device protocol.\n");
            println!("usage: nekorisu-server [serve | healthcheck | --version]\n");
            println!("Everything is set with environment variables; see docker-compose.example.yml.");
            return std::process::ExitCode::SUCCESS;
        }
        Some("healthcheck") => return healthcheck().await,
        None | Some("serve") => {}
        Some(other) => {
            eprintln!("unknown command: {other} (try --help)");
            return std::process::ExitCode::from(2);
        }
    }
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,sqlx=warn")))
        .init();
    match run().await {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("{error}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// For the container's HEALTHCHECK: the image has no curl. Asks /healthz on the
/// HTTP address and exits 0 if it says ok.
async fn healthcheck() -> std::process::ExitCode {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let Ok(settings) = Settings::from_env(&|k| std::env::var(k).ok()) else {
        return std::process::ExitCode::FAILURE;
    };
    let mut addr = settings.http_addr;
    if addr.ip().is_unspecified() {
        addr.set_ip(std::net::Ipv4Addr::LOCALHOST.into());
    }
    let result = tokio::time::timeout(Duration::from_secs(3), async {
        let mut stream = tokio::net::TcpStream::connect(addr).await?;
        stream
            .write_all(b"GET /healthz HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
            .await?;
        let mut reply = String::new();
        stream.read_to_string(&mut reply).await?;
        Ok::<_, std::io::Error>(reply)
    })
    .await;
    match result {
        Ok(Ok(reply)) if reply.starts_with("HTTP/1.1 200") => std::process::ExitCode::SUCCESS,
        _ => std::process::ExitCode::FAILURE,
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let settings = Settings::from_env(&|k| std::env::var(k).ok())?;
    std::fs::create_dir_all(&settings.data_dir)?;

    let pool = db::connect(&settings.database_url).await?;
    let store = Store::new(pool.clone());
    store.migrate().await?;
    let auth = Auth::new(pool).await?;

    let (signer, created) = FileSigner::load_or_create(&settings.signing_key_file)?;
    let signer: Arc<dyn Signer> = Arc::new(signer);
    if created {
        tracing::info!(
            key_id = %signer.key_id(),
            file = %settings.signing_key_file.display(),
            "made the config signing key; back it up with the data directory: without it every device must be pointed at the server again"
        );
    }
    tracing::info!(public_key = %signer.public_key(), "config signing key (devices pin this)");

    let publisher = Arc::new(Publisher::new(
        settings.config_dir.clone(),
        store.clone(),
        signer.clone(),
    ));
    let _ = publisher.check().await; // errors are logged and shown in the panel
    let source = publisher.status();
    publisher.spawn(Duration::from_secs(30));

    let provider = storage::provider(&settings)?;
    tracing::info!(storage = %provider.describe(), "storage");
    let archive = Arc::new(Archive::new(provider));

    if let Some(code) = auth.prepare_setup().await? {
        tracing::warn!("first run: open the panel and set the administrator password. Setup code: {code}");
    }
    if settings.device_token.is_none() {
        tracing::warn!("NEKORISU_DEVICE_TOKEN is not set: devices cannot fetch the account config or report");
    }
    if settings.mcp_token.is_none() {
        tracing::info!("NEKORISU_MCP_TOKEN is not set: the MCP endpoint is off");
    }

    let state = Arc::new(AppState {
        settings: settings.clone(),
        archive,
        store: store.clone(),
        auth,
        signer,
        source,
    });

    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        shutdown_signal().await;
        let _ = stop_tx.send(true);
    });
    let stopped = |mut rx: tokio::sync::watch::Receiver<bool>| async move {
        let _ = rx.wait_for(|stop| *stop).await;
    };

    let http = tokio::net::TcpListener::bind(settings.http_addr).await?;
    tracing::info!(addr = %settings.http_addr, "panel and MCP");
    let http_server = axum::serve(http, router(state)).with_graceful_shutdown(stopped(stop_rx.clone()));

    let tls = match &settings.grpc_tls {
        Some((cert, key)) => Some(Tls {
            cert_pem: std::fs::read(cert)?,
            key_pem: std::fs::read(key)?,
        }),
        None => None,
    };
    tracing::info!(addr = %settings.grpc_addr, tls = tls.is_some(), "device protocol");
    let service = CloudService::new(
        store,
        ServiceOptions {
            device_token: settings.device_token.clone(),
            public_config: settings.public_config,
        },
    );
    let grpc_server = serve_grpc(settings.grpc_addr, service, tls, stopped(stop_rx));

    let (http_result, grpc_result) = tokio::join!(http_server, grpc_server);
    http_result?;
    grpc_result?;
    tracing::info!("stopped");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    {
        let mut term =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal handler");
        tokio::select! { _ = ctrl_c => {}, _ = term.recv() => {} }
    }
    #[cfg(not(unix))]
    ctrl_c.await;
}
