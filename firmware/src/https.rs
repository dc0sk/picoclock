//! HTTPS listener on port 443 (Pico 2 W) and the life of the certificate behind it.
//!
//! The key and certificate chain live in their own flash sector. At start-up the stored ones are used if they are still
//! valid and belong together; otherwise (first boot, a corrupt record, an expired upload) the device makes a self-signed
//! ECDSA P-256 certificate for its own name, stores it, and shows its fingerprint in `status`. `cert new` replaces it on
//! demand, and an upload through the web page replaces it with the user's own (see `web.rs`). Nothing is shipped in the
//! firmware: a key in an open-source repository would be public.

use alloc::sync::Arc;

use defmt::{info, warn};
use embassy_net::Stack;
use embassy_net::tcp::{TcpListener, TcpSocket};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use picoclock_core::nts::DriverRng;
use picoclock_core::tlsstore::Material;
use picoclock_core::{config, x509};
use rand_core::RngCore;
use static_cell::StaticCell;

use crate::tls::{self, TlsStream};
use crate::{heap, state, web};

const PORT: u16 = 443;

struct Buffers {
    incoming: [u8; tls::INCOMING],
    outgoing: [u8; tls::OUTGOING],
    rx: [u8; 4096],
    tx: [u8; 4096],
}

/// A new self-signed certificate for `names` with a fresh key.
fn generate(now: i64, names: &[&str]) -> Option<Material> {
    let mut rng = DriverRng;
    let mut key = [0u8; 32];
    loop {
        rng.fill_bytes(&mut key);
        if x509::secret_to_pkcs8(&key).is_ok() {
            break; // zero or not below the group order: draw again
        }
    }
    let mut serial = [0u8; 8];
    rng.fill_bytes(&mut serial);
    let cert = x509::generate_self_signed(key, serial, names, now).ok()?;
    Material::single(key, &cert, false)
}

/// The material to serve now: the stored one if it is valid at `now`, otherwise a new self-signed one (stored).
fn current_or_new(now: i64, names: &[&str], force_new: bool) -> Option<Material> {
    if !force_new {
        if let Some(m) = state::tls_material() {
            if x509::validate_pair(m.leaf(), &m.key, now).is_ok() {
                return Some(m);
            }
            warn!("https: the stored certificate is not usable (expired, wrong time or damaged); making a new one");
        }
    }
    let m = generate(now, names)?;
    state::set_tls_material(m.clone());
    state::TLS_SAVE.signal(());
    Some(m)
}

fn publish(m: &Material) {
    let info = x509::inspect(m.leaf());
    state::with_runtime(|r| {
        r.https_state = 2;
        r.cert_fp = x509::fingerprint(m.leaf());
        r.cert_not_after = info.map(|i| i.not_after).unwrap_or(0);
        r.cert_uploaded = m.uploaded;
    });
}

#[embassy_executor::task]
pub async fn https_task(stack: Stack<'static>) -> ! {
    state::with_runtime(|r| r.https_state = 1);
    // a certificate needs the time and the device name
    let (mut now, mac_last) = loop {
        let st = state::status();
        match (st.utc_unix, st.mac_last) {
            (Some(t), Some(m)) if t > 1_704_067_200 => break (t, m),
            _ => Timer::after_secs(2).await,
        }
    };
    let host = config::hostname([0, 0, 0, 0, 0, mac_last]);
    let mut local = heapless::String::<32>::new();
    let _ = local.push_str(&host);
    let _ = local.push_str(".local");

    static BUFFERS: StaticCell<Buffers> = StaticCell::new();
    let bufs = BUFFERS.init(Buffers { incoming: [0; tls::INCOMING], outgoing: [0; tls::OUTGOING], rx: [0; 4096], tx: [0; 4096] });
    let mut listener = TcpListener::new(stack).unwrap();
    let mut listening = false;
    let mut server: Option<Arc<rustls::ServerConfig>> = None;
    let mut rebuild = true;
    loop {
        if !state::with_config(|c| c.https) {
            // switched off: stop listening, forget the configuration; `status` says "off"
            if listening {
                listener.close();
                listening = false;
            }
            server = None;
            rebuild = true;
            state::with_runtime(|r| r.https_state = 1);
            Timer::after_secs(2).await;
            continue;
        }
        let new_requested = state::CERT_NEW.try_take().is_some();
        if state::CERT_RELOAD.try_take().is_some() || new_requested {
            rebuild = true;
        }
        if rebuild || server.is_none() {
            if let Some(t) = state::status().utc_unix {
                now = t;
            }
            let started = Instant::now();
            let built = current_or_new(now, &[host.as_str(), local.as_str()], new_requested).and_then(|m| {
                let chain: heapless::Vec<&[u8], 4> = m.certs().collect();
                let pkcs8 = x509::secret_to_pkcs8(&m.key).ok()?;
                let config = tls::server_config(&chain, &pkcs8).ok()?;
                publish(&m);
                Some(config)
            });
            match built {
                Some(config) => {
                    info!("https: configuration ready in {} ms", started.elapsed().as_millis());
                    server = Some(config);
                    rebuild = false;
                }
                None => {
                    warn!("https: cannot build the TLS configuration; trying again in a minute");
                    Timer::after_secs(60).await;
                    continue;
                }
            }
        }
        if !listening {
            if listener.listen(PORT).is_err() {
                Timer::after_secs(2).await;
                continue;
            }
            listening = true;
        }
        let Ok(Ok(token)) = with_timeout(Duration::from_secs(2), listener.accept()).await else { continue };
        let Some(config) = server.clone() else { continue };
        let Buffers { incoming, outgoing, rx, tx } = &mut *bufs;
        let Ok(mut sock) = TcpSocket::new(stack, &mut rx[..], &mut tx[..]) else { continue };
        sock.set_timeout(Some(Duration::from_secs(10)));
        if sock.accept(token).await.is_err() {
            continue;
        }
        let started = Instant::now();
        let result = with_timeout(Duration::from_secs(20), serve(&mut sock, config, &mut incoming[..], &mut outgoing[..])).await;
        match result {
            Ok(Ok(())) => info!("https: served a request in {} ms (heap {} used, {} free)", started.elapsed().as_millis(), heap::usage().0, heap::usage().1),
            Ok(Err(e)) => warn!("https: connection failed: {:?}", defmt::Debug2Format(&e)),
            Err(_) => warn!("https: connection timed out"),
        }
        sock.abort();
        let _ = with_timeout(Duration::from_secs(1), sock.flush()).await;
    }
}

async fn serve(sock: &mut TcpSocket<'_, '_>, config: Arc<rustls::ServerConfig>, incoming: &mut [u8], outgoing: &mut [u8]) -> Result<(), tls::TlsError> {
    let mut tls = TlsStream::new(sock, config, incoming, outgoing)?;
    web::serve(&mut tls, true).await;
    let _ = tls.close().await;
    Ok(())
}
