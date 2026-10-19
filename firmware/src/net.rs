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
use picoclock_core::cli::ApSeen;
use picoclock_core::geo;
use picoclock_core::netutil::{self, JoinPlan, Seen};
use picoclock_core::sources::{Sample, Source};

use crate::nts::Nts;
use crate::{ntp, state};

const RESYNC: Duration = Duration::from_secs(3600);
const NTP_RETRY: Duration = Duration::from_secs(30);
/// The update check: first one 3 minutes after joining, then daily; a failure is retried after an hour, a rate limit or a
/// switched-off check looked at again after six hours.
const UPDATE_FIRST: Duration = Duration::from_secs(180);
const UPDATE_EVERY: Duration = Duration::from_secs(24 * 3600);
const UPDATE_RETRY: Duration = Duration::from_secs(3600);
const UPDATE_RECHECK: Duration = Duration::from_secs(6 * 3600);
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
        record_access_points(&mut control, &cfg.wifi[slot].ssid).await;
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
            r.lease_changes = 0;
            r.dhcp_server = None;
            r.link_rssi = None;
            r.aps = [None; 6];
            r.wifi_up = false;
            r.wifi_slot = None;
            r.ip = None;
            r.gateway = None;
        });
        control.gpio_set(0, false).await;
        let _ = control.leave().await;
    }
}

/// Records which access points advertise the joined network's name and the signal of the link, so that `status` shows which one
/// the clock probably joined (the chip picks the access point; its address is not reported). The scan only listens for a moment.
async fn record_access_points(control: &mut Control<'static>, ssid: &str) {
    let link = control.get_rssi().await;
    let mut found: heapless::Vec<ApSeen, 12> = heapless::Vec::new();
    let mut opts = cyw43::ScanOptions::default();
    opts.ssid = heapless::String::try_from(ssid).ok();
    let mut scanner = control.scan(opts).await;
    let _ = with_timeout(Duration::from_secs(3), async {
        while let Some(bss) = scanner.next().await {
            if bss.ssid_len as usize == ssid.len() && !found.is_full() && !found.iter().any(|a| a.bssid == bss.bssid) {
                let _ = found.push(ApSeen { bssid: bss.bssid, channel: (bss.chanspec & 0xff) as u8, rssi: bss.rssi });
            }
        }
    })
    .await;
    drop(scanner);
    found.sort_unstable_by_key(|a| core::cmp::Reverse(a.rssi));
    info!("wifi: link {} dBm; {} access point(s) with this name", link, found.len());
    for a in found.iter() {
        info!("wifi:   {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} channel {} {} dBm", a.bssid[0], a.bssid[1], a.bssid[2], a.bssid[3], a.bssid[4], a.bssid[5], a.channel, a.rssi);
    }
    state::with_runtime(|r| {
        r.link_rssi = (link != 0).then_some(link as i16);
        r.aps = [None; 6];
        for (slot, a) in r.aps.iter_mut().zip(found.iter()) {
            *slot = Some(*a);
        }
    });
}

/// Asks GitHub for the latest release and records what was found; returns when to ask again. Nothing is installed.
async fn update_check(stack: Stack<'static>, nts: &mut Nts) -> Duration {
    use picoclock_core::update::{self, Check, Outcome, Version};
    let mut response = [0u8; 4096];
    let request = update::request();
    let got = nts.https_get(stack, update::HOST, request.as_bytes(), picoclock_core::nts_ke::UPDATE_ROOTS, &mut response).await;
    let now = state::status().utc_unix;
    let (check, latest, again) = match got {
        Some(n) => match update::parse_response(&response[..n], Version::running()) {
            Outcome::RateLimited => (Check::RateLimited, None, UPDATE_RECHECK),
            other => {
                let (c, v) = <(Check, Option<Version>)>::from(other);
                (c, v, if c == Check::Failed { UPDATE_RETRY } else { UPDATE_EVERY })
            }
        },
        // no valid time yet counts as "not now", not as a failure to remember for a day
        None if now.is_none() => (Check::Never, None, UPDATE_FIRST),
        None => (Check::Failed, None, UPDATE_RETRY),
    };
    info!("update: check finished: {}", defmt::Debug2Format(&check));
    state::with_runtime(|r| {
        r.update = check;
        r.update_latest = latest;
        if check != Check::Never {
            r.update_checked_unix = now;
        }
    });
    again
}

/// The first request of the download only needs the redirect; the body of that answer is not read.
#[cfg(all(feature = "https", not(feature = "custom-images")))]
struct RedirectOnly;

#[cfg(all(feature = "https", not(feature = "custom-images")))]
impl picoclock_core::nts_ke::StreamSink for RedirectOnly {
    async fn head(&mut self, _: &picoclock_core::download::Head) -> bool {
        false
    }
    async fn body(&mut self, _: &[u8]) -> bool {
        false
    }
}

/// The second request: the update file goes block by block to the storage task, which stages it like an upload.
#[cfg(all(feature = "https", not(feature = "custom-images")))]
struct Download {
    sink: Option<crate::install::BlockSink>,
    /// The storage task refused the announced size; the reason is already in the status.
    refused: bool,
}

#[cfg(all(feature = "https", not(feature = "custom-images")))]
impl picoclock_core::nts_ke::StreamSink for Download {
    async fn head(&mut self, head: &picoclock_core::download::Head) -> bool {
        let (200, Some(len)) = (head.status, head.content_length) else { return false };
        state::INSTALL_IN.send(state::InstallMsg::Begin(len)).await;
        if state::INSTALL_REPLY.wait().await {
            self.sink = Some(crate::install::BlockSink::new(len));
            true
        } else {
            self.refused = true;
            false
        }
    }
    async fn body(&mut self, data: &[u8]) -> bool {
        match self.sink.as_mut() {
            Some(sink) => {
                sink.push(data).await;
                state::feed_watchdog();
                true
            }
            None => false,
        }
    }
}

/// `update download` (Pico 2 W, official builds): fetches the update file of the release the check found, from GitHub via one
/// redirect to its download host, into the download slot. Nothing is installed: the file is checked like an upload (signature,
/// chip, newer) and waits for `update apply` or the web page's Install button.
#[cfg(all(feature = "https", not(feature = "custom-images")))]
async fn update_download(stack: Stack<'static>, nts: &mut Nts) {
    use picoclock_core::download;
    use picoclock_core::image::Install;
    use picoclock_core::update::Check;
    let (check, latest) = state::with_runtime(|r| (r.update, r.update_latest));
    let (Check::Newer, Some(version)) = (check, latest) else {
        warn!("download: no newer release is known");
        return;
    };
    let Some(claim) = state::InstallClaim::take() else {
        warn!("download: another update file is being received");
        return;
    };
    state::set_install(Install::Downloading(version));
    info!("download: asking github.com for {}", defmt::Display2Format(&download::asset_name(version)));
    let mut failed = true;
    'steps: {
        let first = download::first_request(version);
        let Some(head) = nts.https_stream(stack, download::HOST, first.as_bytes(), picoclock_core::nts_ke::UPDATE_ROOTS, &mut RedirectOnly).await else { break 'steps };
        let redirected = matches!(head.status, 301 | 302 | 303 | 307 | 308);
        let Some(second) = head.location.as_deref().filter(|_| redirected).and_then(download::second_request) else {
            warn!("download: github.com answered {} without a redirect to the download host", head.status);
            break 'steps;
        };
        info!("download: redirected; fetching from {}", download::ASSET_HOST);
        let mut sink = Download { sink: None, refused: false };
        let got = nts.https_stream(stack, download::ASSET_HOST, second.as_bytes(), picoclock_core::nts_ke::DOWNLOAD_ROOTS, &mut sink).await;
        match got {
            Some(head) if head.status == 200 && sink.sink.as_ref().is_some_and(|s| s.remaining == 0) => {
                state::INSTALL_IN.send(state::InstallMsg::Finish).await;
                let staged = state::INSTALL_REPLY.wait().await;
                info!("download: finished, staged={}", staged);
                failed = false; // staged or refused by the checks: the storage task has put the reason in the status
            }
            Some(head) => warn!("download: the download host answered {}", head.status),
            None => {
                if sink.refused {
                    failed = false; // the size was refused; the reason is in the status
                }
            }
        }
        if failed && sink.sink.is_some() {
            state::INSTALL_IN.send(state::InstallMsg::Abort).await;
        }
    }
    drop(claim);
    if failed {
        state::set_install(Install::DownloadFailed);
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
    // the first update check a few minutes after joining, so that it does not pile onto the first sync and key exchange
    let mut next_update = Instant::now() + UPDATE_FIRST;
    loop {
        let cfg = state::config_snapshot();
        let mac_last = state::status().mac.map_or(0, |m| m[5]);
        if !iface.is_link_up() || creds(&cfg, slot) != *joined_with || cfg.device_name(mac_last).as_str() != name {
            return;
        }
        if let Some(lease) = iface.dhcpv4_lease() {
            let ntp = lease.options.get(42).map(netutil::dhcp_ntp_servers).unwrap_or_default();
            let server = octets(lease.server.identifier);
            let (addr, router) = (octets(lease.address.address()), lease.router.map(octets));
            state::with_runtime(|r| {
                // the server, address or gateway changing while the link stays up: a second DHCP server answered a renewal
                if r.ip.is_some() && (r.dhcp_server != Some(server) || r.ip != Some(addr) || r.gateway != router) {
                    r.lease_changes = r.lease_changes.saturating_add(1);
                    warn!("net: the lease changed while connected: server {} address {} gateway {}", server, addr, router);
                }
                r.dhcp_server = Some(server);
                r.prefix = lease.address.prefix_len();
                r.dns = lease.dns_servers.first().map(|d| octets(*d));
            });
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
        else {
            // the lease ran out: do not go on showing the old address as if it were current
            state::with_runtime(|r| {
                if r.ip.take().is_some() {
                    warn!("net: no DHCP lease any more");
                }
                r.gateway = None;
                r.dhcp_server = None;
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
                + if !cfg.time_sources.contains(&Source::Ntp) {
                    NTP_RETRY // not a selected source: ask again later in case that changes
                } else {
                    match ntp::query(stack, iface, &cfg.ntp_servers, nts).await {
                        Some((secs, ms, secure)) => {
                            let sample = Sample { unix_ms: secs as i64 * 1000 + ms as i64, at_ms: Instant::now().as_millis() };
                            let _ = state::SAMPLES.try_send(state::Offer { src: Source::Ntp, sample, secure });
                            RESYNC
                        }
                        None => NTP_RETRY,
                    }
                };
        }
        if state::UPDATE_NOW.try_take().is_some() {
            next_update = Instant::now();
            state::with_runtime(|r| r.update = picoclock_core::update::Check::Checking);
        }
        #[cfg(all(feature = "https", not(feature = "custom-images")))]
        if state::UPDATE_DOWNLOAD.try_take().is_some() {
            update_download(stack, nts).await;
        }
        if Instant::now() >= next_update {
            let asked = state::with_runtime(|r| r.update == picoclock_core::update::Check::Checking);
            next_update = Instant::now()
                + if cfg.update_check || asked {
                    update_check(stack, nts).await
                } else {
                    UPDATE_RECHECK // switched off: look again later in case that changes
                };
        }
        if cfg.wants_geolocation() {
            // `next_geo` starts in the past, so the first pass after joining (or after switching to auto) looks up at once.
            if Instant::now() >= next_geo {
                next_geo = Instant::now()
                    + match geolocate(stack).await {
                        Some((name, rule)) => {
                            info!("geo: {} -> {}", name.as_str(), rule);
                            // the cache is written to flash, but unsaved terminal edits are not
                            state::edit_and_save(false, |c| {
                                let _ = c.set("geo_rule", rule);
                            });
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
            let synced = matches!(last, Some(s) if (0..FRESH_SECS).contains(&(now - s)));
            if let Some(reply) = picoclock_core::sntp::build_server_reply(&buf[..n], now, frac, last, synced) {
                let _ = socket.send_to(&reply, from).await;
            }
        }
        info!("ntp server: stopped");
    }
}
