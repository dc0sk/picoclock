//! Timezone lookup by public IP address. There is no `no_std` crate for this; it is one plain-HTTP request to
//! ip-api.com (free for non-commercial use; HTTPS is paid-only), so the exchange is unauthenticated and the
//! feature is opt-in (`timezone=auto`). The reply is only trusted as far as it names a zone in our own table.

use crate::tz;

pub const HOST: &str = "ip-api.com";
pub const PORT: u16 = 80;
/// HTTP/1.0 so the server never uses chunked encoding.
pub const REQUEST: &[u8] =
    b"GET /json/?fields=status,timezone HTTP/1.0\r\nHost: ip-api.com\r\nUser-Agent: picoclock\r\nConnection: close\r\n\r\n";

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

/// Extracts the IANA zone from a raw HTTP response and maps it to a POSIX rule from the built-in table.
/// Returns `(zone name, rule)`.
pub fn parse_response(resp: &[u8]) -> Option<(&str, &'static str)> {
    let text = core::str::from_utf8(resp).ok()?;
    let (head, body) = text.split_once("\r\n\r\n")?;
    if !head.starts_with("HTTP/1.") || head.split(' ').nth(1) != Some("200") {
        return None;
    }
    if json_str(body, "status")? != "success" {
        return None;
    }
    let name = json_str(body, "timezone")?;
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
        assert_eq!(parse_response(r.as_bytes()), Some(("Europe/Berlin", "CET-1CEST,M3.5.0,M10.5.0/3")));
        let r = http("{ \"status\" : \"success\", \"timezone\" : \"Asia/Tokyo\" }");
        assert_eq!(parse_response(r.as_bytes()).map(|x| x.1), Some("JST-9"));
    }

    // VERIFIES: REQ-TZ-04
    #[test]
    fn parses_captured_ip_api_response() {
        // Captured from ip-api.com on 2026-10-15; note it answers HTTP/1.1 keep-alive even to an HTTP/1.0 request.
        let r = "HTTP/1.1 200 OK\r\nDate: Thu, 15 Oct 2026 17:51:00 GMT\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: 47\r\nAccess-Control-Allow-Origin: *\r\nX-Ttl: 60\r\nX-Rl: 44\r\nConnection: keep-alive\r\n\r\n{\"status\":\"success\",\"timezone\":\"Europe/Berlin\"}";
        assert_eq!(parse_response(r.as_bytes()).map(|x| x.0), Some("Europe/Berlin"));
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
            assert_eq!(parse_response(bad.as_bytes()), None, "{bad}");
        }
        assert_eq!(parse_response(&[0xFF, 0xFE]), None);
    }
}
