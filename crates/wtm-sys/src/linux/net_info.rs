//! IP addresses, DNS suffix and MAC of each interface (getifaddrs,
//! systemd-resolved link state, sysfs), and Wi-Fi link details
//! (/proc/net/wireless, `iw`). Keyed by interface index, the "LUID" that
//! [`crate::system::net_adapters`] reports.

use std::collections::BTreeMap;
use std::ffi::CStr;
use std::fs;
use std::net::{Ipv4Addr, Ipv6Addr};

use crate::linux::util::{read_trim, read_u64};

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Addresses {
    pub luid: u64,
    pub ipv4: Vec<Ipv4Addr>,
    pub ipv6: Vec<Ipv6Addr>,
    pub dns_suffix: String,
    #[serde(default)]
    pub mac: String,
}

pub fn mac_text(bytes: &[u8]) -> String {
    if bytes.iter().all(|b| *b == 0) {
        return String::new();
    }
    bytes
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join("-")
}

const MAX_ITEMS: usize = 64;

fn dns_suffix(index: u64) -> String {
    let link =
        fs::read_to_string(format!("/run/systemd/resolve/netif/{index}")).unwrap_or_default();
    if let Some(d) = link.lines().find_map(|l| l.strip_prefix("DOMAINS="))
        && let Some(first) = d.split_whitespace().next()
    {
        return first.trim_start_matches('~').to_owned();
    }
    String::new()
}

fn mac_of(name: &str) -> String {
    let raw = read_trim(format!("/sys/class/net/{name}/address")).unwrap_or_default();
    let bytes: Vec<u8> = raw
        .split(':')
        .filter_map(|b| u8::from_str_radix(b, 16).ok())
        .collect();
    mac_text(&bytes)
}

pub fn addresses() -> crate::Result<Vec<Addresses>> {
    let mut by_name: BTreeMap<String, Addresses> = BTreeMap::new();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: out-pointer to a list the C library allocates.
    if unsafe { libc::getifaddrs(&mut head) } != 0 {
        return Err(crate::linux::util::last_os("getifaddrs"));
    }
    let mut cur = head;
    while !cur.is_null() {
        // SAFETY: a node of the list getifaddrs returned, alive until freed.
        let ifa = unsafe { &*cur };
        cur = ifa.ifa_next;
        if ifa.ifa_addr.is_null() || ifa.ifa_name.is_null() {
            continue;
        }
        // SAFETY: NUL-terminated name owned by the list.
        let name = unsafe { CStr::from_ptr(ifa.ifa_name) }
            .to_string_lossy()
            .into_owned();
        if name == "lo" {
            continue;
        }
        if by_name.len() >= MAX_ITEMS && !by_name.contains_key(&name) {
            continue;
        }
        let entry = by_name.entry(name.clone()).or_insert_with(|| {
            let index = read_u64(format!("/sys/class/net/{name}/ifindex")).unwrap_or(0);
            Addresses {
                luid: index,
                dns_suffix: dns_suffix(index),
                mac: mac_of(&name),
                ..Default::default()
            }
        });
        // SAFETY: sa_family is valid for any sockaddr.
        let family = i32::from(unsafe { (*ifa.ifa_addr).sa_family });
        if family == libc::AF_INET && entry.ipv4.len() < MAX_ITEMS {
            // SAFETY: an AF_INET address is a sockaddr_in.
            let sin = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in) };
            entry
                .ipv4
                .push(Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr)));
        } else if family == libc::AF_INET6 && entry.ipv6.len() < MAX_ITEMS {
            // SAFETY: an AF_INET6 address is a sockaddr_in6.
            let sin6 = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in6) };
            entry.ipv6.push(Ipv6Addr::from(sin6.sin6_addr.s6_addr));
        }
    }
    // SAFETY: frees the list getifaddrs allocated, once.
    unsafe { libc::freeifaddrs(head) };
    Ok(by_name.into_values().collect())
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WifiLink {
    pub luid: u64,
    pub ssid: String,
    pub signal: u32,
    pub standard: Option<&'static str>,
}

/// The radio standard from `iw dev X link` bitrate flags.
fn standard(iw: &str) -> Option<&'static str> {
    let rate = iw
        .lines()
        .find(|l| l.contains("tx bitrate") || l.contains("rx bitrate"))?;
    Some(if rate.contains("EHT") {
        "802.11be"
    } else if rate.contains("HE-MCS") || rate.contains(" HE ") {
        "802.11ax"
    } else if rate.contains("VHT") {
        "802.11ac"
    } else if rate.contains("MCS") {
        "802.11n"
    } else {
        return None;
    })
}

pub fn wifi_links() -> Vec<WifiLink> {
    let Ok(text) = fs::read_to_string("/proc/net/wireless") else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in text.lines().skip(2) {
        let Some((name, rest)) = line.split_once(':') else {
            continue;
        };
        let name = name.trim();
        let quality: f64 = rest
            .split_whitespace()
            .nth(1)
            .and_then(|q| q.trim_end_matches('.').parse().ok())
            .unwrap_or(0.0);
        let iw = crate::linux::util::run("iw", &["dev", name, "link"]).unwrap_or_default();
        if iw.contains("Not connected") {
            continue;
        }
        let ssid = iw
            .lines()
            .find_map(|l| l.trim().strip_prefix("SSID: ").map(str::to_owned))
            .unwrap_or_default();
        out.push(WifiLink {
            luid: read_u64(format!("/sys/class/net/{name}/ifindex")).unwrap_or(0),
            ssid,
            // Link quality is out of 70 on most drivers.
            signal: ((quality / 70.0) * 100.0).round().clamp(0.0, 100.0) as u32,
            standard: standard(&iw),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_reads() {
        assert_eq!(
            mac_text(&[0, 0x1a, 0x2b, 0x3c, 0x4d, 0x5e]),
            "00-1A-2B-3C-4D-5E"
        );
        assert_eq!(mac_text(&[0; 6]), "");
        addresses().unwrap();
        assert_eq!(
            standard("\ttx bitrate: 1200.9 MBit/s 80MHz HE-MCS 11 HE-NSS 2"),
            Some("802.11ax")
        );
    }
}
