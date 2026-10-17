//! WiFi supervision: joins the configured network, keeps time synced over NTP, and (when `timezone=auto`) looks the
//! timezone up from the public IP address. Re-reads the configuration continuously, so changes made over USB or the
//! web page take effect without a reboot.

use cyw43::{Control, JoinError, JoinOptions};
use defmt::{info, warn};
use embassy_net::Stack;
use embassy_net::dns::DnsQueryType;
use embassy_net::iface::Iface;
use embassy_net::tcp::TcpSocket;
use embassy_net::wire::{IpAddr, SocketAddr};
use embassy_time::{Duration, Instant, Timer, with_timeout};
use embedded_io_async::Write as _;
use picoclock_core::config::Config;
use picoclock_core::cli::GeoTry;
use picoclock_core::geo;
use picoclock_core::netutil::{self, JoinPlan, Seen};

use crate::nts::Nts;
use crate::{ntp, state};

const RESYNC: Duration = Duration::from_secs(3600);
const NTP_RETRY: Duration = Duration::from_secs(30);
const GEO_REFRESH: Duration = Duration::from_secs(24 * 3600);
const GEO_RETRY: Duration = Duration::from_secs(300);
/// A join attempt that has not finished by then is abandoned: the radio driver waits for an event that can be lost,
/// and without a limit one lost event leaves the clock offline until reboot.
const JOIN_TIMEOUT: Duration = Duration::from_secs(15);
/// Attempts per slot for errors that are often transient (an access point may reject the first authentication).
const JOIN_ATTEMPTS: u8 = 3;
const JOIN_GAP: Duration = Duration::from_secs(1);
/// Pause after every stored network failed, before starting over (shorter wait if the user changes a setting).
const JOIN_RETRY: Duration = Duration::from_secs(10);
/// After joining, the link and a DHCP lease must arrive within this time.
const CONFIG_TIMEOUT: Duration = Duration::from_secs(30);
/// How long the scan for `wifi_select strongest` may take.
const SCAN_TIME: Duration = Duration::from_secs(8);

type Creds = (heapless::String<64>, heapless::String<64>);

fn creds(cfg: &Config, slot: usize) -> Creds {
    (cfg.wifi[slot].ssid.clone(), cfg.wifi[slot].password.clone())
}

fn octets(a: embassy_net::wire::Ipv4Addr) -> [u8; 4] {
    a.octets()
}

#[embassy_executor::task]
pub async fn wifi_task(mut control: Control<'static>, stack: Stack<'static>, iface: Iface<'static>) -> ! {
    let mac = control.address().await;
    state::with_runtime(|r| r.mac = Some(mac));
    let mut nts = Nts::new();
    loop {
        let cfg = state::config_snapshot();
        // Name on the network: the `hostname` setting, or picoclock-<last MAC octet>. Set before DHCP runs so the router sees
        // it; a change is picked up by `online` returning, which brings us back here to join (and ask DHCP) again.
        let name = cfg.device_name(mac[5]);
        let _ = stack.set_hostname(&name);
        if cfg.wifi.iter().all(|n| n.ssid.is_empty()) {
            info!("wifi: no SSID configured; waiting (set one on the USB terminal or the web page)");
            state::CONFIG_CHANGED.wait().await;
            continue;
        }
        // Try the stored networks in slot order, or strongest first (`wifi_select`); the first one that joins wins.
        let mut seen: heapless::Vec<Seen, 24> = heapless::Vec::new();
        if cfg.wifi_strongest {
            scan(&mut control, &mut seen).await;
        }
        state::with_runtime(|r| r.wifi_scanned = cfg.wifi_strongest.then_some(seen.len() as u8));
        let ssids: [&str; picoclock_core::config::WIFI_SLOTS] = core::array::from_fn(|i| cfg.wifi[i].ssid.as_str());
        let order = netutil::wifi_order(&ssids, &seen, cfg.wifi_strongest);
        let mut joined = None;
        // Every stored network is tried once before any is retried (`JoinPlan`), so a network with a wrong passphrase,
        // which costs a handshake timeout of about 10 s per attempt, does not hold up the next one.
        let mut plan = JoinPlan::new(&order, JOIN_ATTEMPTS);
        while let Some((slot, attempt)) = plan.next() {
            let net = &cfg.wifi[slot];
            info!("wifi: joining slot {} ({}), attempt {}", slot + 1, net.ssid.as_str(), attempt);
            let opts = if net.password.is_empty() { JoinOptions::new_open() } else { JoinOptions::new(net.password.as_bytes()) };
            match with_timeout(JOIN_TIMEOUT, control.join(&net.ssid, opts)).await {
                Ok(Ok(())) => {
                    joined = Some(slot);
                    break;
                }
                // A wrong passphrase or a network that is not there will not get better in a second.
                Ok(Err(e @ (JoinError::InvalidPassphrase | JoinError::NetworkNotFound))) => {
                    warn!("wifi: slot {} join failed: {:?}", slot + 1, e);
                    plan.failed(slot, true);
                }
                Ok(Err(e)) => {
                    warn!("wifi: slot {} join failed: {:?}", slot + 1, e);
                    plan.failed(slot, false);
                }
                Err(_) => {
                    warn!("wifi: slot {} join timed out", slot + 1);
                    let _ = control.leave().await;
                    plan.failed(slot, false);
                }
            }
            Timer::after(JOIN_GAP).await;
        }
        let Some(slot) = joined else {
            // Nothing in range: retry later, but sooner if the user changes something.
            let _ = with_timeout(JOIN_RETRY, state::CONFIG_CHANGED.wait()).await;
            continue;
        };
        let up = with_timeout(CONFIG_TIMEOUT, async {
            iface.wait_link_up().await;
            iface.wait_config_up().await;
        })
        .await;
        if up.is_err() {
            warn!("wifi: joined slot {} but no link or DHCP lease in time; starting over", slot + 1);
            let _ = control.leave().await;
            continue;
        }
        info!("wifi: up on slot {}", slot + 1);
        // mDNS: listen for queries to the multicast group (the radio's filter follows the stack's memberships)
        let joined_group = iface.join_multicast_group(embassy_net::wire::Ipv4Addr::new(224, 0, 0, 251));
        state::with_runtime(|r| r.mdns_group = if joined_group.is_ok() { 1 } else { 2 });
        if let Err(e) = joined_group {
            warn!("mdns: joining the multicast group failed: {:?}", defmt::Debug2Format(&e));
        }
        control.gpio_set(0, true).await; // onboard LED = connected
        online(stack, &iface, slot, &creds(&cfg, slot), &name, &mut nts).await;
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

/// Collects the networks in range (name and signal) with one passive scan, at most `SCAN_TIME` long.
async fn scan(control: &mut Control<'static>, seen: &mut heapless::Vec<Seen, 24>) {
    let mut scanner = control.scan(Default::default()).await;
    let _ = with_timeout(SCAN_TIME, async {
        while let Some(bss) = scanner.next().await {
            let Ok(ssid) = core::str::from_utf8(&bss.ssid[..(bss.ssid_len as usize).min(32)]) else { continue };
            if ssid.is_empty() {
                continue; // hidden network
            }
            if let (Ok(ssid), true) = (heapless::String::try_from(ssid), !seen.is_full()) {
                let _ = seen.push(Seen { ssid, rssi: bss.rssi });
            }
        }
    })
    .await;
    drop(scanner);
    info!("wifi: scan saw {} access points", seen.len());
}

/// Runs while connected; returns when the link drops or the credentials change.
async fn online(stack: Stack<'static>, iface: &Iface<'static>, slot: usize, joined_with: &Creds, name: &str, nts: &mut Nts) {
    let mut next_sync = Instant::now();
    let mut next_geo = Instant::now();
    loop {
        let cfg = state::config_snapshot();
        let mac_last = state::status().mac.map_or(0, |m| m[5]);
        if !iface.is_link_up() || creds(&cfg, slot) != *joined_with || cfg.device_name(mac_last).as_str() != name {
            return;
        }
        if let Some(lease) = iface.dhcpv4_lease() {
            let ntp = lease.options.get(42).map(netutil::dhcp_ntp_servers).unwrap_or_default();
            state::with_runtime(|r| {
                r.dhcp_ntp_n = ntp.len() as u8;
                for (slot, a) in r.dhcp_ntp.iter_mut().zip(ntp.iter()) {
                    *slot = *a;
                }
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
                + match ntp::query(stack, iface, &cfg.ntp_servers, nts).await {
                    Some((secs, ms, secure)) => {
                        state::with_runtime(|r| r.sync_secure = secure);
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
                            state::mark_cache_changed();
                            GEO_REFRESH
                        }
                        None => GEO_RETRY,
                    };
            }
        }
        Timer::after_millis(500).await;
    }
}

/// Asks the providers in order (see `geo::PROVIDERS`); returns the zone name and its POSIX rule from the first one
/// that answers with a zone in the built-in table.
async fn geolocate(stack: Stack<'static>) -> Option<(heapless::String<48>, &'static str)> {
    for (n, provider) in geo::PROVIDERS.iter().enumerate() {
        // one retry: a single dropped connection is the commonest failure; the outcome of the last try stays in `status`
        for attempt in 0..2 {
            if let Some(found) = geolocate_with(stack, provider, n).await {
                return Some(found);
            }
            warn!("geo: {} gave no usable answer (attempt {})", provider.host, attempt + 1);
            if attempt == 0 {
                Timer::after_secs(2).await;
            }
        }
    }
    None
}

/// One plain-HTTP request to one provider.
async fn geolocate_with(stack: Stack<'static>, provider: &geo::Provider, slot: usize) -> Option<(heapless::String<48>, &'static str)> {
    let stage = |n: u8, bytes: usize| {
        state::with_runtime(|r| {
            r.geo[slot].stage = n;
            r.geo[slot].bytes = bytes as u16;
        });
    };
    state::with_runtime(|r| r.geo[slot] = GeoTry::default());
    let Some(addr) = stack.dns_query(provider.host, DnsQueryType::A).await.ok().and_then(|a| a.first().copied()) else {
        stage(1, 0);
        return None;
    };
    let IpAddr::V4(a) = addr;
    state::with_runtime(|r| r.geo[slot].addr = a.octets());
    let mut rx = [0u8; 1024];
    let mut tx = [0u8; 192];
    let mut sock = TcpSocket::new(stack, &mut rx, &mut tx).ok()?;
    sock.set_timeout(Some(Duration::from_secs(8)));
    if sock.connect(SocketAddr::new(addr, geo::PORT)).await.is_err() {
        stage(2, 0);
        return None;
    }
    if sock.write_all(provider.request).await.is_err() {
        stage(3, 0);
        return None;
    }

    // Some servers keep the connection open, so stop as soon as a complete reply has arrived.
    let mut buf = [0u8; 1024];
    let mut len = 0;
    let found = with_timeout(Duration::from_secs(8), async {
        while len < buf.len() {
            match sock.read(&mut buf[len..]).await {
                Ok(0) => {
                    state::with_runtime(|r| r.geo[slot].end = 1);
                    break;
                }
                Err(_) => {
                    state::with_runtime(|r| r.geo[slot].end = 2);
                    break;
                }
                Ok(n) => len += n,
            }
            if let Some((name, rule)) = geo::parse_response(&buf[..len], provider.format) {
                return heapless::String::try_from(name).ok().map(|n| (n, rule));
            }
        }
        None
    })
    .await
    .ok()
    .flatten();
    sock.abort();
    stage(if found.is_some() { 5 } else { 4 }, len);
    if found.is_none() && len > 0 {
        warn!("geo: reply was {=[u8]:a}", &buf[..len.min(120)]);
    }
    found
}

/// Answers multicast DNS queries for `<device name>.local` (setting `mdns`) with the device's address.
#[embassy_executor::task]
pub async fn mdns_task(stack: Stack<'static>) -> ! {
    use embassy_net::udp::{UdpMetadata, UdpSocket};
    use embassy_net::wire::{Ipv4Addr, ListenSocketAddr};
    const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
    const PORT: u16 = 5353;
    loop {
        let st = state::status();
        let (Some(ip), Some(mac), true) = (st.ip, st.mac, state::with_config(|c| c.mdns) && st.wifi_up) else {
            Timer::after_secs(2).await;
            continue;
        };
        let Ok(mut socket) = UdpSocket::new(stack) else {
            Timer::after_secs(5).await;
            continue;
        };
        if socket.bind(PORT, ListenSocketAddr::UNSPECIFIED).is_err() {
            warn!("mdns: cannot bind UDP 5353");
            Timer::after_secs(5).await;
            continue;
        }
        let host = state::with_config(|c| c.device_name(mac[5]));
        info!("mdns: answering for {}.local", host.as_str());
        let mut buf = [0u8; 512];
        let mut reply = [0u8; 96];
        loop {
            let st = state::status();
            if !state::with_config(|c| c.mdns) || !st.wifi_up || st.ip != Some(ip) || state::with_config(|c| c.device_name(mac[5])) != host {
                break;
            }
            let Ok(Ok((n, from))) = with_timeout(Duration::from_secs(2), socket.recv_from(&mut buf)).await else { continue };
            state::with_runtime(|r| r.mdns_seen = r.mdns_seen.wrapping_add(1));
            if let Some(len) = netutil::mdns_answer(&buf[..n], &host, ip, &mut reply) {
                state::with_runtime(|r| r.mdns_answered = r.mdns_answered.wrapping_add(1));
                // a resolver on another port asked as a plain unicast DNS client: answer it directly; otherwise to the group
                // (built fresh: the received metadata names the multicast group as our local address, which is no valid source)
                let to: UdpMetadata = if from.remote_addr.port == PORT {
                    SocketAddr::new(IpAddr::V4(GROUP), PORT).into()
                } else {
                    SocketAddr::new(from.remote_addr.addr, from.remote_addr.port).into()
                };
                let sent = socket.send_to(&reply[..len], to).await;
                if sent.is_err() {
                    warn!("mdns: sending the answer failed");
                    state::with_runtime(|r| r.mdns_answered = r.mdns_answered.wrapping_sub(1)); // only answers that went out count
                }
            }
        }
        info!("mdns: stopped");
    }
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
