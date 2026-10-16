//! HTTP configuration page on port 80. Serves one connection at a time with a hard deadline per request, which is
//! plenty for a settings page and keeps RAM use tiny. Parsing and rendering live in `picoclock_core::http`.
//!
//! This port is plain HTTP: anyone who can sniff your LAN can read the password. On the Pico 2 W the same page is served over
//! TLS by `https.rs`; `serve` does not care which it is talking to.

use core::fmt::Write as _;

use defmt::{info, warn};
use embassy_net::Stack;
use embassy_net::tcp::{TcpListener, TcpSocket};
use embassy_time::{Duration, Timer, with_timeout};
use embedded_io_async::{Read, Write};
use picoclock_core::http::{self, Method, Parse, Request};

use crate::state;

const REQUEST_DEADLINE: Duration = Duration::from_secs(6);
const PORT: u16 = 80;

async fn respond<S: Write>(io: &mut S, status: &str, ctype: &str, extra: &str, body: &str) -> Result<(), S::Error> {
    let mut head = heapless::String::<256>::new();
    let _ = write!(
        head,
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n{extra}Cache-Control: no-store\r\nX-Frame-Options: DENY\r\nConnection: close\r\n\r\n",
        body.len()
    );
    io.write_all(head.as_bytes()).await?;
    io.write_all(body.as_bytes()).await?;
    io.flush().await
}

async fn page<S: Write>(io: &mut S, msg: Option<(&str, bool)>, secure: bool) -> Result<(), S::Error> {
    let mut body = heapless::String::<12288>::new();
    let (cfg, st) = (state::config_snapshot(), state::status());
    if http::render_page(&cfg, &st, msg, secure, &mut body).is_err() {
        return respond(io, "500 Internal Server Error", "text/plain", "", "page too large\n").await;
    }
    respond(io, "200 OK", "text/html; charset=utf-8", "", &body).await
}

async fn handle<S: Write>(io: &mut S, req: &Request<'_>, secure: bool) -> Result<(), S::Error> {
    let cfg = state::config_snapshot();
    // While HTTPS is up, the plain port only points to it: nothing is served and nothing is accepted over it.
    if !secure && cfg.https && cfg.http_redirect && state::status().https_state == 2 {
        let mut own = heapless::String::<20>::new();
        if let Some([a, b, c, d]) = state::status().ip {
            let _ = write!(own, "{a}.{b}.{c}.{d}");
        }
        return match http::https_redirect(req, &own) {
            Some(location) => {
                let mut extra = heapless::String::<160>::new();
                let _ = write!(extra, "Location: {location}\r\n");
                respond(io, "307 Temporary Redirect", "text/plain", &extra, "this page is served over HTTPS\n").await
            }
            // not a GET, or a path that cannot be put in a header: never answered over the plain connection
            None => respond(io, "403 Forbidden", "text/plain", "", "use https:// for this\n").await,
        };
    }
    handle_authorized(io, req, secure, &cfg).await
}

async fn handle_authorized<S: Write>(io: &mut S, req: &Request<'_>, secure: bool, cfg: &picoclock_core::config::Config) -> Result<(), S::Error> {
    if !http::authorized(&cfg, req.authorization) {
        return respond(io, "401 Unauthorized", "text/plain", "WWW-Authenticate: Basic realm=\"picoclock\"\r\n", "login required\n").await;
    }
    match (&req.method, req.path) {
        (Method::Get, "/") => page(io, None, secure).await,
        (Method::Get, "/zones") => {
            // ~10 KB: stream in chunks rather than buffering.
            let total: usize = http::zone_names().map(|n| n.len() + 1).sum();
            let mut head = heapless::String::<160>::new();
            let _ = write!(head, "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n");
            io.write_all(head.as_bytes()).await?;
            let mut chunk = heapless::String::<1024>::new();
            for name in http::zone_names() {
                if chunk.len() + name.len() + 1 > chunk.capacity() {
                    io.write_all(chunk.as_bytes()).await?;
                    chunk.clear();
                }
                let _ = chunk.push_str(name);
                let _ = chunk.push('\n');
            }
            io.write_all(chunk.as_bytes()).await?;
            io.flush().await
        }
        (Method::Post, "/save") => {
            if !http::same_origin(req, secure) {
                return respond(io, "403 Forbidden", "text/plain", "", "cross-site request refused\n").await;
            }
            let mut errors = heapless::String::<256>::new();
            let changed = state::with_config(|c| http::apply_form(req.body, c, &mut errors));
            if changed > 0 {
                state::mark_changed(true);
            }
            let mut msg = heapless::String::<320>::new();
            let _ = write!(msg, "{changed} setting(s) changed and saved. {errors}");
            page(io, Some((&msg, !errors.is_empty())), secure).await
        }
        #[cfg(feature = "https")]
        (Method::Post, "/cert") => {
            if !secure {
                return respond(io, "403 Forbidden", "text/plain", "", "a certificate and key are only accepted over HTTPS\n").await;
            }
            if !http::same_origin(req, secure) {
                return respond(io, "403 Forbidden", "text/plain", "", "cross-site request refused\n").await;
            }
            let now = state::status().utc_unix.unwrap_or(0);
            match http::parse_cert_form(req.body, now) {
                Ok(material) => {
                    state::set_tls_material(material);
                    state::TLS_SAVE.signal(());
                    state::CERT_RELOAD.signal(());
                    page(io, Some(("Certificate replaced. New connections use it; reload the page and your browser may ask you to trust it.", false)), secure).await
                }
                Err(e) => page(io, Some((e.message(), true)), secure).await,
            }
        }
        (Method::Get | Method::Post, _) => respond(io, "404 Not Found", "text/plain", "", "not found\n").await,
    }
}

/// Serves one request on `io`, which is either the plain TCP socket or a TLS stream (`secure`).
pub async fn serve<S: Read + Write>(io: &mut S, secure: bool) {
    let mut buf = [0u8; http::MAX_REQUEST];
    let mut len = 0;
    let result = with_timeout(REQUEST_DEADLINE, async {
        loop {
            match http::parse_request(&buf[..len]) {
                Parse::Complete(req) => return handle(io, &req, secure).await.map_err(|_| ()),
                Parse::Incomplete => {}
                Parse::Bad => return respond(io, "400 Bad Request", "text/plain", "", "bad request\n").await.map_err(|_| ()),
                Parse::TooLarge => return respond(io, "413 Payload Too Large", "text/plain", "", "too large\n").await.map_err(|_| ()),
            }
            match io.read(&mut buf[len..]).await {
                Ok(0) | Err(_) => return Err(()),
                Ok(n) => len += n,
            }
        }
    })
    .await;
    if !matches!(result, Ok(Ok(()))) {
        warn!("web: request failed or timed out");
    }
}

#[embassy_executor::task]
pub async fn web_task(stack: Stack<'static>) -> ! {
    static RX: static_cell::StaticCell<[u8; 1536]> = static_cell::StaticCell::new();
    static TX: static_cell::StaticCell<[u8; 2048]> = static_cell::StaticCell::new();
    let (rx, tx) = (RX.init([0; 1536]), TX.init([0; 2048]));
    let mut listener = TcpListener::new(stack).unwrap();
    let mut listening = false;
    loop {
        // Not listening at all while the web interface is switched off.
        let enabled = state::with_config(|c| c.web_enabled);
        if !enabled {
            if listening {
                listener.close();
                listening = false;
            }
            Timer::after_secs(2).await;
            continue;
        }
        if !listening {
            if listener.listen(PORT).is_err() {
                Timer::after_secs(2).await;
                continue;
            }
            listening = true;
        }
        let Ok(Ok(token)) = with_timeout(Duration::from_secs(2), listener.accept()).await else { continue };
        let Ok(mut sock) = TcpSocket::new(stack, &mut rx[..], &mut tx[..]) else { continue };
        sock.set_timeout(Some(Duration::from_secs(5)));
        if sock.accept(token).await.is_err() {
            continue;
        }
        info!("web: connection");
        serve(&mut sock, false).await;
        sock.close();
        let _ = with_timeout(Duration::from_secs(2), sock.flush()).await;
        sock.abort();
    }
}
