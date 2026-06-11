use reader::models::Track;
use serde_json::Value;

pub mod botguard;
pub mod clients;
pub mod cookies;
pub mod decipher;
pub mod discover;
pub mod innertube;
pub mod isolated_profile;
pub mod manual_cookies;
pub mod mix;
pub mod mutations;
pub mod oauth;
pub mod player;
pub mod playlists;
pub mod ytdlp_resolve;
pub mod search;
pub mod verify_session_keepalive;

pub use player::YtStreamInfo;

use std::sync::atomic::{AtomicU8, Ordering};

// Remembered "what last resolved a stream" so repeat tracks skip dead ends.
const STRAT_UNKNOWN: u8 = 0;
const STRAT_NATIVE: u8 = 1;
const STRAT_YTDLP_COOKIES: u8 = 2;
const STRAT_YTDLP_ANON: u8 = 3;
static PREFERRED_STRATEGY: AtomicU8 = AtomicU8::new(STRAT_UNKNOWN);

fn preferred_strategy() -> u8 {
    PREFERRED_STRATEGY.load(Ordering::Relaxed)
}
fn set_preferred_strategy(s: u8) {
    PREFERRED_STRATEGY.store(s, Ordering::Relaxed);
}

pub const SOURCE_PREFIX: &str = "ytmusic";

/// Surfaced by auth-only operations (like/unlike, add-to-playlist,
/// liked-songs sync) when the YT backend is in anonymous mode.
/// Callers should detect this and either skip the action or hint at
/// signing in.
pub const ANON_AUTH_REQUIRED: &str = "YouTube Music not signed in";

/// Stable per-Google-account id derived from the SAPISID cookie. Used
/// as the server-identity cache-bust key so switching YT accounts
/// invalidates synced library/favorites state. Never logged.
pub fn derive_user_id(cookies: &str) -> Option<String> {
    use sha1::{Digest, Sha1};
    let sapisid = cookies.split(';').find_map(|p| {
        let (k, v) = p.trim().split_once('=')?;
        (k == "SAPISID" || k == "__Secure-3PAPISID").then(|| v.to_string())
    })?;
    let mut h = Sha1::new();
    h.update(sapisid.as_bytes());
    Some(format!("yt-{}", hex::encode(&h.finalize()[..6])))
}

pub struct YouTubeMusicClient {
    cookies: Option<String>,
}

impl YouTubeMusicClient {
    pub fn new() -> Self {
        Self { cookies: None }
    }

    pub fn with_cookies(cookies: String) -> Self {
        // Normalize the empty string (anonymous-mode marker, stored as
        // access_token: Some("")) to None so every `self.cookies` check
        // — auth-only guards, is_anonymous, the public-surface
        // unwrap_or("") — treats anonymous consistently.
        Self {
            cookies: (!cookies.is_empty()).then_some(cookies),
        }
    }

    /// The raw browser cookie header, if this session is cookie-based. Returns
    /// `None` for OAuth sessions (whose token is `oauth:<…>`, not a cookie jar)
    /// so cookie-only consumers — chiefly yt-dlp's `--cookies` file — never get
    /// handed a Bearer token to misinterpret.
    fn cookie_jar(&self) -> Option<&str> {
        self.cookies
            .as_deref()
            .filter(|c| !c.starts_with(oauth::OAUTH_PREFIX))
    }

    pub async fn search_tracks(&self, query: &str) -> Result<Vec<Track>, String> {
        search::music_search_tracks(query, self.cookies.as_deref()).await
    }

    /// Playlist / album / artist sections for the search page. Works
    /// anonymously — public catalog entities don't need auth.
    pub async fn search_sections(&self, query: &str) -> Result<search::SearchSections, String> {
        search::music_search_sections(query, self.cookies.as_deref()).await
    }

    pub async fn resolve_artist_channel_id(
        &self,
        query: &str,
    ) -> Result<Option<String>, String> {
        search::resolve_artist_channel_id(query, self.cookies.as_deref()).await
    }

    /// List the user's saved playlists (everything under Library →
    /// Playlists, minus the Liked Music auto-playlist). Returns summary
    /// rows; call [`get_playlist_entries`] for the tracks of any given
    /// playlist.
    /// Library playlists view (FEmusic_liked_playlists). Auth-only —
    /// returns Ok(vec![]) in anonymous mode so the playlists tab
    /// just shows empty rather than erroring.
    pub async fn list_playlists(
        &self,
    ) -> Result<Vec<playlists::YtPlaylistSummary>, String> {
        let Some(cookies) = self.cookies.as_deref() else {
            return Ok(Vec::new());
        };
        playlists::list_playlists(cookies).await
    }

    /// Playlist contents. Public playlists work anonymously; the
    /// user's personal/private ones obviously won't.
    pub async fn get_playlist_entries(
        &self,
        playlist_id: &str,
    ) -> Result<Vec<Track>, String> {
        playlists::get_playlist_entries(playlist_id, self.cookies.as_deref().unwrap_or("")).await
    }

    pub async fn stream_playlist_entries<F>(
        &self,
        playlist_id: &str,
        on_batch: F,
    ) -> Result<(), String>
    where
        F: FnMut(Vec<Track>),
    {
        playlists::stream_playlist_entries(playlist_id, self.cookies.as_deref().unwrap_or(""), on_batch).await
    }

    // Mutations are inherently auth-only — keep the explicit "not
    // signed in" error so callers (favorite toggle, add-to-playlist
    // modal) can surface a clear "sign in to enable" message.
    pub async fn like_video(&self, video_id: &str) -> Result<(), String> {
        let cookies = self.cookies.as_deref().ok_or(ANON_AUTH_REQUIRED)?;
        mutations::like_video(video_id, cookies).await
    }

    pub async fn unlike_video(&self, video_id: &str) -> Result<(), String> {
        let cookies = self.cookies.as_deref().ok_or(ANON_AUTH_REQUIRED)?;
        mutations::unlike_video(video_id, cookies).await
    }

    pub async fn add_to_playlist(
        &self,
        playlist_id: &str,
        video_id: &str,
    ) -> Result<(), String> {
        let cookies = self.cookies.as_deref().ok_or(ANON_AUTH_REQUIRED)?;
        mutations::add_to_playlist(playlist_id, video_id, cookies).await
    }

    pub async fn remove_from_playlist(
        &self,
        playlist_id: &str,
        video_id: &str,
    ) -> Result<(), String> {
        let cookies = self.cookies.as_deref().ok_or(ANON_AUTH_REQUIRED)?;
        mutations::remove_from_playlist(playlist_id, video_id, cookies).await
    }

    pub async fn create_playlist(
        &self,
        title: &str,
        description: &str,
        video_ids: &[&str],
    ) -> Result<String, String> {
        let cookies = self.cookies.as_deref().ok_or(ANON_AUTH_REQUIRED)?;
        mutations::create_playlist(title, description, video_ids, cookies).await
    }

    pub async fn delete_playlist(&self, playlist_id: &str) -> Result<(), String> {
        let cookies = self.cookies.as_deref().ok_or(ANON_AUTH_REQUIRED)?;
        mutations::delete_playlist(playlist_id, cookies).await
    }

    /// Stream the user's full Liked Music playlist page by page. The
    /// callback fires once per ~100-track batch as soon as it arrives,
    /// so the UI can populate incrementally instead of waiting for the
    /// whole library to download. Walks `continuationItemRenderer`
    /// tokens until exhausted.
    pub async fn stream_liked_songs<F>(&self, mut on_page: F) -> Result<(), String>
    where
        F: FnMut(Vec<Track>),
    {
        // Liked Music is auth-only — anonymous callers get an empty
        // list rather than an error so favorites views render the
        // standard empty state without surfacing a stack trace.
        let Some(cookies) = self.cookies.as_deref() else {
            let _ = &mut on_page;
            return Ok(());
        };
        let resp: Value = innertube::browse("VLLM", cookies).await?;
        // Only a genuine sign-in prompt means expired. No shelf with no
        // prompt just means the user has no liked songs — return empty.
        if is_signed_out(&resp) {
            return Err("Sign-in prompt returned — cookies expired".to_string());
        }
        if !has_playlist_shelf(&resp) {
            return Ok(());
        }
        // YT's continuation pagination commonly repeats one or more tracks at page
        // boundaries; dedup against a video-id set across the entire stream so the
        // callback always sees unique tracks.
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let dedup = |page: Vec<Track>, seen: &mut std::collections::HashSet<String>| -> Vec<Track> {
            page.into_iter()
                .filter(|t| {
                    let id = t
                        .path
                        .to_string_lossy()
                        .split(':')
                        .nth(1)
                        .unwrap_or("")
                        .to_string();
                    !id.is_empty() && seen.insert(id)
                })
                .collect()
        };

        let (page1, mut next) = search::walk_playlist_shelf(&resp);
        let page1 = dedup(page1, &mut seen);
        if !page1.is_empty() {
            on_page(page1);
        }
        while let Some(token) = next.take() {
            let page = innertube::browse_continuation(&token, cookies).await?;
            let (more, next_token) = search::walk_playlist_continuation(&page);
            let more = dedup(more, &mut seen);
            // An empty page after dedup means YT either gave us only
            // duplicates of already-seen tracks or no new content. Even
            // if it returned a continuation token, looping again would
            // hammer the same endpoint without progress, so stop.
            if more.is_empty() {
                break;
            }
            on_page(more);
            next = next_token;
        }
        Ok(())
    }

    /// Buffered convenience around [`stream_liked_songs`] — collects all
    /// pages before returning. Use only when the caller doesn't care
    /// about incremental updates.
    pub async fn get_liked_songs(&self) -> Result<Vec<Track>, String> {
        let mut all = Vec::new();
        self.stream_liked_songs(|page| all.extend(page)).await?;
        Ok(all)
    }

    /// Resolves a playable stream URL. Primary path is native sig/n
    /// deciphering + the in-app PO-token minter (see `player::resolve`).
    /// When that fails entirely — most commonly YouTube escalating a
    /// signed-in session to a `LOGIN_REQUIRED` bot check — and a local
    /// `yt-dlp` is installed, fall back to it (it carries the full
    /// bot-check machinery and uses our cookies). yt-dlp is never the
    /// primary path, so binary-free installs are unaffected.
    pub async fn get_stream(&self, video_id: &str) -> Result<YtStreamInfo, String> {
        let ytdlp = ytdlp_resolve::find_ytdlp().is_some();

        // Latency: once we learn which path actually works for this session,
        // try it FIRST so every later track skips the slow dead ends. A
        // bot-flagged session fails the whole native chain (decipher + PO mint
        // + ANDROID_VR + bare clients, seconds each) every single track — so
        // after the first track resolves via anonymous yt-dlp we go straight
        // there. The full chain still runs as a fallback if the shortcut fails.
        if ytdlp && preferred_strategy() == STRAT_YTDLP_ANON {
            if let Ok(info) = ytdlp_resolve::resolve(video_id, None).await {
                return Ok(info);
            }
        }

        let native = player::resolve(video_id, self.cookies.as_deref()).await;
        let native_err = match native {
            Ok(info) => {
                set_preferred_strategy(STRAT_NATIVE);
                return Ok(info);
            }
            Err(e) => e,
        };
        if !ytdlp {
            return Err(native_err);
        }
        eprintln!("[yt-player] native resolve failed ({native_err}) — trying yt-dlp fallback");

        // Try yt-dlp WITH cookies first (needed for Premium / age-gated), but
        // if that fails retry ANONYMOUSLY: a signed-in session that's been
        // flagged as a bot returns an empty format list ("Requested format is
        // not available"), whereas the same track resolves fine without
        // cookies. Non-Premium playback doesn't need the cookies anyway.
        // When the native error is itself a bot check, the cookies are already
        // flagged — skip straight to the anonymous attempt to save a round.
        let bot_flagged = native_err.contains("LOGIN_REQUIRED") || native_err.contains("not a bot");
        let mut ydl_err = None;
        if self.cookie_jar().is_some() && !bot_flagged {
            match ytdlp_resolve::resolve(video_id, self.cookie_jar()).await {
                Ok(info) => {
                    set_preferred_strategy(STRAT_YTDLP_COOKIES);
                    return Ok(info);
                }
                Err(e) => {
                    eprintln!("[yt-player] yt-dlp (signed-in) failed ({e}) — retrying anonymously");
                    ydl_err = Some(e);
                }
            }
        }
        match ytdlp_resolve::resolve(video_id, None).await {
            Ok(info) => {
                set_preferred_strategy(STRAT_YTDLP_ANON);
                Ok(info)
            }
            Err(anon_err) => Err(format!(
                "{native_err}; yt-dlp fallback also failed: {}",
                ydl_err.unwrap_or(anon_err)
            )),
        }
    }

    // Public surfaces — work anonymously. `cookies.as_deref().unwrap_or("")`
    // hands an empty header to the parser, which the lower-level
    // discover/mix `post` and innertube::browse now interpret as "skip
    // SAPISID auth headers" (see browse_maybe_auth / discover::post).

    pub async fn start_mix(&self, seed_video_id: &str) -> Result<Vec<Track>, String> {
        mix::start_mix(seed_video_id, self.cookies.as_deref().unwrap_or("")).await
    }

    pub async fn discover_home(&self) -> Result<discover::DiscoverHome, String> {
        discover::fetch_home(self.cookies.as_deref().unwrap_or("")).await
    }

    pub async fn discover_continuation(
        &self,
        token: &str,
    ) -> Result<discover::DiscoverHome, String> {
        discover::fetch_continuation(token, self.cookies.as_deref().unwrap_or("")).await
    }

    pub async fn fetch_album_tracks(&self, browse_id: &str) -> Result<Vec<Track>, String> {
        discover::fetch_album_tracks(browse_id, self.cookies.as_deref().unwrap_or("")).await
    }

    pub async fn fetch_artist(&self, channel_id: &str) -> Result<discover::YtArtist, String> {
        discover::fetch_artist(channel_id, self.cookies.as_deref().unwrap_or("")).await
    }

    /// Confirms the cookie session is actually signed in — InnerTube
    /// `/browse?browseId=VLLM` (Liked Music) is the canonical probe: it
    /// returns a `signInEndpoint`-bearing message renderer for anonymous
    /// callers and real playlist content for signed-in ones.
    pub async fn validate_cookies(&self) -> Result<(), String> {
        // Anonymous mode has no cookies to validate — succeed silently
        // so callers (settings probe, keepalive) treat it as healthy.
        let Some(cookies) = self.cookies.as_deref() else {
            return Ok(());
        };
        // Expiry is signalled by a SIGN-IN PROMPT, not by the absence of a
        // playlist shelf — a user with zero liked songs / saved playlists is
        // still validly signed in. Probing on the shelf alone produced false
        // "expired" errors (empty playlists, empty library). One transient
        // retry guards against a flaky single response.
        for attempt in 0..2 {
            match innertube::browse("VLLM", cookies).await {
                Ok(json) if is_signed_out(&json) => {
                    return Err(
                        "Sign-in prompt returned — YouTube cookies expired or signed out".into(),
                    );
                }
                Ok(_) => return Ok(()),
                Err(e) if attempt == 1 => return Err(e),
                Err(_) => {
                    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
                }
            }
        }
        Ok(())
    }

    /// True when no cookies are configured — the YT backend is in
    /// anonymous mode (Browse + play public surfaces work; Liked,
    /// Library Playlists, follow/like mutations are disabled).
    /// Used by UI gates to swap auth-only views for a 'sign in to
    /// enable' empty state.
    pub fn is_anonymous(&self) -> bool {
        // with_cookies normalizes "" → None, so absence of cookies is
        // exactly anonymous mode.
        self.cookies.is_none()
    }
}

/// True only when the response carries a sign-in prompt — the real signal
/// that cookies expired. A `signInEndpoint` anywhere, or a button/message
/// pointing at the accounts page, both count. Absence of content does NOT.
fn is_signed_out(json: &Value) -> bool {
    fn walk(v: &Value) -> bool {
        match v {
            Value::Object(map) => {
                if map.contains_key("signInEndpoint") {
                    return true;
                }
                map.values().any(walk)
            }
            Value::Array(arr) => arr.iter().any(walk),
            _ => false,
        }
    }
    walk(json)
}

fn has_playlist_shelf(json: &Value) -> bool {
    json.pointer(
        "/contents/twoColumnBrowseResultsRenderer/secondaryContents/sectionListRenderer/contents",
    )
    .or_else(|| {
        json.pointer(
            "/contents/singleColumnBrowseResultsRenderer/tabs/0/tabRenderer/content/sectionListRenderer/contents",
        )
    })
    .and_then(|v| v.as_array())
    .map(|arr| arr.iter().any(|shelf| shelf.get("musicPlaylistShelfRenderer").is_some()))
    .unwrap_or(false)
}

impl Default for YouTubeMusicClient {
    fn default() -> Self {
        Self::new()
    }
}
