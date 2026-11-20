//! TCP/UDP endpoints with their owning process, from /proc/net and the
//! socket inodes under /proc/PID/fd (other users' processes need root).

use std::collections::HashMap;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use serde::{Deserialize, Serialize};

use crate::linux::util;
use crate::{Error, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Protocol {
    Tcp,
    Udp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TcpState {
    Closed,
    Listen,
    SynSent,
    SynReceived,
    Established,
    FinWait1,
    FinWait2,
    CloseWait,
    Closing,
    LastAck,
    TimeWait,
    DeleteTcb,
    Unknown(u32),
}

impl TcpState {
    /// From the kernel's `TCP_*` state number.
    fn from_linux(raw: u32) -> Self {
        match raw {
            1 => Self::Established,
            2 => Self::SynSent,
            3 => Self::SynReceived,
            4 => Self::FinWait1,
            5 => Self::FinWait2,
            6 => Self::TimeWait,
            7 => Self::Closed,
            8 => Self::CloseWait,
            9 => Self::LastAck,
            10 => Self::Listen,
            11 => Self::Closing,
            other => Self::Unknown(other),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Connection {
    pub protocol: Protocol,
    pub local: SocketAddr,
    pub remote: Option<SocketAddr>,
    pub state: Option<TcpState>,
    pub pid: u32,
}

/// Socket inode -> owning PID.
pub(crate) fn socket_owners() -> HashMap<u64, u32> {
    let mut owners = HashMap::new();
    for pid in util::numeric_entries("/proc") {
        let Ok(rd) = fs::read_dir(format!("/proc/{pid}/fd")) else {
            continue;
        };
        for e in rd.flatten() {
            if let Ok(target) = fs::read_link(e.path()) {
                let t = target.to_string_lossy();
                if let Some(inode) = t.strip_prefix("socket:[").and_then(|r| r.strip_suffix(']'))
                    && let Ok(inode) = inode.parse()
                {
                    owners.entry(inode).or_insert(pid);
                }
            }
        }
    }
    owners
}

/// `0100007F:1F90` (IPv4) or 32 hex digits (IPv6, four host-order words).
pub(crate) fn parse_addr(text: &str) -> Option<SocketAddr> {
    let (ip, port) = text.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let ip = match ip.len() {
        8 => IpAddr::V4(Ipv4Addr::from(
            u32::from_str_radix(ip, 16).ok()?.swap_bytes(),
        )),
        32 => {
            let mut bytes = [0u8; 16];
            for i in 0..4 {
                let word = u32::from_str_radix(&ip[i * 8..i * 8 + 8], 16).ok()?;
                bytes[i * 4..i * 4 + 4].copy_from_slice(&word.to_le_bytes());
            }
            IpAddr::V6(Ipv6Addr::from(bytes))
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

fn table(path: &str, protocol: Protocol, owners: &HashMap<u64, u32>, out: &mut Vec<Connection>) {
    let Ok(text) = fs::read_to_string(path) else {
        return;
    };
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 10 {
            continue;
        }
        let (Some(local), Some(remote)) = (parse_addr(f[1]), parse_addr(f[2])) else {
            continue;
        };
        let state = u32::from_str_radix(f[3], 16).unwrap_or(0);
        let inode: u64 = f[9].parse().unwrap_or(0);
        let pid = owners.get(&inode).copied().unwrap_or(0);
        let (remote, state) = match protocol {
            Protocol::Tcp => {
                let s = TcpState::from_linux(state);
                (
                    if s == TcpState::Listen {
                        None
                    } else {
                        Some(remote)
                    },
                    Some(s),
                )
            }
            Protocol::Udp => ((state == 1 && remote.port() != 0).then_some(remote), None),
        };
        out.push(Connection {
            protocol,
            local,
            remote,
            state,
            pid,
        });
    }
}

pub fn connections() -> Result<Vec<Connection>> {
    let owners = socket_owners();
    let mut out = Vec::new();
    table("/proc/net/tcp", Protocol::Tcp, &owners, &mut out);
    table("/proc/net/tcp6", Protocol::Tcp, &owners, &mut out);
    table("/proc/net/udp", Protocol::Udp, &owners, &mut out);
    table("/proc/net/udp6", Protocol::Udp, &owners, &mut out);
    if out.is_empty() && fs::metadata("/proc/net/tcp").is_err() {
        return Err(Error::Unsupported("/proc/net"));
    }
    Ok(out)
}

/// Resets a TCP connection (IPv4 or IPv6) with the kernel's sock_diag
/// destroy, through `ss -K`. Needs root.
pub fn close_tcp(local: SocketAddr, remote: SocketAddr) -> Result<()> {
    let host = |a: &SocketAddr| match a.ip() {
        IpAddr::V4(v) => v.to_string(),
        IpAddr::V6(v) => format!("[{v}]"),
    };
    let src = host(&local);
    let dst = host(&remote);
    let sport = format!(":{}", local.port());
    let dport = format!(":{}", remote.port());
    let out = util::run_checked(
        "ss",
        &[
            "-K", "-t", "src", &src, "sport", "=", &sport, "dst", &dst, "dport", "=", &dport,
        ],
    )?;
    // ss prints the sockets it destroyed after the header line.
    if out.lines().count() <= 1 {
        return Err(Error::Message(
            "no matching connection was closed (needs root)".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_addresses() {
        assert_eq!(
            parse_addr("0100007F:1F90").unwrap(),
            "127.0.0.1:8080".parse().unwrap()
        );
        assert_eq!(
            parse_addr("00000000000000000000000001000000:0050").unwrap(),
            "[::1]:80".parse().unwrap()
        );
    }

    #[test]
    fn finds_own_listener() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let all = connections().unwrap();
        let mine = all
            .iter()
            .find(|c| c.local.port() == port && c.protocol == Protocol::Tcp)
            .unwrap();
        assert_eq!(mine.state, Some(TcpState::Listen));
        assert_eq!(mine.pid, std::process::id());
    }
}
