//! Everything the container is told through environment variables.

use std::net::SocketAddr;
use std::path::PathBuf;

use url::{Host, Url};

use crate::client_ip::{Cidr, parse_trusted_proxies};

#[derive(Clone)]
pub struct WebDavSettings {
    pub url: String,
    pub user: Option<String>,
    pub password: Option<String>,
    /// The address devices use for the same folder, when the server reaches
    /// the storage another way (an address inside the cluster or on this
    /// machine). Shown to devices instead of `url`.
    pub device_url: Option<String>,
}

impl WebDavSettings {
    /// What a device fills in with `nekorisu storage add`.
    pub fn for_devices(&self) -> &str {
        self.device_url.as_deref().unwrap_or(&self.url)
    }
}

/// An address only this machine or its cluster can reach: localhost, a name
/// without dots, a Kubernetes service name, a private or loopback IP.
pub fn internal_host(url: &str) -> bool {
    let Ok(url) = Url::parse(url) else {
        return false;
    };
    match url.host() {
        Some(Host::Domain(d)) => {
            let d = d.to_ascii_lowercase();
            d == "localhost" || !d.contains('.') || d.ends_with(".svc") || d.contains(".svc.") || d.ends_with(".local")
        }
        Some(Host::Ipv4(ip)) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        Some(Host::Ipv6(ip)) => ip.is_loopback() || (ip.segments()[0] & 0xfe00) == 0xfc00,
        None => false,
    }
}

#[derive(Clone)]
pub struct Settings {
    pub database_url: String,
    pub data_dir: PathBuf,
    pub config_dir: PathBuf,
    pub http_addr: SocketAddr,
    pub grpc_addr: SocketAddr,
    pub grpc_tls: Option<(PathBuf, PathBuf)>,
    pub webdav: Option<WebDavSettings>,
    pub device_token: Option<String>,
    pub public_config: bool,
    pub signing_key_file: PathBuf,
    pub cookie_secure: bool,
    /// The server's public address, like `https://nekorisu.example.com:8443`
    /// (no trailing slash). The MCP endpoint and its OAuth sign-in are on only
    /// when it is set: the issuer and the token audience come from it. Also
    /// used for the device connect command in the panel.
    pub public_url: Option<String>,
    /// Where devices reach the device protocol, when it is not the public
    /// address (a gRPC port of its own, as in the Docker Compose example).
    pub device_url: Option<String>,
    /// The public connection notes (`/connect`, `/.well-known/nekorisu`).
    pub connect_page: bool,
    /// Put the storage address and user name on the public notes too, for a
    /// server only reachable on a private network. Off: only after sign-in.
    pub connect_show_storage: bool,
    /// Accept clients that name themselves with an https URL (OAuth Client ID
    /// Metadata Documents). Off makes such clients fall back to registering.
    pub oauth_metadata_documents: bool,
    /// Reverse proxies whose `X-Real-IP` and `X-Forwarded-For` are believed
    /// (`crate::client_ip`). Empty: the peer address is the client.
    pub trusted_proxies: Vec<Cidr>,
    /// A DNS-over-HTTPS resolver (RFC 8484) for the hosts of client metadata
    /// documents, instead of the system's. Its host is an IP address.
    pub dns_over_https: Option<Url>,
}

/// Never prints a token or a password, only whether one is set.
impl std::fmt::Debug for Settings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Settings")
            .field("database_url", &self.database_url)
            .field("data_dir", &self.data_dir)
            .field("config_dir", &self.config_dir)
            .field("http_addr", &self.http_addr)
            .field("grpc_addr", &self.grpc_addr)
            .field("grpc_tls", &self.grpc_tls.is_some())
            .field("webdav", &self.webdav.as_ref().map(|d| d.url.as_str()))
            .field("device_token_set", &self.device_token.is_some())
            .field("public_config", &self.public_config)
            .field("public_url", &self.public_url)
            .field("device_url", &self.device_url)
            .field("connect_page", &self.connect_page)
            .field("connect_show_storage", &self.connect_show_storage)
            .field("oauth_metadata_documents", &self.oauth_metadata_documents)
            .field("trusted_proxies", &self.trusted_proxies.len())
            .field("dns_over_https", &self.dns_over_https.is_some())
            .finish_non_exhaustive()
    }
}

fn flag(text: &str) -> Option<bool> {
    match text.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" | "" => Some(false),
        _ => None,
    }
}

/// Checks NEKORISU_PUBLIC_URL (or NEKORISU_DEVICE_URL, `name`): https (http
/// only on localhost), just the origin, written without a trailing slash.
fn public_url(name: &str, text: &str) -> Result<String, String> {
    let bad = |why: &str| {
        format!(
            "{name} {text:?}: {why}; write it like https://nekorisu.example.com or https://nekorisu.example.com:8443"
        )
    };
    let url = Url::parse(text.trim()).map_err(|_| bad("not a URL"))?;
    let local = match url.host() {
        Some(Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(ip)) => ip.is_loopback(),
        Some(Host::Ipv6(ip)) => ip.is_loopback(),
        None => return Err(bad("no host")),
    };
    match url.scheme() {
        "https" => {}
        "http" if local => {}
        _ => return Err(bad("must be https (http only for localhost)")),
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(bad("only the scheme, host and port, no path"));
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

/// Checks NEKORISU_DNS_OVER_HTTPS: an https URL whose host is an IP address.
/// A name would itself be looked up through the system DNS, which is what
/// this setting is there to get around (a fake-IP proxy answers every name).
fn dns_over_https(text: &str) -> Result<Url, String> {
    let bad = |why: &str| {
        format!(
            "NEKORISU_DNS_OVER_HTTPS {text:?}: {why}; write it like https://1.1.1.1/dns-query or https://223.5.5.5/dns-query"
        )
    };
    let url = Url::parse(text.trim()).map_err(|_| bad("not a URL"))?;
    if url.scheme() != "https" {
        return Err(bad("must be https"));
    }
    match url.host() {
        Some(Host::Ipv4(_) | Host::Ipv6(_)) => {}
        Some(Host::Domain(_)) => {
            return Err(bad(
                "the host must be an IP address, since looking up a name would go through the system DNS this is meant to avoid",
            ));
        }
        None => return Err(bad("no host")),
    }
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(bad("no user name, password or fragment"));
    }
    Ok(url)
}

impl Settings {
    /// `get` reads one variable; tests pass their own.
    pub fn from_env(get: &dyn Fn(&str) -> Option<String>) -> Result<Settings, String> {
        let var = |name: &str| get(name).filter(|v| !v.trim().is_empty());
        // NAME, or NAME_FILE naming a file that holds it (Docker secrets).
        let secret = |name: &str| -> Result<Option<String>, String> {
            if let Some(path) = var(&format!("{name}_FILE")) {
                let text = std::fs::read_to_string(&path).map_err(|e| format!("{name}_FILE {path}: {e}"))?;
                return Ok(Some(text.trim_end_matches(['\r', '\n']).to_string()).filter(|t| !t.is_empty()));
            }
            Ok(var(name))
        };
        let boolean = |name: &str, default: bool| -> Result<bool, String> {
            match var(name) {
                None => Ok(default),
                Some(v) => flag(&v).ok_or_else(|| format!("{name} must be true or false, not {v:?}")),
            }
        };
        let addr = |name: &str, default: &str| -> Result<SocketAddr, String> {
            let text = var(name).unwrap_or_else(|| default.to_string());
            text.parse()
                .map_err(|_| format!("{name} must look like 0.0.0.0:8080, not {text:?}"))
        };

        let data_dir = PathBuf::from(var("NEKORISU_DATA_DIR").unwrap_or_else(|| "data".to_string()));
        let database_url =
            var("DATABASE_URL").unwrap_or_else(|| format!("sqlite://{}", data_dir.join("nekorisu.db").display()));
        if !database_url.starts_with("sqlite:") {
            return Err("DATABASE_URL: only SQLite is supported in this version (sqlite://...)".to_string());
        }

        let webdav = match var("NEKORISU_WEBDAV_URL") {
            None => None,
            Some(url) => {
                let mut url = url;
                if let Some(root) = var("NEKORISU_WEBDAV_ROOT") {
                    url = format!("{}/{}", url.trim_end_matches('/'), root.trim_matches('/'));
                }
                let device_url = match var("NEKORISU_DEVICE_WEBDAV_URL") {
                    None => None,
                    Some(u) => match Url::parse(u.trim()) {
                        Ok(parsed) if matches!(parsed.scheme(), "http" | "https") && parsed.password().is_none() => {
                            Some(u.trim().trim_end_matches('/').to_string())
                        }
                        _ => {
                            return Err(format!(
                                "NEKORISU_DEVICE_WEBDAV_URL {u:?}: write the http(s) address devices use, without a password"
                            ));
                        }
                    },
                };
                Some(WebDavSettings {
                    url,
                    user: var("NEKORISU_WEBDAV_USER"),
                    password: secret("NEKORISU_WEBDAV_PASSWORD")?,
                    device_url,
                })
            }
        };
        let tls = match (var("NEKORISU_GRPC_TLS_CERT_FILE"), var("NEKORISU_GRPC_TLS_KEY_FILE")) {
            (Some(c), Some(k)) => Some((PathBuf::from(c), PathBuf::from(k))),
            (None, None) => None,
            _ => return Err("NEKORISU_GRPC_TLS_CERT_FILE and NEKORISU_GRPC_TLS_KEY_FILE go together".to_string()),
        };

        Ok(Settings {
            database_url,
            config_dir: PathBuf::from(var("NEKORISU_CONFIG_DIR").unwrap_or_else(|| "config".to_string())),
            http_addr: addr("NEKORISU_HTTP_ADDR", "127.0.0.1:8080")?,
            grpc_addr: addr("NEKORISU_GRPC_ADDR", "0.0.0.0:7070")?,
            grpc_tls: tls,
            webdav,
            device_token: secret("NEKORISU_DEVICE_TOKEN")?,
            public_config: boolean("NEKORISU_PUBLIC_CONFIG", false)?,
            signing_key_file: var("NEKORISU_SIGNING_KEY_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|| data_dir.join("signing-key.json")),
            cookie_secure: boolean("NEKORISU_COOKIE_SECURE", false)?,
            public_url: var("NEKORISU_PUBLIC_URL")
                .map(|u| public_url("NEKORISU_PUBLIC_URL", &u))
                .transpose()?,
            device_url: var("NEKORISU_DEVICE_URL")
                .map(|u| public_url("NEKORISU_DEVICE_URL", &u))
                .transpose()?,
            connect_page: boolean("NEKORISU_CONNECT_PAGE", true)?,
            connect_show_storage: boolean("NEKORISU_CONNECT_SHOW_STORAGE", false)?,
            oauth_metadata_documents: boolean("NEKORISU_OAUTH_CLIENT_METADATA_DOCUMENTS", true)?,
            trusted_proxies: parse_trusted_proxies(&var("NEKORISU_TRUSTED_PROXIES").unwrap_or_default())?,
            dns_over_https: var("NEKORISU_DNS_OVER_HTTPS").map(|u| dns_over_https(&u)).transpose()?,
            data_dir,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    fn settings(pairs: &[(&str, &str)]) -> Result<Settings, String> {
        let map: HashMap<String, String> = pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        Settings::from_env(&|k| map.get(k).cloned())
    }

    #[test]
    fn defaults() {
        let s = settings(&[]).unwrap();
        assert_eq!(s.http_addr.to_string(), "127.0.0.1:8080");
        assert_eq!(s.grpc_addr.to_string(), "0.0.0.0:7070");
        assert_eq!(s.database_url, "sqlite://data/nekorisu.db");
        assert_eq!(s.signing_key_file, PathBuf::from("data/signing-key.json"));
        assert!(s.webdav.is_none() && s.device_token.is_none() && s.public_url.is_none());
        assert!(!s.public_config && !s.cookie_secure && s.oauth_metadata_documents);
        assert!(s.trusted_proxies.is_empty() && s.dns_over_https.is_none());
        assert!(s.connect_page && !s.connect_show_storage && s.device_url.is_none());
    }

    #[test]
    fn trusted_proxies_are_ranges_or_a_startup_error() {
        let s = settings(&[("NEKORISU_TRUSTED_PROXIES", "10.42.0.0/16, 2001:db8::/32")]).unwrap();
        assert_eq!(s.trusted_proxies.len(), 2);
        assert!(s.trusted_proxies[0].contains("10.42.3.4".parse().unwrap()));
        for bad in [
            "10.42.0.0/40",
            "10.42.0.0/16 2001:db8::/32",
            "ingress-nginx",
            "10.42.0.0/16,/8",
        ] {
            let error = settings(&[("NEKORISU_TRUSTED_PROXIES", bad)]).err().unwrap();
            assert!(error.starts_with("NEKORISU_TRUSTED_PROXIES"), "{bad}: {error}");
        }
    }

    #[test]
    fn the_dns_over_https_resolver_is_named_by_ip_address() {
        let url = |v: &str| settings(&[("NEKORISU_DNS_OVER_HTTPS", v)]).map(|s| s.dns_over_https.unwrap());
        assert_eq!(
            url("https://1.1.1.1/dns-query").unwrap().as_str(),
            "https://1.1.1.1/dns-query"
        );
        assert_eq!(
            url(" https://[2001:db8::53]:8443/dns-query ").unwrap().as_str(),
            "https://[2001:db8::53]:8443/dns-query"
        );
        let error = url("https://dns.example.com/dns-query").unwrap_err();
        assert!(error.contains("must be an IP address"), "{error}");
        for bad in [
            "http://192.0.2.53/dns-query",
            "https://user:pw@192.0.2.53/dns-query",
            "192.0.2.53",
            "not a url",
        ] {
            assert!(url(bad).unwrap_err().starts_with("NEKORISU_DNS_OVER_HTTPS"), "{bad}");
        }
    }

    #[test]
    fn follows_the_data_dir() {
        let s = settings(&[("NEKORISU_DATA_DIR", "/data")]).unwrap();
        assert_eq!(s.database_url, "sqlite:///data/nekorisu.db");
        assert_eq!(s.signing_key_file, PathBuf::from("/data/signing-key.json"));
    }

    #[test]
    fn reads_secrets_from_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        std::fs::write(&path, "from-file\n").unwrap();
        let s = settings(&[
            ("NEKORISU_DEVICE_TOKEN_FILE", path.to_str().unwrap()),
            ("NEKORISU_DEVICE_TOKEN", "ignored"),
        ])
        .unwrap();
        assert_eq!(s.device_token.as_deref(), Some("from-file"));
        assert!(settings(&[("NEKORISU_DEVICE_TOKEN_FILE", "/nonexistent/x")]).is_err());
    }

    #[test]
    fn webdav_root_joins_the_url() {
        let s = settings(&[
            ("NEKORISU_WEBDAV_URL", "https://dav.example/files/"),
            ("NEKORISU_WEBDAV_ROOT", "/backup/"),
            ("NEKORISU_WEBDAV_USER", "u"),
            ("NEKORISU_WEBDAV_PASSWORD", "p"),
        ])
        .unwrap();
        let dav = s.webdav.unwrap();
        assert_eq!(dav.url, "https://dav.example/files/backup");
        assert_eq!(dav.password.as_deref(), Some("p"));
        assert_eq!(dav.for_devices(), "https://dav.example/files/backup");
    }

    #[test]
    fn devices_can_get_their_own_storage_address() {
        let s = settings(&[
            (
                "NEKORISU_WEBDAV_URL",
                "http://openlist.storage.svc.cluster.local:5244/dav",
            ),
            ("NEKORISU_DEVICE_WEBDAV_URL", "https://dav.example.com/dav/"),
        ])
        .unwrap();
        assert_eq!(s.webdav.unwrap().for_devices(), "https://dav.example.com/dav");
        assert!(
            settings(&[
                ("NEKORISU_WEBDAV_URL", "https://dav.example/files"),
                ("NEKORISU_DEVICE_WEBDAV_URL", "https://u:p@dav.example/files"),
            ])
            .is_err()
        );
        for (url, internal) in [
            ("http://openlist.storage.svc.cluster.local:5244/dav", true),
            ("http://openlist:5244/dav", true),
            ("http://192.168.1.5/dav", true),
            ("http://localhost:8080/dav", true),
            ("https://dav.example.com/dav", false),
            ("https://203.0.113.7/dav", false),
        ] {
            assert_eq!(internal_host(url), internal, "{url}");
        }
    }

    #[test]
    fn the_public_url_is_just_an_origin() {
        let url = |v: &str| settings(&[("NEKORISU_PUBLIC_URL", v)]).map(|s| s.public_url.unwrap());
        assert_eq!(
            url("https://nekorisu.example.com").unwrap(),
            "https://nekorisu.example.com"
        );
        assert_eq!(
            url("https://Nekorisu.Example.com:8443/").unwrap(),
            "https://nekorisu.example.com:8443"
        );
        assert_eq!(
            url("https://nekorisu.example.com:443").unwrap(),
            "https://nekorisu.example.com"
        );
        assert_eq!(url("http://localhost:8080").unwrap(), "http://localhost:8080");
        assert_eq!(url("http://127.0.0.1:8080").unwrap(), "http://127.0.0.1:8080");
        for bad in [
            "http://nekorisu.example.com",
            "https://nekorisu.example.com/panel",
            "https://nekorisu.example.com/?x=1",
            "https://me@nekorisu.example.com",
            "nekorisu.example.com",
        ] {
            assert!(url(bad).unwrap_err().contains("NEKORISU_PUBLIC_URL"), "{bad}");
        }
        let off = settings(&[("NEKORISU_OAUTH_CLIENT_METADATA_DOCUMENTS", "false")]).unwrap();
        assert!(!off.oauth_metadata_documents);
    }

    #[test]
    fn refuses_what_it_cannot_do() {
        assert!(
            settings(&[("DATABASE_URL", "postgres://x")])
                .unwrap_err()
                .contains("SQLite")
        );
        assert!(settings(&[("NEKORISU_HTTP_ADDR", "nope")]).is_err());
        assert!(settings(&[("NEKORISU_PUBLIC_CONFIG", "maybe")]).is_err());
        assert!(settings(&[("NEKORISU_GRPC_TLS_CERT_FILE", "c")]).is_err());
        assert!(settings(&[("NEKORISU_PUBLIC_CONFIG", "true")]).unwrap().public_config);
    }
}
