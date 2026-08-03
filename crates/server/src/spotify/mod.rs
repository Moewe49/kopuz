//! Spotify playlist import. Two acquisition paths feed the same
//! YT Music matcher/cloner:
//!
//! 1. **Public URL** ([`embed`]) — scrapes the `open.spotify.com/embed`
//!    page for a playlist/album. No login, no API key. The embed ships
//!    roughly the first 100 tracks; bigger playlists need path 2.
//! 2. **Account connection** ([`auth`] + [`api`]) — OAuth PKCE with a
//!    user-supplied client id (free, created in Spotify's developer
//!    dashboard). Unlocks the user's own (private) playlists and Liked
//!    Songs, fully paginated.
//!
//! [`matcher`] resolves each Spotify track to a YT Music video id and
//! [`matcher::clone_to_ytmusic`] creates the playlist in the signed-in
//! YT Music account, so the result shows up in the normal library —
//! playable, editable, syncable like any other YT playlist.

pub mod api;
pub mod auth;
pub mod embed;
pub mod matcher;
pub mod pathfinder;
pub mod search;

/// One track as Spotify describes it — the matcher's input.
#[derive(Debug, Clone, PartialEq)]
pub struct SpotifyTrack {
    pub title: String,
    pub artists: Vec<String>,
    pub duration_secs: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpotifyPlaylist {
    pub name: String,
    pub tracks: Vec<SpotifyTrack>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpotifyEntityKind {
    Playlist,
    Album,
}

impl SpotifyEntityKind {
    pub fn as_path(&self) -> &'static str {
        match self {
            Self::Playlist => "playlist",
            Self::Album => "album",
        }
    }
}

/// Accepts the shapes people actually paste:
/// `https://open.spotify.com/playlist/<id>?si=…`,
/// `https://open.spotify.com/intl-de/album/<id>`,
/// `spotify:playlist:<id>`, or a bare 22-char base62 id (assumed
/// playlist). Returns the entity kind plus the bare id.
pub fn parse_spotify_url(input: &str) -> Option<(SpotifyEntityKind, String)> {
    let input = input.trim();

    let kind_of = |s: &str| match s {
        "playlist" => Some(SpotifyEntityKind::Playlist),
        "album" => Some(SpotifyEntityKind::Album),
        _ => None,
    };

    if let Some(rest) = input.strip_prefix("spotify:") {
        let mut parts = rest.split(':');
        let kind = kind_of(parts.next()?)?;
        let id = parts.next()?;
        return valid_id(id).then(|| (kind, id.to_string()));
    }

    let no_scheme = input
        .strip_prefix("https://")
        .or_else(|| input.strip_prefix("http://"))
        .unwrap_or(input);
    let path = no_scheme.strip_prefix("open.spotify.com/")?;
    let mut segments = path.split('/').filter(|s| !s.is_empty());
    let mut seg = segments.next()?;
    // Locale prefix like `intl-de` sits before the entity segment.
    if seg.starts_with("intl-") {
        seg = segments.next()?;
    }
    let kind = kind_of(seg)?;
    let id = segments.next()?.split(['?', '#']).next()?;
    valid_id(id).then(|| (kind, id.to_string()))
}

fn valid_id(id: &str) -> bool {
    id.len() >= 16 && id.chars().all(|c| c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_playlist_url() {
        let (kind, id) =
            parse_spotify_url("https://open.spotify.com/playlist/37i9dQZF1DXcBWIGoYBM5M").unwrap();
        assert_eq!(kind, SpotifyEntityKind::Playlist);
        assert_eq!(id, "37i9dQZF1DXcBWIGoYBM5M");
    }

    #[test]
    fn parses_url_with_locale_and_query() {
        let (kind, id) = parse_spotify_url(
            "https://open.spotify.com/intl-de/album/4aawyAB9vmqN3uQ7FjRGTy?si=abc123",
        )
        .unwrap();
        assert_eq!(kind, SpotifyEntityKind::Album);
        assert_eq!(id, "4aawyAB9vmqN3uQ7FjRGTy");
    }

    #[test]
    fn parses_spotify_uri() {
        let (kind, id) = parse_spotify_url("spotify:playlist:37i9dQZF1DXcBWIGoYBM5M").unwrap();
        assert_eq!(kind, SpotifyEntityKind::Playlist);
        assert_eq!(id, "37i9dQZF1DXcBWIGoYBM5M");
    }

    #[test]
    fn rejects_track_and_garbage() {
        assert!(
            parse_spotify_url("https://open.spotify.com/track/20jbSiX29FDX4oQxBXyUEi").is_none()
        );
        assert!(parse_spotify_url("https://example.com/playlist/x").is_none());
        assert!(parse_spotify_url("not a url").is_none());
    }
}
