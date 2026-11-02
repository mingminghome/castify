use std::net::{Ipv4Addr, SocketAddr};

/// First non-loopback IPv4 address, preferring RFC1918.
pub fn local_ipv4() -> Option<Ipv4Addr> {
    let mut found: Vec<Ipv4Addr> = Vec::new();
    if let Ok(s) = std::net::UdpSocket::bind("0.0.0.0:0") {
        if s.connect("8.8.8.8:80").is_ok() {
            if let Ok(SocketAddr::V4(v4)) = s.local_addr() {
                let ip = *v4.ip();
                if !ip.is_loopback() && !ip.is_unspecified() {
                    found.push(ip);
                }
            }
        }
    }
    found.into_iter().next()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_ip_is_v4_or_none() {
        if let Some(ip) = local_ipv4() {
            assert!(!ip.is_loopback());
        }
    }
}
