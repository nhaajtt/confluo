//! Network interface discovery and interface-bound TCP connections.
//!
//! Binding a socket to a *source address* is not enough to pick the outgoing
//! interface: on Windows and macOS the routing table still decides, so with
//! several default routes every connection can leave through the same adapter.
//! This crate pins the socket to the interface itself:
//!
//! * Windows: `IP_UNICAST_IF` / `IPV6_UNICAST_IF`
//! * Linux:   `SO_BINDTODEVICE`
//! * macOS:   `IP_BOUND_IF` / `IPV6_BOUND_IF`

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use socket2::{Domain, Protocol, Socket, TcpKeepalive, Type};
use tokio::net::{TcpSocket, TcpStream};

mod sys;
mod watch;

pub use watch::watch_changes;

/// Physical medium of an interface, as far as the OS reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Medium {
    Ethernet,
    Wifi,
    Cellular,
    Tunnel,
    Loopback,
    Other,
}

/// A network interface as seen by the operating system.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interface {
    /// OS interface index (stable while the adapter exists).
    pub index: u32,
    /// OS name: adapter GUID on Windows, `eth0`-style name elsewhere.
    pub name: String,
    /// Human label ("Ethernet 5", "Wi-Fi").
    pub label: String,
    /// Driver / hardware description when available.
    pub description: String,
    pub medium: Medium,
    pub up: bool,
    /// Whether a default route goes through this interface.
    pub has_gateway: bool,
    /// Default gateways reached through this interface.
    pub gateways: Vec<IpAddr>,
    /// Link speed in bits per second, 0 if unknown.
    pub link_bps: u64,
    pub addrs: Vec<IpAddr>,
    /// DNS servers configured for this interface (may be empty).
    pub dns: Vec<IpAddr>,
}

impl Interface {
    /// First routable IPv4 address.
    pub fn v4(&self) -> Option<Ipv4Addr> {
        self.addrs.iter().find_map(|a| match a {
            IpAddr::V4(v) if !v.is_link_local() && !v.is_loopback() => Some(*v),
            _ => None,
        })
    }

    /// First routable (non link-local) IPv6 address.
    pub fn v6(&self) -> Option<Ipv6Addr> {
        self.addrs.iter().find_map(|a| match a {
            IpAddr::V6(v) if !v.is_loopback() && (v.segments()[0] & 0xffc0) != 0xfe80 => Some(*v),
            _ => None,
        })
    }

    /// True when the interface can carry traffic to somewhere else.
    pub fn usable(&self) -> bool {
        self.up && self.medium != Medium::Loopback && (self.v4().is_some() || self.v6().is_some())
    }

    pub fn bind_spec(&self) -> BindSpec {
        BindSpec { index: self.index, name: self.name.clone(), v4: self.v4(), v6: self.v6() }
    }
}

/// What a socket needs to be pinned to an interface.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BindSpec {
    pub index: u32,
    pub name: String,
    pub v4: Option<Ipv4Addr>,
    pub v6: Option<Ipv6Addr>,
}

impl BindSpec {
    /// Whether this interface can reach a remote address of that family.
    pub fn supports(&self, remote: &SocketAddr) -> bool {
        match remote {
            SocketAddr::V4(_) => self.v4.is_some(),
            SocketAddr::V6(_) => self.v6.is_some(),
        }
    }
}

/// Lists every interface the OS knows about, including down and loopback ones.
pub fn list_interfaces() -> io::Result<Vec<Interface>> {
    sys::list_interfaces()
}

/// Lists interfaces that are up and have a routable address.
pub fn usable_interfaces() -> io::Result<Vec<Interface>> {
    Ok(list_interfaces()?.into_iter().filter(Interface::usable).collect())
}

/// Creates an unconnected, non-blocking TCP socket pinned to `bind` (if any).
pub fn bound_socket(bind: Option<&BindSpec>, remote: &SocketAddr) -> io::Result<Socket> {
    let domain = if remote.is_ipv4() { Domain::IPV4 } else { Domain::IPV6 };
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))?;
    socket.set_nonblocking(true)?;
    socket.set_tcp_nodelay(true)?;
    let keepalive =
        TcpKeepalive::new().with_time(Duration::from_secs(30)).with_interval(Duration::from_secs(10));
    socket.set_tcp_keepalive(&keepalive)?;
    if let Some(bind) = bind {
        if !bind.supports(remote) {
            return Err(io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                "interface has no address of the remote's family",
            ));
        }
        sys::bind_to_interface(&socket, bind, remote.is_ipv4())?;
        let local: SocketAddr = match remote {
            SocketAddr::V4(_) => SocketAddr::new(IpAddr::V4(bind.v4.unwrap_or(Ipv4Addr::UNSPECIFIED)), 0),
            SocketAddr::V6(_) => SocketAddr::new(IpAddr::V6(bind.v6.unwrap_or(Ipv6Addr::UNSPECIFIED)), 0),
        };
        socket.bind(&local.into())?;
    }
    Ok(socket)
}

/// Opens a TCP connection that leaves through `bind` (or the OS default route).
pub async fn connect(
    bind: Option<&BindSpec>,
    remote: SocketAddr,
    timeout: Duration,
) -> io::Result<TcpStream> {
    let socket = bound_socket(bind, &remote)?;
    let socket = TcpSocket::from_std_stream(socket.into());
    match tokio::time::timeout(timeout, socket.connect(remote)).await {
        Ok(r) => r,
        Err(_) => Err(io::Error::new(io::ErrorKind::TimedOut, "connect timed out")),
    }
}

/// Connects to the first reachable address, racing families (RFC 8305 style):
/// each next attempt starts after a short stagger unless the previous one already failed.
pub async fn connect_any(
    bind: Option<&BindSpec>,
    addrs: &[SocketAddr],
    timeout: Duration,
) -> io::Result<(TcpStream, SocketAddr)> {
    const STAGGER: Duration = Duration::from_millis(250);
    let candidates: Vec<SocketAddr> =
        interleave_families(addrs).into_iter().filter(|a| bind.is_none_or(|b| b.supports(a))).collect();
    if candidates.is_empty() {
        return Err(io::Error::new(io::ErrorKind::AddrNotAvailable, "no address usable on this interface"));
    }
    let mut set = tokio::task::JoinSet::new();
    let mut last_err = None;
    let mut next = 0usize;
    loop {
        if next < candidates.len() {
            let addr = candidates[next];
            let bind = bind.cloned();
            set.spawn(async move { (connect(bind.as_ref(), addr, timeout).await, addr) });
            next += 1;
        }
        if set.is_empty() {
            break;
        }
        let wait = if next < candidates.len() { STAGGER } else { timeout + Duration::from_secs(1) };
        match tokio::time::timeout(wait, set.join_next()).await {
            Ok(Some(Ok((Ok(stream), addr)))) => return Ok((stream, addr)),
            Ok(Some(Ok((Err(e), _)))) => last_err = Some(e),
            Ok(Some(Err(e))) => last_err = Some(io::Error::other(e)),
            Ok(None) => break,
            Err(_) => {} // stagger elapsed: start the next candidate
        }
    }
    Err(last_err.unwrap_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "connect timed out")))
}

fn interleave_families(addrs: &[SocketAddr]) -> Vec<SocketAddr> {
    let first_v6 = addrs.first().is_some_and(SocketAddr::is_ipv6);
    let (first, second): (Vec<SocketAddr>, Vec<SocketAddr>) =
        addrs.iter().partition(|a| a.is_ipv6() == first_v6);
    let mut out = Vec::with_capacity(addrs.len());
    let (mut a, mut b) = (first.into_iter(), second.into_iter());
    loop {
        match (a.next(), b.next()) {
            (None, None) => break,
            (x, y) => {
                out.extend(x);
                out.extend(y);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_at_least_loopback() {
        let all = list_interfaces().unwrap();
        assert!(all.iter().any(|i| i.medium == Medium::Loopback), "{all:#?}");
    }

    #[test]
    fn interleaves() {
        let a: Vec<SocketAddr> =
            ["1.1.1.1:1", "2.2.2.2:1", "[::1]:1"].iter().map(|s| s.parse().unwrap()).collect();
        let out = interleave_families(&a);
        assert!(out[0].is_ipv4() && out[1].is_ipv6() && out[2].is_ipv4());
    }

    /// A socket pinned to an interface must carry that interface's address as
    /// its source when it reaches a listener on the same interface.
    #[tokio::test]
    async fn bound_connect_uses_interface_address() {
        let ifaces = usable_interfaces().unwrap();
        for iface in ifaces.iter().filter(|i| i.v4().is_some()) {
            let ip = iface.v4().unwrap();
            let Ok(listener) = tokio::net::TcpListener::bind((ip, 0)).await else { continue };
            let addr = listener.local_addr().unwrap();
            let spec = iface.bind_spec();
            let (client, server) =
                tokio::join!(connect(Some(&spec), addr, Duration::from_secs(3)), listener.accept());
            let client = client.unwrap_or_else(|e| panic!("{}: {e}", iface.label));
            let (_, peer) = server.unwrap();
            assert_eq!(peer.ip(), IpAddr::V4(ip));
            assert_eq!(client.local_addr().unwrap().ip(), IpAddr::V4(ip));
        }
    }
}
