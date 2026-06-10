//! Hand-supplied YT Music session cookies — the sign-in path that
//! works everywhere, including Windows, where the isolated-browser
//! flow is double-blocked (Google renders ServiceLogin blank in a
//! fresh profile, and Chrome 127+ App-Bound Encryption stops us from
//! decrypting the profile's cookie DB anyway).
//!
//! Two sources:
//! - **Paste**: the user copies the `Cookie:` request header from the
//!   browser DevTools on an open music.youtube.com tab (the canonical
//!   ytmusicapi "browser auth" method). The browser already decrypted
//!   everything, so encryption schemes never matter.
//! - **Firefox import**: read youtube.com cookies straight from the
//!   user's Firefox/LibreWolf profile via `rookie` — Firefox doesn't
//!   use app-bound encryption, so this works on Windows too.

/// Cookies that must be present for SAPISIDHASH auth + a signed-in
/// session. SAPISID may arrive under its `__Secure-3PAPISID` alias.
const REQUIRED_ANY_APISID: [&str; 2] = ["SAPISID", "__Secure-3PAPISID"];
const REQUIRED_ANY_SID: [&str; 2] = ["SID", "__Secure-3PSID"];

/// Normalize a hand-pasted cookie blob into a clean `Cookie:` header.
///
/// Accepts what people actually paste:
/// - a raw `key=value; key=value` header line,
/// - the same with a leading `Cookie:` label (DevTools "copy value"
///   vs. copying the whole header row),
/// - multi-line pastes (DevTools wraps long headers),
/// - a Netscape `cookies.txt` export (yt-dlp's `--cookies` format).
pub fn sanitize_header(input: &str) -> Result<String, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("empty input".to_string());
    }

    let pairs: Vec<(String, String)> = if looks_like_netscape(trimmed) {
        parse_netscape(trimmed)
    } else {
        parse_header_blob(trimmed)
    };

    let header = pairs
        .iter()
        .filter(|(k, v)| header_safe(k) && header_safe(v))
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ");

    let has = |names: &[&str]| {
        pairs
            .iter()
            .any(|(k, _)| names.iter().any(|n| k == n))
    };
    if !has(&REQUIRED_ANY_APISID) || !has(&REQUIRED_ANY_SID) {
        return Err(
            "Missing auth cookies (SAPISID/SID). Copy the FULL Cookie header from a signed-in \
             music.youtube.com tab — DevTools (F12) → Network → click any request → Request \
             Headers → Cookie."
                .to_string(),
        );
    }
    Ok(header)
}

/// Read youtube.com cookies from the user's Firefox (or LibreWolf)
/// profile. Works on every desktop OS — Firefox has no app-bound
/// encryption. The user must be signed in to YouTube there. Not
/// available on mobile (no desktop browser profile; `rookie` doesn't
/// build for android/ios).
#[cfg(any(target_os = "android", target_os = "ios"))]
pub async fn extract_from_firefox() -> Result<String, String> {
    Err("Firefox cookie import is not available on mobile".to_string())
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub async fn extract_from_firefox() -> Result<String, String> {
    let cookies = tokio::task::spawn_blocking(|| {
        let domains = Some(vec!["youtube.com".to_string()]);
        rookie::firefox(domains.clone())
            .or_else(|ff_err| {
                rookie::librewolf(domains)
                    .map_err(|lw_err| format!("Firefox: {ff_err}; LibreWolf: {lw_err}"))
            })
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| format!("cookie task: {e}"))??;

    let header = cookies
        .iter()
        .filter(|c| !c.value.is_empty() && header_safe(&c.name) && header_safe(&c.value))
        .map(|c| format!("{}={}", c.name, c.value))
        .collect::<Vec<_>>()
        .join("; ");
    sanitize_header(&header).map_err(|_| {
        "No signed-in YouTube session found in Firefox — open music.youtube.com there, \
         sign in, then retry."
            .to_string()
    })
}

fn looks_like_netscape(s: &str) -> bool {
    s.starts_with("# Netscape") || s.lines().any(|l| l.split('\t').count() >= 7)
}

fn parse_netscape(s: &str) -> Vec<(String, String)> {
    s.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() < 7 || !f[0].contains("youtube.com") {
                return None;
            }
            Some((f[5].trim().to_string(), f[6].trim().to_string()))
        })
        .collect()
}

fn parse_header_blob(s: &str) -> Vec<(String, String)> {
    // Join wrapped lines, drop an optional leading "Cookie:" label.
    let joined = s.lines().map(str::trim).collect::<Vec<_>>().join(" ");
    let body = joined
        .strip_prefix("cookie:")
        .or_else(|| joined.strip_prefix("Cookie:"))
        .or_else(|| joined.strip_prefix("COOKIE:"))
        .unwrap_or(&joined);
    body.split(';')
        .filter_map(|pair| {
            let (k, v) = pair.trim().split_once('=')?;
            let k = k.trim();
            let v = v.trim();
            (!k.is_empty() && !v.is_empty()).then(|| (k.to_string(), v.to_string()))
        })
        .collect()
}

fn header_safe(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| (0x20..0x7f).contains(&b) && b != b';' && b != b',')
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = "VISITOR_INFO1_LIVE=abc; SID=g.a000xyz; __Secure-3PSID=g.a000xyz; \
                        SAPISID=ab/cdef; __Secure-3PAPISID=ab/cdef; PREF=f6=400";

    #[test]
    fn accepts_plain_header() {
        let h = sanitize_header(GOOD).unwrap();
        assert!(h.contains("SAPISID=ab/cdef"));
        assert!(h.contains("SID=g.a000xyz"));
    }

    #[test]
    fn strips_cookie_label_and_newlines() {
        let input = format!("Cookie: {}", GOOD.replace("; ", ";\n  "));
        let h = sanitize_header(&input).unwrap();
        assert!(h.contains("SAPISID=ab/cdef"));
        assert!(!h.to_lowercase().starts_with("cookie:"));
    }

    #[test]
    fn rejects_missing_auth_cookies() {
        let err = sanitize_header("VISITOR_INFO1_LIVE=abc; PREF=f6=400").unwrap_err();
        assert!(err.contains("SAPISID"));
    }

    #[test]
    fn parses_netscape_cookies_txt() {
        let txt = "# Netscape HTTP Cookie File\n\
                   .youtube.com\tTRUE\t/\tTRUE\t0\tSID\tg.a000xyz\n\
                   .youtube.com\tTRUE\t/\tTRUE\t0\tSAPISID\tab/cdef\n\
                   .example.com\tTRUE\t/\tTRUE\t0\tOTHER\tnope\n";
        let h = sanitize_header(txt).unwrap();
        assert!(h.contains("SAPISID=ab/cdef"));
        assert!(!h.contains("OTHER"));
    }

    #[test]
    fn rejects_empty() {
        assert!(sanitize_header("   ").is_err());
    }
}
