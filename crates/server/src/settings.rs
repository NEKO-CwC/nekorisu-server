//! Everything the container is told through environment variables.

use std::net::SocketAddr;
use std::path::PathBuf;

#[derive(Clone)]
pub struct WebDavSettings {
    pub url: String,
    pub user: Option<String>,
    pub password: Option<String>,
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
    pub mcp_token: Option<String>,
    pub public_config: bool,
    pub signing_key_file: PathBuf,
    pub cookie_secure: bool,
    /// The address devices should use, for the connect command in the panel.
    pub public_url: Option<String>,
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
            .field("mcp_token_set", &self.mcp_token.is_some())
            .field("public_config", &self.public_config)
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
                Some(WebDavSettings {
                    url,
                    user: var("NEKORISU_WEBDAV_USER"),
                    password: secret("NEKORISU_WEBDAV_PASSWORD")?,
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
            mcp_token: secret("NEKORISU_MCP_TOKEN")?,
            public_config: boolean("NEKORISU_PUBLIC_CONFIG", false)?,
            signing_key_file: var("NEKORISU_SIGNING_KEY_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|| data_dir.join("signing-key.json")),
            cookie_secure: boolean("NEKORISU_COOKIE_SECURE", false)?,
            public_url: var("NEKORISU_PUBLIC_URL"),
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
        assert!(s.webdav.is_none() && s.device_token.is_none() && s.mcp_token.is_none());
        assert!(!s.public_config && !s.cookie_secure);
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
            ("NEKORISU_MCP_TOKEN", "plain"),
        ])
        .unwrap();
        assert_eq!(s.device_token.as_deref(), Some("from-file"));
        assert_eq!(s.mcp_token.as_deref(), Some("plain"));
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
