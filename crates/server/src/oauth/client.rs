//! Who is asking, and where they may be sent back to.
//!
//! A client either registered here (RFC 7591, `/oauth/register`) or names
//! itself with an https URL whose JSON document describes it (OAuth Client ID
//! Metadata Documents). Fetching that URL is a request the server makes on a
//! stranger's behalf, so [`HttpFetcher`] only talks to public addresses.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use url::{Host, Url};

use super::dns::DohResolver;

/// A registered client may list at most this many redirect URIs.
pub const MAX_REDIRECT_URIS: usize = 10;
/// A metadata document may list more (one per platform); the rest are ignored.
const MAX_DOCUMENT_REDIRECT_URIS: usize = 50;
const MAX_URI_LEN: usize = 2000;
const MAX_CLIENT_ID_LEN: usize = 1024;
const MAX_NAME_CHARS: usize = 80;

#[derive(Clone, Debug)]
pub struct Client {
    pub id: String,
    /// Cleaned for display; may be empty for a registered client that gave none.
    pub name: String,
    pub redirect_uris: Vec<String>,
}

impl Client {
    /// Its id is an https URL (a Client ID Metadata Document).
    pub fn is_metadata_document(&self) -> bool {
        is_url_client_id(&self.id)
    }
}

/// Whether a client id is URL-shaped, and so names a metadata document.
pub fn is_url_client_id(id: &str) -> bool {
    let lower = id.get(..8).unwrap_or_default().to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://")
}

/// What a person sees as the name: no control or direction-changing
/// characters (they can make one name look like another), and not too long.
pub fn clean_name(name: &str) -> String {
    let kept: String = name
        .chars()
        .filter(|c| !c.is_control() && !is_bidi_control(*c))
        .take(MAX_NAME_CHARS)
        .collect();
    kept.trim().to_string()
}

fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

fn is_loopback_host(url: &Url) -> bool {
    match url.host() {
        Some(Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(ip)) => ip == Ipv4Addr::LOCALHOST,
        Some(Host::Ipv6(ip)) => ip == Ipv6Addr::LOCALHOST,
        None => false,
    }
}

/// Whether a URI may be registered as a redirect URI: https anywhere, or http
/// on the loopback interface (`localhost`, `127.0.0.1`, `[::1]`) for programs
/// on the user's own computer. No fragment, no user name or password.
pub fn valid_redirect_uri(text: &str) -> bool {
    if text.len() > MAX_URI_LEN {
        return false;
    }
    let Ok(url) = Url::parse(text) else {
        return false;
    };
    if url.fragment().is_some() || !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    match url.scheme() {
        "https" => url.host().is_some(),
        "http" => is_loopback_host(&url),
        _ => false,
    }
}

/// Whether `given` is one of the client's redirect URIs. Exact string match,
/// except that a loopback http URI matches on any port (RFC 8252 §7.3: the
/// program takes whatever port is free when it starts), with the same host,
/// path and query.
pub fn redirect_matches(registered: &[String], given: &str) -> bool {
    if registered.iter().any(|r| r == given) {
        return true;
    }
    if !valid_redirect_uri(given) {
        return false;
    }
    let Ok(g) = Url::parse(given) else {
        return false;
    };
    if g.scheme() != "http" {
        return false;
    }
    registered.iter().any(|r| {
        Url::parse(r).is_ok_and(|r| {
            r.scheme() == "http"
                && is_loopback_host(&r)
                && r.host() == g.host()
                && r.path() == g.path()
                && r.query() == g.query()
        })
    })
}

/// Where a redirect URI leads, for the consent page: `host[:port]`, and
/// whether that is a program on this computer.
pub fn describe_redirect(uri: &str) -> (String, bool) {
    match Url::parse(uri) {
        Ok(url) => {
            let host = url.host_str().unwrap_or_default().to_string();
            let shown = match url.port() {
                Some(port) => format!("{host}:{port}"),
                None => host,
            };
            (shown, url.scheme() == "http" && is_loopback_host(&url))
        }
        Err(_) => (uri.to_string(), false),
    }
}

/// The origin of a redirect URI as a CSP source (`scheme://host[:port]`), so
/// the consent form may be redirected there. None for IPv6 hosts, which CSP
/// source expressions cannot name.
pub fn csp_origin(uri: &str) -> Option<String> {
    let url = Url::parse(uri).ok()?;
    let host = match url.host()? {
        Host::Ipv6(_) => return None,
        Host::Ipv4(ip) => ip.to_string(),
        Host::Domain(d) => d.to_string(),
    };
    Some(match url.port() {
        Some(port) => format!("{}://{host}:{port}", url.scheme()),
        None => format!("{}://{host}", url.scheme()),
    })
}

/// Checks a URL client id before anything is fetched: https, a host, a path
/// other than `/`, no fragment or user info, and written the way the URL
/// parser writes it (no dot segments, no upper-case host, no default port).
pub fn metadata_document_url(id: &str) -> Result<Url, String> {
    let bad = || "client_id 不是一个可用的应用说明网址（要 https、带路径、不带 # 和用户名）。".to_string();
    if id.len() > MAX_CLIENT_ID_LEN {
        return Err(bad());
    }
    let url = Url::parse(id).map_err(|_| bad())?;
    if url.scheme() != "https"
        || url.host().is_none()
        || url.path() == "/"
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.as_str() != id
    {
        return Err(bad());
    }
    Ok(url)
}

/// Reads a client's metadata document. It must name itself (`client_id` equal
/// to its URL), have a `client_name` and at least one redirect URI this server
/// accepts, and be a public client (no secret, nothing but `none` at the token
/// endpoint). The reason is shown on the error page.
pub fn parse_metadata_document(id: &str, body: &[u8]) -> Result<Client, String> {
    let doc: Value = serde_json::from_slice(body).map_err(|_| "应用的说明文档不是 JSON。".to_string())?;
    let obj = doc
        .as_object()
        .ok_or_else(|| "应用的说明文档不是 JSON 对象。".to_string())?;
    if obj.get("client_id").and_then(Value::as_str) != Some(id) {
        return Err("应用说明文档里的 client_id 和它自己的网址不一致。".to_string());
    }
    let name = obj
        .get("client_name")
        .and_then(Value::as_str)
        .map(clean_name)
        .filter(|n| !n.is_empty())
        .ok_or_else(|| "应用的说明文档没有 client_name。".to_string())?;
    if obj.contains_key("client_secret") {
        return Err("应用的说明文档里不能有 client_secret。".to_string());
    }
    match obj.get("token_endpoint_auth_method").and_then(Value::as_str) {
        None | Some("none") => {}
        Some(_) => return Err("这台服务端只支持公开客户端（token_endpoint_auth_method 为 none）。".to_string()),
    }
    let redirect_uris: Vec<String> = obj
        .get("redirect_uris")
        .and_then(Value::as_array)
        .ok_or_else(|| "应用的说明文档没有 redirect_uris。".to_string())?
        .iter()
        .filter_map(Value::as_str)
        .filter(|u| valid_redirect_uri(u))
        .take(MAX_DOCUMENT_REDIRECT_URIS)
        .map(str::to_owned)
        .collect();
    if redirect_uris.is_empty() {
        return Err("应用的说明文档里没有这台服务端接受的返回地址（https，或本机的 http）。".to_string());
    }
    Ok(Client {
        id: id.to_string(),
        name,
        redirect_uris,
    })
}

/// Fetches a client's metadata document. Injectable so tests need no network.
#[async_trait]
pub trait MetadataFetcher: Send + Sync {
    /// The body at `url`, or why it could not be had (for the log).
    async fn fetch(&self, url: &Url) -> Result<Vec<u8>, String>;
}

/// The real fetcher: https only, public addresses only (checked after DNS
/// resolution, and the connection is pinned to the checked addresses so a
/// second lookup cannot change them; TLS still checks the certificate against
/// the host name), no proxy, a few redirects at most (each checked the same
/// way), a short timeout and a small size cap.
pub struct HttpFetcher {
    pub timeout: Duration,
    pub max_bytes: usize,
    pub max_redirects: usize,
    /// Look host names up through this DNS-over-HTTPS resolver instead of the
    /// system's (`NEKORISU_DNS_OVER_HTTPS`), on every redirect too.
    pub dns_over_https: Option<DohResolver>,
}

impl Default for HttpFetcher {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(5),
            max_bytes: 64 * 1024,
            max_redirects: 3,
            dns_over_https: None,
        }
    }
}

#[async_trait]
impl MetadataFetcher for HttpFetcher {
    async fn fetch(&self, url: &Url) -> Result<Vec<u8>, String> {
        // reqwest is built without a bundled TLS provider; use ring, as the storage does.
        let _ = rustls::crypto::ring::default_provider().install_default();
        tokio::time::timeout(self.timeout, self.fetch_following(url.clone()))
            .await
            .map_err(|_| "timed out".to_string())?
    }
}

impl HttpFetcher {
    async fn fetch_following(&self, mut url: Url) -> Result<Vec<u8>, String> {
        for _ in 0..=self.max_redirects {
            if url.scheme() != "https" {
                return Err(format!("not https: {url}"));
            }
            let (domain, addrs) = public_addresses(&url, self.dns_over_https.as_ref()).await?;
            let mut builder = reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .connect_timeout(self.timeout)
                .timeout(self.timeout)
                .user_agent(concat!("nekorisu-server/", env!("CARGO_PKG_VERSION")));
            if let Some(domain) = &domain {
                builder = builder.resolve_to_addrs(domain, &addrs);
            }
            let client = builder
                .build()
                .map_err(|e| format!("cannot build the HTTP client: {e}"))?;
            let mut response = client
                .get(url.clone())
                .header(reqwest::header::ACCEPT, "application/json")
                .send()
                .await
                .map_err(|e| format!("request failed: {e}"))?;
            let status = response.status();
            if status.is_redirection() {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or("a redirect without a Location")?;
                url = url
                    .join(location)
                    .map_err(|_| "a redirect to a bad address".to_string())?;
                continue;
            }
            if !status.is_success() {
                return Err(format!("HTTP {}", status.as_u16()));
            }
            if response.content_length().is_some_and(|n| n > self.max_bytes as u64) {
                return Err("the document is too big".to_string());
            }
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|e| format!("reading failed: {e}"))? {
                if body.len() + chunk.len() > self.max_bytes {
                    return Err("the document is too big".to_string());
                }
                body.extend_from_slice(&chunk);
            }
            return Ok(body);
        }
        Err("too many redirects".to_string())
    }
}

/// The addresses to connect to for `url`, all of them public. A domain is
/// returned with them so the connection can be pinned; an IP host needs no
/// lookup. Names are looked up through `doh` when given, else the system.
pub async fn public_addresses(
    url: &Url,
    doh: Option<&DohResolver>,
) -> Result<(Option<String>, Vec<SocketAddr>), String> {
    let port = url.port_or_known_default().unwrap_or(443);
    let refuse = || format!("not a public address: {}", url.host_str().unwrap_or_default());
    match url.host() {
        Some(Host::Ipv4(ip)) if is_public(IpAddr::V4(ip)) => Ok((None, vec![SocketAddr::new(ip.into(), port)])),
        Some(Host::Ipv6(ip)) if is_public(IpAddr::V6(ip)) => Ok((None, vec![SocketAddr::new(ip.into(), port)])),
        Some(Host::Domain(domain)) => {
            let lower = domain.to_ascii_lowercase();
            if lower == "localhost" || lower.ends_with(".localhost") {
                return Err(refuse());
            }
            let found: Vec<IpAddr> = match doh {
                Some(doh) => doh.lookup(domain).await.map_err(|why| {
                    format!("cannot resolve {domain} through DNS over HTTPS (NEKORISU_DNS_OVER_HTTPS): {why}")
                })?,
                None => tokio::net::lookup_host((domain, port))
                    .await
                    .map_err(|_| format!("cannot resolve {domain}"))?
                    .map(|a| a.ip())
                    .collect(),
            };
            if found.is_empty() {
                return Err(format!("cannot resolve {domain}"));
            }
            // Only the public ones are used, and the connection is pinned to them.
            let public: Vec<SocketAddr> = found
                .iter()
                .filter(|ip| is_public(**ip))
                .map(|ip| SocketAddr::new(*ip, port))
                .collect();
            if public.is_empty() {
                return Err(not_public(domain, &found, doh.is_some()));
            }
            Ok((Some(domain.to_string()), public))
        }
        _ => Err(refuse()),
    }
}

/// Why a name was refused. When the system resolver gave only fake-IP
/// addresses, says so: the real address is hidden, and DNS over HTTPS finds it.
fn not_public(domain: &str, found: &[IpAddr], through_doh: bool) -> String {
    let mut why = format!("not a public address: {domain}");
    if !through_doh && found.iter().any(|ip| is_fake_ip(*ip)) {
        why.push_str(
            " (the system DNS answered with 198.18.0.0/15 or fc00::/18: probably a fake-IP DNS proxy such as \
             sing-box or Clash, which hides the real address; set NEKORISU_DNS_OVER_HTTPS to a resolver at an \
             IP address, like https://1.1.1.1/dns-query, to look names up past it)",
        );
    }
    why
}

/// The ranges fake-IP DNS proxies hand out by default: 198.18.0.0/15
/// (benchmarking) for IPv4, fc00::/18 (unique local) for IPv6.
fn is_fake_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_benchmarking_v4(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => is_benchmarking_v4(v4),
            None => v6.segments()[0] == 0xfc00 && v6.segments()[1] & 0xc000 == 0,
        },
    }
}

/// 198.18.0.0/15, set aside for benchmarking (RFC 2544).
fn is_benchmarking_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    o[0] == 198 && o[1] & 0xfe == 18
}

/// Whether an address is on the public internet: not loopback, private,
/// shared (CGNAT, which tailnets use), link-local (cloud metadata),
/// benchmarking, multicast, documentation or reserved, in either family,
/// including IPv4 addresses written inside IPv6 ones.
///
/// 198.18.0.0/15 is where fake-IP DNS proxies (sing-box, Clash) put every
/// outside name; such an address says nothing about where a connection
/// really goes, so it is not public. NEKORISU_DNS_OVER_HTTPS gets the real one.
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => {
            if v6.is_loopback() || v6.is_unspecified() {
                return false;
            }
            // ::ffff:a.b.c.d and the deprecated ::a.b.c.d
            if let Some(v4) = v6.to_ipv4() {
                return is_public_v4(v4);
            }
            let s = v6.segments();
            let embedded = |hi: u16, lo: u16| Ipv4Addr::new((hi >> 8) as u8, hi as u8, (lo >> 8) as u8, lo as u8);
            // NAT64 (64:ff9b::/96) and 6to4 (2002::/16) carry an IPv4 address.
            if s[0] == 0x64 && s[1] == 0xff9b {
                return is_public_v4(embedded(s[6], s[7]));
            }
            if s[0] == 0x2002 {
                return is_public_v4(embedded(s[1], s[2]));
            }
            let global_unicast = (s[0] & 0xe000) == 0x2000;
            let documentation = s[0] == 0x2001 && s[1] == 0x0db8;
            let teredo = s[0] == 0x2001 && s[1] == 0;
            let benchmarking = s[0] == 0x2001 && s[1] == 0x0002 && s[2] == 0;
            global_unicast && !documentation && !teredo && !benchmarking
        }
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let o = ip.octets();
    !(ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_multicast()
        || o[0] == 0
        || (o[0] == 100 && (o[1] & 0xc0) == 64)
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
        || is_benchmarking_v4(ip)
        || o[0] >= 240)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_uris_are_https_or_loopback_http() {
        for good in [
            "https://app.example.com/callback",
            "http://localhost:33418/callback",
            "http://127.0.0.1/callback",
            "http://[::1]:8000/cb?x=1",
        ] {
            assert!(valid_redirect_uri(good), "{good}");
        }
        for bad in [
            "http://app.example.com/callback",
            "http://127.0.0.2/callback",
            "http://localhost.evil.example/cb",
            "https://app.example.com/cb#frag",
            "https://user:pw@app.example.com/cb",
            "myapp://callback",
            "javascript:alert(1)",
            "not a url",
        ] {
            assert!(!valid_redirect_uri(bad), "{bad}");
        }
    }

    #[test]
    fn loopback_matches_on_any_port_and_nothing_else_does() {
        let registered = vec![
            "http://localhost/callback".to_string(),
            "https://app.example.com/cb".to_string(),
        ];
        assert!(redirect_matches(&registered, "http://localhost:51234/callback"));
        assert!(redirect_matches(&registered, "http://localhost/callback"));
        assert!(redirect_matches(&registered, "https://app.example.com/cb"));
        // Another path, host, scheme or query is not the same URI.
        assert!(!redirect_matches(&registered, "http://localhost:51234/other"));
        assert!(!redirect_matches(&registered, "http://127.0.0.1:51234/callback"));
        assert!(!redirect_matches(&registered, "http://localhost:51234/callback?x=1"));
        assert!(!redirect_matches(&registered, "https://app.example.com:8443/cb"));
        assert!(!redirect_matches(&registered, "https://app.example.com/cb/"));
        assert!(!redirect_matches(&registered, "https://evil.example/cb"));
    }

    #[test]
    fn describes_where_a_redirect_goes() {
        assert_eq!(
            describe_redirect("http://127.0.0.1:4242/cb"),
            ("127.0.0.1:4242".to_string(), true)
        );
        assert_eq!(
            describe_redirect("https://app.example.com/cb"),
            ("app.example.com".to_string(), false)
        );
        assert_eq!(
            csp_origin("http://localhost:4242/cb").as_deref(),
            Some("http://localhost:4242")
        );
        assert_eq!(csp_origin("http://[::1]:4242/cb"), None);
    }

    #[test]
    fn names_lose_control_and_direction_characters() {
        assert_eq!(clean_name("  Good\u{202e}App\n "), "GoodApp");
        assert_eq!(clean_name(&"x".repeat(500)).chars().count(), MAX_NAME_CHARS);
    }

    #[test]
    fn metadata_document_urls_are_strict() {
        assert!(metadata_document_url("https://app.example.com/oauth/client.json").is_ok());
        for bad in [
            "http://app.example.com/client.json",
            "https://app.example.com/",
            "https://app.example.com",
            "https://APP.example.com/client.json",
            "https://app.example.com:443/client.json",
            "https://app.example.com/a/../client.json",
            "https://app.example.com/client.json#x",
            "https://me@app.example.com/client.json",
        ] {
            assert!(metadata_document_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn metadata_documents_must_describe_themselves() {
        let id = "https://app.example.com/client.json";
        let doc = |v: Value| serde_json::to_vec(&v).unwrap();
        let ok = parse_metadata_document(
            id,
            &doc(serde_json::json!({
                "client_id": id, "client_name": "Example Agent",
                "redirect_uris": ["http://localhost/callback", "myapp://ignored"],
                "token_endpoint_auth_method": "none"
            })),
        )
        .unwrap();
        assert_eq!(ok.name, "Example Agent");
        assert_eq!(ok.redirect_uris, ["http://localhost/callback"]);

        let base = serde_json::json!({ "client_id": id, "client_name": "X", "redirect_uris": ["http://localhost/cb"] });
        let with = |key: &str, value: Value| {
            let mut v = base.clone();
            v[key] = value;
            doc(v)
        };
        assert!(parse_metadata_document(id, &with("client_id", "https://other.example/client.json".into())).is_err());
        assert!(parse_metadata_document(id, &with("client_name", "".into())).is_err());
        assert!(parse_metadata_document(id, &with("redirect_uris", serde_json::json!(["myapp://x"]))).is_err());
        assert!(parse_metadata_document(id, &with("client_secret", "s".into())).is_err());
        assert!(parse_metadata_document(id, &with("token_endpoint_auth_method", "private_key_jwt".into())).is_err());
        assert!(parse_metadata_document(id, b"not json").is_err());
        assert!(parse_metadata_document(id, &base.to_string().into_bytes()).is_ok());
    }

    #[test]
    fn only_public_addresses_are_public() {
        for private in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.1",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "100.100.100.100",
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "192.0.2.1",
            "::1",
            "::",
            "fd00::1",
            "fe80::1",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "64:ff9b::a00:1",
            "2002:c0a8:101::1",
            "2001:db8::1",
            // Benchmarking, where fake-IP DNS proxies put every outside name.
            "198.18.0.6",
            "198.19.255.255",
            "::ffff:198.18.0.6",
            "64:ff9b::c612:6",
            "fc00::6",
        ] {
            assert!(!is_public(private.parse().unwrap()), "{private}");
        }
        for public in [
            "1.1.1.1",
            "8.8.8.8",
            "198.17.255.255",
            "198.20.0.1",
            "2606:4700:4700::1111",
            "::ffff:1.1.1.1",
        ] {
            assert!(is_public(public.parse().unwrap()), "{public}");
        }
    }

    #[test]
    fn a_fake_ip_answer_says_how_to_get_past_it() {
        let ips = |list: &[&str]| list.iter().map(|t| t.parse().unwrap()).collect::<Vec<IpAddr>>();
        let fakes: [&[&str]; 4] = [
            &["198.18.0.6"],
            &["fc00::6"],
            &["10.0.0.1", "198.19.1.1"],
            &["::ffff:198.18.3.4"],
        ];
        for fake in fakes {
            let why = not_public("app.example.com", &ips(fake), false);
            assert!(why.starts_with("not a public address: app.example.com"), "{why}");
            assert!(
                why.contains("fake-IP") && why.contains("NEKORISU_DNS_OVER_HTTPS"),
                "{fake:?}: {why}"
            );
        }
        // Other private answers, or answers that came through DNS over HTTPS: no hint.
        let others: [(&[&str], bool); 3] = [
            (&["10.0.0.1", "fd00::1"], false),
            (&["198.18.0.6"], true),
            (&["fc00:4000::1"], false),
        ];
        for (found, doh) in others {
            let why = not_public("app.example.com", &ips(found), doh);
            assert_eq!(why, "not a public address: app.example.com", "{found:?}");
        }
    }

    #[tokio::test]
    async fn the_real_fetcher_refuses_what_is_not_public() {
        let fetcher = HttpFetcher::default();
        for url in [
            "https://127.0.0.1/client.json",
            "https://localhost/client.json",
            "https://app.localhost/client.json",
            "https://10.0.0.1/client.json",
            "https://169.254.169.254/latest/meta-data",
            "https://100.100.1.1/client.json",
            "https://[::1]/client.json",
            "https://[fd12::1]/client.json",
            "https://[::ffff:192.168.0.1]/client.json",
            "http://app.example.com/client.json",
        ] {
            let error = fetcher.fetch(&Url::parse(url).unwrap()).await.unwrap_err();
            assert!(
                error.starts_with("not a public address") || error.starts_with("not https"),
                "{url}: {error}"
            );
        }
    }
}
