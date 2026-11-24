use std::ops::Range;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use bytes::{Bytes, BytesMut};
use percent_encoding::percent_decode_str;
use quick_xml::Reader;
use quick_xml::XmlVersion;
use quick_xml::events::Event;
use reqwest::{Method, StatusCode};
use url::Url;

use crate::{Capabilities, Entry, Error, Provider, Result, clean};

/// Reads beyond this many bytes are refused, so a server that ignores `Range`
/// cannot make us hold a whole huge file in memory.
const MAX_RESPONSE: u64 = 256 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct WebDavConfig {
    /// The directory that holds `nekorisu/v1`, e.g. `https://dav.example/files/`.
    pub url: String,
    pub user: Option<String>,
    pub password: Option<String>,
    pub timeout: Duration,
}

impl WebDavConfig {
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            user: None,
            password: None,
            timeout: Duration::from_secs(60),
        }
    }
}

const RANGE_UNKNOWN: u8 = 0;
const RANGE_YES: u8 = 1;
const RANGE_NO: u8 = 2;

pub struct WebDav {
    client: reqwest::Client,
    base: Url,
    user: Option<String>,
    password: Option<String>,
    range: AtomicU8,
}

impl WebDav {
    pub fn new(config: WebDavConfig) -> Result<Self> {
        // reqwest is built without a bundled TLS provider; use ring. Already
        // installed (by us or the caller) is fine.
        let _ = rustls::crypto::ring::default_provider().install_default();
        let mut base =
            Url::parse(&config.url).map_err(|_| Error::Other("the WebDAV address is not a valid URL".into()))?;
        if !matches!(base.scheme(), "http" | "https") {
            return Err(Error::Other("the WebDAV address must be http or https".into()));
        }
        base.set_query(None);
        base.set_fragment(None);
        // Credentials belong in the config fields, not in the URL we may print.
        let _ = base.set_username("");
        let _ = base.set_password(None);
        if !base.path().ends_with('/') {
            let path = format!("{}/", base.path());
            base.set_path(&path);
        }
        let client = reqwest::Client::builder()
            .timeout(config.timeout)
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|e| Error::Other(format!("cannot build the HTTP client: {e}")))?;
        Ok(Self {
            client,
            base,
            user: config.user.filter(|u| !u.is_empty()),
            password: config.password,
            range: AtomicU8::new(RANGE_UNKNOWN),
        })
    }

    fn url_for(&self, path: &str) -> Url {
        let mut url = self.base.clone();
        if !path.is_empty() {
            let mut segments = url.path_segments_mut().expect("http url has a path");
            segments.pop_if_empty();
            segments.extend(path.split('/'));
        }
        url
    }

    fn request(&self, method: Method, url: Url) -> reqwest::RequestBuilder {
        let req = self.client.request(method, url);
        match &self.user {
            Some(user) => req.basic_auth(user, self.password.as_deref()),
            None => req,
        }
    }

    fn status_error(status: StatusCode, path: &str) -> Error {
        match status {
            StatusCode::NOT_FOUND | StatusCode::GONE => Error::NotFound(path.to_string()),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Error::Unauthorized,
            other => Error::Other(format!("unexpected HTTP status {other} for {path:?}")),
        }
    }

    async fn propfind(&self, path: &str, depth: &str) -> Result<Option<Vec<Response>>> {
        let url = self.url_for(path);
        let response = self
            .request(Method::from_bytes(b"PROPFIND").expect("valid method"), url)
            .header("Depth", depth)
            .header("Content-Type", "application/xml")
            .body(PROPFIND_BODY)
            .send()
            .await
            .map_err(net_error)?;
        let status = response.status();
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if status != StatusCode::MULTI_STATUS && !status.is_success() {
            return Err(Self::status_error(status, path));
        }
        let body = read_capped(response, 32 * 1024 * 1024).await?;
        let text = String::from_utf8_lossy(&body);
        Ok(Some(parse_multistatus(&text)?))
    }

    /// The path of an href relative to the storage root, if it lies under it.
    fn relative(&self, href: &str) -> Option<String> {
        let path = match Url::parse(href) {
            Ok(url) => url.path().to_string(),
            Err(_) => href.split(['?', '#']).next().unwrap_or(href).to_string(),
        };
        let decoded = percent_decode_str(&path).decode_utf8_lossy().into_owned();
        let base = percent_decode_str(self.base.path()).decode_utf8_lossy().into_owned();
        let base = base.trim_end_matches('/');
        let rest = decoded.strip_prefix(base)?;
        if !rest.is_empty() && !rest.starts_with('/') {
            return None;
        }
        Some(rest.trim_matches('/').to_string())
    }
}

#[async_trait]
impl Provider for WebDav {
    fn describe(&self) -> String {
        let host = self.base.host_str().unwrap_or("?");
        let port = self.base.port().map(|p| format!(":{p}")).unwrap_or_default();
        format!("webdav {}://{host}{port}{}", self.base.scheme(), self.base.path())
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            range_read: match self.range.load(Ordering::Relaxed) {
                RANGE_YES => Some(true),
                RANGE_NO => Some(false),
                _ => None,
            },
        }
    }

    async fn list(&self, dir: &str) -> Result<Vec<Entry>> {
        let dir = clean(dir)?;
        let Some(responses) = self.propfind(&dir, "1").await? else {
            return Err(Error::NotFound(dir));
        };
        let mut out = Vec::new();
        for response in responses {
            let Some(path) = self.relative(&response.href) else {
                continue;
            };
            if path == dir {
                continue; // the directory itself
            }
            let name = path.rsplit('/').next().unwrap_or_default().to_string();
            if name.is_empty() {
                continue;
            }
            out.push(Entry {
                name,
                path,
                is_dir: response.is_dir,
                size: response.size,
                modified_ms: response.modified_ms,
            });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    async fn stat(&self, path: &str) -> Result<Option<Entry>> {
        let path = clean(path)?;
        let Some(responses) = self.propfind(&path, "0").await? else {
            return Ok(None);
        };
        let Some(first) = responses.into_iter().next() else {
            return Ok(None);
        };
        Ok(Some(Entry {
            name: path.rsplit('/').next().unwrap_or_default().to_string(),
            path,
            is_dir: first.is_dir,
            size: first.size,
            modified_ms: first.modified_ms,
        }))
    }

    async fn read(&self, path: &str, range: Option<Range<u64>>) -> Result<Bytes> {
        let path = clean(path)?;
        if let Some(r) = &range
            && r.end <= r.start
        {
            return Ok(Bytes::new());
        }
        let mut request = self.request(Method::GET, self.url_for(&path));
        if let Some(r) = &range {
            request = request.header("Range", format!("bytes={}-{}", r.start, r.end - 1));
        }
        let response = request.send().await.map_err(net_error)?;
        let status = response.status();
        match (&range, status) {
            (Some(_), StatusCode::PARTIAL_CONTENT) => {
                self.range.store(RANGE_YES, Ordering::Relaxed);
                read_capped(response, MAX_RESPONSE).await
            }
            (Some(_), StatusCode::RANGE_NOT_SATISFIABLE) => Ok(Bytes::new()),
            (Some(r), StatusCode::OK) => {
                // The server ignored Range and sent the whole file.
                self.range.store(RANGE_NO, Ordering::Relaxed);
                let body = read_capped(response, MAX_RESPONSE).await?;
                let len = body.len() as u64;
                let start = r.start.min(len) as usize;
                let end = r.end.min(len).max(start as u64) as usize;
                Ok(body.slice(start..end))
            }
            (None, s) if s.is_success() => read_capped(response, MAX_RESPONSE).await,
            (_, s) => Err(Self::status_error(s, &path)),
        }
    }
}

fn net_error(error: reqwest::Error) -> Error {
    // `without_url` so a URL can never leak into a log line.
    Error::Other(format!("network error: {}", error.without_url()))
}

async fn read_capped(mut response: reqwest::Response, limit: u64) -> Result<Bytes> {
    if let Some(len) = response.content_length()
        && len > limit
    {
        return Err(Error::TooLarge(format!("{len} bytes")));
    }
    let mut out = BytesMut::new();
    while let Some(chunk) = response.chunk().await.map_err(net_error)? {
        if out.len() as u64 + chunk.len() as u64 > limit {
            return Err(Error::TooLarge(format!("more than {limit} bytes")));
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out.freeze())
}

const PROPFIND_BODY: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:"><d:prop><d:resourcetype/><d:getcontentlength/><d:getlastmodified/></d:prop></d:propfind>"#;

#[derive(Debug, Default)]
struct Response {
    href: String,
    is_dir: bool,
    size: u64,
    modified_ms: Option<i64>,
}

fn parse_multistatus(xml: &str) -> Result<Vec<Response>> {
    let mut reader = Reader::from_str(xml);
    let mut out = Vec::new();
    let mut current: Option<Response> = None;
    let mut text = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                match local_name(&e).as_str() {
                    "response" => current = Some(Response::default()),
                    "collection" => {
                        if let Some(r) = current.as_mut() {
                            r.is_dir = true;
                        }
                    }
                    _ => {}
                }
                text.clear();
            }
            Ok(Event::Empty(e)) => {
                if local_name(&e) == "collection"
                    && let Some(r) = current.as_mut()
                {
                    r.is_dir = true;
                }
            }
            Ok(Event::Text(t)) => text.push_str(&t.xml_content(XmlVersion::Implicit1_0)),
            Ok(Event::GeneralRef(r)) => {
                let name: &str = &r;
                match name {
                    "amp" => text.push('&'),
                    "lt" => text.push('<'),
                    "gt" => text.push('>'),
                    "quot" => text.push('"'),
                    "apos" => text.push('\''),
                    other => {
                        let digits = other.strip_prefix('#');
                        let code = match digits {
                            Some(d) => match d.strip_prefix('x') {
                                Some(hex) => u32::from_str_radix(hex, 16).ok(),
                                None => d.parse().ok(),
                            },
                            None => None,
                        };
                        if let Some(c) = code.and_then(char::from_u32) {
                            text.push(c);
                        }
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref().as_bytes()).into_owned();
                if let Some(r) = current.as_mut() {
                    let value = text.trim();
                    match name.as_str() {
                        "href" => r.href = value.to_string(),
                        "getcontentlength" => r.size = value.parse().unwrap_or(0),
                        "getlastmodified" => {
                            r.modified_ms = httpdate::parse_http_date(value)
                                .ok()
                                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                                .map(|d| d.as_millis() as i64);
                        }
                        _ => {}
                    }
                }
                if name == "response"
                    && let Some(r) = current.take()
                {
                    out.push(r);
                }
                text.clear();
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(xml_error(e)),
        }
    }
    Ok(out)
}

fn local_name(e: &quick_xml::events::BytesStart<'_>) -> String {
    e.local_name().as_ref().to_string()
}

fn xml_error(error: impl std::fmt::Display) -> Error {
    Error::Other(format!("cannot read the WebDAV listing: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_listing() {
        let xml = r#"<?xml version="1.0"?>
<D:multistatus xmlns:D="DAV:">
 <D:response><D:href>/dav/root/</D:href><D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop></D:propstat></D:response>
 <D:response><D:href>/dav/root/a%20b.json</D:href><D:propstat><D:prop>
   <D:resourcetype/><D:getcontentlength>12</D:getcontentlength>
   <D:getlastmodified>Sun, 06 Nov 1994 08:49:37 GMT</D:getlastmodified></D:prop></D:propstat></D:response>
 <D:response><D:href>http://h/dav/root/sub/</D:href><D:propstat><D:prop><D:resourcetype><D:collection></D:collection></D:resourcetype></D:prop></D:propstat></D:response>
</D:multistatus>"#;
        let parsed = parse_multistatus(xml).unwrap();
        assert_eq!(parsed.len(), 3);
        assert!(parsed[0].is_dir);
        assert_eq!(parsed[1].size, 12);
        assert_eq!(parsed[1].modified_ms, Some(784_111_777_000));
        assert!(parsed[2].is_dir);

        let dav = WebDav::new(WebDavConfig::new("http://h/dav/root")).unwrap();
        assert_eq!(dav.relative("/dav/root/a%20b.json").as_deref(), Some("a b.json"));
        assert_eq!(dav.relative("http://h/dav/root/sub/").as_deref(), Some("sub"));
        assert_eq!(dav.relative("/dav/root/").as_deref(), Some(""));
        assert_eq!(dav.relative("/dav/rootx/a").as_deref(), None);
        assert_eq!(dav.relative("/other/a").as_deref(), None);
    }

    #[test]
    fn builds_urls_with_encoded_segments() {
        let dav = WebDav::new(WebDavConfig::new("http://h/dav/root/")).unwrap();
        assert_eq!(dav.url_for("a b/c#d").as_str(), "http://h/dav/root/a%20b/c%23d");
        assert_eq!(dav.url_for("").as_str(), "http://h/dav/root/");
    }

    #[test]
    fn describe_hides_credentials() {
        let mut config = WebDavConfig::new("https://user:secret@h:8443/dav");
        config.user = Some("user".into());
        config.password = Some("secret".into());
        let dav = WebDav::new(config).unwrap();
        let text = dav.describe();
        assert_eq!(text, "webdav https://h:8443/dav/");
        assert!(!text.contains("secret"));
    }
}
