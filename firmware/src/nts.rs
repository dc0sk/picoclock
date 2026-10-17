//! NTS (RFC 8915) client transport: NTS-KE over a TCP socket, then authenticated NTP over UDP. The protocol itself is in
//! `picoclock_core::nts` and `picoclock_core::nts_ke`; this file owns the sockets, the buffers and the sessions.
//!
//! The key exchange needs several public-key operations (measured on the RP2040: roughly 0.3 to 0.6 s each), and they
//! run without yielding, so the other tasks pause for a few seconds while it runs. That happens once per boot (the
//! cookies are reused for every later sync) and again only after a NAK or when the cookies are used up.

use defmt::{info, warn};
use embassy_net::Stack;
use embassy_net::dns::DnsQueryType;
use embassy_net::tcp::TcpSocket;
use embassy_net::udp::UdpSocket;
use embassy_net::wire::{ListenSocketAddr, SocketAddr};
use embassy_time::{Duration, Instant, with_timeout};
use embedded_tls::TlsClock;
use heapless::{String, Vec};
use picoclock_core::nts::{DriverRng, NTS_KE_PORT, NtsError, Session};
use picoclock_core::nts_ke::{self, KeError};
use rand_core::RngCore;
use static_cell::StaticCell;

use crate::state;

const SESSIONS: usize = 2;
const KE_TIMEOUT: Duration = Duration::from_secs(45);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const REPLY_TIMEOUT: Duration = Duration::from_secs(3);
const ATTEMPTS: u8 = 2;

/// The device clock for certificate dates: the RTC, which holds UTC; `None` until it holds a valid time.
pub struct DeviceClock;

impl TlsClock for DeviceClock {
    fn now() -> Option<u64> {
        state::status().utc_unix.map(|t| t.max(0) as u64)
    }
}

struct Buffers {
    tls_read: [u8; 16640],
    tls_write: [u8; 2048],
    tcp_rx: [u8; 4096],
    tcp_tx: [u8; 1024],
}

pub struct Nts {
    sessions: Vec<(String<64>, Session), SESSIONS>,
    buffers: &'static mut Buffers,
}

impl Nts {
    /// Allocates the buffers (about 24 KiB); call once.
    pub fn new() -> Self {
        static BUFFERS: StaticCell<Buffers> = StaticCell::new();
        Self { sessions: Vec::new(), buffers: BUFFERS.init(Buffers { tls_read: [0; 16640], tls_write: [0; 2048], tcp_rx: [0; 4096], tcp_tx: [0; 1024] }) }
    }

    fn position(&self, host: &str) -> Option<usize> {
        self.sessions.iter().position(|(h, _)| h.as_str() == host)
    }

    fn forget(&mut self, host: &str) {
        if let Some(i) = self.position(host) {
            self.sessions.swap_remove(i);
        }
    }

    /// One authenticated time query to the NTS server `host`: returns UTC as (unix seconds, ms) corrected for half the
    /// round trip. Performs the key exchange first when there is no session or no cookie left, and once more after a NAK.
    pub async fn query(&mut self, stack: Stack<'static>, host: &str) -> Option<(u64, u16)> {
        for round in 0..2 {
            if self.position(host).is_none_or(|i| self.sessions[i].1.cookies_left() == 0) {
                self.forget(host);
                let session = self.key_exchange(stack, host).await?;
                if self.sessions.is_full() {
                    self.sessions.swap_remove(0);
                }
                let _ = self.sessions.push((String::try_from(host).ok()?, session));
            }
            let i = self.position(host)?;
            match exchange(stack, &mut self.sessions[i].1).await {
                Ok(t) => {
                    info!("nts: {} answered with an authenticated reply, unix {}, {} cookies left", host, t.0, self.sessions[i].1.cookies_left());
                    return Some(t);
                }
                Err(Failure::Nak) => {
                    warn!("nts: {} sent an NTS NAK (cookies no longer valid)", host);
                    self.forget(host);
                    if round == 1 {
                        return None;
                    }
                }
                Err(Failure::Other) => return None,
            }
        }
        None
    }

    async fn key_exchange(&mut self, stack: Stack<'static>, host: &str) -> Option<Session> {
        if DeviceClock::now().is_none() {
            warn!("nts: {} skipped: the clock has no valid time, so certificate dates cannot be checked", host);
            return None;
        }
        let addr = match with_timeout(Duration::from_secs(8), stack.dns_query(host, DnsQueryType::A)).await {
            Ok(Ok(a)) => *a.first()?,
            _ => {
                warn!("nts: cannot resolve {}", host);
                return None;
            }
        };
        // the built-in authorities plus the ones the user added; the chain may end at any of them
        let user = state::roots_snapshot();
        let mut roots: heapless::Vec<&[u8], 8> = heapless::Vec::new();
        for r in nts_ke::BUILT_IN_ROOTS.iter().copied().chain(user.iter()) {
            let _ = roots.push(r);
        }
        let Buffers { tls_read, tls_write, tcp_rx, tcp_tx } = &mut *self.buffers;
        let mut socket = TcpSocket::new(stack, &mut tcp_rx[..], &mut tcp_tx[..]).ok()?;
        socket.set_timeout(Some(Duration::from_secs(20)));
        if !matches!(with_timeout(CONNECT_TIMEOUT, socket.connect(SocketAddr::new(addr, NTS_KE_PORT))).await, Ok(Ok(()))) {
            warn!("nts: cannot connect to {}:{}", host, NTS_KE_PORT);
            return None;
        }
        info!("nts: key exchange with {} (the clock is busy for a few seconds)", host);
        let started = Instant::now();
        let outcome = with_timeout(KE_TIMEOUT, nts_ke::key_exchange::<_, DeviceClock>(socket, host, &roots, &mut tls_read[..], &mut tls_write[..])).await;
        let code = match &outcome {
            Ok(Ok(_)) => 1,
            Ok(Err(KeError::Tls(_))) => 2,
            Ok(Err(KeError::Nts(NtsError::NoAgreement))) => 3,
            Ok(Err(KeError::Nts(_))) | Ok(Err(KeError::Closed)) => 4,
            Ok(Err(KeError::NoTime)) | Err(_) => 5,
        };
        state::with_runtime(|r| {
            r.nts_ke = code;
            r.nts_ke_ms = started.elapsed().as_millis().min(u16::MAX as u64) as u16;
            r.nts_ke_host = [0; 32];
            let n = host.len().min(32);
            r.nts_ke_host[..n].copy_from_slice(&host.as_bytes()[..n]);
        });
        match outcome {
            Ok(Ok(session)) => {
                info!("nts: key exchange with {} done in {} ms, {} cookies", host, started.elapsed().as_millis(), session.cookies_left());
                Some(session)
            }
            // the chain does not end at any trusted root, or the name or dates do not fit
            Ok(Err(KeError::Tls(e))) => {
                warn!("nts: TLS with {} failed after {} ms: {:?}", host, started.elapsed().as_millis(), e);
                None
            }
            Ok(Err(KeError::Nts(NtsError::NoAgreement))) => {
                warn!("nts: {} does not offer NTPv4 with AES-SIV-CMAC-256", host);
                None
            }
            Ok(Err(KeError::NoTime)) => None,
            Ok(Err(KeError::Nts(_))) | Ok(Err(KeError::Closed)) => {
                warn!("nts: {} sent a bad or incomplete key exchange message", host);
                None
            }
            Err(_) => {
                warn!("nts: key exchange with {} timed out", host);
                None
            }
        }
    }
}

enum Failure {
    /// The server no longer accepts our cookies: a new key exchange is needed.
    Nak,
    Other,
}

async fn exchange(stack: Stack<'static>, session: &mut Session) -> Result<(u64, u16), Failure> {
    let addr = *with_timeout(Duration::from_secs(5), stack.dns_query(session.server.as_str(), DnsQueryType::A))
        .await
        .ok()
        .and_then(|r| r.ok())
        .and_then(|a| a.first().copied())
        .as_ref()
        .ok_or(Failure::Other)?;
    let mut socket = UdpSocket::new(stack).map_err(|_| Failure::Other)?;
    socket.bind(0, ListenSocketAddr::UNSPECIFIED).map_err(|_| Failure::Other)?;
    for attempt in 0..ATTEMPTS {
        let sent = Instant::now();
        let mut rng = DriverRng;
        let nonce = rng.next_u64().to_be_bytes();
        let (request, pending) = session.build_request(&mut rng, nonce).map_err(|_| Failure::Other)?;
        socket.send_to(&request, SocketAddr::new(addr, session.port)).await.map_err(|_| Failure::Other)?;
        let mut buf = [0u8; 1500];
        let Ok(Ok((n, from))) = with_timeout(REPLY_TIMEOUT, socket.recv_from(&mut buf)).await else {
            warn!("nts: no reply (attempt {})", attempt + 1);
            continue;
        };
        if from.remote_addr.addr != addr {
            continue;
        }
        match session.handle_response(&buf[..n], &pending) {
            Ok((secs, ms)) => {
                let total_ms = ms + (sent.elapsed().as_millis() / 2) as u16;
                return Ok((secs + (total_ms / 1000) as u64, total_ms % 1000));
            }
            Err(NtsError::Nak) => return Err(Failure::Nak),
            Err(_) => warn!("nts: a reply was refused (not authentic or not ours)"),
        }
    }
    Err(Failure::Other)
}
