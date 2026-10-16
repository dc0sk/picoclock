//! HTTP configuration page on port 80. Serves one connection at a time with a hard deadline per request, which is
//! plenty for a settings page and keeps RAM use tiny. Parsing and rendering live in `picoclock_core::http`.
//!
//! The page is plain HTTP: anyone who can sniff your LAN can read the password. Treat it as LAN-only.

use core::fmt::Write as _;

use defmt::{info, warn};
use embassy_net::Stack;
use embassy_net::tcp::{TcpListener, TcpSocket};
use embassy_time::{Duration, Timer, with_timeout};
use embedded_io_async::Write as _;
use picoclock_core::http::{self, Method, Parse, Request};

use crate::state;

const REQUEST_DEADLINE: Duration = Duration::from_secs(6);
const PORT: u16 = 80;

async fn respond(sock: &mut TcpSocket<'_, '_>, status: &str, ctype: &str, extra: &str, body: &str) -> Result<(), embassy_net::tcp::Error> {
    let mut head = heapless::String::<256>::new();
    let _ = write!(
        head,
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n{extra}Cache-Control: no-store\r\nX-Frame-Options: DENY\r\nConnection: close\r\n\r\n",
        body.len()
    );
    sock.write_all(head.as_bytes()).await?;
    sock.write_all(body.as_bytes()).await?;
    sock.flush().await
}

async fn page(sock: &mut TcpSocket<'_, '_>, msg: Option<(&str, bool)>) -> Result<(), embassy_net::tcp::Error> {
    let mut body = heapless::String::<12288>::new();
    let (cfg, st) = (state::config_snapshot(), state::status());
    if http::render_page(&cfg, &st, msg, &mut body).is_err() {
        return respond(sock, "500 Internal Server Error", "text/plain", "", "page too large\n").await;
    }
    respond(sock, "200 OK", "text/html; charset=utf-8", "", &body).await
}

async fn handle(sock: &mut TcpSocket<'_, '_>, req: &Request<'_>) -> Result<(), embassy_net::tcp::Error> {
    let cfg = state::config_snapshot();
    if !http::authorized(&cfg, req.authorization) {
        return respond(sock, "401 Unauthorized", "text/plain", "WWW-Authenticate: Basic realm=\"picoclock\"\r\n", "login required\n").await;
    }
    match (&req.method, req.path) {
        (Method::Get, "/") => page(sock, None).await,
        (Method::Get, "/zones") => {
            // ~10 KB: stream in chunks rather than buffering.
            let total: usize = http::zone_names().map(|n| n.len() + 1).sum();
            let mut head = heapless::String::<160>::new();
            let _ = write!(head, "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n");
            sock.write_all(head.as_bytes()).await?;
            let mut chunk = heapless::String::<512>::new();
            for name in http::zone_names() {
                if chunk.len() + name.len() + 1 > chunk.capacity() {
                    sock.write_all(chunk.as_bytes()).await?;
                    chunk.clear();
                }
                let _ = chunk.push_str(name);
                let _ = chunk.push('\n');
            }
            sock.write_all(chunk.as_bytes()).await?;
            sock.flush().await
        }
        (Method::Post, "/save") => {
            if !http::same_origin(req) {
                return respond(sock, "403 Forbidden", "text/plain", "", "cross-site request refused\n").await;
            }
            let mut errors = heapless::String::<256>::new();
            let changed = state::with_config(|c| http::apply_form(req.body, c, &mut errors));
            if changed > 0 {
                state::mark_changed(true);
            }
            let mut msg = heapless::String::<320>::new();
            let _ = write!(msg, "{changed} setting(s) changed and saved. {errors}");
            page(sock, Some((&msg, !errors.is_empty()))).await
        }
        (Method::Get | Method::Post, _) => respond(sock, "404 Not Found", "text/plain", "", "not found\n").await,
    }
}

async fn serve(sock: &mut TcpSocket<'_, '_>) {
    let mut buf = [0u8; http::MAX_REQUEST];
    let mut len = 0;
    let result = with_timeout(REQUEST_DEADLINE, async {
        loop {
            match http::parse_request(&buf[..len]) {
                Parse::Complete(req) => return handle(sock, &req).await.map_err(|_| ()),
                Parse::Incomplete => {}
                Parse::Bad => return respond(sock, "400 Bad Request", "text/plain", "", "bad request\n").await.map_err(|_| ()),
                Parse::TooLarge => return respond(sock, "413 Payload Too Large", "text/plain", "", "too large\n").await.map_err(|_| ()),
            }
            match sock.read(&mut buf[len..]).await {
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
        serve(&mut sock).await;
        sock.close();
        let _ = with_timeout(Duration::from_secs(2), sock.flush()).await;
        sock.abort();
    }
}
