//! Looking up the hosts of client metadata documents through DNS over HTTPS
//! (RFC 8484), when `NEKORISU_DNS_OVER_HTTPS` is set.
//!
//! Why: some hosts run a fake-IP DNS proxy (sing-box, Clash) that answers
//! every name with an address of its own from 198.18.0.0/15 and routes the
//! connection itself. The SSRF check then sees only the made-up address and
//! cannot tell where a request would really go. A resolver reached over
//! HTTPS at an IP address (so finding the resolver needs no DNS) gives the
//! real addresses, which are checked and pinned as before.
//!
//! The DNS wire format is written and read here: one question goes out (A or
//! AAAA, recursion desired); the A and AAAA records of the answer section
//! come back, other records are skipped. Every read is bounds-checked, and a
//! compressed name may only point backwards, each pointer before the last,
//! so it cannot loop. Truncated (TC) or malformed answers, answers to another
//! query (ID or question) and resolver errors are refused.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use rand_core::{OsRng, RngCore};
use url::Url;

pub const TYPE_A: u16 = 1;
pub const TYPE_AAAA: u16 = 28;
const CLASS_IN: u16 = 1;
const HEADER_LEN: usize = 12;
const MAX_NAME_LEN: usize = 255;
const MAX_LABEL_LEN: usize = 63;
/// The flags of a standard query: recursion desired.
const FLAGS_QUERY: u16 = 0x0100;
const FLAG_RESPONSE: u16 = 0x8000;
const FLAG_TRUNCATED: u16 = 0x0200;
const MEDIA_TYPE: &str = "application/dns-message";
/// An answer with the A or AAAA records of one name is a few hundred bytes.
const MAX_RESPONSE_BYTES: usize = 8 * 1024;
const TIMEOUT: Duration = Duration::from_secs(3);

const TRUNCATED: &str = "the answer is cut short";

/// A query for `name` (dotted, an optional trailing dot), asking for recursion.
pub fn encode_query(id: u16, name: &str, qtype: u16) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(HEADER_LEN + name.len() + 6);
    out.extend_from_slice(&id.to_be_bytes());
    out.extend_from_slice(&FLAGS_QUERY.to_be_bytes());
    out.extend_from_slice(&1u16.to_be_bytes()); // one question
    out.extend_from_slice(&[0; 6]); // no answer, authority or additional records
    out.extend_from_slice(&wire_name(name)?);
    out.extend_from_slice(&qtype.to_be_bytes());
    out.extend_from_slice(&CLASS_IN.to_be_bytes());
    Ok(out)
}

/// A name as length-prefixed labels ending in the root, in lower case.
fn wire_name(name: &str) -> Result<Vec<u8>, String> {
    let bare = name.strip_suffix('.').unwrap_or(name);
    if bare.is_empty() || !bare.is_ascii() {
        return Err(format!("cannot look up {name:?}"));
    }
    let mut out = Vec::with_capacity(bare.len() + 2);
    for label in bare.split('.') {
        if label.is_empty() || label.len() > MAX_LABEL_LEN {
            return Err(format!("cannot look up {name:?}: a label is empty or longer than 63"));
        }
        out.push(label.len() as u8);
        out.extend(label.bytes().map(|b| b.to_ascii_lowercase()));
    }
    out.push(0);
    if out.len() > MAX_NAME_LEN {
        return Err(format!("cannot look up {name:?}: longer than 255 bytes"));
    }
    Ok(out)
}

fn u16_at(buf: &[u8], at: usize) -> Result<u16, String> {
    match buf.get(at..at.checked_add(2).ok_or(TRUNCATED)?) {
        Some(b) => Ok(u16::from_be_bytes([b[0], b[1]])),
        None => Err(TRUNCATED.to_string()),
    }
}

/// Reads the name at `start`, following compression pointers. Returns it in
/// uncompressed wire form, lower case, and where the name ends in the message.
fn read_name(buf: &[u8], start: usize) -> Result<(Vec<u8>, usize), String> {
    let mut name = Vec::new();
    let mut at = start;
    let mut end = None;
    // A pointer must point before this: first the name's own start, then each
    // pointer's target, so every jump goes further back and the walk ends.
    let mut before = start;
    loop {
        let len = *buf.get(at).ok_or(TRUNCATED)? as usize;
        match len & 0xc0 {
            0x00 if len == 0 => {
                name.push(0);
                if name.len() > MAX_NAME_LEN {
                    return Err("a name longer than 255 bytes".to_string());
                }
                return Ok((name, end.unwrap_or(at + 1)));
            }
            0x00 => {
                let label = buf.get(at + 1..at + 1 + len).ok_or(TRUNCATED)?;
                name.push(len as u8);
                name.extend(label.iter().map(u8::to_ascii_lowercase));
                if name.len() >= MAX_NAME_LEN {
                    return Err("a name longer than 255 bytes".to_string());
                }
                at += 1 + len;
            }
            0xc0 => {
                let low = *buf.get(at + 1).ok_or(TRUNCATED)? as usize;
                let target = ((len & 0x3f) << 8) | low;
                if target >= before {
                    return Err("a compressed name that does not point backwards".to_string());
                }
                end.get_or_insert(at + 2);
                before = target;
                at = target;
            }
            _ => return Err("a name with an unknown label type".to_string()),
        }
    }
}

fn rcode_name(rcode: u16) -> String {
    match rcode {
        1 => "FORMERR".to_string(),
        2 => "SERVFAIL".to_string(),
        4 => "NOTIMP".to_string(),
        5 => "REFUSED".to_string(),
        other => format!("RCODE {other}"),
    }
}

/// The addresses in the answer to the query `encode_query(id, name, qtype)`.
/// A name that does not exist (NXDOMAIN) has none.
pub fn parse_response(buf: &[u8], id: u16, name: &str, qtype: u16) -> Result<Vec<IpAddr>, String> {
    if buf.len() < HEADER_LEN {
        return Err(TRUNCATED.to_string());
    }
    if u16_at(buf, 0)? != id {
        return Err("the answer is for another query (its ID differs)".to_string());
    }
    let flags = u16_at(buf, 2)?;
    if flags & FLAG_RESPONSE == 0 || (flags >> 11) & 0x0f != 0 {
        return Err("not an answer to a standard query".to_string());
    }
    if flags & FLAG_TRUNCATED != 0 {
        return Err("the resolver truncated the answer (TC)".to_string());
    }
    let (questions, answers) = (u16_at(buf, 4)?, u16_at(buf, 6)?);
    if questions != 1 {
        return Err("the answer does not repeat the one question".to_string());
    }
    let (asked, mut at) = read_name(buf, HEADER_LEN)?;
    if asked != wire_name(name)? || u16_at(buf, at)? != qtype || u16_at(buf, at + 2)? != CLASS_IN {
        return Err("the answer is for another question".to_string());
    }
    at += 4;
    match flags & 0x0f {
        0 => {}
        3 => return Ok(Vec::new()), // NXDOMAIN
        rcode => return Err(format!("the resolver answered {}", rcode_name(rcode))),
    }
    let mut found = Vec::new();
    for _ in 0..answers {
        let (_, after_name) = read_name(buf, at)?;
        let rtype = u16_at(buf, after_name)?;
        let class = u16_at(buf, after_name + 2)?;
        // a 32-bit TTL, then the data's length
        let len = u16_at(buf, after_name + 8)? as usize;
        let start = after_name + 10;
        let data = buf.get(start..start + len).ok_or(TRUNCATED)?;
        at = start + len;
        match (rtype, class) {
            (TYPE_A, CLASS_IN) => {
                let octets: [u8; 4] = data
                    .try_into()
                    .map_err(|_| "an A record that is not 4 bytes".to_string())?;
                found.push(IpAddr::V4(Ipv4Addr::from(octets)));
            }
            (TYPE_AAAA, CLASS_IN) => {
                let octets: [u8; 16] = data
                    .try_into()
                    .map_err(|_| "an AAAA record that is not 16 bytes".to_string())?;
                found.push(IpAddr::V6(Ipv6Addr::from(octets)));
            }
            _ => {} // CNAME and the rest: the resolver has followed them already
        }
    }
    Ok(found)
}

/// Asks one RFC 8484 resolver, over HTTPS to an IP address, with POST.
pub struct DohResolver {
    url: Url,
}

impl DohResolver {
    /// `url` was checked by the settings: https, an IP address as host.
    pub fn new(url: Url) -> Self {
        Self { url }
    }

    /// The A and AAAA addresses of `name`. Both questions must be answered;
    /// any failure is an error (the caller then refuses to fetch).
    pub async fn lookup(&self, name: &str) -> Result<Vec<IpAddr>, String> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(TIMEOUT)
            .timeout(TIMEOUT)
            .user_agent(concat!("nekorisu-server/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| format!("cannot build the HTTP client: {e}"))?;
        let (v4, v6) = tokio::join!(self.ask(&client, name, TYPE_A), self.ask(&client, name, TYPE_AAAA));
        let mut found = v4?;
        found.extend(v6?);
        Ok(found)
    }

    async fn ask(&self, client: &reqwest::Client, name: &str, qtype: u16) -> Result<Vec<IpAddr>, String> {
        let id = OsRng.next_u32() as u16;
        let query = encode_query(id, name, qtype)?;
        let mut response = client
            .post(self.url.clone())
            .header(reqwest::header::CONTENT_TYPE, MEDIA_TYPE)
            .header(reqwest::header::ACCEPT, MEDIA_TYPE)
            .body(query)
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    "the resolver did not answer in time".to_string()
                } else {
                    format!("the request failed: {e}")
                }
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("the resolver answered HTTP {}", status.as_u16()));
        }
        let media_type = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .map(|v| v.trim().to_ascii_lowercase());
        if media_type.as_deref() != Some(MEDIA_TYPE) {
            return Err(format!("the resolver did not answer with {MEDIA_TYPE}"));
        }
        if response.content_length().is_some_and(|n| n > MAX_RESPONSE_BYTES as u64) {
            return Err("the answer is too big".to_string());
        }
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| format!("reading the answer failed: {e}"))?
        {
            if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
                return Err("the answer is too big".to_string());
            }
            body.extend_from_slice(&chunk);
        }
        parse_response(&body, id, name, qtype)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: u16 = 0x1234;

    /// A response to `encode_query(ID, name, qtype)` with these answer
    /// records (already encoded) and flags.
    fn response(name: &str, qtype: u16, flags: u16, records: &[Vec<u8>]) -> Vec<u8> {
        let mut out = encode_query(ID, name, qtype).unwrap();
        out[2..4].copy_from_slice(&flags.to_be_bytes());
        out[6..8].copy_from_slice(&(records.len() as u16).to_be_bytes());
        for r in records {
            out.extend_from_slice(r);
        }
        out
    }

    /// One answer record: owner name (already encoded), type, class IN, TTL 60, data.
    fn record(owner: &[u8], rtype: u16, data: &[u8]) -> Vec<u8> {
        let mut out = owner.to_vec();
        out.extend_from_slice(&rtype.to_be_bytes());
        out.extend_from_slice(&CLASS_IN.to_be_bytes());
        out.extend_from_slice(&60u32.to_be_bytes());
        out.extend_from_slice(&(data.len() as u16).to_be_bytes());
        out.extend_from_slice(data);
        out
    }

    /// A pointer to the question's name, which starts right after the header.
    const TO_QUESTION: [u8; 2] = [0xc0, 0x0c];
    const OK: u16 = 0x8180; // a response, recursion desired and available, NOERROR

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn encodes_a_query() {
        let q = encode_query(0xbeef, "App.Example.com.", TYPE_AAAA).unwrap();
        let mut expected = vec![0xbe, 0xef, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        expected.extend_from_slice(b"\x03app\x07example\x03com\x00");
        expected.extend_from_slice(&[0, 28, 0, 1]);
        assert_eq!(q, expected);

        assert!(encode_query(1, "", TYPE_A).is_err());
        assert!(encode_query(1, ".", TYPE_A).is_err());
        assert!(encode_query(1, "a..example.com", TYPE_A).is_err());
        assert!(encode_query(1, &format!("{}.example.com", "a".repeat(64)), TYPE_A).is_err());
        assert!(encode_query(1, &format!("{}.example.com", "a".repeat(63)), TYPE_A).is_ok());
        let long = vec!["a".repeat(63); 4].join("."); // 4 * 64 + 1 = 257 bytes
        assert!(encode_query(1, &long, TYPE_A).is_err());
        assert!(
            encode_query(1, "bücher.example", TYPE_A).is_err(),
            "punycode comes from the URL parser"
        );
    }

    #[test]
    fn reads_a_records_with_compressed_names() {
        // What 1.1.1.1 sends for example.com (with documentation addresses).
        let r = response(
            "example.com",
            TYPE_A,
            OK,
            &[
                record(&TO_QUESTION, TYPE_A, &[192, 0, 2, 1]),
                record(&TO_QUESTION, TYPE_A, &[198, 51, 100, 2]),
            ],
        );
        assert_eq!(
            parse_response(&r, ID, "example.com", TYPE_A).unwrap(),
            [ip("192.0.2.1"), ip("198.51.100.2")]
        );
        // The name is compared without regard to case or a trailing dot.
        assert!(parse_response(&r, ID, "EXAMPLE.com.", TYPE_A).is_ok());
    }

    #[test]
    fn follows_a_cname_chain_and_keeps_only_addresses() {
        // app.example.com CNAME edge.example.net, then its addresses. Their
        // owner is a pointer into the CNAME's data, where the target is
        // written out.
        let question_end = HEADER_LEN + "app.example.com".len() + 2 + 4;
        let cname_data = b"\x04edge\x07example\x03net\x00".to_vec();
        let cname = record(&TO_QUESTION, 5, &cname_data);
        // The CNAME's data starts after its owner (2), type, class, TTL, length (10).
        let target_at = question_end + 2 + 10;
        let to_target = [0xc0 | (target_at >> 8) as u8, target_at as u8];
        let r = response(
            "app.example.com",
            TYPE_A,
            OK,
            &[
                cname,
                record(&to_target, TYPE_A, &[203, 0, 113, 7]),
                record(&to_target, 99, b"v=spf1 -all"), // something else: skipped
                record(&to_target, TYPE_AAAA, &ip_bytes("2001:db8::7")),
            ],
        );
        assert_eq!(
            parse_response(&r, ID, "app.example.com", TYPE_A).unwrap(),
            [ip("203.0.113.7"), ip("2001:db8::7")]
        );

        // A pointer into the middle of a name works too: "www" + pointer to
        // the "example.com" inside the question.
        let mut owner = b"\x03www".to_vec();
        owner.extend_from_slice(&[0xc0, 0x0c + 4]);
        let r = response(
            "app.example.com",
            TYPE_A,
            OK,
            &[record(&owner, TYPE_A, &[192, 0, 2, 9])],
        );
        assert_eq!(
            parse_response(&r, ID, "app.example.com", TYPE_A).unwrap(),
            [ip("192.0.2.9")]
        );
    }

    fn ip_bytes(text: &str) -> [u8; 16] {
        match ip(text) {
            IpAddr::V6(v6) => v6.octets(),
            IpAddr::V4(_) => unreachable!(),
        }
    }

    #[test]
    fn no_such_name_is_no_addresses_and_resolver_errors_are_errors() {
        let nx = response("gone.example.com", TYPE_A, 0x8183, &[]);
        assert_eq!(
            parse_response(&nx, ID, "gone.example.com", TYPE_A).unwrap(),
            Vec::<IpAddr>::new()
        );
        let empty = response("v4only.example.com", TYPE_AAAA, OK, &[]);
        assert_eq!(
            parse_response(&empty, ID, "v4only.example.com", TYPE_AAAA).unwrap(),
            Vec::<IpAddr>::new()
        );
        for (flags, why) in [(0x8182, "SERVFAIL"), (0x8185, "REFUSED"), (0x8181, "FORMERR")] {
            let r = response("example.com", TYPE_A, flags, &[]);
            let error = parse_response(&r, ID, "example.com", TYPE_A).unwrap_err();
            assert!(error.contains(why), "{error}");
        }
    }

    #[test]
    fn refuses_answers_to_another_query() {
        let r = response(
            "example.com",
            TYPE_A,
            OK,
            &[record(&TO_QUESTION, TYPE_A, &[192, 0, 2, 1])],
        );
        let wrong_id = parse_response(&r, ID + 1, "example.com", TYPE_A).unwrap_err();
        assert!(wrong_id.contains("ID"), "{wrong_id}");
        assert!(parse_response(&r, ID, "example.org", TYPE_A).is_err());
        assert!(parse_response(&r, ID, "example.com", TYPE_AAAA).is_err());
        // A query, not an answer.
        let q = encode_query(ID, "example.com", TYPE_A).unwrap();
        assert!(parse_response(&q, ID, "example.com", TYPE_A).is_err());
        // Two questions.
        let mut two = r.clone();
        two[5] = 2;
        assert!(parse_response(&two, ID, "example.com", TYPE_A).is_err());
    }

    #[test]
    fn refuses_truncated_and_malformed_answers() {
        let r = response(
            "example.com",
            TYPE_A,
            OK,
            &[
                record(&TO_QUESTION, TYPE_A, &[192, 0, 2, 1]),
                record(&TO_QUESTION, TYPE_AAAA, &ip_bytes("2001:db8::1")),
            ],
        );
        assert!(parse_response(&r, ID, "example.com", TYPE_A).is_ok());
        // Cut anywhere: an error, never a panic or a partial list.
        for cut in 0..r.len() {
            assert!(
                parse_response(&r[..cut], ID, "example.com", TYPE_A).is_err(),
                "cut at {cut}"
            );
        }
        // The TC flag: the resolver says it left records out.
        let tc = response("example.com", TYPE_A, OK | FLAG_TRUNCATED, &[]);
        assert!(
            parse_response(&tc, ID, "example.com", TYPE_A)
                .unwrap_err()
                .contains("TC")
        );
        // An A record of the wrong length.
        let bad = response(
            "example.com",
            TYPE_A,
            OK,
            &[record(&TO_QUESTION, TYPE_A, &[192, 0, 2, 1, 0])],
        );
        assert!(parse_response(&bad, ID, "example.com", TYPE_A).is_err());
        // More answers announced than there are.
        let mut more = r.clone();
        more[7] = 3;
        assert!(parse_response(&more, ID, "example.com", TYPE_A).is_err());
    }

    #[test]
    fn compression_cannot_loop_or_point_forwards() {
        // A pointer to itself, one pointing forwards, one into its own labels,
        // and an unknown label type.
        let question_end = HEADER_LEN + "example.com".len() + 2 + 4;
        let at = question_end as u16;
        for owner in [
            vec![0xc0 | (at >> 8) as u8, at as u8],
            vec![0xc0, 0xff],
            {
                let mut o = b"\x03www".to_vec();
                o.extend_from_slice(&[0xc0 | (at >> 8) as u8, at as u8]);
                o
            },
            vec![0x40, 0x00],
        ] {
            let r = response("example.com", TYPE_A, OK, &[record(&owner, TYPE_A, &[192, 0, 2, 1])]);
            assert!(parse_response(&r, ID, "example.com", TYPE_A).is_err(), "{owner:?}");
        }
        // A pointer to a pointer, each further back: fine. The second record's
        // owner points at the first record's owner, which points at the question.
        let r = response(
            "example.com",
            TYPE_A,
            OK,
            &[
                record(&TO_QUESTION, TYPE_A, &[192, 0, 2, 1]),
                record(&[0xc0, question_end as u8], TYPE_A, &[192, 0, 2, 2]),
            ],
        );
        assert_eq!(
            parse_response(&r, ID, "example.com", TYPE_A).unwrap(),
            [ip("192.0.2.1"), ip("192.0.2.2")]
        );
    }
}
