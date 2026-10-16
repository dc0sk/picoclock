//! WiFi supervision: joins the configured network, keeps time synced over NTP, and (when `timezone=auto`) looks the
//! timezone up from the public IP address. Re-reads the configuration continuously, so changes made over USB or the
//! web page take effect without a reboot.

use cyw43::{Control, JoinOptions};
use defmt::{info, warn};
use embassy_net::Stack;
use embassy_net::dns::DnsQueryType;
use embassy_net::iface::Iface;
use embassy_net::tcp::TcpSocket;
use embassy_net::wire::SocketAddr;
use embassy_time::{Duration, Instant, Timer, with_timeout};
use embedded_io_async::Write as _;
use picoclock_core::config::Config;
use picoclock_core::geo;

use crate::{ntp, state};

const RESYNC: Duration = Duration::from_secs(3600);
const NTP_RETRY: Duration = Duration::from_secs(30);
const GEO_REFRESH: Duration = Duration::from_secs(24 * 3600);
const GEO_RETRY: Duration = Duration::from_secs(300);
const JOIN_RETRY: Duration = Duration::from_secs(30);

type Creds = (heapless::String<64>, heapless::String<64>);

fn creds(cfg: &Config, slot: usize) -> Creds {
    (cfg.wifi[slot].ssid.clone(), cfg.wifi[slot].password.clone())
}

fn octets(a: embassy_net::wire::Ipv4Addr) -> [u8; 4] {
    a.octets()
}

#[embassy_executor::task]
pub async fn wifi_task(mut control: Control<'static>, stack: Stack<'static>, iface: Iface<'static>) -> ! {
    // Name on the network: picoclock-<last MAC octet>. Must be set before DHCP runs so the router sees it.
    let mac = control.address().await;
    let _ = stack.set_hostname(&picoclock_core::config::hostname(mac));
    state::with_runtime(|r| r.mac_last = Some(mac[5]));
    loop {
        let cfg = state::config_snapshot();
        if cfg.wifi.iter().all(|n| n.ssid.is_empty()) {
            info!("wifi: no SSID configured; waiting (set one on the USB terminal or the web page)");
            state::CONFIG_CHANGED.wait().await;
            continue;
        }
        // Try the stored networks in slot order; the first one that joins wins.
        let mut joined = None;
        for (slot, net) in cfg.wifi.iter().enumerate().filter(|(_, n)| !n.ssid.is_empty()) {
            info!("wifi: joining slot {} ({})", slot + 1, net.ssid.as_str());
            let opts = if net.password.is_empty() { JoinOptions::new_open() } else { JoinOptions::new(net.password.as_bytes()) };
            match control.join(&net.ssid, opts).await {
                Ok(()) => {
                    joined = Some(slot);
                    break;
                }
                Err(e) => warn!("wifi: slot {} join failed: {:?}", slot + 1, e),
            }
        }
        let Some(slot) = joined else {
            // Nothing in range: retry later, but sooner if the user changes something.
            let _ = with_timeout(JOIN_RETRY, state::CONFIG_CHANGED.wait()).await;
            continue;
        };
        iface.wait_link_up().await;
        iface.wait_config_up().await;
        info!("wifi: up on slot {}", slot + 1);
        control.gpio_set(0, true).await; // onboard LED = connected
        online(stack, &iface, slot, &creds(&cfg, slot)).await;
        warn!("wifi: leaving network");
        state::with_runtime(|r| {
            r.wifi_up = false;
            r.wifi_slot = None;
            r.ip = None;
            r.gateway = None;
        });
        control.gpio_set(0, false).await;
        let _ = control.leave().await;
    }
}

/// Runs while connected; returns when the link drops or the credentials change.
async fn online(stack: Stack<'static>, iface: &Iface<'static>, slot: usize, joined_with: &Creds) {
    let mut next_sync = Instant::now();
    let mut next_geo = Instant::now();
    loop {
        let cfg = state::config_snapshot();
        if !iface.is_link_up() || creds(&cfg, slot) != *joined_with {
            return;
        }
        if let Some(lease) = iface.dhcpv4_lease() {
            state::with_runtime(|r| {
                r.wifi_up = true;
                r.wifi_slot = Some(slot as u8 + 1);
                r.ip = Some(octets(lease.address.address()));
                r.gateway = lease.router.map(octets);
            });
        }
        if state::SYNC_NOW.try_take().is_some() {
            next_sync = Instant::now();
        }
        if state::GEO_NOW.try_take().is_some() {
            next_geo = Instant::now();
        }
        if Instant::now() >= next_sync {
            next_sync = Instant::now()
                + match ntp::query(stack, iface, &cfg.ntp_servers).await {
                    Some((secs, ms)) => {
                        state::NTP_TIME.signal((secs, ms, Instant::now()));
                        RESYNC
                    }
                    None => NTP_RETRY,
                };
        }
        if cfg.wants_geolocation() {
            // `next_geo` starts in the past, so the first pass after joining (or after switching to auto) looks up at once.
            if Instant::now() >= next_geo {
                next_geo = Instant::now()
                    + match geolocate(stack).await {
                        Some((name, rule)) => {
                            info!("geo: {} -> {}", name.as_str(), rule);
                            state::with_config(|c| {
                                let _ = c.set("geo_rule", rule);
                            });
                            state::mark_changed(false);
                            GEO_REFRESH
                        }
                        None => GEO_RETRY,
                    };
            }
        }
        Timer::after_millis(500).await;
    }
}

/// One plain-HTTP request to the geolocation service; returns the zone name and its POSIX rule.
async fn geolocate(stack: Stack<'static>) -> Option<(heapless::String<48>, &'static str)> {
    let addr = stack.dns_query(geo::HOST, DnsQueryType::A).await.ok()?.first().copied()?;
    let mut rx = [0u8; 768];
    let mut tx = [0u8; 192];
    let mut sock = TcpSocket::new(stack, &mut rx, &mut tx).ok()?;
    sock.set_timeout(Some(Duration::from_secs(8)));
    sock.connect(SocketAddr::new(addr, geo::PORT)).await.ok()?;
    sock.write_all(geo::REQUEST).await.ok()?;

    // The server keeps the connection open, so stop as soon as a complete reply has arrived.
    let mut buf = [0u8; 768];
    let mut len = 0;
    let found = with_timeout(Duration::from_secs(8), async {
        while len < buf.len() {
            match sock.read(&mut buf[len..]).await {
                Ok(0) | Err(_) => break,
                Ok(n) => len += n,
            }
            if let Some((name, rule)) = geo::parse_response(&buf[..len]) {
                return heapless::String::try_from(name).ok().map(|n| (n, rule));
            }
        }
        None
    })
    .await
    .ok()
    .flatten();
    sock.abort();
    if found.is_none() {
        warn!("geo: no usable reply");
    }
    found
}

/// Optional NTP server (`ntp_serve=on`): answers client requests on UDP 123 from the RTC. Accuracy is about +-1 s
/// because the DS3231 has whole-second resolution; replies are marked unsynchronised unless NTP synced within a day.
#[embassy_executor::task]
pub async fn ntp_server_task(stack: Stack<'static>) -> ! {
    use embassy_net::udp::UdpSocket;
    use embassy_net::wire::ListenSocketAddr;
    const FRESH_SECS: i64 = 24 * 3600;
    loop {
        if !state::with_config(|c| c.ntp_serve) || !state::with_runtime(|r| r.wifi_up) {
            Timer::after_secs(2).await;
            continue;
        }
        let Ok(mut socket) = UdpSocket::new(stack) else {
            Timer::after_secs(5).await;
            continue;
        };
        if socket.bind(123, ListenSocketAddr::UNSPECIFIED).is_err() {
            warn!("ntp server: cannot bind UDP 123");
            Timer::after_secs(5).await;
            continue;
        }
        info!("ntp server: listening");
        let mut buf = [0u8; 128];
        loop {
            if !state::with_config(|c| c.ntp_serve) || !state::with_runtime(|r| r.wifi_up) {
                break;
            }
            let Ok(Ok((n, from))) = with_timeout(Duration::from_secs(2), socket.recv_from(&mut buf)).await else { continue };
            let Some((base, at)) = state::anchor() else { continue };
            let us = at.elapsed().as_micros();
            let now = base + (us / 1_000_000) as i64;
            let frac = (((us % 1_000_000) << 32) / 1_000_000) as u32;
            let last = state::status().last_sync_unix;
            let synced = matches!(last, Some(s) if now - s < FRESH_SECS);
            if let Some(reply) = picoclock_core::sntp::build_server_reply(&buf[..n], now, frac, last, synced) {
                let _ = socket.send_to(&reply, from).await;
            }
        }
        info!("ntp server: stopped");
    }
}
