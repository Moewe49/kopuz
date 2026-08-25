//! Reading the address someone typed.
//!
//! Nobody types `https://ms-01.tailnet:8484/`. They type `ms-01:8484`, and an
//! error message about an unknown scheme is a poor answer to something this
//! guessable.
//!
//! The second job here matters more. The token authenticates but does not
//! encrypt, so plain `http://` to a public address puts the shared secret on
//! the wire in the clear. That is worth saying on screen, at the moment the
//! address is typed — a warning nobody sees is not a warning.

/// Fill in what was left out: no scheme means `http://`, and a trailing slash
/// is not a difference.
pub fn normalise_url(input: &str) -> String {
    let trimmed = input.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    }
}

/// The host, without scheme, credentials, port or path.
fn host_of(url: &str) -> &str {
    let after_scheme = url.split_once("://").map_or(url, |(_, rest)| rest);
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    // `user:pass@host` — the part that matters is after the last `@`.
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    // `[::1]:8484` — brackets, because a bare IPv6 address is full of colons
    // and splitting on the first one would return "".
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split(']').next().unwrap_or("");
    }
    authority.split(':').next().unwrap_or("")
}

/// Whether this host is one only the listener's own machines can reach.
fn is_private_host(host: &str) -> bool {
    let host = host.trim().to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    if host == "localhost" || host == "::1" || host == "[::1]" {
        return true;
    }
    if host.ends_with(".local") || host.ends_with(".internal") || host.ends_with(".home") {
        return true;
    }
    // A name with no dot in it is a LAN name — `ms-01`, `fileserver`. There is
    // no such thing as a publicly routable single-label host.
    if !host.contains('.') && !host.contains(':') {
        return true;
    }
    let octets: Vec<u8> = host
        .split('.')
        .filter_map(|part| part.parse::<u8>().ok())
        .collect();
    if octets.len() != 4 || host.split('.').count() != 4 {
        return false;
    }
    match octets[..] {
        [127, ..] => true,
        [10, ..] => true,
        [192, 168, ..] => true,
        [172, b, ..] => (16..32).contains(&b),
        // 100.64/10 is carrier-grade NAT, and it is the range Tailscale hands
        // out — the most likely way this ever gets used across the internet.
        [100, b, ..] => (64..128).contains(&b),
        [169, 254, ..] => true,
        _ => false,
    }
}

/// Whether the shared secret would cross the open internet unencrypted.
///
/// True means: plain HTTP to somewhere that is not the listener's own network.
/// Anyone on the path can read the token and then read and write everything
/// the relay holds. The fix is a reverse proxy with a certificate, or a
/// private network such as Tailscale — not a longer token.
pub fn token_travels_in_the_clear(url: &str) -> bool {
    let url = url.trim().to_ascii_lowercase();
    if url.is_empty() {
        return false;
    }
    if url.starts_with("https://") {
        return false;
    }
    !is_private_host(host_of(&url))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bare_host_and_port_becomes_a_url() {
        assert_eq!(normalise_url("ms-01:8484"), "http://ms-01:8484");
        assert_eq!(
            normalise_url("  192.168.1.50:8484  "),
            "http://192.168.1.50:8484"
        );
        assert_eq!(
            normalise_url("https://kopuz.example.net/"),
            "https://kopuz.example.net"
        );
        assert_eq!(normalise_url("http://ms-01:8484"), "http://ms-01:8484");
        assert_eq!(normalise_url("   "), "");
    }

    #[test]
    fn the_host_survives_ports_paths_and_credentials() {
        assert_eq!(host_of("http://ms-01:8484/v1/state/mixes"), "ms-01");
        assert_eq!(host_of("https://user:pw@example.net:443/x"), "example.net");
        assert_eq!(host_of("http://[::1]:8484"), "::1");
        assert_eq!(host_of("http://192.168.1.50"), "192.168.1.50");
    }

    /// The whole point of the warning is that it fires when it should and
    /// stays quiet when it should not. A warning that cries wolf on the
    /// listener's own LAN teaches them to ignore the one that matters.
    #[test]
    fn plaintext_is_only_a_problem_off_the_listeners_own_network() {
        for quiet in [
            "http://localhost:8484",
            "http://127.0.0.1:8484",
            "http://[::1]:8484",
            "http://192.168.1.50:8484",
            "http://10.0.0.5:8484",
            "http://172.16.4.1:8484",
            "http://172.31.255.255:8484",
            // Tailscale hands out 100.64/10.
            "http://100.101.102.103:8484",
            "http://ms-01:8484",
            "http://ms-01.local:8484",
            "https://kopuz.example.net",
            "",
        ] {
            assert!(
                !token_travels_in_the_clear(quiet),
                "should not warn: {quiet}"
            );
        }
        for loud in [
            "http://kopuz.example.net",
            "http://203.0.113.7:8484",
            // 172.32 is outside the private range, however much it looks like
            // it should be inside it.
            "http://172.32.0.1:8484",
            "http://100.128.0.1:8484",
            "http://8.8.8.8",
        ] {
            assert!(token_travels_in_the_clear(loud), "should warn: {loud}");
        }
    }
}
