//! Shared outbound HTTP destination policy, including local/LAN access.
pub fn check_http_host(raw_host: &str) -> Result<(), String> {
    let host = raw_host
        .trim_end_matches('.')
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_ascii_lowercase();

    let is_metadata = if host == "metadata.google.internal" {
        true
    } else if let Some(ip) = normalize_host_to_ip(&host) {
        match ip {
            std::net::IpAddr::V4(v4) => v4 == std::net::Ipv4Addr::new(169, 254, 169, 254),
            std::net::IpAddr::V6(v6) => {
                v6 == std::net::Ipv6Addr::new(0xfd00, 0x0ec2, 0, 0, 0, 0, 0, 0x0254)
                    || v6.to_ipv4_mapped() == Some(std::net::Ipv4Addr::new(169, 254, 169, 254))
                    || v6.to_ipv4() == Some(std::net::Ipv4Addr::new(169, 254, 169, 254))
            }
        }
    } else {
        false
    };

    if is_metadata {
        return Err(format!("refusing to fetch {host}: cloud metadata endpoint"));
    }

    Ok(())
}

/// Parse a single numbers-and-dots octet in decimal, `0x`-prefixed hex, or leading-zero octal.
fn parse_inet_aton_part(part: &str) -> Option<u32> {
    if let Some(hex) = part.strip_prefix("0x").or_else(|| part.strip_prefix("0X")) {
        if hex.is_empty() {
            return None;
        }
        return u32::from_str_radix(hex, 16).ok();
    }
    if part.len() > 1 && part.starts_with('0') {
        return u32::from_str_radix(part, 8).ok();
    }
    part.parse::<u32>().ok()
}

/// Parse a numbers-and-dots host (1-4 dot-separated parts, each decimal/hex/octal) into an
/// `Ipv4Addr` following the classic BSD `inet_aton` combination rules.
fn parse_inet_aton(host: &str) -> Option<std::net::Ipv4Addr> {
    if host.is_empty() {
        return None;
    }
    let parts: Vec<&str> = host.split('.').collect();
    if parts.is_empty() || parts.len() > 4 || parts.iter().any(|p| p.is_empty()) {
        return None;
    }

    let values: Vec<u32> = parts
        .iter()
        .map(|p| parse_inet_aton_part(p))
        .collect::<Option<_>>()?;

    let addr: u32 = match values.as_slice() {
        [a] => *a,
        [a, b] => {
            if *a > 0xff || *b > 0x00ff_ffff {
                return None;
            }
            (*a << 24) | *b
        }
        [a, b, c] => {
            if *a > 0xff || *b > 0xff || *c > 0xffff {
                return None;
            }
            (*a << 24) | (*b << 16) | *c
        }
        [a, b, c, d] => {
            if *a > 0xff || *b > 0xff || *c > 0xff || *d > 0xff {
                return None;
            }
            (*a << 24) | (*b << 16) | (*c << 8) | *d
        }
        _ => return None,
    };

    Some(std::net::Ipv4Addr::from(addr))
}

/// Normalize a host string to a canonical [`std::net::IpAddr`] if it represents a numeric IP,
/// covering standard dotted-decimal/IPv6 forms as well as alternate numeric encodings
/// (decimal dword, hex, octal, and short "numbers-and-dots" forms) that a resolver or the OS
/// network stack would still resolve to the same address. Returns `None` for non-numeric
/// hostnames.
fn normalize_host_to_ip(host: &str) -> Option<std::net::IpAddr> {
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        return Some(ip);
    }
    parse_inet_aton(host).map(std::net::IpAddr::V4)
}
