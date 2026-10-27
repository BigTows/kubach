//! Which addresses the image loader may connect to (#201).
//!
//! Image URLs come from chart authors, so a chart could point Kubach at the
//! user's own machine or network: `http://127.0.0.1:…`, the cloud metadata
//! service at `169.254.169.254`, a router on `192.168.…`. Only public
//! addresses are allowed, checked for every address a host resolves to.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// Whether connecting to `ip` is refused: loopback, private, link-local,
/// unspecified, multicast, broadcast and other special-purpose ranges, also
/// when written as an IPv6 address that embeds an IPv4 one.
pub fn is_forbidden(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => v4_forbidden(ip),
        IpAddr::V6(ip) => v6_forbidden(ip),
    }
}

fn v4_forbidden(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_multicast()
        || ip.is_broadcast()
        // 0.0.0.0/8, "this network".
        || a == 0
        // 100.64.0.0/10, carrier-grade NAT.
        || (a == 100 && (b & 0xC0) == 64)
        // 192.0.0.0/24, IETF protocol assignments.
        || (a == 192 && b == 0 && c == 0)
        // 198.18.0.0/15, benchmarking.
        || (a == 198 && (b & 0xFE) == 18)
        // 240.0.0.0/4, reserved.
        || a >= 240
}

fn v6_forbidden(ip: Ipv6Addr) -> bool {
    let segments = ip.segments();
    if let Some(v4) = embedded_v4(ip) {
        return v4_forbidden(v4);
    }
    ip.is_unspecified()
        || ip.is_loopback()
        || ip.is_multicast()
        // fc00::/7, unique local.
        || (segments[0] & 0xFE00) == 0xFC00
        // fe80::/10, link-local.
        || (segments[0] & 0xFFC0) == 0xFE80
        // fec0::/10, site-local (deprecated, still routed by some networks).
        || (segments[0] & 0xFFC0) == 0xFEC0
}

/// The IPv4 address inside an IPv4-mapped (`::ffff:a.b.c.d`),
/// IPv4-compatible (`::a.b.c.d`), NAT64 (`64:ff9b::a.b.c.d`) or 6to4
/// (`2002:aabb:ccdd::`) address.
fn embedded_v4(ip: Ipv6Addr) -> Option<Ipv4Addr> {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return Some(v4);
    }
    let s = ip.segments();
    let tail = |hi: u16, lo: u16| Ipv4Addr::from((u32::from(hi) << 16) | u32::from(lo));
    match s {
        // `::` and `::1` are themselves, not 0.0.0.0 and 0.0.0.1.
        [0, 0, 0, 0, 0, 0, 0, 0 | 1] => None,
        [0, 0, 0, 0, 0, 0, hi, lo] => Some(tail(hi, lo)),
        [0x64, 0xFF9B, 0, 0, 0, 0, hi, lo] => Some(tail(hi, lo)),
        [0x2002, hi, lo, ..] => Some(tail(hi, lo)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::is_forbidden;
    use std::net::IpAddr;

    fn forbidden(ip: &str) -> bool {
        is_forbidden(ip.parse::<IpAddr>().unwrap())
    }

    #[test]
    fn local_and_private_v4_are_refused() {
        for ip in [
            "127.0.0.1",
            "127.1.2.3",
            "0.0.0.0",
            "0.1.2.3",
            "10.0.0.1",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "100.64.0.1",
            "192.0.0.170",
            "198.18.0.1",
            "224.0.0.1",
            "239.255.255.250",
            "240.0.0.1",
            "255.255.255.255",
        ] {
            assert!(forbidden(ip), "{ip}");
        }
    }

    #[test]
    fn public_v4_is_allowed() {
        for ip in [
            "1.1.1.1",
            "8.8.8.8",
            "140.82.112.3",
            "172.15.0.1",
            "172.32.0.1",
            "100.128.0.1",
            "198.20.0.1",
            "192.0.2.1",
        ] {
            assert!(!forbidden(ip), "{ip}");
        }
    }

    #[test]
    fn local_and_private_v6_are_refused() {
        for ip in [
            "::",
            "::1",
            "fc00::1",
            "fd12:3456::1",
            "fe80::1",
            "febf::1",
            "fec0::1",
            "ff02::1",
            "ff0e::1",
        ] {
            assert!(forbidden(ip), "{ip}");
        }
    }

    #[test]
    fn v6_with_an_embedded_private_v4_is_refused() {
        for ip in [
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::ffff:10.0.0.1",
            "::ffff:0.0.0.0",
            "::127.0.0.1",
            "::192.168.0.1",
            "64:ff9b::127.0.0.1",
            "64:ff9b::a9fe:a9fe",
            "2002:7f00:1::",
            "2002:c0a8:101::1",
        ] {
            assert!(forbidden(ip), "{ip}");
        }
    }

    #[test]
    fn public_v6_is_allowed() {
        for ip in [
            "2606:4700:4700::1111",
            "2a00:1450:4001:80b::200e",
            "::ffff:1.1.1.1",
            "64:ff9b::808:808",
            "2002:808:808::1",
        ] {
            assert!(!forbidden(ip), "{ip}");
        }
    }
}
