//! NTP transport: resolves the configured servers and queries them in order until one answers. An entry `nts:<host>`
//! is queried with NTS (`crate::nts`); every other entry with plain SNTP.

use defmt::{info, warn};
use embassy_net::Stack;
use embassy_net::dns::DnsQueryType;
use embassy_net::iface::Iface;
use embassy_net::udp::UdpSocket;
use embassy_net::wire::{IpAddr, ListenSocketAddr, SocketAddr};
use embassy_time::{Duration, Instant, with_timeout};
use picoclock_core::sntp;

use crate::nts::Nts;

const NTP_PORT: u16 = 123;
const ATTEMPTS_PER_SERVER: u8 = 2;
const REPLY_TIMEOUT: Duration = Duration::from_secs(3);

/// Queries each configured server in order; returns UTC as (unix seconds, ms) corrected for half the round-trip, and
/// whether the answer was authenticated (NTS). The order of the list is the only fallback policy: an `nts:` entry that
/// fails is followed by the next entry, whatever kind it is, and a list of `nts:` entries alone never falls back to
/// plain NTP.
pub async fn query(stack: Stack<'static>, iface: &Iface<'static>, servers: &str, nts: &mut Nts) -> Option<(u64, u16, bool)> {
    for name in servers.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if let Some(host) = picoclock_core::config::nts_host(name) {
            if let Some((secs, ms)) = nts.query(stack, host).await {
                return Some((secs, ms, true));
            }
            continue;
        }
        if name.eq_ignore_ascii_case("dhcp") {
            // the servers the router handed out (option 42); none offered = nothing to try
            let offered = iface.dhcpv4_lease().and_then(|l| l.options.get(42).map(picoclock_core::netutil::dhcp_ntp_servers)).unwrap_or_default();
            for a in offered {
                if let Some(t) = ask(stack, name, IpAddr::V4(a.into())).await {
                    return Some((t.0, t.1, false));
                }
            }
            continue;
        }
        let Some(addr) = resolve(stack, iface, name).await else {
            warn!("ntp: cannot resolve {}", name);
            continue;
        };
        if let Some(t) = ask(stack, name, addr).await {
            return Some((t.0, t.1, false));
        }
    }
    None
}

/// Up to `ATTEMPTS_PER_SERVER` requests to one address.
async fn ask(stack: Stack<'static>, name: &str, addr: IpAddr) -> Option<(u64, u16)> {
    for _ in 0..ATTEMPTS_PER_SERVER {
        match exchange(stack, addr).await {
            Ok(t) => {
                info!("ntp: {} ({}) answered, unix {}", name, addr, t.0);
                return Some(t);
            }
            Err(()) => warn!("ntp: no valid reply from {} ({})", name, addr),
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

    // A random origin nonce (hardware TRNG on the Pico 2 W): the uptime shown by `status` would make it guessable.
    let sent = Instant::now();
    let nonce = crate::crypto::next_u64().to_be_bytes();
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
