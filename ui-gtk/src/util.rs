pub fn chrono_like_timestamp() -> String {
    // Tiny helper: avoid pulling chrono dependency.
    use std::time::{SystemTime, UNIX_EPOCH};
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("{}", ms)
}

pub fn fake_remote_device_id() -> String {
    // Stable enough uniqueness without adding a uuid dependency.
    format!("ui-test-{}-{}", std::process::id(), chrono_like_timestamp())
}

/// Normalize a user-provided relay address for *client connections*.
///
/// Users sometimes type `0.0.0.0:PORT` (or `[::]:PORT`) in the UI, which is a
/// valid *bind* address but not a meaningful *connect* target. In that case we
/// transparently rewrite it to the local loopback address.
///
/// - `0.0.0.0:PORT` -> `127.0.0.1:PORT`
/// - `[::]:PORT`    -> `[::1]:PORT`
///
/// Hostnames (e.g. `example.com:8080`) are preserved.
pub fn normalize_relay_addr_for_connect(input: &str) -> String {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    let s = input.trim();
    if s.is_empty() {
        return String::new();
    }

    match s.parse::<SocketAddr>() {
        Ok(mut sa) => {
            match sa.ip() {
                IpAddr::V4(v4) if v4.is_unspecified() => {
                    sa.set_ip(IpAddr::V4(Ipv4Addr::LOCALHOST));
                }
                IpAddr::V6(v6) if v6.is_unspecified() => {
                    sa.set_ip(IpAddr::V6(Ipv6Addr::LOCALHOST));
                }
                _ => {}
            }
            sa.to_string()
        }
        // Not an IP literal (likely a hostname). Keep as-is.
        Err(_) => s.to_string(),
    }
}
