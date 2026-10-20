//! Made-up data for the README screenshots: nothing here comes from a real clock (names, addresses, MACs and fingerprints are
//! invented). Shared by the `web_screens` and `terminal_screens` examples.

#![allow(dead_code)]

use picoclock_core::cli::{ApSeen, Status};
use picoclock_core::config::Config;
use picoclock_core::rootstore::Roots;
use picoclock_core::sources::{Report, Source, State};

pub const NOW: i64 = 1_792_324_800; // 2026-10-18 12:00:00 UTC, an invented moment

fn b64(der: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for chunk in der.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |a, (i, &b)| a | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            s.push(if i <= chunk.len() { T[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    s
}

pub fn config() -> Config {
    let mut cfg = Config::default();
    for (k, v) in [
        ("wifi1_ssid", "HomeNetwork"),
        ("wifi1_password", "an-example-passphrase"),
        ("wifi2_ssid", "Workshop"),
        ("wifi2_password", "another-example-one"),
        ("timezone", "Europe/Berlin"),
        ("ntp_servers", "gateway,nts:time.cloudflare.com,pool.ntp.org"),
        ("time_sources", "dcf77,ntp"),
        ("time_mode", "vote"),
        ("vote_ntp", "2"),
        ("hourly_chime", "on"),
        ("ham_mode", "on"),
        ("hostname", "workshop-clock"),
        ("dcf77_learned", "58"),
    ] {
        cfg.set(k, v).unwrap();
    }
    cfg
}

pub fn status() -> Status {
    let fake = |last: u8, ch, rssi| Some(ApSeen { bssid: [0x02, 0x00, 0x5e, 0x10, 0x00, last], channel: ch, rssi });
    Status {
        utc_unix: Some(NOW),
        last_sync_unix: Some(NOW - 640),
        wifi_up: true,
        wifi_slot: Some(1),
        mac: Some([0x02, 0x00, 0x5e, 0x10, 0x00, 0x2a]),
        ip: Some([192, 168, 0, 42]),
        gateway: Some([192, 168, 0, 1]),
        dhcp_server: Some([192, 168, 0, 1]),
        prefix: 24,
        dns: Some([192, 168, 0, 1]),
        link_rssi: Some(-52),
        aps: [fake(0x11, 1, -51), fake(0x21, 1, -74), None, None, None, None],
        temp_c10: Some(224),
        chip_temp_c10: Some(318),
        light_raw: 2310,
        uptime_s: 93_784,
        sync_secure: false,
        https_state: 2,
        rng_hardware: true,
        dhcp_ntp: [[192, 168, 0, 1], [0; 4], [0; 4], [0; 4]],
        dhcp_ntp_n: 1,
        roots_user: 1,
        mdns_group: 1,
        mdns_seen: 12,
        mdns_answered: 3,
        heap_used: 5_200,
        heap_peak: 7_400,
        heap_size: 147_456,
        cert_fp: core::array::from_fn(|i| (i as u8).wrapping_mul(37).wrapping_add(11)),
        cert_not_after: NOW + 3650 * 86_400,
        sources: [
            Some(Report { source: Source::Ntp, state: State::Used, offset_ms: Some(2), age_s: Some(640) }),
            Some(Report { source: Source::Dcf77, state: State::Used, offset_ms: Some(-12), age_s: Some(95) }),
            Some(Report { source: Source::Gps, state: State::Off, offset_ms: None, age_s: None }),
        ],
        dcf: picoclock_core::dcf77::Stats { pulses: 5_420, frames_ok: 83, frames_bad: 6, pulses_bad: 11 },
        ..Default::default()
    }
}

pub fn roots() -> Roots {
    let mut roots = Roots::new();
    let cert = picoclock_core::x509::generate_self_signed([7; 32], [7; 8], &["example-time-authority.test"], NOW).unwrap();
    roots.add_pem(&format!("-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----\n", b64(&cert)), Some(NOW)).unwrap();
    roots
}
