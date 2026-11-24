use std::net::IpAddr;

/// Hosts a visitor page must not be able to reach from the guest machine.
pub fn is_restricted_host(host: &str) -> bool {
    let host = host.trim().trim_matches(['[', ']']);
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") {
        return true;
    }
    let Ok(ip) = host.parse::<IpAddr>() else {
        return false;
    };
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, _, _] = v4.octets();
            v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || a == 0
                || (a == 100 && (b & 0b1100_0000) == 64)
        }
        IpAddr::V6(v6) => {
            let first = v6.segments()[0];
            v6.is_loopback()
                || v6.is_unspecified()
                || (first & 0xfe00) == 0xfc00
                || (first & 0xffc0) == 0xfe80
        }
    }
}

/// Guest webviews may navigate only to their own random loopback proxy.
pub fn navigation_allowed(url: &url::Url, port: u16) -> bool {
    if url.scheme() != "http" || !url.username().is_empty() || url.password().is_some() {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    if is_restricted_host(host) {
        return host == "127.0.0.1" && url.port() == Some(port);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_private_and_metadata_addresses() {
        for host in [
            "127.0.0.1",
            "localhost",
            "10.1.2.3",
            "192.168.0.8",
            "172.16.0.1",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "::1",
            "fd00::1",
        ] {
            assert!(is_restricted_host(host), "{host}");
        }
        assert!(!is_restricted_host("example.com"));
        assert!(!is_restricted_host("1.1.1.1"));
    }

    #[test]
    fn navigation_stays_on_the_proxy_port() {
        let allowed = url::Url::parse("http://127.0.0.1:43123/index.html").unwrap();
        assert!(navigation_allowed(&allowed, 43123));

        let other_port = url::Url::parse("http://127.0.0.1:80/").unwrap();
        assert!(!navigation_allowed(&other_port, 43123));

        for raw in [
            "http://192.168.1.1/",
            "http://10.0.0.8/",
            "http://169.254.169.254/latest/meta-data",
            "http://localhost:43123/",
            "https://127.0.0.1:43123/",
            "http://user:pass@127.0.0.1:43123/",
            "http://example.com/",
        ] {
            let url = url::Url::parse(raw).unwrap();
            assert!(!navigation_allowed(&url, 43123), "{raw}");
        }
    }
}
