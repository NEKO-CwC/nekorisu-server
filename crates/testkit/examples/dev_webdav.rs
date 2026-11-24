//! Serves a directory over WebDAV for local development (see seed.rs).
//!
//!     cargo run -p nekorisu-testkit --example dev_webdav -- dev-data/dav [127.0.0.1:4918]

use std::convert::Infallible;
use std::net::SocketAddr;

use dav_server::{DavHandler, fakels::FakeLs, localfs::LocalFs};
use hyper::service::service_fn;
use hyper_util::rt::{TokioExecutor, TokioIo};

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let dir = args.next().unwrap_or_else(|| "dev-data/dav".into());
    let addr: SocketAddr = args
        .next()
        .unwrap_or_else(|| "127.0.0.1:4918".into())
        .parse()
        .expect("an address");
    let handler = DavHandler::builder()
        .filesystem(LocalFs::new(&dir, false, false, false))
        .locksystem(FakeLs::new())
        .build_handler();
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("WebDAV on http://{addr}/ serving {dir}");
    loop {
        let (stream, _) = listener.accept().await?;
        let handler = handler.clone();
        tokio::spawn(async move {
            let service = service_fn(move |req| {
                let handler = handler.clone();
                async move { Ok::<_, Infallible>(handler.handle(req).await) }
            });
            let _ = hyper_util::server::conn::auto::Builder::new(TokioExecutor::new())
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
    }
}
