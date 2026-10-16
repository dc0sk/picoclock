//! SNTP transport: resolves the configured servers and queries them in order until one answers.

use defmt::{info, warn};
use embassy_net::Stack;
use embassy_net::dns::DnsQueryType;
use embassy_net::iface::Iface;
use embassy_net::udp::UdpSocket;
use embassy_net::wire::{IpAddr, ListenSocketAddr, SocketAddr};
use embassy_time::{Duration, Instant, with_timeout};
use picoclock_core::sntp;

const NTP_PORT: u16 = 123;
const ATTEMPTS_PER_SERVER: u8 = 2;
const REPLY_TIMEOUT: Duration = Duration::from_secs(3);

/// Queries each configured server in order; returns UTC as (unix seconds, ms) corrected for half the round-trip.
pub async fn query(stack: Stack<'static>, iface: &Iface<'static>, servers: &str) -> Option<(u64, u16)> {
    for name in servers.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let Some(addr) = resolve(stack, iface, name).await else {
            warn!("ntp: cannot resolve {}", name);
            continue;
        };
        for _ in 0..ATTEMPTS_PER_SERVER {
            match exchange(stack, addr).await {
                Ok(t) => {
                    info!("ntp: {} ({}) answered, unix {}", name, addr, t.0);
                    return Some(t);
                }
                Err(()) => warn!("ntp: no valid reply from {} ({})", name, addr),
            }
        }
    }
    None
}

async fn resolve(stack: Stack<'static>, iface: &Iface<'static>, name: &str) -> Option<IpAddr> {
    if name.eq_ignore_ascii_case("gateway") {
        let gw = iface.dhcpv4_lease()?.router?;
        return Some(IpAddr::V4(gw));
    }
    stack.dns_query(name, DnsQueryType::A).await.ok()?.first().copied()
}

async fn exchange(stack: Stack<'static>, server: IpAddr) -> Result<(u64, u16), ()> {
    let mut socket = UdpSocket::new(stack).map_err(|_| ())?;
    socket.bind(0, ListenSocketAddr::UNSPECIFIED).map_err(|_| ())?;

    // Uptime in microseconds is unique per request and unguessable enough for an origin check.
    let sent = Instant::now();
    let nonce = sent.as_micros().to_be_bytes();
    socket
        .send_to(&sntp::build_request(nonce), SocketAddr::new(server, NTP_PORT))
        .await
        .map_err(|_| ())?;

    let mut buf = [0u8; 128];
    let (n, from) = with_timeout(REPLY_TIMEOUT, socket.recv_from(&mut buf))
        .await
        .map_err(|_| ())?
        .map_err(|_| ())?;
    if from.remote_addr.addr != server {
        return Err(());
    }
    let (secs, ms) = sntp::parse_response(&buf[..n], nonce).map_err(|_| ())?;
    let half_rtt_ms = (sent.elapsed().as_millis() / 2) as u16;
    let total_ms = ms + half_rtt_ms;
    Ok((secs + (total_ms / 1000) as u64, total_ms % 1000))
}
