//! Network bytes per process from the kernel's TCP counters: a sock_diag
//! (NETLINK_SOCK_DIAG) dump with `tcp_info` gives every TCP socket's bytes
//! received and acknowledged as sent; socket inodes map to processes
//! through /proc/PID/fd. Each [`NetTracer::take`] returns the growth since
//! the previous one. UDP (and so QUIC) has no per-socket byte counters and
//! is not counted. Needs root to see other users' sockets.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::linux::util::last_os;

pub type Traffic = HashMap<u32, (u64, u64)>;

const NETLINK_SOCK_DIAG: i32 = 4;
const SOCK_DIAG_BY_FAMILY: u16 = 20;
const NLM_F_REQUEST: u16 = 1;
const NLM_F_DUMP: u16 = 0x300;
const NLMSG_DONE: u16 = 3;
const NLMSG_ERROR: u16 = 2;
const INET_DIAG_INFO: u16 = 2;
/// Offsets in `struct tcp_info` (Linux 4.1+).
const BYTES_ACKED_AT: usize = 120;
const BYTES_RECEIVED_AT: usize = 128;

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct NlMsgHdr {
    len: u32,
    kind: u16,
    flags: u16,
    seq: u32,
    pid: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct InetDiagSockId {
    sport: u16,
    dport: u16,
    src: [u32; 4],
    dst: [u32; 4],
    interface: u32,
    cookie: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct InetDiagReqV2 {
    family: u8,
    protocol: u8,
    ext: u8,
    pad: u8,
    states: u32,
    id: InetDiagSockId,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Request {
    header: NlMsgHdr,
    body: InetDiagReqV2,
}

/// Socket cookie -> (inode, received, sent) for every TCP socket.
pub(crate) fn tcp_sockets() -> crate::Result<HashMap<u64, (u64, u64, u64)>> {
    // SAFETY: plain socket creation.
    let fd = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
            NETLINK_SOCK_DIAG,
        )
    };
    if fd < 0 {
        return Err(last_os("socket(NETLINK_SOCK_DIAG)"));
    }
    // SAFETY: fresh fd owned here.
    let sock = unsafe { <std::os::fd::OwnedFd as std::os::fd::FromRawFd>::from_raw_fd(fd) };
    let raw = std::os::fd::AsRawFd::as_raw_fd(&sock);
    let mut out = HashMap::new();
    for family in [libc::AF_INET as u8, libc::AF_INET6 as u8] {
        let req = Request {
            header: NlMsgHdr {
                len: size_of::<Request>() as u32,
                kind: SOCK_DIAG_BY_FAMILY,
                flags: NLM_F_REQUEST | NLM_F_DUMP,
                seq: u32::from(family),
                pid: 0,
            },
            body: InetDiagReqV2 {
                family,
                protocol: libc::IPPROTO_TCP as u8,
                ext: 1 << (INET_DIAG_INFO - 1),
                pad: 0,
                states: u32::MAX,
                id: InetDiagSockId::default(),
            },
        };
        // SAFETY: sends the bytes of a plain repr(C) struct.
        let sent = unsafe { libc::send(raw, (&raw const req).cast(), size_of::<Request>(), 0) };
        if sent < 0 {
            return Err(last_os("send(sock_diag)"));
        }
        let mut buf = vec![0u8; 64 * 1024];
        'recv: loop {
            // SAFETY: buf is writable for its length.
            let n = unsafe { libc::recv(raw, buf.as_mut_ptr().cast(), buf.len(), 0) };
            if n < 0 {
                return Err(last_os("recv(sock_diag)"));
            }
            let data = &buf[..n as usize];
            let mut at = 0usize;
            while at + 16 <= data.len() {
                let len =
                    u32::from_ne_bytes(data[at..at + 4].try_into().unwrap_or_default()) as usize;
                let kind = u16::from_ne_bytes(data[at + 4..at + 6].try_into().unwrap_or_default());
                if len < 16 || at + len > data.len() {
                    break 'recv;
                }
                if kind == NLMSG_DONE || kind == NLMSG_ERROR {
                    break 'recv;
                }
                parse_message(&data[at + 16..at + len], &mut out);
                at += (len + 3) & !3;
            }
        }
    }
    Ok(out)
}

/// One `inet_diag_msg` and its attributes.
fn parse_message(msg: &[u8], out: &mut HashMap<u64, (u64, u64, u64)>) {
    // inet_diag_msg: family,state,timer,retrans (4) + sockid (48) +
    // expires,rqueue,wqueue,uid,inode (20) = 72 bytes.
    if msg.len() < 72 {
        return;
    }
    let cookie_lo = u32::from_ne_bytes(msg[44..48].try_into().unwrap_or_default());
    let cookie_hi = u32::from_ne_bytes(msg[48..52].try_into().unwrap_or_default());
    let cookie = (u64::from(cookie_hi) << 32) | u64::from(cookie_lo);
    let inode = u64::from(u32::from_ne_bytes(
        msg[68..72].try_into().unwrap_or_default(),
    ));
    let mut at = 72usize;
    while at + 4 <= msg.len() {
        let len = usize::from(u16::from_ne_bytes([msg[at], msg[at + 1]]));
        let kind = u16::from_ne_bytes([msg[at + 2], msg[at + 3]]);
        if len < 4 || at + len > msg.len() {
            break;
        }
        if kind == INET_DIAG_INFO {
            let info = &msg[at + 4..at + len];
            if info.len() >= BYTES_RECEIVED_AT + 8 {
                let acked = u64::from_ne_bytes(
                    info[BYTES_ACKED_AT..BYTES_ACKED_AT + 8]
                        .try_into()
                        .unwrap_or_default(),
                );
                let received = u64::from_ne_bytes(
                    info[BYTES_RECEIVED_AT..BYTES_RECEIVED_AT + 8]
                        .try_into()
                        .unwrap_or_default(),
                );
                out.insert(cookie, (inode, received, acked));
            }
        }
        at += (len + 3) & !3;
    }
}

#[derive(Debug, Default)]
struct State {
    /// cookie -> (received, sent) at the previous take.
    last: HashMap<u64, (u64, u64)>,
    primed: bool,
}

#[derive(Debug)]
pub struct NetTracer {
    state: Mutex<State>,
}

impl NetTracer {
    /// `name` is kept for the Windows API's sake (an ETW session name).
    pub fn start(_name: &str) -> crate::Result<Self> {
        let tracer = Self {
            state: Mutex::new(State::default()),
        };
        tracer.take();
        Ok(tracer)
    }

    /// Bytes (received, sent) per process since the previous call.
    pub fn take(&self) -> Traffic {
        let mut out = Traffic::new();
        let Ok(sockets) = tcp_sockets() else {
            return out;
        };
        let owners = crate::net::socket_owners();
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut next = HashMap::with_capacity(sockets.len());
        for (cookie, (inode, rx, tx)) in sockets {
            next.insert(cookie, (rx, tx));
            // Sockets that existed at start were counted from then on;
            // later ones from their first byte.
            let (prx, ptx) = match state.last.get(&cookie) {
                Some(p) => *p,
                None if !state.primed => (rx, tx),
                None => (0, 0),
            };
            let (drx, dtx) = (rx.saturating_sub(prx), tx.saturating_sub(ptx));
            if drx == 0 && dtx == 0 {
                continue;
            }
            let Some(&pid) = owners.get(&inode) else {
                continue;
            };
            let e = out.entry(pid).or_default();
            e.0 += drx;
            e.1 += dtx;
        }
        state.last = next;
        state.primed = true;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    #[test]
    fn counts_own_loopback_traffic() {
        let tracer = NetTracer::start("t").unwrap();
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let mut c = std::net::TcpStream::connect(addr).unwrap();
        let (mut s, _) = l.accept().unwrap();
        c.write_all(&[7u8; 100_000]).unwrap();
        let mut got = vec![0u8; 100_000];
        s.read_exact(&mut got).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(50));
        let t = tracer.take();
        let (rx, tx) = t.get(&std::process::id()).copied().unwrap_or_default();
        assert!(rx >= 100_000 && tx >= 100_000, "rx {rx} tx {tx}");
    }
}
