//! Tiny HTTP/1.x server-side helpers for the configuration web page: request parsing with hard limits, Basic auth,
//! form decoding and the HTML page. All input is untrusted LAN input, so everything here is bounded.

use core::fmt::Write;

use crate::cli::Status;
use crate::config::{Config, Group, KEYS};

pub const MAX_REQUEST: usize = 8192;
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

/// Whether a factory reset request may be served: not over the plain port while HTTPS is on, because without a web password
/// nothing else would stop any host on the LAN from erasing the clock. (A build without TLS passes `https_on = false`.)
pub fn reset_allowed(secure: bool, https_on: bool) -> bool {
    secure || !https_on
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
        // `Content-Length : 9` is not the Content-Length header (RFC 9112 section 5.1): refuse it rather than read it as an
        // unknown header and drop the body it announces. A name that is empty or starts with a space (obsolete line folding)
        // is refused too.
        if name.is_empty() || name.starts_with([' ', '\t']) || name.ends_with([' ', '\t']) {
            return Parse::Bad;
        }
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

/// CSRF defence for browser-submitted forms: if the browser sent an Origin, its scheme must be the one of this connection
/// (`https` when `secure`, `http` otherwise) and its host must be the Host we were asked for.
pub fn same_origin(req: &Request, secure: bool) -> bool {
    match (req.origin, req.host) {
        (None, _) => true, // non-browser clients (curl) send no Origin
        (Some(o), Some(h)) => o.split_once("://").is_some_and(|(scheme, oh)| scheme == if secure { "https" } else { "http" } && oh == h),
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

/// Where a plain-HTTP page request should be sent when HTTPS is available: `https://<host><path>` for a GET. The host is
/// the Host header if it is made only of host-name characters (so nothing can be injected into the `Location` header), and
/// otherwise `fallback_host`, the clock's own address (a client without a usable Host header still gets to HTTPS). The port
/// is dropped, because the HTTPS port is the default one. `None` for a request that is not a GET or has an unsafe path:
/// that is refused, never served over the plain connection.
pub fn https_redirect(req: &Request, fallback_host: &str) -> Option<heapless::String<128>> {
    if req.method != Method::Get {
        return None;
    }
    let host = req.host.filter(|h| host_part(h).is_some()).unwrap_or(fallback_host);
    let bare = host_part(host)?;
    let path_ok = req.path.starts_with('/') && req.path.bytes().all(|b| (0x21..0x7F).contains(&b));
    if !path_ok {
        return None;
    }
    let mut s = heapless::String::new();
    write!(s, "https://{bare}{}", req.path).ok()?;
    Some(s)
}

/// The host of a Host header value without its port, if it is only host-name characters: `name`, `name:80`, `1.2.3.4:80`,
/// `[::1]:80`.
fn host_part(host: &str) -> Option<&str> {
    let bare = if let Some(rest) = host.strip_prefix('[') {
        let end = rest.find(']')?;
        if !rest[end + 1..].is_empty() && !rest[end + 1..].starts_with(':') {
            return None;
        }
        &host[..end + 2]
    } else {
        host.split(':').next()?
    };
    (!bare.is_empty() && bare.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'))).then_some(bare)
}

/// What the roots form asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum RootForm {
    /// Add the certificate(s) in this PEM text.
    Add(heapless::String<{ crate::cli::PASTE_MAX }>),
    /// Remove user root number n (1-based).
    Remove(usize),
}

/// Reads the roots form: `pem=<text>` or `remove=<n>`. `None` if neither is usable.
pub fn parse_root_form(body: &[u8]) -> Option<RootForm> {
    let body = core::str::from_utf8(body).ok()?;
    let mut buf = [0u8; crate::cli::PASTE_MAX];
    for pair in body.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        match k {
            "pem" => {
                let text = url_decode(v, &mut buf)?;
                return (!text.trim().is_empty()).then(|| heapless::String::try_from(text).ok().map(RootForm::Add))?;
            }
            "remove" => return v.trim().parse::<usize>().ok().filter(|&n| n >= 1).map(RootForm::Remove),
            _ => {}
        }
    }
    None
}

/// What the factory reset form sent.
#[derive(Debug, PartialEq, Eq)]
pub enum ResetForm {
    /// No confirmation word yet (the button on the main page): show the question.
    Ask,
    /// The word was entered correctly.
    Confirmed,
    /// A word was entered, but not the right one.
    Refused,
}

/// Reads the factory reset form: `confirm=<word>`. Anything but the exact word never erases.
pub fn parse_reset_form(body: &[u8]) -> ResetForm {
    let Ok(body) = core::str::from_utf8(body) else { return ResetForm::Refused };
    for pair in body.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        if k == "confirm" {
            let mut buf = [0u8; 32];
            return match url_decode(v, &mut buf) {
                Some(word) if crate::cli::reset_confirmed(word) => ResetForm::Confirmed,
                _ => ResetForm::Refused,
            };
        }
    }
    ResetForm::Ask
}

/// The second question of the factory reset: what is erased and a field for the confirmation word.
pub fn render_reset_confirm(msg: Option<&str>, out: &mut impl Write) -> core::fmt::Result {
    out.write_str(PAGE_HEAD)?;
    if let Some(m) = msg {
        write!(out, "<p class=\"msg err\">{}</p>", Esc(m))?;
    }
    write!(
        out,
        "<h2>Factory reset</h2><p>{}</p><form method=post action=/reset><label>Type {} to confirm<input name=confirm autocomplete=off required></label>\
<button>Erase everything</button></form><p><a href=/>Cancel</a></p></body></html>",
        Esc(crate::cli::RESET_WARNING),
        crate::cli::RESET_WORD
    )
}

/// Shown after a confirmed factory reset, just before the clock restarts.
pub const RESET_DONE_PAGE: &str = "<!doctype html><meta charset=utf-8><title>picoclock</title><body style=\"font-family:sans-serif;max-width:40rem;margin:2rem auto\">\
<h1>picoclock</h1><p>Everything is erased and the clock is restarting. It will not join a WiFi network until you give it one over the USB terminal.</p></body>";

/// Why a certificate upload was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum CertFormError {
    /// A field is missing or empty.
    Missing,
    /// A field is larger than we accept.
    TooLarge,
    /// The certificate text is not PEM with P-256 certificates.
    BadCertificate,
    /// The key is not a P-256 private key in PEM or DER.
    BadKey,
    /// The first certificate is not for this key (wrong order, or another key).
    Mismatch,
    Expired,
    NotYetValid,
}

impl CertFormError {
    pub fn message(&self) -> &'static str {
        match self {
            CertFormError::Missing => "both the certificate and the private key are needed",
            CertFormError::TooLarge => "the certificate chain is too large (at most about 3 KB, four certificates)",
            CertFormError::BadCertificate => "the certificate must be PEM with ECDSA P-256 certificates (RSA and other curves are not supported)",
            CertFormError::BadKey => "the private key must be a P-256 key in PEM (PRIVATE KEY or EC PRIVATE KEY)",
            CertFormError::Mismatch => "the first certificate does not belong to this private key; put the server's own certificate first",
            CertFormError::Expired => "that certificate has expired",
            CertFormError::NotYetValid => "that certificate is not valid yet (or the clock is wrong)",
        }
    }
}

/// Reads the certificate upload form (`cert` = PEM chain, `key` = PEM key) and checks it against the time `now_unix`.
pub fn parse_cert_form(body: &[u8], now_unix: i64) -> Result<crate::tlsstore::Material, CertFormError> {
    let body = core::str::from_utf8(body).map_err(|_| CertFormError::BadCertificate)?;
    // the PEM text of a full chain: base64 is 4/3 of the DER size, plus line breaks and the BEGIN/END lines of four certificates
    let (mut cert_buf, mut key_buf) = ([0u8; crate::x509::MAX_CHAIN * 3 / 2 + 512], [0u8; 1024]);
    let (mut cert, mut key) = (None, None);
    for pair in body.split('&').filter(|p| !p.is_empty()) {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        match k {
            "cert" if cert.is_none() => cert = Some(url_decode(v, &mut cert_buf).ok_or(CertFormError::TooLarge)?),
            "key" if key.is_none() => key = Some(url_decode(v, &mut key_buf).ok_or(CertFormError::BadKey)?), // longer than any P-256 key: some other kind of key
            _ => {}
        }
    }
    let (cert, key) = (cert.filter(|c| !c.trim().is_empty()).ok_or(CertFormError::Missing)?, key.filter(|k| !k.trim().is_empty()).ok_or(CertFormError::Missing)?);
    let mut chain = heapless::Vec::new();
    let mut lens = heapless::Vec::new();
    crate::x509::parse_pem_chain(cert, &mut chain, &mut lens).map_err(|e| match e {
        crate::x509::X509Error::TooLarge => CertFormError::TooLarge,
        _ => CertFormError::BadCertificate,
    })?;
    let secret = crate::x509::parse_private_key(key.as_bytes()).map_err(|_| CertFormError::BadKey)?;
    let material = crate::tlsstore::Material { key: secret, uploaded: true, chain, lens };
    let checked = {
        let mut certs = material.certs();
        let leaf = certs.next().ok_or(CertFormError::BadCertificate)?;
        if !certs.all(crate::x509::is_certificate_der) {
            return Err(CertFormError::BadCertificate);
        }
        crate::x509::validate_pair(leaf, &secret, now_unix)
    };
    match checked {
        Ok(_) => Ok(material),
        Err(crate::x509::X509Error::KeyMismatch) => Err(CertFormError::Mismatch),
        Err(crate::x509::X509Error::Expired) => Err(CertFormError::Expired),
        Err(crate::x509::X509Error::NotYetValid) => Err(CertFormError::NotYetValid),
        Err(_) => Err(CertFormError::BadCertificate),
    }
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
input,select,textarea{width:100%;box-sizing:border-box;padding:.4rem;font:inherit}button{margin-top:1rem;padding:.5rem 1.2rem;font:inherit}\
.msg{background:#eef;padding:.5rem;border-radius:4px}.err{background:#fdd}pre{background:#f4f4f4;padding:.5rem;overflow:auto}\
@media(prefers-color-scheme:dark){body{background:#111;color:#eee}small{color:#aaa}pre{background:#222}.msg{background:#223}.err{background:#522}input,select,textarea{background:#222;color:#eee;border:1px solid #555}}textarea{font:12px monospace}\
nav{display:flex;flex-wrap:wrap;gap:.3rem;margin:.5rem 0}nav a{padding:.6rem .9rem;border:1px solid #888;border-radius:4px;text-decoration:none;color:inherit}nav a[aria-current]{background:#468;color:#fff}\
</style></head><body><h1>picoclock</h1>";

/// Room for the `status` and `time` text on the page. A fully connected clock needs about 1 KB (a test keeps it honest);
/// a smaller buffer cut the text off, mid-line, without a word.
const STATUS_BUF: usize = 2560;

/// The pages of the web interface: one per group of settings, so that no page has to hold everything at once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tab {
    Status,
    Wifi,
    Time,
    Clock,
    Web,
    Security,
}

impl Tab {
    pub const ALL: [Tab; 6] = [Tab::Status, Tab::Wifi, Tab::Time, Tab::Clock, Tab::Web, Tab::Security];

    pub fn slug(self) -> &'static str {
        match self {
            Tab::Status => "status",
            Tab::Wifi => "wifi",
            Tab::Time => "time",
            Tab::Clock => "clock",
            Tab::Web => "web",
            Tab::Security => "security",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Tab::Status => "Status",
            Tab::Wifi => "WiFi",
            Tab::Time => "Time",
            Tab::Clock => "Clock",
            Tab::Web => "Web",
            Tab::Security => "Security",
        }
    }

    fn group(self) -> Option<Group> {
        match self {
            Tab::Wifi => Some(Group::Wifi),
            Tab::Time => Some(Group::Time),
            Tab::Clock => Some(Group::Clock),
            Tab::Web => Some(Group::Web),
            Tab::Status | Tab::Security => None,
        }
    }
}

/// Splits a request path into the path and the query (without the `?`).
pub fn split_query(path: &str) -> (&str, &str) {
    path.split_once('?').unwrap_or((path, ""))
}

/// The tab named by `tab=...` in a query string; anything else is the status page. The text is only compared, never echoed.
pub fn tab_from_query(query: &str) -> Tab {
    query
        .split('&')
        .find_map(|p| p.strip_prefix("tab="))
        .and_then(|v| Tab::ALL.into_iter().find(|t| t.slug().eq_ignore_ascii_case(v)))
        .unwrap_or(Tab::Status)
}

/// Renders one page of the web interface. Fails (`fmt::Error`) only if `out` is too small.
pub fn render_page(cfg: &Config, st: &Status, msg: Option<(&str, bool)>, secure: bool, roots: &crate::rootstore::Roots, tab: Tab, out: &mut impl Write) -> core::fmt::Result {
    out.write_str(PAGE_HEAD)?;
    out.write_str("<nav>")?;
    for t in Tab::ALL {
        write!(out, "<a href=\"/?tab={}\"{}>{}</a>", t.slug(), if t == tab { " aria-current=page" } else { "" }, t.label())?;
    }
    out.write_str("</nav>")?;
    if let Some((m, is_err)) = msg {
        write!(out, "<p class=\"msg{}\">{}</p>", if is_err { " err" } else { "" }, Esc(m))?;
    }
    match tab {
        Tab::Status => {
            out.write_str("<pre>")?;
            let mut status = heapless::String::<STATUS_BUF>::new();
            crate::cli::execute("status", &mut cfg.clone(), st, &mut status);
            let _ = crate::cli::execute("time", &mut cfg.clone(), st, &mut status);
            write!(out, "{}</pre>", Esc(&status))?;
            // only asks GitHub whether a newer release exists; nothing is installed
            out.write_str("<form method=post action=/update-check><button>Check for updates now</button></form>")?;
        }
        Tab::Security => security(st, secure, roots, out)?,
        Tab::Wifi | Tab::Time | Tab::Clock | Tab::Web => {
            // each tab is its own form: saving sends only the settings shown on it
            write!(out, "<form method=post action=\"/save?tab={}\">", tab.slug())?;
            for k in KEYS.iter().filter(|k| k.name != "geo_rule" && Some(k.group) == tab.group()) {
                setting(cfg, k, out)?;
            }
            out.write_str("<button>Save</button></form>")?;
            if tab == Tab::Time {
                out.write_str("<p><a href=/zones>List of timezone names</a></p>")?;
            }
        }
    }
    write!(out, "<hr><p><small>picoclock {} is free software under the <a href=\"https://www.gnu.org/licenses/agpl-3.0.html\">GNU AGPL v3 or later</a>. Source code: <a href=\"https://github.com/dc0sk/picoclock\">github.com/dc0sk/picoclock</a></small></p></body></html>", crate::VERSION)
}

/// One setting as a labelled field.
fn setting(cfg: &Config, k: &crate::config::KeyInfo, out: &mut impl Write) -> core::fmt::Result {
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
    out.write_str("</label>")
}

/// The factory reset, and (over TLS, once HTTPS runs) the certificate and the NTS trust anchors.
fn security(st: &Status, secure: bool, roots: &crate::rootstore::Roots, out: &mut impl Write) -> core::fmt::Result {
    // The button only asks; the erase needs the confirmation word on the next page.
    out.write_str("<form method=post action=/reset><h2>Factory reset</h2><p>Erase all settings and start over. You are asked to confirm on the next page.</p><button>Factory reset...</button></form>")?;
    // Replacing the certificate means sending a private key: offered only over TLS, and only once the HTTPS server runs.
    if secure && st.https_state == 2 {
        out.write_str(
            "<form method=post action=/cert><h2>HTTPS certificate</h2><p>Replace the certificate with your own. P-256 (ECDSA) only, in PEM form: \
the server's own certificate first, then any intermediates, and the matching private key. The key is stored on the clock and never shown again.</p>\
<label>Certificate chain (PEM)<textarea name=cert rows=8 placeholder=\"-----BEGIN CERTIFICATE-----\"></textarea></label>\
<label>Private key (PEM)<textarea name=key rows=4 placeholder=\"-----BEGIN PRIVATE KEY-----\" autocomplete=off></textarea></label>\
<button>Replace certificate</button></form>",
        )?;
        // Trusting a certificate authority changes who the clock believes when it asks the time over NTS, so this is offered
        // only over TLS too (a plain-HTTP form could be rewritten by anyone on the LAN).
        out.write_str("<h2>Trusted authorities for NTS</h2><p>The clock trusts the built-in authorities (ISRG Root X2 and X1, SSL.com ECC) plus these:</p><pre>")?;
        if roots.is_empty() {
            out.write_str("(none added)")?;
        }
        let now = st.utc_unix;
        for i in 0..roots.len() {
            match roots.describe(i) {
                Some(info) => write!(out, "{}\n", Esc(&crate::rootstore::summary(i + 1, &info, now)))?,
                None => write!(out, "{}  (unreadable)\n", i + 1)?,
            }
        }
        out.write_str(
            "</pre><form method=post action=/root>\
<label>Add a certificate authority (PEM; compare its fingerprint with the one your CA publishes)<textarea name=pem rows=6 placeholder=\"-----BEGIN CERTIFICATE-----\"></textarea></label>\
<button>Add</button></form>\
<form method=post action=/root><label>Remove number<input name=remove type=number min=1 max=4 required></label><button>Remove</button></form>",
        )?;
    }
    Ok(())
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

    // VERIFIES: REQ-WEB-02
    #[test]
    fn parses_get_and_post() {
        let Parse::Complete(r) = req("GET / HTTP/1.1\r\nHost: 10.0.0.5\r\nAuthorization: Basic abc\r\n\r\n") else { panic!() };
        assert_eq!((r.method, r.path, r.host, r.authorization), (Method::Get, "/", Some("10.0.0.5"), Some("Basic abc")));
        let Parse::Complete(r) = req("POST /save HTTP/1.1\r\ncontent-length: 7\r\n\r\nhour12=on") else { panic!() };
        assert_eq!(r.body, b"hour12=");
    }

    // VERIFIES: REQ-WEB-02
    #[test]
    fn incomplete_until_body_arrives() {
        assert_eq!(req("GET / HTTP/1.1\r\nHost: x\r\n"), Parse::Incomplete);
        assert_eq!(req("POST /save HTTP/1.1\r\nContent-Length: 10\r\n\r\nabc"), Parse::Incomplete);
    }

    // VERIFIES: REQ-WEB-02
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

    // VERIFIES: REQ-WEB-02
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

    // VERIFIES: REQ-WEB-03
    #[test]
    fn csrf_origin_check() {
        let mk = |o, h| Request { method: Method::Post, path: "/", body: b"", authorization: None, host: h, origin: o };
        for secure in [false, true] {
            assert!(same_origin(&mk(None, Some("1.2.3.4")), secure));
            assert!(!same_origin(&mk(Some("http://evil.example"), Some("1.2.3.4")), secure));
            assert!(!same_origin(&mk(Some("https://evil.example"), Some("1.2.3.4")), secure));
            assert!(!same_origin(&mk(Some("null"), Some("1.2.3.4")), secure));
            assert!(!same_origin(&mk(Some("http://1.2.3.4"), None), secure));
            assert!(!same_origin(&mk(Some("1.2.3.4"), Some("1.2.3.4")), secure), "no scheme");
        }
        assert!(same_origin(&mk(Some("http://1.2.3.4"), Some("1.2.3.4")), false));
        assert!(!same_origin(&mk(Some("https://1.2.3.4"), Some("1.2.3.4")), false), "an https page posting to the plain http port");
        assert!(same_origin(&mk(Some("https://1.2.3.4"), Some("1.2.3.4")), true));
        assert!(!same_origin(&mk(Some("http://1.2.3.4"), Some("1.2.3.4")), true), "a plain http page posting to the https port");
        assert!(same_origin(&mk(Some("https://picoclock-d6:443"), Some("picoclock-d6:443")), true), "a port is part of both");
    }

    // VERIFIES: REQ-WEB-04
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

    // VERIFIES: REQ-CFG-01 REQ-CFG-03 REQ-WEB-01
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

    // VERIFIES: REQ-WEB-05 REQ-CFG-03
    #[test]
    fn page_escapes_and_hides_secrets() {
        let mut c = Config::default();
        c.set("wifi_ssid", "<script>\"x\"&").unwrap();
        c.set("wifi_password", "topsecret1").unwrap();
        c.set("web_password", "adminpw").unwrap();
        let mut page = heapless::String::<12288>::new();
        for tab in [Tab::Wifi, Tab::Web, Tab::Clock] {
            let mut one = heapless::String::<12288>::new();
            render_page(&c, &Status::default(), Some(("a <b> message", true)), false, &crate::rootstore::Roots::new(), tab, &mut one).unwrap();
            assert!(one.len() < 6000, "{tab:?} is {} bytes", one.len());
            let _ = page.push_str(&one);
        }
        assert!(!page.contains("<script>"), "ssid must be escaped");
        assert!(page.contains("&lt;script&gt;&quot;x&quot;&amp;"));
        assert!(page.contains("a &lt;b&gt; message"));
        assert!(!page.contains("topsecret1") && !page.contains("adminpw"));
        assert!(page.contains("leave blank to keep"));
        assert!(page.contains("name=hour12><option value=on"));
    }

    // VERIFIES: REQ-WEB-01 REQ-WEB-05
    #[test]
    fn page_with_all_slots_full_fits() {
        let mut c = Config::default();
        for n in 1..=crate::config::WIFI_SLOTS {
            c.set(&format!("wifi{n}_ssid"), &"<".repeat(32)).unwrap();
            c.set(&format!("wifi{n}_password"), &"p".repeat(63)).unwrap();
        }
        let mut page = heapless::String::<12288>::new();
        render_page(&c, &Status::default(), None, false, &crate::rootstore::Roots::new(), Tab::Wifi, &mut page).expect("worst-case page must fit");
        assert!(page.len() < 6000, "{} bytes: the WiFi tab leaves room", page.len());
        assert!(page.contains("name=wifi8_ssid") && page.contains("name=wifi8_password"));
        assert!(!page.contains(&"p".repeat(63)));
    }

    // VERIFIES: REQ-CFG-09
    #[test]
    fn factory_reset_takes_two_steps_on_the_web_page() {
        assert_eq!(parse_reset_form(b""), ResetForm::Ask, "the button alone only asks");
        assert_eq!(parse_reset_form(b"other=1"), ResetForm::Ask);
        assert_eq!(parse_reset_form(b"confirm=RESET"), ResetForm::Confirmed);
        assert_eq!(parse_reset_form(b"x=1&confirm=RESET"), ResetForm::Confirmed);
        for bad in ["confirm=", "confirm=reset", "confirm=YES", "confirm=RESET%20NOW", "confirm=RESE", "confirm=%FF"] {
            assert_eq!(parse_reset_form(bad.as_bytes()), ResetForm::Refused, "{bad}");
        }
        assert_eq!(parse_reset_form(&[0xff, 0xfe]), ResetForm::Refused, "not text");
        // the main page only has the button; the confirmation word is asked on the second page
        let mut page = heapless::String::<12288>::new();
        render_page(&Config::default(), &Status::default(), None, false, &crate::rootstore::Roots::new(), Tab::Security, &mut page).unwrap();
        assert!(page.contains("action=/reset") && !page.contains("name=confirm"));
        let mut ask = heapless::String::<4096>::new();
        render_reset_confirm(Some("The word did not match <b>"), &mut ask).unwrap();
        assert!(ask.contains("name=confirm") && ask.contains("action=/reset") && ask.contains("erases ALL settings"));
        assert!(ask.contains("&lt;b&gt;") && !ask.contains("<b>"), "the message is escaped");
    }

    // VERIFIES: REQ-CFG-07
    #[test]
    fn page_shows_name_and_mac_address() {
        let st = Status { mac: Some([0x28, 0xcd, 0xc1, 0x12, 0x34, 0xab]), ..Default::default() };
        let mut page = heapless::String::<12288>::new();
        render_page(&Config::default(), &st, None, false, &crate::rootstore::Roots::new(), Tab::Status, &mut page).unwrap();
        assert!(page.contains("mac     : 28:cd:c1:12:34:ab") && page.contains("name    : picoclock-ab"));
        let mut c = Config::default();
        c.set("hostname", "uhr").unwrap();
        let mut page = heapless::String::<12288>::new();
        render_page(&c, &st, None, false, &crate::rootstore::Roots::new(), Tab::Status, &mut page).unwrap();
        assert!(page.contains("name    : uhr"));
        let mut page = heapless::String::<12288>::new();
        render_page(&c, &st, None, false, &crate::rootstore::Roots::new(), Tab::Web, &mut page).unwrap();
        assert!(page.contains("name=hostname value=\"uhr\""));
        // the form changes the name like the terminal does, and a bad one is refused
        let mut errors = heapless::String::<256>::new();
        assert_eq!(apply_form(b"hostname=Wohnzimmer", &mut c, &mut errors), 1);
        assert_eq!(c.hostname.as_str(), "wohnzimmer");
        assert_eq!(apply_form(b"hostname=bad%20name", &mut c, &mut errors), 0);
        assert!(errors.contains("invalid hostname") && c.hostname.as_str() == "wohnzimmer");
    }

    // VERIFIES: REQ-WEB-01
    #[test]
    fn status_block_of_a_busy_clock_is_not_cut_off() {
        // every optional line of `status` present, with long values: the longest the page can show
        let st = Status {
            utc_unix: Some(1_800_000_000),
            last_sync_unix: Some(1_799_999_000),
            wifi_up: true,
            wifi_slot: Some(8),
            wifi_scanned: Some(24),
            mac: Some([0x2c, 0xcf, 0x67, 0xb8, 0x84, 0xd6]),
            ip: Some([192, 168, 121, 41]),
            gateway: Some([192, 168, 121, 1]),
            temp_c10: Some(-123),
            chip_temp_c10: Some(332),
            light_raw: 4095,
            scan_late_max_us: 123_456,
            scan_late_count: 99_999,
            uptime_s: 4_000_000,
            sync_secure: true,
            https_state: 2,
            rng_hardware: true,
            dhcp_ntp: [[192, 168, 121, 1], [10, 255, 255, 254], [172, 16, 100, 100], [192, 168, 100, 200]],
            dhcp_ntp_n: 4,
            roots_user: 4,
            nts_ke: 1,
            nts_ke_ms: 65_000,
            nts_ke_host: [b'h'; 32],
            mdns_group: 1,
            mdns_seen: 65_000,
            mdns_answered: 65_000,
            heap_used: 147_456,
            heap_peak: 147_456,
            heap_size: 147_456,
            cert_fp: [0xAB; 32],
            cert_not_after: 2_100_000_000,
            cert_uploaded: true,
            unsaved: true,
            watchdog_reset: false,
            sync_dcf: false,
            sources: [
                Some(crate::sources::Report { source: crate::sources::Source::Ntp, state: crate::sources::State::NoMajority, offset_ms: Some(-3_600_000), age_s: Some(10_800) }),
                Some(crate::sources::Report { source: crate::sources::Source::Dcf77, state: crate::sources::State::Standby, offset_ms: Some(3_600_000), age_s: Some(900) }),
            ],
            dcf: crate::dcf77::Stats { pulses: 4_000_000_000, frames_ok: 4_000_000_000, frames_bad: 4_000_000_000, pulses_bad: 4_000_000_000 },
            dhcp_server: Some([192, 168, 121, 1]),
            prefix: 24,
            dns: Some([192, 168, 121, 1]),
            lease_changes: 255,
            link_rssi: Some(-92),
            aps: [Some(crate::cli::ApSeen { bssid: [0xdc, 0x39, 0x6f, 0xce, 0x6d, 0xf9], channel: 11, rssi: -92 }); 6],
            update: crate::update::Check::Newer,
            update_latest: Some(crate::update::Version { major: 65535, minor: 65535, patch: 65535 }),
            update_checked_unix: Some(1_700_000_000),
            geo: [crate::cli::GeoTry { stage: 5, bytes: 999, end: 1, addr: [255, 255, 255, 255] }; crate::geo::PROVIDERS.len()],
        };
        let mut c = Config::default();
        c.set("hostname", &"h".repeat(crate::config::HOSTNAME_MAX)).unwrap();
        c.set("timezone", "Europe/Berlin").unwrap();
        c.set("time_sources", "dcf77,ntp").unwrap();
        c.set("time_mode", "vote").unwrap();
        let mut page = heapless::String::<12288>::new();
        render_page(&c, &st, None, true, &crate::rootstore::Roots::new(), Tab::Status, &mut page).unwrap();
        let pre = page.split("<pre>").nth(1).unwrap().split("</pre>").next().unwrap();
        // the last line of `status` and both lines of `time` must be whole
        assert!(pre.contains("\nscan    :"), "{pre}");
        assert!(pre.contains("\nlocal"), "{pre}");
        assert!(pre.trim_end().lines().last().unwrap().starts_with("local"), "nothing torn after the last line: {pre}");
        assert!(pre.len() < STATUS_BUF - 100, "keep headroom: {} of {}", pre.len(), STATUS_BUF);
    }

    // VERIFIES: REQ-WEB-02
    #[test]
    fn header_names_with_spaces_are_refused() {
        for bad in [
            "POST /save HTTP/1.1\r\nContent-Length : 9\r\n\r\nhour12=on",
            "POST /save HTTP/1.1\r\nContent-Length\t: 9\r\n\r\nhour12=on",
            "GET / HTTP/1.1\r\n Host: x\r\n\r\n",
            "GET / HTTP/1.1\r\n: x\r\n\r\n",
        ] {
            assert_eq!(req(bad), Parse::Bad, "{bad:?}");
        }
        assert!(matches!(req("POST /save HTTP/1.1\r\nContent-Length: 9\r\n\r\nhour12=on"), Parse::Complete(_)));
    }

    // VERIFIES: REQ-WEB-02 REQ-WEB-01
    #[test]
    fn page_overflow_is_an_error_not_a_panic() {
        let mut tiny = heapless::String::<100>::new();
        assert!(render_page(&Config::default(), &Status::default(), None, false, &crate::rootstore::Roots::new(), Tab::Status, &mut tiny).is_err());
    }

    fn get(host: Option<&'static str>, path: &'static str) -> Request<'static> {
        Request { method: Method::Get, path, body: b"", authorization: None, host, origin: None }
    }

    // VERIFIES: REQ-WEB-08
    #[test]
    fn redirect_target_is_built_only_from_safe_parts() {
        let r = |h, p| https_redirect(&get(h, p), "192.0.2.7").map(|s| s.to_string());
        assert_eq!(r(Some("picoclock-d6"), "/"), Some("https://picoclock-d6/".into()));
        assert_eq!(r(Some("192.168.1.41"), "/zones"), Some("https://192.168.1.41/zones".into()));
        assert_eq!(r(Some("192.168.1.41:80"), "/"), Some("https://192.168.1.41/".into()), "the port is dropped");
        assert_eq!(r(Some("picoclock-d6.local:8080"), "/x"), Some("https://picoclock-d6.local/x".into()));
        assert_eq!(r(Some("[fe80::1]:80"), "/"), Some("https://[fe80::1]/".into()));
        assert_eq!(r(Some("[::1]"), "/"), Some("https://[::1]/".into()));
        // nothing that could break out of the header line or point somewhere else: such a Host is replaced by our own address
        for bad in ["evil.example\r\nSet-Cookie: x=1", "a b", "a/b", "evil.example/x", "a@b", "host\"", "", "[::1]x", "[::1", "a\u{e9}.example"] {
            let got = r(Some(bad), "/").unwrap();
            assert_eq!(got, "https://192.0.2.7/", "{bad:?} -> {got}");
        }
        assert_eq!(r(None, "/x"), Some("https://192.0.2.7/x".into()), "no Host header (HTTP/1.0)");
        // a path that is not safe to put in a header is refused outright
        for bad in ["/a b", "/a\r\nX: y", "x", "/\u{e9}"] {
            assert_eq!(r(Some("h"), bad), None, "{bad:?}");
        }
        // only a GET is redirected: a POST would carry its data over the plain connection
        let post = Request { method: Method::Post, ..get(Some("h"), "/save") };
        assert_eq!(https_redirect(&post, "192.0.2.7"), None);
        // and a bad fallback never produces a bad header either
        assert_eq!(https_redirect(&get(None, "/"), "evil\r\nX: y"), None);
    }

    // VERIFIES: REQ-WEB-11
    #[test]
    fn every_setting_is_on_exactly_one_tab() {
        let c = Config::default();
        let page = |tab| {
            let mut p = heapless::String::<12288>::new();
            render_page(&c, &Status::default(), None, false, &crate::rootstore::Roots::new(), tab, &mut p).unwrap();
            p
        };
        for k in KEYS.iter().filter(|k| k.name != "geo_rule") {
            let on: std::vec::Vec<_> = Tab::ALL.into_iter().filter(|t| page(*t).contains(&format!("<label>{}<small>", k.name))).collect();
            assert_eq!(on.len(), 1, "{} appears on {on:?}", k.name);
        }
        assert!(Tab::ALL.into_iter().all(|t| !page(t).contains("<label>geo_rule<")), "the location cache is not a setting");
        // the status text only on the status tab, the settings forms post back to their own tab
        for t in Tab::ALL {
            let p = page(t);
            assert_eq!(p.contains("<pre>"), t == Tab::Status, "{t:?}");
            if matches!(t, Tab::Wifi | Tab::Time | Tab::Clock | Tab::Web) {
                assert!(p.contains(&format!("action=\"/save?tab={}\"", t.slug())), "{t:?}");
            }
            // six links, the open one marked
            assert_eq!(p.matches("<nav>").count() + p.matches("<a href=\"/?tab=").count(), 7, "{t:?}");
            assert_eq!(p.matches("aria-current=page").count(), 1);
            assert!(p.contains(&format!("href=\"/?tab={}\" aria-current=page", t.slug())), "{t:?}");
            assert!(p.len() < 6500, "{t:?} is {} bytes", p.len());
        }
    }

    // VERIFIES: REQ-UPD-01
    #[test]
    fn only_the_status_page_offers_the_update_check_and_it_shows_the_result() {
        let mut c = Config::default();
        let st = Status { update: crate::update::Check::Newer, update_latest: crate::update::Version::parse("9.9.9"), update_checked_unix: Some(1_800_000_000), utc_unix: Some(1_800_000_600), ..Default::default() };
        for tab in Tab::ALL {
            let mut p = heapless::String::<12288>::new();
            render_page(&c, &st, None, true, &crate::rootstore::Roots::new(), tab, &mut p).unwrap();
            assert_eq!(p.contains("action=/update-check"), tab == Tab::Status, "{tab:?}");
            assert_eq!(p.contains("9.9.9 is available"), tab == Tab::Status, "the result is part of the status text on the Status page: {tab:?}");
        }
        c.set("update_check", "off").unwrap();
        assert!(!c.update_check);
    }

    // VERIFIES: REQ-WEB-11
    #[test]
    fn the_tab_comes_from_the_query_and_is_never_echoed() {
        for (q, want) in [("", Tab::Status), ("tab=wifi", Tab::Wifi), ("tab=WIFI", Tab::Wifi), ("x=1&tab=time", Tab::Time), ("tab=clock&x", Tab::Clock),
            ("tab=web", Tab::Web), ("tab=security", Tab::Security), ("tab=%ff", Tab::Status), ("tab=<script>", Tab::Status), ("tab=", Tab::Status),
            ("tab=wifi2", Tab::Status), ("tabs=wifi", Tab::Status), ("xtab=wifi", Tab::Status)] {
            assert_eq!(tab_from_query(q), want, "{q:?}");
        }
        assert_eq!(split_query("/save?tab=wifi"), ("/save", "tab=wifi"));
        assert_eq!(split_query("/zones"), ("/zones", ""));
        assert_eq!(split_query("/?tab=time&a=b?c"), ("/", "tab=time&a=b?c"));
        let mut page = heapless::String::<12288>::new();
        render_page(&Config::default(), &Status::default(), None, false, &crate::rootstore::Roots::new(), tab_from_query("tab=<script>alert(1)</script>"), &mut page).unwrap();
        assert!(!page.contains("alert(1)"));
    }

    // VERIFIES: REQ-WEB-11 REQ-WEB-01
    #[test]
    fn the_busiest_pages_fit_with_room_to_spare() {
        // the busy status block alone, and the security page with four trust anchors and a long error message
        let mut busy = std::string::String::new();
        let mut roots = crate::rootstore::Roots::new();
        for seed in 1..=4u8 {
            let cert = crate::x509::generate_self_signed([seed; 32], [seed; 8], &["a-fairly-long-authority-name.example"], 1_792_000_000).unwrap();
            roots.add_pem(&format!("-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n", b64(&cert)), None).unwrap();
        }
        let st = Status { https_state: 2, utc_unix: Some(1_800_000_000), ..Default::default() };
        let message = "x".repeat(300);
        for tab in Tab::ALL {
            let mut p = heapless::String::<12288>::new();
            render_page(&Config::default(), &st, Some((&message, true)), true, &roots, tab, &mut p).unwrap();
            assert!(p.len() < 8000, "{tab:?}: {} bytes of 12288", p.len());
            busy.push_str(&format!("{tab:?}={} ", p.len()));
        }
        assert!(!busy.is_empty());
    }

    // VERIFIES: REQ-WEB-11
    #[test]
    fn a_tab_saves_what_it_sends_and_ignores_nothing_it_knows() {
        // the form of one tab posts only its own settings; a body with a setting from another tab is still applied (the
        // parser does not know the tabs), and blank secrets keep the old value
        let mut c = Config::default();
        c.set("web_password", "keepme").unwrap();
        let mut errs = std::string::String::new();
        assert_eq!(apply_form(b"hostname=lab&web_password=&hour12=on", &mut c, &mut errs), 2);
        assert_eq!((c.hostname.as_str(), c.hour12, c.web_password.as_str()), ("lab", true, "keepme"));
    }

    // VERIFIES: REQ-WEB-08
    #[test]
    fn upload_form_is_offered_only_over_tls_and_only_when_https_runs() {
        let c = Config::default();
        let ready = Status { https_state: 2, ..Default::default() };
        let page = |st: &Status, secure| {
            let mut p = heapless::String::<12288>::new();
            render_page(&c, st, None, secure, &crate::rootstore::Roots::new(), Tab::Security, &mut p).unwrap();
            p
        };
        assert!(page(&ready, true).contains("action=/cert") && page(&ready, true).contains("name=key"));
        assert!(!page(&ready, false).contains("action=/cert"), "never on plain HTTP: it would send a private key in the clear");
        assert!(!page(&Status { https_state: 1, ..Default::default() }, true).contains("action=/cert"));
        assert!(!page(&Status::default(), true).contains("action=/cert"), "not in a build without HTTPS");
        assert!(page(&ready, true).len() < 11_000);
        let p = page(&Status::default(), false);
        assert!(p.contains("https://github.com/dc0sk/picoclock") && p.contains("AGPL v3") && p.contains(&format!("picoclock {} is free", crate::VERSION)), "version, repository link and license on the page");
    }

    fn pem(label: &str, der: &[u8]) -> std::string::String {
        const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = std::string::String::new();
        for chunk in der.chunks(3) {
            let n = chunk.iter().enumerate().fold(0u32, |a, (i, &b)| a | (b as u32) << (16 - 8 * i));
            for i in 0..4 {
                s.push(if i <= chunk.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
            }
        }
        let lines: std::vec::Vec<_> = s.as_bytes().chunks(64).map(|c| std::string::String::from_utf8_lossy(c).into_owned()).collect();
        format!("-----BEGIN {label}-----\n{}\n-----END {label}-----\n", lines.join("\n"))
    }

    fn enc(s: &str) -> std::string::String {
        s.bytes().map(|b| if b.is_ascii_alphanumeric() { (b as char).to_string() } else { format!("%{b:02X}") }).collect()
    }

    const NOW: i64 = 1_792_000_000;

    fn pair(secret: u8) -> (std::vec::Vec<u8>, std::string::String) {
        let key = [secret; 32];
        let cert = crate::x509::generate_self_signed(key, [1; 8], &["picoclock-d6"], NOW).unwrap();
        (cert.to_vec(), pem("PRIVATE KEY", &crate::x509::secret_to_pkcs8(&key).unwrap()))
    }

    // VERIFIES: REQ-WEB-08
    #[test]
    fn certificate_upload_is_checked_before_it_is_accepted() {
        let (cert, key) = pair(0x31);
        let form = |c: &str, k: &str| format!("cert={}&key={}", enc(c), enc(k)).into_bytes();
        let ok = parse_cert_form(&form(&pem("CERTIFICATE", &cert), &key), NOW).expect("a matching pair");
        assert!(ok.uploaded);
        assert_eq!(ok.leaf(), &cert[..]);
        assert_eq!(ok.key, [0x31; 32]);
        // fields in the other order, extra fields, and a chain with an intermediate-shaped second certificate
        let (cert2, _) = pair(0x32);
        let chain = format!("{}{}", pem("CERTIFICATE", &cert), pem("CERTIFICATE", &cert2));
        let swapped = format!("key={}&junk=1&cert={}", enc(&key), enc(&chain)).into_bytes();
        let two = parse_cert_form(&swapped, NOW).unwrap();
        assert_eq!(two.certs().count(), 2);
        // refusals, each with its own reason
        assert_eq!(parse_cert_form(b"", NOW).err(), Some(CertFormError::Missing));
        assert_eq!(parse_cert_form(&form(&pem("CERTIFICATE", &cert), ""), NOW).err(), Some(CertFormError::Missing));
        assert_eq!(parse_cert_form(&form("", &key), NOW).err(), Some(CertFormError::Missing));
        assert_eq!(parse_cert_form(&form("not a certificate", &key), NOW).err(), Some(CertFormError::BadCertificate));
        assert_eq!(parse_cert_form(&form(&pem("CERTIFICATE", &cert), "not a key"), NOW).err(), Some(CertFormError::BadKey));
        let long_key = format!("-----BEGIN RSA PRIVATE KEY-----\n{}\n-----END RSA PRIVATE KEY-----\n", "A".repeat(1700));
        assert_eq!(parse_cert_form(&form(&pem("CERTIFICATE", &cert), &long_key), NOW).err(), Some(CertFormError::BadKey), "a key longer than any P-256 key is a key problem, not a chain problem");
        let (_, other_key) = pair(0x33);
        assert_eq!(parse_cert_form(&form(&pem("CERTIFICATE", &cert), &other_key), NOW).err(), Some(CertFormError::Mismatch));
        let intermediate_first = format!("{}{}", pem("CERTIFICATE", &cert2), pem("CERTIFICATE", &cert));
        assert_eq!(parse_cert_form(&form(&intermediate_first, &key), NOW).err(), Some(CertFormError::Mismatch), "the server's own certificate must come first");
        assert_eq!(parse_cert_form(&form(&pem("CERTIFICATE", &cert), &key), NOW + 3651 * 86_400).err(), Some(CertFormError::Expired));
        assert_eq!(parse_cert_form(&form(&pem("CERTIFICATE", &cert), &key), NOW - 3 * 86_400).err(), Some(CertFormError::NotYetValid));
        // garbage as a "second certificate", and a certificate where the key should be
        let junk = format!("{}{}", pem("CERTIFICATE", &cert), pem("CERTIFICATE", b"\x01\x02\x03 not der at all, long enough to pass"));
        assert_eq!(parse_cert_form(&form(&junk, &key), NOW).err(), Some(CertFormError::BadCertificate));
        assert_eq!(parse_cert_form(&form(&pem("CERTIFICATE", &cert), &pem("CERTIFICATE", &cert)), NOW).err(), Some(CertFormError::BadKey));
        // too much data never panics: a field beyond the buffers, five certificates, invalid percent escapes
        let huge = format!("cert={}&key=x", "A".repeat(5000)).into_bytes();
        assert!(parse_cert_form(&huge, NOW).is_err());
        let five = pem("CERTIFICATE", &cert).repeat(5);
        assert_eq!(parse_cert_form(&form(&five, &key), NOW).err(), Some(CertFormError::TooLarge));
        assert!(parse_cert_form(b"cert=%zz&key=%", NOW).is_err());
        assert!(parse_cert_form(&[0xFF, 0xFE], NOW).is_err());
        // the message for every error exists and says something
        for e in [CertFormError::Missing, CertFormError::TooLarge, CertFormError::BadCertificate, CertFormError::BadKey, CertFormError::Mismatch, CertFormError::Expired, CertFormError::NotYetValid] {
            assert!(e.message().len() > 20);
        }
    }

    // VERIFIES: REQ-WEB-10
    #[test]
    fn a_full_size_certificate_upload_fits_one_request() {
        // the largest chain the page promises: 3 KB of DER in two certificates (stand-ins, not certificates) as PEM, percent-encoded as a
        // browser sends it, and a key
        let der = vec![0x30u8; crate::x509::MAX_CHAIN / 2];
        let (cert, key) = (format!("{}{}", pem("CERTIFICATE", &der), pem("CERTIFICATE", &der)), pem("PRIVATE KEY", &[0x31u8; 138]));
        let body = format!("cert={}&key={}", enc(&cert), enc(&key));
        assert!(body.len() > 4096, "{} bytes: this is what the old 4 KiB limit refused", body.len());
        let head = format!("POST /cert HTTP/1.1\r\nHost: 192.168.1.2\r\nContent-Length: {}\r\n\r\n", body.len());
        let request = format!("{head}{body}");
        assert!(request.len() < MAX_REQUEST);
        assert!(matches!(parse_request(request.as_bytes()), Parse::Complete(_)));
        // the form parser gets as far as judging the certificate: the size is not what it objects to
        assert_ne!(parse_cert_form(body.as_bytes(), NOW).err(), Some(CertFormError::TooLarge));
        // anything over the limit is refused as too large, whatever it is
        let over = format!("POST /cert HTTP/1.1\r\nContent-Length: {}\r\n\r\n", MAX_REQUEST);
        assert_eq!(parse_request(over.as_bytes()), Parse::TooLarge);
    }

    // VERIFIES: REQ-WEB-09
    #[test]
    fn a_factory_reset_is_not_served_over_plain_http_while_https_is_on() {
        assert!(reset_allowed(true, true));
        assert!(!reset_allowed(false, true));
        assert!(reset_allowed(false, false), "a build or setting without HTTPS keeps the web reset");
        assert!(reset_allowed(true, false));
    }

    // VERIFIES: REQ-NTS-09
    #[test]
    fn roots_form_is_parsed_strictly() {
        let pem = "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n";
        let body = format!("pem={}", enc(pem));
        assert_eq!(parse_root_form(body.as_bytes()), Some(RootForm::Add(heapless::String::try_from(pem).unwrap())));
        assert_eq!(parse_root_form(b"remove=2"), Some(RootForm::Remove(2)));
        assert_eq!(parse_root_form(b"x=1&remove=3"), Some(RootForm::Remove(3)));
        for bad in ["", "pem=", "pem=%20%0A", "remove=", "remove=0", "remove=-1", "remove=abc", "remove=1.5", "other=1", "pem=%zz", "pem=%"] {
            assert_eq!(parse_root_form(bad.as_bytes()), None, "{bad:?}");
        }
        assert_eq!(parse_root_form(&[0xFF, 0xFE]), None);
        // more than a certificate can be: refused, not cut short
        let huge = format!("pem={}", "A".repeat(crate::cli::PASTE_MAX + 100));
        assert_eq!(parse_root_form(huge.as_bytes()), None);
    }

    // VERIFIES: REQ-NTS-09
    #[test]
    fn roots_section_only_over_tls_and_with_the_list() {
        let c = Config::default();
        let ready = Status { https_state: 2, ..Default::default() };
        let mut roots = crate::rootstore::Roots::new();
        for seed in 1..=4u8 {
            let cert = crate::x509::generate_self_signed([seed; 32], [seed; 8], &[&format!("authority-number-{seed}.example")], 1_792_000_000).unwrap();
            let pem = format!("-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n", b64(&cert));
            roots.add_pem(&pem, None).unwrap();
        }
        let page = |st: &Status, secure| {
            let mut p = heapless::String::<12288>::new();
            render_page(&c, st, None, secure, &roots, Tab::Security, &mut p).expect("the page with four roots must fit the buffer");
            p
        };
        let p = page(&ready, true);
        assert!(p.contains("action=/root") && p.contains("name=pem") && p.contains("authority-number-4.example") && p.contains("name=remove"));
        assert!(!page(&ready, false).contains("action=/root"), "never over plain HTTP");
        assert!(!page(&Status { https_state: 1, ..Default::default() }, true).contains("action=/root"));
        assert!(!page(&Status::default(), true).contains("authority-number"), "no list without a running HTTPS server");
    }

    fn b64(der: &[u8]) -> std::string::String {
        const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = std::string::String::new();
        for chunk in der.chunks(3) {
            let n = chunk.iter().enumerate().fold(0u32, |a, (i, &b)| a | (b as u32) << (16 - 8 * i));
            for i in 0..4 {
                s.push(if i <= chunk.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
            }
        }
        s
    }
}
