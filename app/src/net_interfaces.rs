//! Usable network adapters (name + IPv4) for the "bind to this adapter" setting.
//!
//! Same shape as `monitors.rs`: `list()` snapshots what the box has right now and
//! nothing is cached. An empty list means "automatic" — bind everywhere, advertise
//! everywhere, i.e. exactly what the app did before this setting existed.

/// One pickable adapter. Deliberately not a full NIC description: the only two
/// things the UI and the engine need are something to show and something to bind.
#[derive(Clone, Debug)]
pub struct Interface {
    /// `en0`/`eth0` on unix, the friendly name ("Wi-Fi", "Ethernet 2") on Windows.
    /// Label material only — never persisted, and it can contain spaces.
    pub name: String,
    /// First IPv4, dotted quad. This is what we persist and what goes into the
    /// fork's `-bind`: an IP can't contain whitespace, and `build_options()`
    /// space-joins its argv tail (a name would be split into an unknown option
    /// and `exit(1)` the engine inside our own process).
    pub ip: String,
}

/// Snapshot the adapters that can currently carry AirPlay traffic (up, non-loopback,
/// with an IPv4). Empty on enumeration failure — the dropdown then hides.
pub fn list() -> Vec<Interface> { sys::list() }

/// The `-bind` argument for a saved `Config::bind_ip`, or `None` for "automatic".
///
/// Parsed, not just trimmed: `config.json` is hand-editable, so this is the trust
/// boundary. A value carrying a space would be re-split by the C side's `split_args()`
/// into an unknown option → the engine refuses to start (since audit #4 it returns
/// a failure instead of `exit(1)`ing the tray, but it still never comes up: quoting
/// in `split_args` only protects values the host itself quotes); a garbage address would
/// surface as an opaque `EADDRNOTAVAIL` from inside httpd. Both are rejected here,
/// and the engine still re-validates against the live adapter list.
pub fn bind_arg(saved: Option<&str>) -> Option<&str> {
    saved.map(str::trim)
         .filter(|s| s.parse::<std::net::Ipv4Addr>().is_ok())
}

/// Text for the adapter combo box: the matching adapter, or the saved address
/// tagged "(not found)" so a pin that outlived its NIC is visibly stale and
/// re-pickable instead of silently doing nothing.
pub fn label(nics: &[Interface], saved: Option<&str>) -> String {
    match saved {
        None => "Automatic".to_string(),
        Some(ip) => nics.iter().find(|n| n.ip == ip)
            .map(|n| format!("{} ({})", n.name, n.ip))
            .unwrap_or_else(|| format!("{ip} (not found)")),
    }
}

#[cfg(unix)]
mod sys {
    use super::Interface;
    use std::net::Ipv4Addr;

    pub fn list() -> Vec<Interface> {
        let mut out: Vec<Interface> = Vec::new();
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        // Nothing is allocated on failure, so this is the only path that skips
        // freeifaddrs().
        if unsafe { libc::getifaddrs(&mut head) } != 0 { return out; }
        let mut cur = head;
        while !cur.is_null() {
            let ifa = unsafe { &*cur };
            // Advance FIRST: every `continue` below would otherwise spin forever.
            cur = ifa.ifa_next;
            if ifa.ifa_addr.is_null() { continue; }
            // sa_family through libc's named field — it is u8 behind sa_len on the
            // BSDs and u16 on Linux, so a hand-rolled *(p as *const u16) reads
            // 0x0210 on macOS and matches nothing.
            if unsafe { (*ifa.ifa_addr).sa_family } != libc::AF_INET as libc::sa_family_t {
                continue;
            }
            let f = ifa.ifa_flags as i32;
            // IFF_POINTOPOINT drops VPN/utun links, which have no broadcast domain
            // for mDNS to work in. The engine's own resolver is looser (up +
            // non-loopback only), so a hand-written config.json can still pin one:
            // this filter is what we OFFER, not what the engine ENFORCES.
            if f & libc::IFF_UP == 0
                || f & libc::IFF_LOOPBACK != 0
                || f & libc::IFF_POINTOPOINT != 0 { continue; }
            let sin = unsafe { &*(ifa.ifa_addr as *const libc::sockaddr_in) };
            let ip = Ipv4Addr::from(sin.sin_addr.s_addr.to_ne_bytes());
            if ip.is_unspecified() { continue; }
            // 169.254/16 is NOT filtered: link-local + mDNS is the direct-cable
            // case Bonjour exists for.
            let name = unsafe { std::ffi::CStr::from_ptr(ifa.ifa_name) }
                .to_string_lossy().into_owned();
            // getifaddrs yields one node per address, so an aliased NIC repeats;
            // we offer one address per adapter and the first one wins.
            if out.iter().any(|i| i.name == name) { continue; }
            out.push(Interface { name, ip: ip.to_string() });
        }
        unsafe { libc::freeifaddrs(head) };
        out
    }
}

#[cfg(windows)]
mod sys {
    use super::Interface;
    use std::net::Ipv4Addr;
    use windows::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, ERROR_SUCCESS};
    use windows::Win32::NetworkManagement::IpHelper::{
        GetAdaptersAddresses, GAA_FLAG_SKIP_ANYCAST, GAA_FLAG_SKIP_DNS_SERVER,
        GAA_FLAG_SKIP_MULTICAST, IP_ADAPTER_ADDRESSES_LH, IF_TYPE_SOFTWARE_LOOPBACK,
        IF_TYPE_TUNNEL,
    };
    use windows::Win32::NetworkManagement::Ndis::IfOperStatusUp;
    use windows::Win32::Networking::WinSock::{AF_INET, SOCKADDR_IN};

    pub fn list() -> Vec<Interface> {
        let mut out: Vec<Interface> = Vec::new();
        // Vec<u64>, not Vec<u8>: IP_ADAPTER_ADDRESSES_LH needs 8-byte alignment.
        let mut buf: Vec<u64> = vec![0; 4096];
        let mut size = (buf.len() * 8) as u32;
        let flags = GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_SKIP_DNS_SERVER;
        let mut filled = false;
        // Size-then-fill, bounded: falling out of the loop still holding
        // ERROR_BUFFER_OVERFLOW and walking the buffer anyway reads uninitialised
        // memory, so the walk is gated on `filled`.
        for _ in 0..3 {
            let rc = unsafe {
                GetAdaptersAddresses(AF_INET.0 as u32, flags, None,
                                     Some(buf.as_mut_ptr() as *mut IP_ADAPTER_ADDRESSES_LH),
                                     &mut size)
            };
            if rc == ERROR_SUCCESS.0 { filled = true; break; }
            if rc != ERROR_BUFFER_OVERFLOW.0 { return out; }   // ERROR_NO_DATA included
            buf = vec![0; (size as usize / 8) + 2];
            size = (buf.len() * 8) as u32;
        }
        if !filled { return out; }

        let mut ad = buf.as_ptr() as *const IP_ADAPTER_ADDRESSES_LH;
        while !ad.is_null() {
            let a = unsafe { &*ad };
            ad = a.Next;
            if a.OperStatus != IfOperStatusUp { continue; }
            if a.IfType == IF_TYPE_SOFTWARE_LOOPBACK || a.IfType == IF_TYPE_TUNNEL { continue; }
            let mut ua = a.FirstUnicastAddress;
            while !ua.is_null() {
                let u = unsafe { &*ua };
                ua = u.Next;
                if u.Address.lpSockaddr.is_null() { continue; }
                let sa = unsafe { &*(u.Address.lpSockaddr as *const SOCKADDR_IN) };
                if sa.sin_family != AF_INET { continue; }
                let ip = Ipv4Addr::from(unsafe { sa.sin_addr.S_un.S_addr }.to_ne_bytes());
                if ip.is_unspecified() { continue; }
                // PWSTR::to_string() is unsafe and does NOT null-check itself.
                let name = if a.FriendlyName.is_null() {
                    String::new()
                } else {
                    unsafe { a.FriendlyName.to_string() }.unwrap_or_default()
                };
                out.push(Interface { name, ip: ip.to_string() });
                break;   // one address per adapter — `-bind` takes exactly one
            }
        }
        out
    }
}

// The targets `engine_stub.rs` exists for: no enumeration, so the dropdown hides
// and an existing pin is still passed through for the engine to validate.
#[cfg(not(any(unix, windows)))]
mod sys {
    pub fn list() -> Vec<super::Interface> { Vec::new() }
}

// This crate sets panic = "abort" in [profile.dev] (FFI callbacks must never
// unwind), so plain `cargo test` refuses to link the harness. Run this with:
//   cargo test --config 'profile.dev.panic="unwind"' net_interfaces
#[cfg(test)]
mod tests {
    use super::*;

    fn nics() -> Vec<Interface> {
        vec![Interface { name: "en0".into(), ip: "192.168.1.50".into() },
             Interface { name: "Ethernet 2".into(), ip: "10.0.0.7".into() }]
    }

    #[test]
    fn bind_arg_is_a_trust_boundary() {
        assert_eq!(bind_arg(None), None);
        assert_eq!(bind_arg(Some(" 192.168.1.50 ")), Some("192.168.1.50"));
        // The whole point: a value with a space would exit(1) the engine.
        assert_eq!(bind_arg(Some("192.168.1.50 -nohold")), None);
        assert_eq!(bind_arg(Some("Ethernet 2")), None);
        assert_eq!(bind_arg(Some("")), None);
        assert_eq!(bind_arg(Some("::1")), None);
        assert_eq!(bind_arg(Some("192.168.1.999")), None);
    }

    #[test]
    fn label_falls_back_when_the_pinned_adapter_is_gone() {
        assert_eq!(label(&nics(), None), "Automatic");
        assert_eq!(label(&nics(), Some("10.0.0.7")), "Ethernet 2 (10.0.0.7)");
        // Adapter unplugged / DHCP moved us: stale but visible and clearable.
        assert_eq!(label(&nics(), Some("192.168.9.9")), "192.168.9.9 (not found)");
        assert_eq!(label(&[], Some("192.168.1.50")), "192.168.1.50 (not found)");
    }
}
