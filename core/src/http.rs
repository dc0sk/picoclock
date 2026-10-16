//! Tiny HTTP/1.x server-side helpers for the configuration web page: request parsing with hard limits, Basic auth,
//! form decoding and the HTML page. All input is untrusted LAN input, so everything here is bounded.

use core::fmt::Write;

use crate::cli::Status;
use crate::config::{Config, KEYS};

pub const MAX_REQUEST: usize = 4096;
const MAX_HEADERS: usize = 32;

#[derive(Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Request<'a> {
    pub method: Method,
    pub path: &'a str,
    pub body: &'a [u8],
    pub authorization: Option<&'a str>,
    pub host: Option<&'a str>,
    pub origin: Option<&'a str>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Parse<'a> {
    /// Need more bytes.
    Incomplete,
    /// Malformed or unsupported; answer 400.
    Bad,
    /// Larger than the receive buffer; answer 413.
    TooLarge,
    Complete(Request<'a>),
}

pub fn parse_request(buf: &[u8]) -> Parse<'_> {
    let Some(head_end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        return if buf.len() >= MAX_REQUEST { Parse::TooLarge } else { Parse::Incomplete };
    };
    let Ok(head) = core::str::from_utf8(&buf[..head_end]) else { return Parse::Bad };
    let mut lines = head.split("\r\n");
    let mut parts = lines.next().unwrap_or("").split(' ');
    let (Some(method), Some(path), Some(version), None) = (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Parse::Bad;
    };
    if !version.starts_with("HTTP/1.") || !path.starts_with('/') || path.len() > 64 {
        return Parse::Bad;
    }
    let method = match method {
        "GET" => Method::Get,
        "POST" => Method::Post,
        _ => return Parse::Bad,
    };
    let (mut content_length, mut authorization, mut host, mut origin) = (None, None, None, None);
    for (n, line) in lines.enumerate() {
        if n >= MAX_HEADERS {
            return Parse::TooLarge;
        }
        let Some((name, value)) = line.split_once(':') else { return Parse::Bad };
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            // A repeated Content-Length is a smuggling signal, not something to be lenient about.
            if content_length.is_some() {
                return Parse::Bad;
            }
            content_length = match value.parse::<usize>() {
                Ok(n) => Some(n),
                Err(_) => return Parse::Bad,
            };
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Parse::Bad;
        } else if name.eq_ignore_ascii_case("authorization") {
            authorization = Some(value);
        } else if name.eq_ignore_ascii_case("host") {
            host = Some(value);
        } else if name.eq_ignore_ascii_case("origin") {
            origin = Some(value);
        }
    }
    let body_len = content_length.unwrap_or(0);
    let total = match (head_end + 4).checked_add(body_len) {
        Some(t) if t <= MAX_REQUEST => t,
        _ => return Parse::TooLarge,
    };
    if method == Method::Get && body_len > 0 {
        return Parse::Bad;
    }
    if buf.len() < total {
        return Parse::Incomplete;
    }
    Parse::Complete(Request { method, path, body: &buf[head_end + 4..total], authorization, host, origin })
}

/// CSRF defence for browser-submitted forms: if the browser sent an Origin, its host must be the Host we were asked for.
pub fn same_origin(req: &Request) -> bool {
    match (req.origin, req.host) {
        (None, _) => true, // non-browser clients (curl) send no Origin
        (Some(o), Some(h)) => o.split_once("://").is_some_and(|(_, oh)| oh == h),
        (Some(_), None) => false,
    }
}

fn b64_val(c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

/// Decodes standard base64 into `out`; returns the length, or `None` on bad input or overflow.
fn b64_decode(s: &str, out: &mut [u8]) -> Option<usize> {
    let s = s.trim_end_matches('=').as_bytes();
    let (mut acc, mut bits, mut n) = (0u32, 0u32, 0usize);
    for &c in s {
        acc = acc << 6 | b64_val(c)? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            *out.get_mut(n)? = (acc >> bits) as u8;
            n += 1;
        }
    }
    Some(n)
}

/// Compares without an early exit so response timing does not reveal a matching prefix.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..a.len().max(b.len()) {
        diff |= a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(1);
    }
    diff == 0
}

/// True if no password is configured, or the header carries `Basic base64(admin:<password>)`.
pub fn authorized(cfg: &Config, authorization: Option<&str>) -> bool {
    if cfg.web_password.is_empty() {
        return true;
    }
    let Some(b64) = authorization.and_then(|h| h.strip_prefix("Basic ")) else { return false };
    let mut buf = [0u8; 96];
    let Some(n) = b64_decode(b64.trim(), &mut buf) else { return false };
    let mut expect = [0u8; 96];
    let mut w = SliceW { buf: &mut expect, n: 0 };
    if write!(w, "admin:{}", cfg.web_password).is_err() {
        return false;
    }
    ct_eq(&buf[..n], &w.buf[..w.n])
}

struct SliceW<'a> {
    buf: &'a mut [u8],
    n: usize,
}
impl Write for SliceW<'_> {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        let end = self.n + s.len();
        self.buf.get_mut(self.n..end).ok_or(core::fmt::Error)?.copy_from_slice(s.as_bytes());
        self.n = end;
        Ok(())
    }
}

/// Percent-decodes one form component (`+` is a space) into `out`. `None` if invalid UTF-8/escape or too long.
fn url_decode<'a>(s: &str, out: &'a mut [u8]) -> Option<&'a str> {
    let b = s.as_bytes();
    let (mut i, mut n) = (0, 0);
    while i < b.len() {
        let byte = match b[i] {
            b'+' => b' ',
            b'%' => {
                let h = core::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
                i += 2;
                u8::from_str_radix(h, 16).ok()?
            }
            c => c,
        };
        *out.get_mut(n)? = byte;
        n += 1;
        i += 1;
    }
    core::str::from_utf8(&out[..n]).ok()
}

/// Applies an `application/x-www-form-urlencoded` body to the config. Blank password fields mean "unchanged".
/// Returns how many settings changed; problems are described in `errors`.
pub fn apply_form(body: &[u8], cfg: &mut Config, errors: &mut impl Write) -> usize {
    let Ok(body) = core::str::from_utf8(body) else {
        let _ = errors.write_str("form is not valid text. ");
        return 0;
    };
    let mut changed = 0;
    for pair in body.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let (mut kb, mut vb) = ([0u8; 32], [0u8; 128]);
        let (Some(k), Some(v)) = (url_decode(k, &mut kb), url_decode(v, &mut vb)) else {
            let _ = errors.write_str("a field was malformed or too long. ");
            continue;
        };
        let Some(info) = KEYS.iter().find(|i| i.name == k) else { continue };
        if info.secret && v.is_empty() || k == "geo_rule" {
            continue;
        }
        let mut before = heapless::String::<160>::new();
        let _ = cfg.write_value(k, &mut before);
        match cfg.set(k, v) {
            Ok(()) => {
                let mut after = heapless::String::<160>::new();
                let _ = cfg.write_value(k, &mut after);
                changed += (before != after) as usize;
            }
            Err(_) => {
                let _ = write!(errors, "invalid {k}: {}. ", info.help);
            }
        }
    }
    changed
}

struct Esc<'a>(&'a str);
impl core::fmt::Display for Esc<'_> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        for c in self.0.chars() {
            match c {
                '&' => f.write_str("&amp;")?,
                '<' => f.write_str("&lt;")?,
                '>' => f.write_str("&gt;")?,
                '"' => f.write_str("&quot;")?,
                '\'' => f.write_str("&#39;")?,
                c => f.write_char(c)?,
            }
        }
        Ok(())
    }
}

const PAGE_HEAD: &str = "<!doctype html><html><head><meta charset=utf-8><meta name=viewport content=\"width=device-width,initial-scale=1\">\
<title>picoclock</title><style>body{font:16px system-ui,sans-serif;max-width:34rem;margin:1rem auto;padding:0 1rem}\
label{display:block;margin-top:.8rem;font-weight:600}small{display:block;color:#666;font-weight:400}\
input,select{width:100%;box-sizing:border-box;padding:.4rem;font:inherit}button{margin-top:1rem;padding:.5rem 1.2rem;font:inherit}\
.msg{background:#eef;padding:.5rem;border-radius:4px}.err{background:#fdd}pre{background:#f4f4f4;padding:.5rem;overflow:auto}\
@media(prefers-color-scheme:dark){body{background:#111;color:#eee}small{color:#aaa}pre{background:#222}.msg{background:#223}.err{background:#522}input,select{background:#222;color:#eee;border:1px solid #555}}\
</style></head><body><h1>picoclock</h1>";

/// Renders the status and settings page. Fails (`fmt::Error`) only if `out` is too small.
pub fn render_page(cfg: &Config, st: &Status, msg: Option<(&str, bool)>, out: &mut impl Write) -> core::fmt::Result {
    out.write_str(PAGE_HEAD)?;
    if let Some((m, is_err)) = msg {
        write!(out, "<p class=\"msg{}\">{}</p>", if is_err { " err" } else { "" }, Esc(m))?;
    }
    out.write_str("<pre>")?;
    let mut status = heapless::String::<512>::new();
    crate::cli::execute("status", &mut cfg.clone(), st, &mut status);
    let _ = crate::cli::execute("time", &mut cfg.clone(), st, &mut status);
    write!(out, "{}</pre><form method=post action=/save>", Esc(&status))?;
    for k in KEYS.iter().filter(|k| k.name != "geo_rule") {
        write!(out, "<label>{}<small>{}</small>", k.name, Esc(k.help))?;
        let mut v = heapless::String::<160>::new();
        if k.secret {
            let _ = write!(out, "<input type=password name={} autocomplete=new-password placeholder=\"{}\">", k.name, 
                if cfg.write_display(k.name, &mut v).is_ok() && v == "(set)" { "(set; leave blank to keep)" } else { "(empty)" });
        } else if matches!(k.help.split(':').next(), Some("on|off")) || k.help == "on|off" {
            let _ = cfg.write_value(k.name, &mut v);
            let on = v == "on";
            write!(out, "<select name={}><option value=on{}>on<option value=off{}>off</select>", k.name,
                if on { " selected" } else { "" }, if on { "" } else { " selected" })?;
        } else {
            let _ = cfg.write_value(k.name, &mut v);
            write!(out, "<input name={} value=\"{}\">", k.name, Esc(&v))?;
        }
        out.write_str("</label>")?;
    }
    out.write_str("<button>Save</button></form><p><a href=/zones>List of timezone names</a></p></body></html>")
}

/// Plain-text list of all timezone names, one per line, streamed by the caller in chunks.
pub fn zone_names() -> impl Iterator<Item = &'static str> {
    crate::tz::zones_with_prefix("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(s: &str) -> Parse<'_> {
        parse_request(s.as_bytes())
    }

    #[test]
    fn parses_get_and_post() {
        let Parse::Complete(r) = req("GET / HTTP/1.1\r\nHost: 10.0.0.5\r\nAuthorization: Basic abc\r\n\r\n") else { panic!() };
        assert_eq!((r.method, r.path, r.host, r.authorization), (Method::Get, "/", Some("10.0.0.5"), Some("Basic abc")));
        let Parse::Complete(r) = req("POST /save HTTP/1.1\r\ncontent-length: 7\r\n\r\nhour12=on") else { panic!() };
        assert_eq!(r.body, b"hour12=");
    }

    #[test]
    fn incomplete_until_body_arrives() {
        assert_eq!(req("GET / HTTP/1.1\r\nHost: x\r\n"), Parse::Incomplete);
        assert_eq!(req("POST /save HTTP/1.1\r\nContent-Length: 10\r\n\r\nabc"), Parse::Incomplete);
    }

    #[test]
    fn rejects_malformed_and_hostile_requests() {
        for bad in [
            "BREW / HTTP/1.1\r\n\r\n",
            "GET / HTTP/2\r\n\r\n",
            "GET nopath HTTP/1.1\r\n\r\n",
            "GET /  HTTP/1.1\r\n\r\n",
            "GET / HTTP/1.1\r\nbroken header\r\n\r\n",
            "POST / HTTP/1.1\r\nContent-Length: abc\r\n\r\n",
            "POST / HTTP/1.1\r\nContent-Length: 1\r\nContent-Length: 1\r\n\r\nx",
            "POST / HTTP/1.1\r\nTransfer-Encoding: chunked\r\n\r\n",
            "GET / HTTP/1.1\r\nContent-Length: 3\r\n\r\nabc",
        ] {
            assert_eq!(req(bad), Parse::Bad, "{bad:?}");
        }
        assert_eq!(parse_request(&[0xFF; 20]), Parse::Incomplete);
        assert_eq!(parse_request(b"GET / HTTP/1.1\xff\r\n\r\n"), Parse::Bad);
    }

    #[test]
    fn enforces_size_limits() {
        assert_eq!(req("POST / HTTP/1.1\r\nContent-Length: 999999\r\n\r\n"), Parse::TooLarge);
        assert_eq!(req("POST / HTTP/1.1\r\nContent-Length: 18446744073709551615\r\n\r\n"), Parse::TooLarge);
        let long_path = format!("GET /{} HTTP/1.1\r\n\r\n", "a".repeat(100));
        assert_eq!(req(&long_path), Parse::Bad);
        let endless = [b'a'; MAX_REQUEST];
        assert_eq!(parse_request(&endless), Parse::TooLarge);
        let mut many = std::string::String::from("GET / HTTP/1.1\r\n");
        for i in 0..40 {
            many.push_str(&format!("X-{i}: y\r\n"));
        }
        many.push_str("\r\n");
        assert_eq!(req(&many), Parse::TooLarge);
    }

    #[test]
    fn csrf_origin_check() {
        let mk = |o, h| Request { method: Method::Post, path: "/", body: b"", authorization: None, host: h, origin: o };
        assert!(same_origin(&mk(None, Some("1.2.3.4"))));
        assert!(same_origin(&mk(Some("http://1.2.3.4"), Some("1.2.3.4"))));
        assert!(!same_origin(&mk(Some("http://evil.example"), Some("1.2.3.4"))));
        assert!(!same_origin(&mk(Some("null"), Some("1.2.3.4"))));
        assert!(!same_origin(&mk(Some("http://1.2.3.4"), None)));
    }

    #[test]
    fn basic_auth() {
        let mut c = Config::default();
        assert!(authorized(&c, None), "no password = open");
        c.set("web_password", "s3cret").unwrap();
        assert!(!authorized(&c, None));
        assert!(!authorized(&c, Some("Basic !!!")));
        assert!(!authorized(&c, Some("Bearer abc")));
        // base64("admin:s3cret")
        assert!(authorized(&c, Some("Basic YWRtaW46czNjcmV0")));
        // wrong user, wrong password, prefix of the right one, and oversized junk
        assert!(!authorized(&c, Some("Basic cm9vdDpzM2NyZXQ=")));
        assert!(!authorized(&c, Some("Basic YWRtaW46d3Jvbmc=")));
        assert!(!authorized(&c, Some("Basic YWRtaW46czNj")));
        assert!(!authorized(&c, Some(&format!("Basic {}", "QUFB".repeat(200)))));
    }

    #[test]
    fn form_apply() {
        let mut c = Config::default();
        c.set("web_password", "keepme").unwrap();
        let mut errs = std::string::String::new();
        let n = apply_form(
            b"wifi1_ssid=Caf%C3%A9+Net&hour12=on&web_password=&timezone=Europe%2FBerlin&brightness=250&bogus=1&geo_rule=JST-9",
            &mut c, &mut errs,
        );
        assert_eq!(c.wifi[0].ssid.as_str(), "Café Net");
        assert!(c.hour12);
        assert_eq!(c.timezone.as_str(), "Europe/Berlin");
        assert_eq!(c.web_password.as_str(), "keepme", "blank secret keeps the old value");
        assert!(c.geo_rule.is_empty(), "cache is not settable from the web");
        assert_eq!(n, 3);
        assert!(errs.contains("invalid brightness"), "{errs}");
        // Unchanged values do not count; bad escapes and overlong fields are reported, not applied.
        let mut errs = std::string::String::new();
        assert_eq!(apply_form(b"hour12=on&wifi_ssid=%zz&timezone=%", &mut c, &mut errs), 0);
        assert!(errs.contains("malformed"));
        let long = format!("wifi_ssid={}", "a".repeat(300));
        let mut errs = std::string::String::new();
        apply_form(long.as_bytes(), &mut c, &mut errs);
        assert!(errs.contains("malformed"));
        assert_eq!(apply_form(&[0xFF, 0xFE], &mut c, &mut errs), 0);
    }

    #[test]
    fn page_escapes_and_hides_secrets() {
        let mut c = Config::default();
        c.set("wifi_ssid", "<script>\"x\"&").unwrap();
        c.set("wifi_password", "topsecret1").unwrap();
        c.set("web_password", "adminpw").unwrap();
        let mut page = heapless::String::<12288>::new();
        render_page(&c, &Status::default(), Some(("a <b> message", true)), &mut page).unwrap();
        assert!(!page.contains("<script>"), "ssid must be escaped");
        assert!(page.contains("&lt;script&gt;&quot;x&quot;&amp;"));
        assert!(page.contains("a &lt;b&gt; message"));
        assert!(!page.contains("topsecret1") && !page.contains("adminpw"));
        assert!(page.contains("leave blank to keep"));
        assert!(page.contains("name=hour12><option value=on"));
        assert!(page.len() < 11000, "page is {} bytes", page.len());
    }

    #[test]
    fn page_with_all_slots_full_fits() {
        let mut c = Config::default();
        for n in 1..=crate::config::WIFI_SLOTS {
            c.set(&format!("wifi{n}_ssid"), &"<".repeat(32)).unwrap();
            c.set(&format!("wifi{n}_password"), &"p".repeat(63)).unwrap();
        }
        let mut page = heapless::String::<12288>::new();
        render_page(&c, &Status::default(), None, &mut page).expect("worst-case page must fit");
        assert!(page.contains("name=wifi8_ssid") && page.contains("name=wifi8_password"));
        assert!(!page.contains(&"p".repeat(63)));
    }

    #[test]
    fn page_overflow_is_an_error_not_a_panic() {
        let mut tiny = heapless::String::<100>::new();
        assert!(render_page(&Config::default(), &Status::default(), None, &mut tiny).is_err());
    }
}
