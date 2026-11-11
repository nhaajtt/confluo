use std::collections::BTreeMap;
use std::ffi::CStr;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;

use socket2::Socket;
use tokio::sync::Notify;

use crate::{BindSpec, Interface, Medium};

pub(crate) fn bind_to_interface(socket: &Socket, bind: &BindSpec, v4: bool) -> io::Result<()> {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    {
        let _ = v4;
        socket.bind_device(Some(bind.name.as_bytes()))
    }
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        let index = std::num::NonZeroU32::new(bind.index);
        if v4 { socket.bind_device_by_index_v4(index) } else { socket.bind_device_by_index_v6(index) }
    }
    #[cfg(not(any(target_os = "linux", target_os = "android", target_os = "macos", target_os = "ios")))]
    {
        // Source-address binding (done by the caller) is the best available here.
        let _ = (socket, bind, v4);
        Ok(())
    }
}

/// # Safety
/// `sa` must be null or point to a socket address filled in by the OS.
unsafe fn sockaddr_ip(sa: *const libc::sockaddr) -> Option<IpAddr> {
    if sa.is_null() {
        return None;
    }
    // SAFETY: guaranteed by the caller; the family selects the layout.
    unsafe {
        match i32::from((*sa).sa_family) {
            libc::AF_INET => {
                let a = &*sa.cast::<libc::sockaddr_in>();
                Some(IpAddr::V4(Ipv4Addr::from(u32::from_be(a.sin_addr.s_addr))))
            }
            libc::AF_INET6 => {
                let a = &*sa.cast::<libc::sockaddr_in6>();
                Some(IpAddr::V6(Ipv6Addr::from(a.sin6_addr.s6_addr)))
            }
            _ => None,
        }
    }
}

fn default_route_ifaces() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(text) = std::fs::read_to_string("/proc/net/route") {
        for line in text.lines().skip(1) {
            let mut f = line.split_whitespace();
            if let (Some(name), Some("00000000")) = (f.next(), f.next()) {
                out.push(name.to_string());
            }
        }
    }
    out
}

fn system_dns() -> Vec<IpAddr> {
    std::fs::read_to_string("/etc/resolv.conf")
        .map(|t| {
            t.lines()
                .filter_map(|l| l.strip_prefix("nameserver"))
                .filter_map(|v| v.trim().parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

fn medium_of(name: &str, flags: u32) -> Medium {
    let starts = |prefixes: &[&str]| prefixes.iter().any(|p| name.starts_with(p));
    if flags & libc::IFF_LOOPBACK as u32 != 0 {
        Medium::Loopback
    } else if std::path::Path::new(&format!("/sys/class/net/{name}/wireless")).exists() || starts(&["wl"]) {
        Medium::Wifi
    } else if starts(&["ww", "rmnet", "pdp_ip"]) {
        Medium::Cellular
    } else if starts(&["tun", "wg", "utun", "ppp"]) {
        Medium::Tunnel
    } else if starts(&["en", "eth"]) {
        Medium::Ethernet
    } else {
        Medium::Other
    }
}

pub(crate) fn list_interfaces() -> io::Result<Vec<Interface>> {
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `head`; it is freed below.
    if unsafe { libc::getifaddrs(&raw mut head) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let gateways = default_route_ifaces();
    let dns = system_dns();
    let mut map: BTreeMap<String, Interface> = BTreeMap::new();
    let mut cur = head;
    // SAFETY: walking the list getifaddrs returned, before freeifaddrs.
    unsafe {
        while !cur.is_null() {
            let ifa = &*cur;
            let name = CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned();
            let flags = ifa.ifa_flags;
            let entry = map.entry(name.clone()).or_insert_with(|| {
                let speed = std::fs::read_to_string(format!("/sys/class/net/{name}/speed"))
                    .ok()
                    .and_then(|s| s.trim().parse::<i64>().ok())
                    .filter(|v| *v > 0)
                    .map_or(0, |mbps| mbps as u64 * 1_000_000);
                Interface {
                    index: libc::if_nametoindex(ifa.ifa_name),
                    label: name.clone(),
                    description: String::new(),
                    medium: medium_of(&name, flags),
                    up: flags & libc::IFF_UP as u32 != 0 && flags & libc::IFF_RUNNING as u32 != 0,
                    has_gateway: gateways.contains(&name),
                    link_bps: speed,
                    addrs: Vec::new(),
                    dns: dns.clone(),
                    name,
                }
            });
            entry.addrs.extend(sockaddr_ip(ifa.ifa_addr));
            cur = ifa.ifa_next;
        }
        libc::freeifaddrs(head);
    }
    Ok(map.into_values().collect())
}

/// No push notification is wired on Unix; the watcher polls instead.
pub(crate) fn change_signal() -> Option<Arc<Notify>> {
    None
}
