//! Spotify → YT Music track matching and playlist cloning.
//!
//! Matching strategy: search YT Music for "title artist", then score
//! every candidate on normalized-title similarity, artist overlap and
//! duration proximity. Album tracks beat user uploads at equal score.
//! Anything under [`MIN_SCORE`] is reported as unmatched rather than
//! silently cloning the wrong song.

use reader::models::Track;
use tokio::task::JoinSet;

use super::{SpotifyPlaylist, SpotifyTrack};
use crate::ytmusic::YouTubeMusicClient;

/// Reject matches scoring below this. Tuned conservative: a wrong
/// song in a cloned playlist is worse than a gap the user can fill.
const MIN_SCORE: f64 = 0.55;
/// How many YT search candidates to score per Spotify track.
const CANDIDATE_LIMIT: usize = 8;
/// Parallel YT searches during the match phase.
const MATCH_CONCURRENCY: usize = 4;
/// Videos sent inline with the playlist/create call.
const CREATE_BATCH: usize = 50;
/// Videos added per `edit_playlist` request for the rest — batched so a big
/// import is a few requests instead of hundreds (which trip YT's 403).
const ADD_BATCH: usize = 50;

#[derive(Debug, Clone)]
pub struct TrackMatch {
    pub spotify: SpotifyTrack,
    /// `None` = no candidate cleared [`MIN_SCORE`].
    pub video_id: Option<String>,
    /// The winning YT track itself, so a caller that wants to *play* the match
    /// (rather than write it into a playlist) doesn't have to search again.
    /// `Some` exactly when `video_id` is.
    pub track: Option<Track>,
    pub matched_label: String,
    pub score: f64,
}

#[derive(Debug, Clone)]
pub enum CloneEvent {
    /// Fired per finished match lookup.
    Matching {
        done: usize,
        total: usize,
        current: String,
    },
    CreatingPlaylist,
    /// Fired per track added after the initial create batch.
    Adding {
        done: usize,
        total: usize,
    },
}

#[derive(Debug, Clone)]
pub struct CloneReport {
    pub playlist_id: String,
    pub playlist_name: String,
    pub total: usize,
    pub matched: usize,
    pub unmatched: Vec<SpotifyTrack>,
}

/// Resolve every Spotify track to a YT video id (bounded concurrency,
/// original order preserved).
pub async fn match_playlist<F>(
    cookies: Option<String>,
    tracks: &[SpotifyTrack],
    mut on_event: F,
) -> Vec<TrackMatch>
where
    F: FnMut(CloneEvent),
{
    let total = tracks.len();
    let mut results: Vec<Option<TrackMatch>> = vec![None; total];
    let mut done = 0usize;

    for (chunk_idx, chunk) in tracks.chunks(MATCH_CONCURRENCY).enumerate() {
        let mut set = JoinSet::new();
        for (offset, sp) in chunk.iter().enumerate() {
            let idx = chunk_idx * MATCH_CONCURRENCY + offset;
            let sp = sp.clone();
            let cookies = cookies.clone();
            set.spawn(async move {
                let m = match_one(cookies, &sp).await;
                (idx, m)
            });
        }
        while let Some(joined) = set.join_next().await {
            if let Ok((idx, m)) = joined {
                done += 1;
                on_event(CloneEvent::Matching {
                    done,
                    total,
                    current: m.spotify.title.clone(),
                });
                results[idx] = Some(m);
            }
        }
    }

    results
        .into_iter()
        .enumerate()
        .map(|(i, m)| {
            m.unwrap_or_else(|| TrackMatch {
                spotify: tracks[i].clone(),
                video_id: None,
                track: None,
                matched_label: String::new(),
                score: 0.0,
            })
        })
        .collect()
}

async fn match_one(_cookies: Option<String>, sp: &SpotifyTrack) -> TrackMatch {
    // Match via ANONYMOUS YT search: matching by title/artist needs no
    // personalization, and an anonymous client does one search instead of the
    // signed-in path's cookie'd-then-retry (which doubles every lookup when the
    // session is stale) — much faster import, and immune to cookie expiry.
    let client = YouTubeMusicClient::new();
    let query = format!("{} {}", sp.title, sp.artists.join(" "));
    let candidates = client.search_tracks(&query).await.unwrap_or_default();

    let mut best: Option<(f64, &Track)> = None;
    for cand in candidates.iter().take(CANDIDATE_LIMIT) {
        let s = score(sp, cand);
        if best.as_ref().is_none_or(|(b, _)| s > *b) {
            best = Some((s, cand));
        }
    }
    match best {
        Some((s, t)) if s >= MIN_SCORE => TrackMatch {
            spotify: sp.clone(),
            video_id: video_id_of(t),
            track: Some(t.clone()),
            matched_label: format!("{} — {}", t.title, t.artist),
            score: s,
        },
        _ => TrackMatch {
            spotify: sp.clone(),
            video_id: None,
            track: None,
            matched_label: String::new(),
            score: best.map(|(s, _)| s).unwrap_or(0.0),
        },
    }
}

/// Match a whole Spotify playlist to *playable* YT tracks, in the original
/// order, dropping the ones nothing confident was found for. Same scoring and
/// concurrency as the import path — this is the "play it now" counterpart to
/// [`import_playlist`], for when the user wants to listen without cloning the
/// playlist into their YouTube account.
pub async fn match_playlist_to_tracks<F>(tracks: &[SpotifyTrack], on_event: F) -> Vec<Track>
where
    F: FnMut(CloneEvent),
{
    match_playlist(None, tracks, on_event)
        .await
        .into_iter()
        .filter_map(|m| m.track)
        .collect()
}

/// Match an arbitrary `(title, artists, duration)` — e.g. a SoundCloud track —
/// to a YT Music video. Returns the matched YT [`Track`] (path
/// `ytmusic:<vid>:…`) when a candidate clears [`MIN_SCORE`], else `None`.
/// Anonymous search; reuses the Spotify scoring so cross-service matching
/// behaves identically. Used to store SC/Spotify tracks as their YT equivalent
/// in a server playlist (which then syncs across devices).
pub async fn match_external_to_yt(
    title: &str,
    artists: &[String],
    duration_secs: u64,
) -> Option<Track> {
    let sp = SpotifyTrack {
        title: title.to_string(),
        artists: artists.to_vec(),
        duration_secs,
    };
    let client = YouTubeMusicClient::new();
    let query = format!("{} {}", title, artists.join(" "));
    let candidates = client.search_tracks(&query).await.unwrap_or_default();
    let mut best: Option<(f64, Track)> = None;
    for cand in candidates.into_iter().take(CANDIDATE_LIMIT) {
        let s = score(&sp, &cand);
        if best.as_ref().is_none_or(|(b, _)| s > *b) {
            best = Some((s, cand));
        }
    }
    best.filter(|(s, _)| *s >= MIN_SCORE).map(|(_, t)| t)
}

/// Create the playlist on the signed-in YT Music account. Returns the
/// new playlist id; the caller refreshes the library so it shows up.
pub async fn clone_to_ytmusic<F>(
    cookies: String,
    name: &str,
    matches: &[TrackMatch],
    mut on_event: F,
) -> Result<CloneReport, String>
where
    F: FnMut(CloneEvent),
{
    let yt = YouTubeMusicClient::with_cookies(cookies);
    let matched_ids: Vec<&str> = matches
        .iter()
        .filter_map(|m| m.video_id.as_deref())
        .collect();
    if matched_ids.is_empty() {
        return Err("No tracks could be matched on YouTube Music".into());
    }

    on_event(CloneEvent::CreatingPlaylist);
    let first: Vec<&str> = matched_ids.iter().take(CREATE_BATCH).copied().collect();
    let playlist_id = yt
        .create_playlist(name, "Imported from Spotify with Kopuz", &first)
        .await?;

    let rest = &matched_ids[first.len().min(matched_ids.len())..];
    let total_rest = rest.len();
    // Add the remaining tracks in BATCHES (one edit_playlist request per chunk),
    // not one request per track — hundreds of individual writes are what trip
    // YouTube's abuse 403. If a batch still gets throttled, retry it with
    // backoff instead of bailing, so big playlists fill completely. `added`
    // tracks what actually landed so the report is honest.
    let mut done = 0usize;
    let mut added = first.len();
    'batches: for chunk in rest.chunks(ADD_BATCH) {
        let mut attempt = 0u32;
        loop {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            match yt.add_videos_to_playlist(&playlist_id, chunk).await {
                Ok(()) => {
                    done += chunk.len();
                    added += chunk.len();
                    on_event(CloneEvent::Adding {
                        done,
                        total: total_rest,
                    });
                    break;
                }
                Err(e) => {
                    attempt += 1;
                    if attempt >= 4 {
                        // Throttled past our patience — stop, but keep (and
                        // report) everything added so far rather than failing.
                        eprintln!("[spotify-clone] add batch gave up after retries: {e}");
                        break 'batches;
                    }
                    // Back off progressively (2s, 4s, 6s) to let the limit reset.
                    tokio::time::sleep(std::time::Duration::from_secs(2 * attempt as u64)).await;
                }
            }
        }
    }

    Ok(CloneReport {
        playlist_id,
        playlist_name: name.to_string(),
        total: matches.len(),
        matched: added,
        unmatched: matches
            .iter()
            .filter(|m| m.video_id.is_none())
            .map(|m| m.spotify.clone())
            .collect(),
    })
}

/// Convenience: match + clone in one call.
pub async fn import_playlist<F>(
    cookies: String,
    playlist: &SpotifyPlaylist,
    mut on_event: F,
) -> Result<CloneReport, String>
where
    F: FnMut(CloneEvent),
{
    let matches = match_playlist(Some(cookies.clone()), &playlist.tracks, &mut on_event).await;
    clone_to_ytmusic(cookies, &playlist.name, &matches, on_event).await
}

/// YT track paths are `ytmusic:<videoId>:…` — segment 1 is the id.
fn video_id_of(t: &Track) -> Option<String> {
    let path = t.path.to_string_lossy().to_string();
    let id = path.split(':').nth(1)?.to_string();
    (!id.is_empty()).then_some(id)
}

fn score(sp: &SpotifyTrack, cand: &Track) -> f64 {
    let title = token_set(&normalize(&sp.title));
    let cand_title = token_set(&normalize(&cand.title));
    let title_sim = jaccard(&title, &cand_title);

    let sp_artists = normalize(&sp.artists.join(" "));
    let mut cand_artists = normalize(&cand.artist);
    if !cand.artists.is_empty() {
        cand_artists = normalize(&cand.artists.join(" "));
    }
    let artist_sim = jaccard(&token_set(&sp_artists), &token_set(&cand_artists));

    // Duration: full credit within 3s, fading to zero at 15s off.
    // Search rows without a parsed duration score neutral-low so a
    // strong title+artist match can still clear the bar.
    let dur_sim = if sp.duration_secs == 0 || cand.duration == 0 {
        0.4
    } else {
        let delta = sp.duration_secs.abs_diff(cand.duration) as f64;
        ((15.0 - delta) / 12.0).clamp(0.0, 1.0)
    };

    title_sim * 0.5 + artist_sim * 0.3 + dur_sim * 0.2
}

/// Lowercase, strip bracketed qualifiers and feat-credits, drop
/// punctuation. "Blinding Lights (feat. X) [Remaster]" → "blinding lights".
fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0i32;
    for c in s.chars() {
        match c {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = (depth - 1).max(0),
            _ if depth == 0 => {
                if c.is_alphanumeric() {
                    out.extend(c.to_lowercase());
                } else if c.is_whitespace() || c == '-' || c == '/' || c == '&' || c == ',' {
                    out.push(' ');
                }
            }
            _ => {}
        }
    }
    // Cut trailing feat-credits that aren't bracketed.
    for marker in [" feat ", " ft ", " featuring "] {
        if let Some(at) = out.find(marker) {
            out.truncate(at);
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn token_set(s: &str) -> std::collections::HashSet<String> {
    s.split_whitespace().map(|t| t.to_string()).collect()
}

fn jaccard(a: &std::collections::HashSet<String>, b: &std::collections::HashSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    // Containment bonus: "title" fully inside "title extended mix"
    // shouldn't be punished too hard for the extra tokens.
    let containment = inter / (a.len().min(b.len()).max(1) as f64);
    ((inter / union) + containment) / 2.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn yt(title: &str, artist: &str, duration: u64) -> Track {
        Track {
            path: PathBuf::from(format!("ytmusic:abc123:urlhex_00")),
            album_id: String::new(),
            title: title.into(),
            artist: artist.into(),
            album: String::new(),
            duration,
            khz: 0,
            bitrate: 0,
            track_number: None,
            disc_number: None,
            musicbrainz_release_id: None,
            musicbrainz_recording_id: None,
            musicbrainz_track_id: None,
            playlist_item_id: None,
            artists: vec![artist.into()],
        }
    }

    fn sp(title: &str, artists: &[&str], dur: u64) -> SpotifyTrack {
        SpotifyTrack {
            title: title.into(),
            artists: artists.iter().map(|s| s.to_string()).collect(),
            duration_secs: dur,
        }
    }

    #[test]
    fn normalizes_feat_and_brackets() {
        assert_eq!(
            normalize("Blinding Lights (feat. Someone) [2020 Remaster]"),
            "blinding lights"
        );
        // Apostrophes vanish entirely — "Don't" and "Dont" normalize
        // to the same token, which is what matching wants.
        assert_eq!(normalize("Don't Stop Me Now"), "dont stop me now");
    }

    #[test]
    fn exact_match_scores_high() {
        let s = score(
            &sp("Blinding Lights", &["The Weeknd"], 200),
            &yt("Blinding Lights", "The Weeknd", 201),
        );
        assert!(s > 0.9, "expected >0.9, got {s}");
    }

    #[test]
    fn wrong_song_scores_low() {
        let s = score(
            &sp("Blinding Lights", &["The Weeknd"], 200),
            &yt("Watermelon Sugar", "Harry Styles", 174),
        );
        assert!(s < MIN_SCORE, "expected <{MIN_SCORE}, got {s}");
    }

    #[test]
    fn cover_version_with_wrong_artist_stays_below_threshold() {
        let s = score(
            &sp("Blinding Lights", &["The Weeknd"], 200),
            &yt("Blinding Lights (Piano Cover)", "Random Pianist", 260),
        );
        assert!(s < 0.75, "cover scored suspiciously high: {s}");
    }

    #[test]
    fn parses_video_id_from_path() {
        assert_eq!(video_id_of(&yt("x", "y", 1)).as_deref(), Some("abc123"));
    }
}
