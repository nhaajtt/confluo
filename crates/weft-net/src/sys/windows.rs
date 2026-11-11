use std::ffi::c_void;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::windows::io::AsRawSocket;
use std::sync::{Arc, OnceLock};

use socket2::Socket;
use tokio::sync::Notify;
use windows_sys::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_SUCCESS, HANDLE};
use windows_sys::Win32::NetworkManagement::IpHelper::{
    GAA_FLAG_INCLUDE_GATEWAYS, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_MULTICAST, GetAdaptersAddresses,
    IP_ADAPTER_ADDRESSES_LH, MIB_NOTIFICATION_TYPE, MIB_UNICASTIPADDRESS_ROW, NotifyUnicastIpAddressChange,
};
use windows_sys::Win32::NetworkManagement::Ndis::IfOperStatusUp;
use windows_sys::Win32::Networking::WinSock::{
    AF_INET, AF_INET6, AF_UNSPEC, IP_UNICAST_IF, IPPROTO_IP, IPPROTO_IPV6, IPV6_UNICAST_IF, SOCKADDR,
    SOCKADDR_IN, SOCKADDR_IN6, SOCKET_ERROR, setsockopt,
};

use crate::{BindSpec, Interface, Medium};

pub(crate) fn bind_to_interface(socket: &Socket, bind: &BindSpec, v4: bool) -> io::Result<()> {
    // IPv4 wants the index in network byte order, IPv6 in host byte order.
    let (level, name, value) = if v4 {
        (IPPROTO_IP, IP_UNICAST_IF, bind.index.to_be())
    } else {
        (IPPROTO_IPV6, IPV6_UNICAST_IF, bind.index)
    };
    // SAFETY: `value` outlives the call and its size is passed correctly.
    let rc = unsafe {
        setsockopt(
            socket.as_raw_socket() as usize,
            level,
            name,
            (&raw const value).cast(),
            size_of::<u32>() as i32,
        )
    };
    if rc == SOCKET_ERROR {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// # Safety
/// `sa` must be null or point to a socket address the OS filled in.
unsafe fn sockaddr_ip(sa: *const SOCKADDR) -> Option<IpAddr> {
    if sa.is_null() {
        return None;
    }
    // SAFETY: guaranteed by the caller; the family selects the layout.
    unsafe {
        match (*sa).sa_family {
            AF_INET => {
                let a = &*sa.cast::<SOCKADDR_IN>();
                Some(IpAddr::V4(Ipv4Addr::from(u32::from_be(a.sin_addr.S_un.S_addr))))
            }
            AF_INET6 => {
                let a = &*sa.cast::<SOCKADDR_IN6>();
                Some(IpAddr::V6(Ipv6Addr::from(a.sin6_addr.u.Byte)))
            }
            _ => None,
        }
    }
}

/// # Safety
/// `p` must be null or a NUL-terminated UTF-16 string.
unsafe fn wide(p: *const u16) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: guaranteed by the caller.
    unsafe {
        let mut len = 0;
        while *p.add(len) != 0 {
            len += 1;
        }
        String::from_utf16_lossy(std::slice::from_raw_parts(p, len))
    }
}

pub(crate) fn list_interfaces() -> io::Result<Vec<Interface>> {
    let flags = GAA_FLAG_INCLUDE_GATEWAYS | GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST;
    let mut size: u32 = 16 * 1024;
    let mut buf: Vec<u64> = Vec::new();
    let mut rc = ERROR_BUFFER_OVERFLOW;
    for _ in 0..4 {
        buf.resize((size as usize).div_ceil(8), 0);
        // SAFETY: the buffer is at least `size` bytes and 8-byte aligned as the struct requires.
        rc = unsafe {
            GetAdaptersAddresses(
                u32::from(AF_UNSPEC),
                flags,
                std::ptr::null(),
                buf.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>(),
                &raw mut size,
            )
        };
        if rc != ERROR_BUFFER_OVERFLOW {
            break;
        }
    }
    if rc != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(rc as i32));
    }
    let mut out = Vec::new();
    let mut cur = buf.as_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
    // SAFETY: the list is a null-terminated chain inside `buf`, valid while `buf` lives.
    unsafe {
        while !cur.is_null() {
            let a = &*cur;
            let mut addrs = Vec::new();
            let mut u = a.FirstUnicastAddress;
            while !u.is_null() {
                addrs.extend(sockaddr_ip((*u).Address.lpSockaddr));
                u = (*u).Next;
            }
            let mut dns = Vec::new();
            let mut d = a.FirstDnsServerAddress;
            while !d.is_null() {
                dns.extend(sockaddr_ip((*d).Address.lpSockaddr));
                d = (*d).Next;
            }
            let mut gateways = Vec::new();
            let mut g = a.FirstGatewayAddress;
            while !g.is_null() {
                gateways.extend(sockaddr_ip((*g).Address.lpSockaddr));
                g = (*g).Next;
            }
            let v4_index = a.Anonymous1.Anonymous.IfIndex;
            let index = if v4_index != 0 { v4_index } else { a.Ipv6IfIndex };
            let name = if a.AdapterName.is_null() {
                String::new()
            } else {
                std::ffi::CStr::from_ptr(a.AdapterName.cast()).to_string_lossy().into_owned()
            };
            let medium = match a.IfType {
                24 => Medium::Loopback,
                6 => Medium::Ethernet,
                71 => Medium::Wifi,
                243 | 244 => Medium::Cellular,
                131 | 23 | 53 => Medium::Tunnel,
                _ => Medium::Other,
            };
            let speed = a.ReceiveLinkSpeed;
            out.push(Interface {
                index,
                name,
                label: wide(a.FriendlyName),
                description: wide(a.Description),
                medium,
                up: a.OperStatus == IfOperStatusUp,
                has_gateway: !gateways.is_empty(),
                gateways,
                link_bps: if speed == u64::MAX { 0 } else { speed },
                addrs,
                dns,
            });
            cur = a.Next;
        }
    }
    Ok(out)
}

static SIGNAL: OnceLock<Option<Arc<Notify>>> = OnceLock::new();

unsafe extern "system" fn on_change(
    _ctx: *const c_void,
    _row: *const MIB_UNICASTIPADDRESS_ROW,
    _kind: MIB_NOTIFICATION_TYPE,
) {
    if let Some(Some(n)) = SIGNAL.get() {
        n.notify_one();
    }
}

/// A process-wide signal fired whenever a unicast address is added or removed.
pub(crate) fn change_signal() -> Option<Arc<Notify>> {
    SIGNAL
        .get_or_init(|| {
            let mut handle: HANDLE = std::ptr::null_mut();
            // SAFETY: the callback is a static fn; the registration lives for the process.
            let rc = unsafe {
                NotifyUnicastIpAddressChange(
                    AF_UNSPEC,
                    Some(on_change),
                    std::ptr::null(),
                    false,
                    &raw mut handle,
                )
            };
            (rc == ERROR_SUCCESS).then(|| Arc::new(Notify::new()))
        })
        .clone()
}
