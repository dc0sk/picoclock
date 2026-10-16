//! Timezone lookup by public IP address. There is no `no_std` crate for this; it is one plain-HTTP request to a free
//! service (HTTPS is paid-only or needs a TLS client), so the exchange is unauthenticated and the feature is opt-in
//! (`timezone=auto`). The reply is only trusted as far as it names a zone in our own table.
//!
//! Several providers are tried in order because a service can be unreachable from a given network: on the first
//! clock, ip-api.com accepted the connection but never acknowledged the request, while others answered normally.

use crate::tz;

pub const PORT: u16 = 80;

/// How a provider's reply is laid out.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// `{"status":"success","timezone":"Europe/Berlin"}`
    IpApi,
    /// `{"success": true, "timezone": {"id": "Europe/Berlin"}}`
    IpWho,
}

pub struct Provider {
    pub host: &'static str,
    /// HTTP/1.0 so the server never uses chunked encoding.
    pub request: &'static [u8],
    pub format: Format,
}

/// Tried in order; the first reply that names a zone in the built-in table wins.
pub const PROVIDERS: [Provider; 2] = [
    Provider {
        host: "ipwho.is",
        request: b"GET /?fields=success,timezone.id HTTP/1.0\r\nHost: ipwho.is\r\nUser-Agent: picoclock\r\nConnection: close\r\n\r\n",
        format: Format::IpWho,
    },
    Provider {
        host: "ip-api.com",
        request: b"GET /json/?fields=status,timezone HTTP/1.0\r\nHost: ip-api.com\r\nUser-Agent: picoclock\r\nConnection: close\r\n\r\n",
        format: Format::IpApi,
    },
];

/// Finds `"key": "value"` in flat JSON and returns the value (no escape handling: values with `\` are rejected).
fn json_str<'a>(body: &'a str, key: &str) -> Option<&'a str> {
    let mut rest = body;
    while let Some(i) = rest.find('"') {
        rest = &rest[i + 1..];
        let end = rest.find('"')?;
        let token = &rest[..end];
        rest = &rest[end + 1..];
        if token == key {
            let after = rest.trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
            let v = &after[..after.find('"')?];
            return if v.contains('\\') { None } else { Some(v) };
        }
    }
    None
}

/// `"key": true` in flat JSON (whitespace after the colon allowed).
fn json_true(body: &str, key: &str) -> bool {
    let pat_end = body.find(format_key(key).as_str());
    match pat_end {
        Some(i) => body[i + key.len() + 2..].trim_start().strip_prefix(':').map(|r| r.trim_start().starts_with("true")).unwrap_or(false),
        None => false,
    }
}

fn format_key(key: &str) -> heapless::String<24> {
    let mut s = heapless::String::new();
    let _ = s.push('"');
    let _ = s.push_str(key);
    let _ = s.push('"');
    s
}

/// Extracts the IANA zone from a raw HTTP response and maps it to a POSIX rule from the built-in table.
/// Returns `(zone name, rule)`.
pub fn parse_response(resp: &[u8], format: Format) -> Option<(&str, &'static str)> {
    let text = core::str::from_utf8(resp).ok()?;
    let (head, body) = text.split_once("\r\n\r\n")?;
    if !head.starts_with("HTTP/1.") || head.split(' ').nth(1) != Some("200") {
        return None;
    }
    let name = match format {
        Format::IpApi => {
            if json_str(body, "status")? != "success" {
                return None;
            }
            json_str(body, "timezone")?
        }
        Format::IpWho => {
            if !json_true(body, "success") {
                return None;
            }
            // `"timezone": { "id": "Europe/Berlin" }`: the first "id" after the "timezone" key.
            let at = body.find("\"timezone\"")?;
            json_str(&body[at + 10..], "id")?
        }
    };
    tz::lookup(name).map(|rule| (name, rule))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http(body: &str) -> std::string::String {
        format!("HTTP/1.0 200 OK\r\nContent-Type: application/json\r\n\r\n{body}")
    }

    // VERIFIES: REQ-TZ-04
    #[test]
    fn parses_real_shaped_reply() {
        let r = http(r#"{"status":"success","timezone":"Europe/Berlin"}"#);
        assert_eq!(parse_response(r.as_bytes(), Format::IpApi), Some(("Europe/Berlin", "CET-1CEST,M3.5.0,M10.5.0/3")));
        let r = http("{ \"status\" : \"success\", \"timezone\" : \"Asia/Tokyo\" }");
        assert_eq!(parse_response(r.as_bytes(), Format::IpApi).map(|x| x.1), Some("JST-9"));
    }

    // VERIFIES: REQ-TZ-04
    #[test]
    fn parses_captured_ip_api_response() {
        // Captured from ip-api.com on 2026-10-15; note it answers HTTP/1.1 keep-alive even to an HTTP/1.0 request.
        let r = "HTTP/1.1 200 OK\r\nDate: Thu, 15 Oct 2026 17:51:00 GMT\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: 47\r\nAccess-Control-Allow-Origin: *\r\nX-Ttl: 60\r\nX-Rl: 44\r\nConnection: keep-alive\r\n\r\n{\"status\":\"success\",\"timezone\":\"Europe/Berlin\"}";
        assert_eq!(parse_response(r.as_bytes(), Format::IpApi).map(|x| x.0), Some("Europe/Berlin"));
    }

    // VERIFIES: REQ-TZ-04
    #[test]
    fn rejects_everything_else() {
        let ok = r#"{"status":"success","timezone":"Europe/Berlin"}"#;
        for bad in [
            http(r#"{"status":"fail","message":"reserved range"}"#),
            http(r#"{"status":"success"}"#),
            http(r#"{"status":"success","timezone":"Mars/Base"}"#),
            http(r#"{"status":"success","timezone":"Europe\/Berlin"}"#),
            http(r#"{"status":"success","timezone":"CET-1CEST,M3.5.0,M10.5.0/3"}"#), // only table names are trusted
            format!("HTTP/1.0 429 Too Many Requests\r\n\r\n{ok}"),
            format!("HTTP/1.0 200 OK\r\n{ok}"), // no header terminator
            std::string::String::new(),
        ] {
            assert_eq!(parse_response(bad.as_bytes(), Format::IpApi), None, "{bad}");
        }
        assert_eq!(parse_response(&[0xFF, 0xFE], Format::IpApi), None);
        assert_eq!(parse_response(&[0xFF, 0xFE], Format::IpWho), None);
    }

    // VERIFIES: REQ-TZ-04
    #[test]
    fn parses_captured_ipwho_response_and_rejects_the_rest() {
        // Captured from ipwho.is on 2026-10-16 (headers shortened), request `/?fields=success,timezone.id`.
        let r = "HTTP/1.1 200 OK\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: 78\r\nConnection: close\r\nServer: cloudflare\r\n\r\n{\n    \"success\": true,\n    \"timezone\": {\n        \"id\": \"Europe/Berlin\"\n    }\n}";
        assert_eq!(parse_response(r.as_bytes(), Format::IpWho), Some(("Europe/Berlin", "CET-1CEST,M3.5.0,M10.5.0/3")));
        // an unfiltered reply has more fields; the zone id is the first "id" after "timezone"
        let full = http(r#"{"ip":"1.2.3.4","success":true,"country":"Japan","timezone":{"id":"Asia/Tokyo","abbr":"JST","offset":32400}}"#);
        assert_eq!(parse_response(full.as_bytes(), Format::IpWho).map(|x| x.1), Some("JST-9"));
        for bad in [
            http(r#"{"success": false, "message": "Invalid IP address"}"#),
            http(r#"{"timezone": {"id": "Europe/Berlin"}}"#), // no success flag
            http(r#"{"success": "true", "timezone": {"id": "Europe/Berlin"}}"#), // a string is not a boolean
            http(r#"{"success": true, "timezone": {"id": "Mars/Base"}}"#),
            http(r#"{"success": true}"#),
            "HTTP/1.1 301 Moved Permanently\r\nLocation: https://ipwho.is/\r\n\r\n".to_string(),
        ] {
            assert_eq!(parse_response(bad.as_bytes(), Format::IpWho), None, "{bad}");
        }
        // each provider's reply is only accepted in its own format
        let api = http(r#"{"status":"success","timezone":"Europe/Berlin"}"#);
        assert_eq!(parse_response(api.as_bytes(), Format::IpWho), None);
        assert_eq!(parse_response(r.as_bytes(), Format::IpApi), None);
    }

    // VERIFIES: REQ-TZ-04
    #[test]
    fn provider_requests_are_well_formed() {
        for p in PROVIDERS.iter() {
            let r = core::str::from_utf8(p.request).unwrap();
            assert!(r.starts_with("GET /") && r.contains(" HTTP/1.0\r\n") && r.ends_with("\r\n\r\n"), "{r}");
            assert!(r.contains(&format!("Host: {}\r\n", p.host)), "Host header must match {}", p.host);
            assert!(p.request.len() <= 192, "must fit the 192-byte send buffer");
        }
    }
}
