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
use picoclock_core::http::{self, Method, Parse, Request, Tab};

use crate::state;

const REQUEST_DEADLINE: Duration = Duration::from_secs(6);
/// An update file may arrive slowly, but not stall, and not take forever.
const UPLOAD_IDLE: Duration = Duration::from_secs(10);
const UPLOAD_TOTAL: Duration = Duration::from_secs(300);
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

async fn page<S: Write>(io: &mut S, msg: Option<(&str, bool)>, secure: bool, tab: Tab) -> Result<(), S::Error> {
    let mut body = heapless::String::<12288>::new();
    let (cfg, st) = (state::config_snapshot(), state::status());
    if http::render_page(&cfg, &st, msg, secure, &state::roots_snapshot(), tab, &mut body).is_err() {
        return respond(io, "500 Internal Server Error", "text/plain", "", "page too large\n").await;
    }
    respond(io, "200 OK", "text/html; charset=utf-8", "", &body).await
}

/// The answer to a form: a redirect to the page it came from, with its message kept for that page to show once. Answering a POST
/// with the page itself would make a reload send the form again (and, for "remove number 1", remove one more authority).
async fn after_post<S: Write>(io: &mut S, msg: &str, is_error: bool, tab: Tab) -> Result<(), S::Error> {
    state::set_flash_message(msg, is_error);
    let mut location = heapless::String::<48>::new();
    let _ = write!(location, "Location: /?tab={}\r\n", tab.slug());
    respond(io, "303 See Other", "text/plain", &location, "done; see the page\n").await
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
    let (path, query) = http::split_query(req.path);
    let tab = http::tab_from_query(query);
    match (&req.method, path) {
        (Method::Get, "/") => {
            let flash = state::take_flash_message();
            page(io, flash.as_ref().map(|(m, e)| (m.as_str(), *e)), secure, tab).await
        }
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
            let changed = state::edit_and_save(true, |c| http::apply_form(req.body, c, &mut errors));
            let mut msg = heapless::String::<320>::new();
            let _ = write!(msg, "{changed} setting(s) changed and saved. {errors}");
            after_post(io, &msg, !errors.is_empty(), tab).await
        }
        (Method::Post, "/update-check") => {
            if !http::same_origin(req, secure) {
                return respond(io, "403 Forbidden", "text/plain", "", "cross-site request refused\n").await;
            }
            state::with_runtime(|r| r.update = picoclock_core::update::Check::Checking);
            state::UPDATE_NOW.signal(());
            // Post/Redirect/Get: the answer to the button is a redirect to a plain page, so reloading that page asks nothing again.
            // The Status page refreshes itself while the check runs.
            respond(io, "303 See Other", "text/plain", "Location: /?tab=status\r\n", "checking; see the status page\n").await
        }
        (Method::Post, "/reset") => {
            // With HTTPS on, the plain port may not erase the clock even when the redirect is switched off: without a web
            // password nothing else stops a host on the LAN. (Builds without TLS have no other way to offer this.)
            #[cfg(feature = "https")]
            let https_on = cfg.https;
            #[cfg(not(feature = "https"))]
            let https_on = false;
            if !http::reset_allowed(secure, https_on) {
                return respond(io, "403 Forbidden", "text/plain", "", "a factory reset is only accepted over HTTPS while HTTPS is on\n").await;
            }
            if !http::same_origin(req, secure) {
                return respond(io, "403 Forbidden", "text/plain", "", "cross-site request refused\n").await;
            }
            let form = http::parse_reset_form(req.body);
            if form == http::ResetForm::Confirmed {
                respond(io, "200 OK", "text/html; charset=utf-8", "", http::RESET_DONE_PAGE).await?;
                state::FACTORY_RESET.signal(());
                return Ok(());
            }
            let mut body = heapless::String::<4096>::new();
            let msg = (form == http::ResetForm::Refused).then_some("That was not the confirmation word, so nothing was erased.");
            if http::render_reset_confirm(msg, &mut body).is_err() {
                return respond(io, "500 Internal Server Error", "text/plain", "", "page too large\n").await;
            }
            respond(io, "200 OK", "text/html; charset=utf-8", "", &body).await
        }
        (Method::Post, "/install") => {
            if !http::upload_allowed(secure, state::status().https_state) {
                return respond(io, "403 Forbidden", "text/plain", "", "installing is only accepted over HTTPS while HTTPS is on\n").await;
            }
            if !http::same_origin(req, secure) {
                return respond(io, "403 Forbidden", "text/plain", "", "cross-site request refused\n").await;
            }
            let picoclock_core::image::Install::Staged(version, _) = state::status().install else {
                return after_post(io, "No image is staged. Upload an update file first.", true, Tab::Security).await;
            };
            let form = http::parse_install_form(req.body);
            if form == http::ResetForm::Confirmed {
                respond(io, "200 OK", "text/html; charset=utf-8", "", http::INSTALL_DONE_PAGE).await?;
                // the clock restarts in the storage task; this only returns when the install was refused
                state::INSTALL_IN.send(state::InstallMsg::Apply).await;
                let _ = state::INSTALL_REPLY.wait().await;
                state::set_flash_message("Not installed: the running image has not confirmed itself yet. Try again in a moment.", true);
                return Ok(());
            }
            let mut body = heapless::String::<2048>::new();
            let msg = (form == http::ResetForm::Refused).then_some("That was not the confirmation word, so nothing was installed.");
            if http::render_install_confirm(version, msg, &mut body).is_err() {
                return respond(io, "500 Internal Server Error", "text/plain", "", "page too large\n").await;
            }
            respond(io, "200 OK", "text/html; charset=utf-8", "", &body).await
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
                    after_post(io, "Certificate replaced. New connections use it; reload the page and your browser may ask you to trust it.", false, Tab::Security).await
                }
                Err(e) => after_post(io, e.message(), true, Tab::Security).await,
            }
        }
        (Method::Post, "/root") => {
            if !secure {
                return respond(io, "403 Forbidden", "text/plain", "", "trusted authorities can only be changed over HTTPS or the USB terminal\n").await;
            }
            if !http::same_origin(req, secure) {
                return respond(io, "403 Forbidden", "text/plain", "", "cross-site request refused\n").await;
            }
            let now = state::status().utc_unix;
            match http::parse_root_form(req.body) {
                Some(http::RootForm::Add(pem)) => {
                    let result = state::change_roots(|r| match r.add_pem(&pem, now) {
                        Ok(added) => (true, Ok(added.len())),
                        Err(e) => (false, Err(e)),
                    });
                    match result {
                        Ok(n) => {
                            let mut msg = heapless::String::<96>::new();
                            let _ = write!(msg, "Added {n} certificate authorit{}; saved to flash.", if n == 1 { "y" } else { "ies" });
                            after_post(io, &msg, false, Tab::Security).await
                        }
                        Err(e) => after_post(io, e.message(), true, Tab::Security).await,
                    }
                }
                Some(http::RootForm::Remove(n)) => {
                    let removed = state::change_roots(|r| {
                        let ok = r.remove(n - 1);
                        (ok, ok)
                    });
                    if removed {
                        after_post(io, "Removed; saved to flash.", false, Tab::Security).await
                    } else {
                        after_post(io, "There is no such number in the list.", true, Tab::Security).await
                    }
                }
                None => after_post(io, "Paste a PEM certificate, or give the number to remove.", true, Tab::Security).await,
            }
        }
        (Method::Get | Method::Post, _) => respond(io, "404 Not Found", "text/plain", "", "not found\n").await,
    }
}

/// What the page says about an upload: like the terminal's line, but pointing at the button instead of `update apply`.
fn upload_message(i: picoclock_core::image::Install) -> heapless::String<300> {
    use picoclock_core::image::{Install, UNSIGNED};
    let mut text = heapless::String::new();
    match i {
        Install::Staged(v, UNSIGNED) => {
            let _ = write!(text, "{v} is staged WITHOUT a signature check (custom build). Press Install below to use it.");
        }
        Install::Staged(v, key) => {
            let _ = write!(text, "{v} is staged (signed by key {}). Press Install below to use it.", key + 1);
        }
        other => {
            picoclock_core::image::describe_install(&mut text, other);
            if let Some(rest) = text.strip_prefix("image   : ") {
                let rest = heapless::String::<300>::try_from(rest).unwrap_or_default();
                text = rest;
            }
        }
    }
    text
}

/// Reads and throws away what is left of a refused upload, so the answer reaches the browser instead of a reset connection.
async fn drain<S: Read>(io: &mut S, mut remaining: usize) {
    let mut sink = [0u8; 512];
    let _ = with_timeout(Duration::from_secs(8), async {
        while remaining > 0 {
            match io.read(&mut sink[..remaining.min(512)]).await {
                Ok(0) | Err(_) => return,
                Ok(n) => remaining -= n,
            }
        }
    })
    .await;
}

async fn refuse_upload<S: Read + Write>(io: &mut S, status: &str, msg: &str, remaining: usize) -> Result<(), ()> {
    respond(io, status, "text/plain", "", msg).await.map_err(|_| ())?;
    drain(io, remaining).await;
    Ok(())
}

/// Collects the body into 4 KiB blocks for the storage task, which writes each into the download slot while the next one arrives.
struct Sink {
    block: heapless::Vec<u8, 4096>,
    remaining: usize,
}

impl Sink {
    async fn push(&mut self, mut data: &[u8]) {
        data = &data[..data.len().min(self.remaining)];
        while !data.is_empty() {
            let n = (self.block.capacity() - self.block.len()).min(data.len());
            let _ = self.block.extend_from_slice(&data[..n]);
            data = &data[n..];
            self.remaining -= n;
            if self.block.is_full() || self.remaining == 0 {
                state::INSTALL_IN.send(state::InstallMsg::Block(core::mem::take(&mut self.block))).await;
            }
        }
    }
}

/// `POST /upload`: the update file as the raw body (REQ-UPD-08). Every refusal comes before the first byte is stored, and a
/// connection that stalls or breaks leaves nothing staged.
async fn upload<S: Read + Write>(io: &mut S, buf: &[u8], secure: bool) -> Result<(), ()> {
    let Some(head) = http::upload_head(buf) else { return Err(()) };
    let (len, req) = (head.content_length, &head.request);
    let early = &buf[head.body_at..];
    let early = &early[..early.len().min(len)];
    let left = len - early.len();
    let cfg = state::config_snapshot();
    if !http::authorized(&cfg, req.authorization) {
        respond(io, "401 Unauthorized", "text/plain", "WWW-Authenticate: Basic realm=\"picoclock\"\r\n", "login required\n").await.map_err(|_| ())?;
        drain(io, left).await;
        return Ok(());
    }
    if !http::upload_allowed(secure, state::status().https_state) {
        return refuse_upload(io, "403 Forbidden", "an update file is only accepted over HTTPS while HTTPS is on\n", left).await;
    }
    if !http::same_origin(req, secure) {
        return refuse_upload(io, "403 Forbidden", "cross-site request refused\n", left).await;
    }
    let Some(_claim) = state::InstallClaim::take() else {
        return refuse_upload(io, "409 Conflict", "another update file is being received\n", left).await;
    };
    state::INSTALL_IN.send(state::InstallMsg::Begin(len)).await;
    if !state::INSTALL_REPLY.wait().await {
        let mut text = upload_message(state::status().install);
        state::set_flash_message(&text, true);
        text.push('\n').ok();
        return refuse_upload(io, "422 Unprocessable Content", &text, left).await;
    }
    let received = with_timeout(UPLOAD_TOTAL, async {
        let mut sink = Sink { block: heapless::Vec::new(), remaining: len };
        sink.push(early).await;
        let mut chunk = [0u8; 1024];
        while sink.remaining > 0 {
            let want = sink.remaining.min(chunk.len());
            match with_timeout(UPLOAD_IDLE, io.read(&mut chunk[..want])).await {
                Ok(Ok(n)) if n > 0 => sink.push(&chunk[..n]).await,
                _ => return false,
            }
        }
        true
    })
    .await
    .unwrap_or(false);
    if !received {
        warn!("web: upload stopped before the end");
        state::INSTALL_IN.send(state::InstallMsg::Abort).await;
        return Err(());
    }
    state::INSTALL_IN.send(state::InstallMsg::Finish).await;
    let staged = state::INSTALL_REPLY.wait().await;
    let mut text = upload_message(state::status().install);
    state::set_flash_message(&text, !staged);
    text.push('\n').ok();
    let status = if staged { "200 OK" } else { "422 Unprocessable Content" };
    respond(io, status, "text/plain", "", &text).await.map_err(|_| ())
}

enum Step {
    Done,
    Upload,
}

/// Serves one request on `io`, which is either the plain TCP socket or a TLS stream (`secure`).
pub async fn serve<S: Read + Write>(io: &mut S, secure: bool) {
    let mut buf = [0u8; http::MAX_REQUEST];
    let mut len = 0;
    // The deadline covers the request head and the ordinary requests. An update file is the one body too large for `buf`; it is
    // handed to `upload`, which has its own limits.
    let result = with_timeout(REQUEST_DEADLINE, async {
        loop {
            if http::upload_head(&buf[..len]).is_some() {
                return Ok(Step::Upload);
            }
            match http::parse_request(&buf[..len]) {
                Parse::Complete(req) => return handle(io, &req, secure).await.map(|()| Step::Done).map_err(|_| ()),
                Parse::Incomplete => {}
                Parse::Bad => return respond(io, "400 Bad Request", "text/plain", "", "bad request\n").await.map(|()| Step::Done).map_err(|_| ()),
                Parse::TooLarge => return respond(io, "413 Payload Too Large", "text/plain", "", "request too large: the limit is 8 KiB (a certificate chain may hold at most about 3 KB of DER data, four certificates)\n").await.map(|()| Step::Done).map_err(|_| ()),
            }
            match io.read(&mut buf[len..]).await {
                Ok(0) | Err(_) => return Err(()),
                Ok(n) => len += n,
            }
        }
    })
    .await;
    let result = match result {
        Ok(Ok(Step::Upload)) => upload(io, &buf[..len], secure).await,
        Ok(Ok(Step::Done)) => Ok(()),
        _ => Err(()),
    };
    if result.is_err() {
        warn!("web: request failed or timed out");
    }
}

#[embassy_executor::task]
pub async fn web_task(stack: Stack<'static>) -> ! {
    static RX: static_cell::StaticCell<[u8; 4096]> = static_cell::StaticCell::new();
    static TX: static_cell::StaticCell<[u8; 2048]> = static_cell::StaticCell::new();
    let (rx, tx) = (RX.init([0; 4096]), TX.init([0; 2048]));
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
