//! Small pieces of network logic kept apart from the radio and the stack so they can be tested on the host: the order in
//! which stored WiFi networks are tried, the NTP servers a DHCP lease names (option 42), and the mDNS answer for the
//! device's own `<name>.local`.

use heapless::Vec;

use crate::config::WIFI_SLOTS;

/// A network the scan saw: its name and signal strength in dBm (closer to 0 is stronger).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seen {
    pub ssid: heapless::String<32>,
    pub rssi: i16,
}

/// The stored networks (`ssids`, indexed by slot; empty = unused) in the order to try them.
///
/// `strongest = false`: slot order. `strongest = true`: the networks the scan saw, strongest first (a network heard from
/// several access points counts with its best signal; equal strength keeps slot order), then the stored ones the scan did
/// not see, in slot order, because a hidden network or a missed beacon must not make a stored network unreachable.
pub fn wifi_order(ssids: &[&str; WIFI_SLOTS], seen: &[Seen], strongest: bool) -> Vec<usize, WIFI_SLOTS> {
    let used = |s: usize| !ssids[s].is_empty();
    let best = |s: usize| seen.iter().filter(|n| n.ssid.as_str() == ssids[s]).map(|n| n.rssi).max();
    let mut out = Vec::new();
    if strongest {
        let mut heard: Vec<(usize, i16), WIFI_SLOTS> = Vec::new();
        for s in (0..WIFI_SLOTS).filter(|&s| used(s)) {
            // two slots with the same name are one network: the first slot represents it
            if ssids[..s].contains(&ssids[s]) {
                continue;
            }
            if let Some(r) = best(s) {
                let _ = heard.push((s, r));
            }
        }
        heard.sort_unstable_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        for (s, _) in heard {
            let _ = out.push(s);
        }
    }
    for s in (0..WIFI_SLOTS).filter(|&s| used(s)) {
        if !out.contains(&s) {
            let _ = out.push(s);
        }
    }
    out
}

/// The unicast IPv4 addresses in a DHCP "NTP servers" option (42): a list of 4-byte addresses. Entries that cannot be a
/// time server (0.0.0.0, loopback, multicast, broadcast) are dropped; a length that is not a multiple of 4 gives nothing.
pub fn dhcp_ntp_servers(option: &[u8]) -> Vec<[u8; 4], 4> {
    let mut out = Vec::new();
    if option.len() % 4 != 0 {
        return out;
    }
    for a in option.chunks_exact(4) {
        let ip = [a[0], a[1], a[2], a[3]];
        if ip[0] == 0 || ip[0] == 127 || ip[0] >= 224 {
            continue;
        }
        if out.push(ip).is_err() {
            break;
        }
    }
    out
}

/// Builds the mDNS response to a query for `<host>.local` (type A or ANY, class IN), or `None` if the packet is not such a
/// query. The answer carries the device's address with the cache-flush bit and a 2 minute lifetime (RFC 6762). `host` is
/// the bare name, e.g. `picoclock-d6`. Returns the number of bytes written to `out`.
pub fn mdns_answer(query: &[u8], host: &str, ip: [u8; 4], out: &mut [u8]) -> Option<usize> {
    // header: id, flags, qdcount, ancount, nscount, arcount
    if query.len() < 12 || query[2] & 0x80 != 0 {
        return None; // too short, or a response rather than a query
    }
    let qd = u16::from_be_bytes([query[4], query[5]]) as usize;
    let mut at = 12;
    let mut asked = false;
    for _ in 0..qd.min(8) {
        // the name: length-prefixed labels, no compression in a question's first name
        let mut labels: Vec<&[u8], 4> = Vec::new();
        loop {
            let len = *query.get(at)? as usize;
            at += 1;
            if len == 0 {
                break;
            }
            if len & 0xC0 != 0 || len > 63 {
                return None; // compression pointer or invalid label: not something we answer
            }
            labels.push(query.get(at..at + len)?).ok()?;
            at += len;
        }
        let qtype = u16::from_be_bytes([*query.get(at)?, *query.get(at + 1)?]);
        let qclass = u16::from_be_bytes([*query.get(at + 2)?, *query.get(at + 3)?]) & 0x7FFF; // top bit: unicast-response wanted
        at += 4;
        let name_matches = labels.len() == 2 && labels[0].eq_ignore_ascii_case(host.as_bytes()) && labels[1].eq_ignore_ascii_case(b"local");
        if name_matches && qclass == 1 && (qtype == 1 || qtype == 255) {
            asked = true;
        }
    }
    if !asked {
        return None;
    }
    let need = 12 + 1 + host.len() + 1 + 5 + 1 + 10 + 4;
    if out.len() < need || host.len() > 63 {
        return None;
    }
    out[..12].copy_from_slice(&[query[0], query[1], 0x84, 0, 0, 0, 0, 1, 0, 0, 0, 0]); // response, authoritative, 1 answer
    let mut n = 12;
    out[n] = host.len() as u8;
    out[n + 1..n + 1 + host.len()].copy_from_slice(host.as_bytes());
    n += 1 + host.len();
    out[n] = 5;
    out[n + 1..n + 6].copy_from_slice(b"local");
    out[n + 6] = 0;
    n += 7;
    out[n..n + 10].copy_from_slice(&[0, 1, 0x80, 1, 0, 0, 0, 120, 0, 4]); // A, IN + cache flush, TTL 120, 4 bytes
    out[n + 10..n + 14].copy_from_slice(&ip);
    Some(n + 14)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seen(list: &[(&str, i16)]) -> std::vec::Vec<Seen> {
        list.iter().map(|(s, r)| Seen { ssid: heapless::String::try_from(*s).unwrap(), rssi: *r }).collect()
    }

    fn slots<'a>(names: &[&'a str]) -> [&'a str; WIFI_SLOTS] {
        let mut a = [""; WIFI_SLOTS];
        a[..names.len()].copy_from_slice(names);
        a
    }

    // VERIFIES: REQ-NET-04
    #[test]
    fn wifi_order_by_slot_or_by_signal() {
        let s = slots(&["home", "", "office", "phone"]);
        let heard = seen(&[("office", -50), ("home", -70), ("phone", -70), ("other", -30)]);
        assert_eq!(wifi_order(&s, &heard, false).as_slice(), &[0, 2, 3], "slot order ignores the scan");
        assert_eq!(wifi_order(&s, &heard, true).as_slice(), &[2, 0, 3], "strongest first; equal strength keeps slot order");
        // unseen stored networks (hidden SSID, missed beacon) still get their turn, after the seen ones
        assert_eq!(wifi_order(&s, &seen(&[("phone", -80)]), true).as_slice(), &[3, 0, 2]);
        assert_eq!(wifi_order(&s, &[], true).as_slice(), &[0, 2, 3], "an empty scan falls back to slot order");
        // several access points of one network count with the best signal; the same name in two slots is tried once
        let two = slots(&["mesh", "other", "mesh"]);
        assert_eq!(wifi_order(&two, &seen(&[("mesh", -80), ("mesh", -40), ("other", -60)]), true).as_slice(), &[0, 1, 2]);
        assert_eq!(wifi_order(&slots(&[]), &heard, true).as_slice(), &[] as &[usize]);
        // a network that is not stored never appears
        assert!(!wifi_order(&s, &heard, true).contains(&1));
    }

    // VERIFIES: REQ-NET-05
    #[test]
    fn dhcp_option_42_gives_usable_unicast_servers() {
        assert_eq!(dhcp_ntp_servers(&[192, 168, 1, 1, 10, 0, 0, 5]).as_slice(), &[[192, 168, 1, 1], [10, 0, 0, 5]]);
        assert_eq!(dhcp_ntp_servers(&[0, 0, 0, 0, 127, 0, 0, 1, 224, 0, 0, 1, 255, 255, 255, 255, 8, 8, 8, 8]).as_slice(), &[[8, 8, 8, 8]]);
        assert!(dhcp_ntp_servers(&[192, 168, 1]).is_empty(), "a length that is not a multiple of 4");
        assert!(dhcp_ntp_servers(&[]).is_empty());
        assert_eq!(dhcp_ntp_servers(&[1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5]).len(), 4, "at most four");
    }

    fn query(name: &[&str], qtype: u16, qclass: u16) -> std::vec::Vec<u8> {
        let mut q = vec![0x12, 0x34, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0];
        for l in name {
            q.push(l.len() as u8);
            q.extend_from_slice(l.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&qtype.to_be_bytes());
        q.extend_from_slice(&qclass.to_be_bytes());
        q
    }

    // VERIFIES: REQ-NET-07
    #[test]
    fn mdns_answers_only_for_its_own_name() {
        let mut out = [0u8; 128];
        let n = mdns_answer(&query(&["picoclock-d6", "local"], 1, 1), "picoclock-d6", [192, 168, 121, 41], &mut out).unwrap();
        assert_eq!(&out[..2], &[0x12, 0x34], "the id is echoed");
        assert_eq!(out[2] & 0x80, 0x80, "a response");
        assert_eq!(&out[n - 4..n], &[192, 168, 121, 41]);
        assert_eq!(&out[n - 14..n - 4], &[0, 1, 0x80, 1, 0, 0, 0, 120, 0, 4]);
        // case-insensitive, ANY accepted, the unicast-response bit in the class is ignored
        assert!(mdns_answer(&query(&["PicoClock-D6", "LOCAL"], 255, 0x8001), "picoclock-d6", [1, 2, 3, 4], &mut out).is_some());
        for (name, t, c) in [
            (vec!["other", "local"], 1u16, 1u16),
            (vec!["picoclock-d6", "com"], 1, 1),
            (vec!["picoclock-d6"], 1, 1),
            (vec!["x", "picoclock-d6", "local"], 1, 1),
            (vec!["picoclock-d6", "local"], 28, 1), // AAAA: we have no IPv6 address to give
            (vec!["picoclock-d6", "local"], 1, 3),
        ] {
            assert_eq!(mdns_answer(&query(&name, t, c), "picoclock-d6", [1, 2, 3, 4], &mut out), None, "{name:?} {t} {c}");
        }
        // a response is not answered; junk and truncations never panic
        let mut resp = query(&["picoclock-d6", "local"], 1, 1);
        resp[2] = 0x84;
        assert_eq!(mdns_answer(&resp, "picoclock-d6", [1, 2, 3, 4], &mut out), None);
        let q = query(&["picoclock-d6", "local"], 1, 1);
        for cut in 0..q.len() {
            assert_eq!(mdns_answer(&q[..cut], "picoclock-d6", [1, 2, 3, 4], &mut out), None, "cut {cut}");
        }
        assert_eq!(mdns_answer(&q, "picoclock-d6", [1, 2, 3, 4], &mut [0u8; 10]), None, "output too small");
        let mut pointer = q.clone();
        pointer[12] = 0xC0;
        assert_eq!(mdns_answer(&pointer, "picoclock-d6", [1, 2, 3, 4], &mut out), None, "a compression pointer in the name");
    }
}
