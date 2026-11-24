//! Who is asking: the client address that rate limits are counted by.
//!
//! The server sees the TCP peer. Behind a reverse proxy (ingress-nginx in
//! k8s) that is the proxy, so the proxy's headers are believed, but only when
//! the peer is inside `NEKORISU_TRUSTED_PROXIES`: anybody can send them.
//!
//! - The peer is not trusted (or no proxies are trusted): the peer.
//! - The peer is trusted: `X-Real-IP`, when it holds exactly one valid
//!   address; else the rightmost `X-Forwarded-For` entry that is not itself
//!   trusted (each proxy appends whom it talked to, so everything left of
//!   that was written by the client and could be anything); else the peer.
//!
//! IPv4 addresses written as IPv6 (`::ffff:192.0.2.1`, which a dual-stack
//! listener reports) are turned back into IPv4 everywhere.
//!
//! Without a peer address (the router driven directly, as in tests, with no
//! listener to say who connected) there is no client address; callers count
//! such requests together under one key ([`crate::limit::RateKey::Unknown`]).

use std::convert::Infallible;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::HeaderMap;
use axum::http::request::Parts;

use crate::state::Shared;

/// An address range, like `10.42.0.0/16` or `2001:db8::/32`; a bare address
/// is a range of one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cidr {
    network: IpAddr,
    prefix: u8,
}

impl Cidr {
    /// Host bits are dropped (`10.42.1.5/16` is `10.42.0.0/16`), and an IPv4
    /// range written as IPv6 (`::ffff:10.0.0.0/104`) is that IPv4 range.
    pub fn parse(text: &str) -> Result<Cidr, String> {
        let text = text.trim();
        let (addr, prefix) = match text.split_once('/') {
            Some((addr, prefix)) => (addr, Some(prefix)),
            None => (text, None),
        };
        let ip: IpAddr = addr
            .parse()
            .map_err(|_| format!("{text:?} is not an address range like 10.42.0.0/16 or 2001:db8::/32"))?;
        let max = if ip.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            None => max,
            Some(p) => p
                .parse::<u8>()
                .ok()
                .filter(|p| *p <= max)
                .ok_or_else(|| format!("{text:?}: the prefix length must be 0 to {max}"))?,
        };
        let (ip, prefix) = match ip {
            IpAddr::V6(v6) if prefix >= 96 => match v6.to_ipv4_mapped() {
                Some(v4) => (IpAddr::V4(v4), prefix - 96),
                None => (ip, prefix),
            },
            _ => (ip, prefix),
        };
        Ok(Cidr {
            network: mask(ip, prefix),
            prefix,
        })
    }

    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = normalize(ip);
        ip.is_ipv4() == self.network.is_ipv4() && mask(ip, self.prefix) == self.network
    }
}

impl std::fmt::Display for Cidr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.network, self.prefix)
    }
}

fn mask(ip: IpAddr, prefix: u8) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => {
            let keep = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
            IpAddr::V4(Ipv4Addr::from(u32::from(v4) & keep))
        }
        IpAddr::V6(v6) => {
            let keep = u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0);
            IpAddr::V6(Ipv6Addr::from(u128::from(v6) & keep))
        }
    }
}

/// `NEKORISU_TRUSTED_PROXIES`: comma-separated ranges; empty means none.
pub fn parse_trusted_proxies(text: &str) -> Result<Vec<Cidr>, String> {
    text.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(Cidr::parse)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|why| format!("NEKORISU_TRUSTED_PROXIES: {why}"))
}

/// `::ffff:a.b.c.d` is `a.b.c.d`.
pub fn normalize(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        IpAddr::V4(_) => ip,
    }
}

fn trusted(ip: IpAddr, proxies: &[Cidr]) -> bool {
    proxies.iter().any(|range| range.contains(ip))
}

/// One address in a header: `192.0.2.1`, `2001:db8::1`, `[2001:db8::1]`, or
/// either with a port.
fn header_address(text: &str) -> Option<IpAddr> {
    let text = text.trim();
    let ip = text
        .parse::<IpAddr>()
        .ok()
        .or_else(|| text.parse::<SocketAddr>().ok().map(|a| a.ip()))
        .or_else(|| {
            text.strip_prefix('[')
                .and_then(|t| t.strip_suffix(']'))
                .and_then(|t| t.parse::<Ipv6Addr>().ok())
                .map(IpAddr::V6)
        })?;
    Some(normalize(ip))
}

/// The client address of a request with this peer and these headers (see the
/// module documentation). None when the peer is not known.
pub fn client_address(peer: Option<IpAddr>, headers: &HeaderMap, proxies: &[Cidr]) -> Option<IpAddr> {
    let peer = normalize(peer?);
    if !trusted(peer, proxies) {
        return Some(peer);
    }
    let mut real = headers.get_all("x-real-ip").iter();
    if let (Some(value), None) = (real.next(), real.next())
        && let Some(ip) = value.to_str().ok().and_then(header_address)
    {
        return Some(ip);
    }
    // Several X-Forwarded-For headers are one list, in order. A value that is
    // not text, or an entry that is not an address, ends the walk: the chain
    // is garbled there, and whatever is left of it cannot be told apart.
    let mut entries: Vec<Option<IpAddr>> = Vec::new();
    for value in headers.get_all("x-forwarded-for") {
        match value.to_str() {
            Ok(text) => entries.extend(text.split(',').map(header_address)),
            Err(_) => entries.push(None),
        }
    }
    for entry in entries.into_iter().rev() {
        match entry {
            Some(ip) if trusted(ip, proxies) => continue,
            Some(ip) => return Some(ip),
            None => break,
        }
    }
    Some(peer)
}

/// The client address of the request being handled; None when the server
/// was not told the peer (see the module documentation).
pub struct ClientIp(pub Option<IpAddr>);

impl FromRequestParts<Shared> for ClientIp {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, state: &Shared) -> Result<Self, Self::Rejection> {
        let peer = parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip());
        Ok(ClientIp(client_address(
            peer,
            &parts.headers,
            &state.settings.trusted_proxies,
        )))
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn ranges_parse_and_contain() {
        let v4 = Cidr::parse("192.0.2.0/24").unwrap();
        assert!(v4.contains(ip("192.0.2.200")));
        assert!(v4.contains(ip("::ffff:192.0.2.7")), "mapped IPv6 is IPv4");
        assert!(!v4.contains(ip("192.0.3.1")));
        assert!(!v4.contains(ip("2001:db8::1")));

        let v6 = Cidr::parse(" 2001:db8:1::/48 ").unwrap();
        assert!(v6.contains(ip("2001:db8:1:ffff::1")));
        assert!(!v6.contains(ip("2001:db8:2::1")));
        assert!(!v6.contains(ip("192.0.2.1")));

        // A bare address, host bits, the whole space, a mapped range.
        assert!(Cidr::parse("198.51.100.7").unwrap().contains(ip("198.51.100.7")));
        assert!(!Cidr::parse("198.51.100.7").unwrap().contains(ip("198.51.100.8")));
        assert_eq!(
            Cidr::parse("10.42.1.5/16").unwrap(),
            Cidr::parse("10.42.0.0/16").unwrap()
        );
        assert_eq!(Cidr::parse("10.42.1.5/16").unwrap().to_string(), "10.42.0.0/16");
        assert!(Cidr::parse("0.0.0.0/0").unwrap().contains(ip("203.0.113.1")));
        assert!(Cidr::parse("::/0").unwrap().contains(ip("2001:db8::1")));
        assert_eq!(
            Cidr::parse("::ffff:10.0.0.0/104").unwrap(),
            Cidr::parse("10.0.0.0/8").unwrap()
        );

        for bad in [
            "",
            "10.0.0.0/33",
            "2001:db8::/129",
            "10.0.0.0/x",
            "example.com/24",
            "10.0.0/8",
            "10.0.0.0/-1",
        ] {
            assert!(Cidr::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_variable_is_a_comma_separated_list() {
        let list = parse_trusted_proxies("10.42.0.0/16, 2001:db8::/32,,").unwrap();
        assert_eq!(list.len(), 2);
        assert!(parse_trusted_proxies("").unwrap().is_empty());
        let error = parse_trusted_proxies("10.42.0.0/16, nonsense").unwrap_err();
        assert!(
            error.starts_with("NEKORISU_TRUSTED_PROXIES") && error.contains("nonsense"),
            "{error}"
        );
    }

    #[test]
    fn without_trusted_proxies_headers_are_ignored() {
        let h = headers(&[("x-real-ip", "203.0.113.9"), ("x-forwarded-for", "203.0.113.9")]);
        assert_eq!(
            client_address(Some(ip("198.51.100.1")), &h, &[]),
            Some(ip("198.51.100.1"))
        );
        assert_eq!(
            client_address(Some(ip("::ffff:198.51.100.1")), &h, &[]),
            Some(ip("198.51.100.1"))
        );
        assert_eq!(client_address(None, &h, &[]), None);
    }

    #[test]
    fn an_untrusted_peer_cannot_claim_another_address() {
        let proxies = [Cidr::parse("192.0.2.0/24").unwrap()];
        let h = headers(&[("x-real-ip", "203.0.113.9"), ("x-forwarded-for", "203.0.113.9")]);
        assert_eq!(
            client_address(Some(ip("198.51.100.1")), &h, &proxies),
            Some(ip("198.51.100.1"))
        );
    }

    #[test]
    fn a_trusted_proxy_is_believed() {
        let proxies = [
            Cidr::parse("192.0.2.0/24").unwrap(),
            Cidr::parse("2001:db8:ffff::/48").unwrap(),
        ];
        let peer = Some(ip("192.0.2.10"));
        let from = |pairs: &[(&'static str, &str)]| client_address(peer, &headers(pairs), &proxies);

        // X-Real-IP first, in any of the forms a proxy writes.
        assert_eq!(from(&[("x-real-ip", "203.0.113.9")]), Some(ip("203.0.113.9")));
        assert_eq!(from(&[("x-real-ip", " ::ffff:203.0.113.9 ")]), Some(ip("203.0.113.9")));
        assert_eq!(from(&[("x-real-ip", "[2001:db8::9]")]), Some(ip("2001:db8::9")));
        assert_eq!(from(&[("x-real-ip", "[2001:db8::9]:443")]), Some(ip("2001:db8::9")));
        assert_eq!(from(&[("x-real-ip", "203.0.113.9:5000")]), Some(ip("203.0.113.9")));
        assert_eq!(
            from(&[("x-real-ip", "203.0.113.9"), ("x-forwarded-for", "198.51.100.4")]),
            Some(ip("203.0.113.9"))
        );

        // Otherwise the rightmost X-Forwarded-For entry that is not a proxy;
        // what the client wrote on the left does not matter.
        assert_eq!(
            from(&[("x-forwarded-for", "198.51.100.66, 203.0.113.9, 192.0.2.20")]),
            Some(ip("203.0.113.9"))
        );
        assert_eq!(
            from(&[("x-forwarded-for", "garbage, 203.0.113.9")]),
            Some(ip("203.0.113.9"))
        );
        assert_eq!(
            from(&[
                ("x-forwarded-for", "198.51.100.66"),
                ("x-forwarded-for", "203.0.113.9, 2001:db8:ffff::1")
            ]),
            Some(ip("203.0.113.9")),
            "several headers are one list"
        );
        // A bad X-Real-IP, or two of them, falls through to X-Forwarded-For.
        assert_eq!(
            from(&[("x-real-ip", "unknown"), ("x-forwarded-for", "203.0.113.9")]),
            Some(ip("203.0.113.9"))
        );
        assert_eq!(
            from(&[
                ("x-real-ip", "198.51.100.1"),
                ("x-real-ip", "198.51.100.2"),
                ("x-forwarded-for", "203.0.113.9")
            ]),
            Some(ip("203.0.113.9"))
        );

        // Otherwise the peer: nothing, only proxies, or a garbled chain.
        assert_eq!(from(&[]), peer);
        assert_eq!(from(&[("x-forwarded-for", "192.0.2.30, 192.0.2.20")]), peer);
        assert_eq!(from(&[("x-forwarded-for", "203.0.113.9, unknown")]), peer);
        assert_eq!(from(&[("x-forwarded-for", "")]), peer);
    }
}
