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
    // A backslash ends the authority too: the URL parser treats `\` as `/`, so
    // `http://ms-01\@evil.com` reaches evil.com, and splitting only on `/`
    // would read the host as the reassuring-looking `ms-01`.
    let authority = after_scheme
        .split(['/', '?', '#', '\\'])
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
    // IPv6, told by its colons. Unique-local (fc00::/7 — the `fc`/`fd` prefix)
    // and link-local (fe80::/10) are the private ranges. Tailscale hands out
    // addresses inside fc00::/7 (fd7a:…), so recognising it keeps the warning
    // quiet on the very path the warning tells you to use — without this a
    // Tailscale IPv6 address nags every time, which teaches people to ignore
    // the warning that matters.
    if host.contains(':') {
        return host.starts_with("fc") || host.starts_with("fd") || host.starts_with("fe80");
    }
    // A single label with no dot — `ms-01`, `fileserver` — but only when it
    // truly looks like a hostname label: letters, digits and hyphens, with at
    // least one letter. The URL parser expands other dotless forms into public
    // addresses that this check would otherwise wave through in silence: an
    // all-digit string is an integer-encoded IPv4 (134744072 becomes 8.8.8.8),
    // `0x…` is hex IPv4 (0x08080808 becomes 8.8.8.8), and a percent-escape or a
    // non-ASCII character can normalise to a public host too. None of those is
    // a real LAN name, so treat anything that is not a clean label as public
    // and let the warning fire.
    if !host.contains('.') {
        if host.starts_with("0x") {
            return false;
        }
        return host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            && host.bytes().any(|b| b.is_ascii_alphabetic());
    }
    // A dotted address is private only as a canonical IPv4 in a private range.
    // A leading-zero octet is rejected: the URL parser reads `010` as octal 8,
    // so `010.0.0.1` is not the 10.0.0.1 it looks like, and calling it private
    // would be the same silent mistake in a different disguise.
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() != 4 {
        return false;
    }
    let mut octets = [0u8; 4];
    for (i, part) in parts.iter().enumerate() {
        if part.len() > 1 && part.starts_with('0') {
            return false;
        }
        match part.parse::<u8>() {
            Ok(v) => octets[i] = v,
            Err(_) => return false,
        }
    }
    match octets {
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

    /// The disguises the URL parser sees through and an unwary check does not.
    /// Every one of these resolves to a public host (8.8.8.8, evil.com), so the
    /// warning MUST fire; the danger is entirely in it staying silent. Proven
    /// against the real parser with a throwaway probe: `reqwest::Url` turns
    /// each of the left-hand forms into the public host on the right.
    #[test]
    fn an_address_that_only_looks_private_still_warns() {
        for disguised in [
            // Integer-encoded IPv4: 134744072 == 8.8.8.8.
            "http://134744072:8484",
            // Hex IPv4: 0x08080808 == 8.8.8.8.
            "http://0x08080808:8484",
            // Percent-escaped dot: evil%2ecom == evil.com.
            "http://evil%2ecom:8484",
            // Leading zero read as octal: 010.0.0.1 == 8.0.0.1, not 10.0.0.1.
            "http://010.0.0.1:8484",
        ] {
            assert!(
                token_travels_in_the_clear(disguised),
                "a disguised public host must still warn: {disguised}"
            );
        }
    }

    /// The mirror case, measured against the real parser rather than assumed.
    /// A backslash in a special-scheme URL terminates the authority, so
    /// `http://ms-01\@evil.com` actually reaches `ms-01` (private) and not
    /// `evil.com` — verified: `reqwest::Url::parse` returns host `ms-01`. The
    /// warning must therefore stay quiet, and `host_of` has to read the host
    /// the same way the parser does, or it would false-alarm on a private one.
    #[test]
    fn a_backslash_terminates_the_authority_like_the_parser() {
        assert_eq!(host_of(r"http://ms-01\@evil.com:8484"), "ms-01");
        assert!(!token_travels_in_the_clear(r"http://ms-01\@evil.com:8484"));
    }

    /// The recommended path must not nag. Tailscale can address a node by its
    /// IPv6 (fd7a:…, inside the unique-local fc00::/7 block) as readily as its
    /// 100.64/10 IPv4, and a warning that fires on the setup its own text
    /// recommends is a warning people learn to click past.
    #[test]
    fn private_ipv6_including_tailscale_stays_quiet() {
        for quiet in [
            "http://[fd7a:115c:a1e0::1]:8484", // Tailscale ULA
            "http://[fd00::1]:8484",           // unique-local
            "http://[fe80::1]:8484",           // link-local
        ] {
            assert!(
                !token_travels_in_the_clear(quiet),
                "a private IPv6 address should not warn: {quiet}"
            );
        }
        // A public IPv6 still does.
        assert!(token_travels_in_the_clear(
            "http://[2606:4700:4700::1111]:8484"
        ));
    }
}
