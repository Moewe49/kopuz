//! A second opinion for the queue.
//!
//! YouTube's own radio (`ytmusic::mix`) is one similarity graph, and a good
//! one — it is built from what people actually play next, not from genre tags.
//! But blending it with itself changes nothing, so a real blend needs a source
//! that disagrees with it. This module is that source: the artist graph from
//! ListenBrainz, which answers "who else do these listeners play" from a
//! completely different population and a completely different signal.
//!
//! Both are behavioural rather than categorical, which is the point. A genre
//! shelf can only offer more of the same label; two co-listening graphs
//! disagreeing with each other is how something arrives that is close without
//! being the same.
//!
//! Nothing here costs the listener anything or needs an account: MusicBrainz
//! and the ListenBrainz *labs* endpoint are both anonymous.

use std::collections::HashSet;
use std::path::Path;

use reader::models::Track;

use crate::ytmusic::SOURCE_PREFIX;

/// Distinct artists to expand from. Each one costs a MusicBrainz lookup at a
/// 1.1 s rate gate plus a ListenBrainz call, so this is the latency budget for
/// the whole blend — three is about two and a half seconds.
const MAX_SEED_ARTISTS: usize = 3;
/// Related artists to keep per seed artist.
const RELATED_PER_SEED: usize = 6;
/// Tracks to audition per related artist. More than a couple is repetitive:
/// they are all by the same artist.
const TRACKS_PER_ARTIST: usize = 2;

/// Identity of a track for de-duplication.
///
/// A YouTube path yields its videoId, so the same song under two different
/// thumbnail tags collapses to one entry. **Anything else yields the whole
/// path** — the previous version returned an empty string for non-YouTube
/// paths and the caller then dropped those tracks entirely, which would have
/// silently deleted every SoundCloud or local track from a blended queue.
pub fn track_key(path: &Path) -> String {
    let s = path.to_string_lossy();
    s.strip_prefix(&format!("{SOURCE_PREFIX}:"))
        .and_then(|rest| rest.split(':').next())
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| s.to_string())
}

/// Round-robin interleave of several track lists, dropping repeats and
/// anything in `exclude`.
///
/// Round-robin rather than concatenation because the whole point of a blend is
/// that the listener meets both sources early. Appending source B after source
/// A means they never reach it.
///
/// Shorter lists simply run out; the weave does **not** pad from the longer
/// one. A "50/50" that quietly becomes 90/10 by track thirty is worse than an
/// honest thirty tracks, because the listener cannot tell that the second
/// source stopped.
pub fn weave(lists: &[Vec<Track>], exclude: &HashSet<String>) -> Vec<Track> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    let longest = lists.iter().map(|l| l.len()).max().unwrap_or(0);
    for i in 0..longest {
        for list in lists {
            let Some(track) = list.get(i) else { continue };
            let key = track_key(&track.path);
            if exclude.contains(&key) {
                continue;
            }
            if seen.insert(key) {
                out.push(track.clone());
            }
        }
    }
    out
}

/// Artist names worth expanding, most frequent first.
///
/// Frequency across the seeds, not just the last track: a queue that ended on
/// one guest feature should still be read as the artist it was mostly about.
fn seed_artists(seeds: &[Track]) -> Vec<String> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for track in seeds {
        let name = scrobble::similar::clean_artist(&track.artist);
        if name.is_empty() {
            continue;
        }
        match counts
            .iter_mut()
            .find(|(n, _)| n.eq_ignore_ascii_case(&name))
        {
            Some((_, c)) => *c += 1,
            None => counts.push((name, 1)),
        }
    }
    // Ties broken by name so the same queue always expands the same artists —
    // a blend that differs between two identical runs cannot be reasoned about.
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    counts.into_iter().map(|(n, _)| n).collect()
}

/// Names of artists related to the seeds, excluding the seeds themselves.
///
/// Separated from the track search so it can be tested and reused: the home
/// screen wants these names to label a mix, not only to search with.
pub async fn related_artists(seeds: &[Track], want: usize) -> Vec<String> {
    let http = crate::ytmusic::innertube::http_client().clone();
    let names = seed_artists(seeds);
    let known: HashSet<String> = names.iter().map(|n| n.to_lowercase()).collect();

    let mut out: Vec<String> = Vec::new();
    for name in names.iter().take(MAX_SEED_ARTISTS) {
        if out.len() >= want {
            break;
        }
        // A busy MusicBrainz is not the same as an unknown artist; neither is
        // actionable here, but only one of them is worth a log line.
        let mbid = match scrobble::similar::artist_mbid(&http, name).await {
            scrobble::similar::Lookup::Found(id) => id,
            scrobble::similar::Lookup::NotFound => continue,
            scrobble::similar::Lookup::Unavailable => {
                tracing::debug!("musicbrainz unavailable for {name}, skipping this seed");
                continue;
            }
        };
        for artist in scrobble::similar::similar_artists(&http, &mbid, 40).await {
            let key = artist.name.to_lowercase();
            if known.contains(&key) || out.iter().any(|n| n.to_lowercase() == key) {
                continue;
            }
            out.push(artist.name);
            if out.len() >= want {
                break;
            }
        }
    }
    out
}

/// Tracks by artists related to the seeds, filtered down to things that are
/// actually songs.
///
/// `exclude` holds [`track_key`]s the caller already has — the current queue
/// and the listening history — so the result is genuinely new rather than a
/// reshuffle of what is already playing.
pub async fn from_artist_graph(
    seeds: &[Track],
    exclude: &HashSet<String>,
    want: usize,
) -> Vec<Track> {
    if seeds.is_empty() || want == 0 {
        return Vec::new();
    }
    let artists = related_artists(seeds, RELATED_PER_SEED * MAX_SEED_ARTISTS).await;
    if artists.is_empty() {
        return Vec::new();
    }

    let yt = crate::ytmusic::YouTubeMusicClient::with_cookies(String::new());
    let mut out: Vec<Track> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for artist in &artists {
        if out.len() >= want {
            break;
        }
        let Ok(found) = yt.search_tracks(artist).await else {
            continue;
        };
        let mut taken = 0usize;
        for track in found {
            if taken >= TRACKS_PER_ARTIST || out.len() >= want {
                break;
            }
            // Compilations, hour-long mixes and reaction uploads outrank real
            // songs on any similarity measure, because they contain a bit of
            // everything. They have to go before the ranking, not after.
            if reader::candidates::reject(&track.title).is_some() {
                continue;
            }
            let key = track_key(&track.path);
            if exclude.contains(&key) || !seen.insert(key) {
                continue;
            }
            taken += 1;
            out.push(track);
        }
    }
    out
}

/// Seed videos for the radio side, spread across the finished queue.
///
/// Spread rather than "the last few", because a continuation seeded only from
/// the tail drifts to whatever single song happened to end the playlist. Both
/// engines used to compute this separately — `sample_evenly` on desktop and a
/// hand-rolled index walk on Android — with the same intent and two chances to
/// be wrong.
fn spread_seeds(ids: &[String], want: usize) -> Vec<String> {
    if ids.is_empty() || want == 0 {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::with_capacity(want);
    // Deduped on the short path too. A queue of four tracks where three are
    // the same song would otherwise seed the same radio three times and
    // interleave it with itself — a blend that is not one.
    if ids.len() <= want {
        for id in ids {
            if !out.contains(id) {
                out.push(id.clone());
            }
        }
        return out;
    }
    for k in 0..want {
        // First and last are always included; the rest sit evenly between.
        let pos = k * (ids.len() - 1) / (want - 1).max(1);
        let id = &ids[pos.min(ids.len() - 1)];
        if !out.contains(id) {
            out.push(id.clone());
        }
    }
    out
}

/// Tracks to ask the artist graph for. The weave alternates, so this is what
/// decides how far into the continuation the blend actually reaches: twelve
/// means the first two dozen tracks alternate, and the radio carries on alone
/// after that.
const GRAPH_SHARE: usize = 12;

/// How long the artist graph gets before the continuation goes ahead without
/// it.
///
/// This runs at end-of-queue, with the music already stopped and the listener
/// waiting. The graph needs a MusicBrainz lookup per seed artist behind a
/// 1.1 s rate gate, plus a search per related artist — several seconds on a
/// good day and unbounded on a bad one. A silent extra wait before the music
/// resumes would be a worse regression than a less varied queue.
const GRAPH_BUDGET: std::time::Duration = std::time::Duration::from_secs(8);

/// The continuation for a queue that just finished: YouTube's radio for the
/// tracks that played, woven together with tracks reached through the artist
/// graph.
///
/// Both sources are fetched concurrently, so the wait is the slower of the two
/// rather than their sum. If the graph misses [`GRAPH_BUDGET`] the radio plays
/// alone — the listener gets music on time, and the reason is logged rather
/// than swallowed.
pub async fn blended_continuation(
    finished: &[Track],
    cookies: &str,
    exclude: &HashSet<String>,
) -> Vec<Track> {
    // Only YouTube tracks can seed a YouTube radio. A local file or a
    // SoundCloud track still counts towards the artist graph below, which
    // works from names rather than ids.
    let prefix = format!("{SOURCE_PREFIX}:");
    let ids: Vec<String> = finished
        .iter()
        .filter(|t| t.path.to_string_lossy().starts_with(&prefix))
        .map(|t| track_key(&t.path))
        .collect();
    let seeds = spread_seeds(&ids, 4);
    if seeds.is_empty() {
        return Vec::new();
    }

    let radio_fut = crate::ytmusic::mix::start_mix_multi(&seeds, cookies);
    let graph_fut = tokio::time::timeout(
        GRAPH_BUDGET,
        from_artist_graph(finished, exclude, GRAPH_SHARE),
    );
    let (radio, graph) = tokio::join!(radio_fut, graph_fut);

    let radio = radio.unwrap_or_default();
    let graph = match graph {
        Ok(g) => g,
        Err(_) => {
            tracing::debug!("artist graph missed its budget; continuing on radio alone");
            Vec::new()
        }
    };
    if graph.is_empty() {
        // Nothing to blend with — this is the previous behaviour, not a
        // failure, and it must stay exactly as good as it was.
        return weave(&[radio], exclude);
    }
    weave(&[radio, graph], exclude)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn track(path: &str, artist: &str, title: &str) -> Track {
        Track {
            path: PathBuf::from(path),
            album_id: String::new(),
            title: title.to_string(),
            artist: artist.to_string(),
            album: String::new(),
            duration: 180,
            khz: 0,
            bitrate: 0,
            track_number: None,
            disc_number: None,
            musicbrainz_release_id: None,
            musicbrainz_recording_id: None,
            musicbrainz_track_id: None,
            playlist_item_id: None,
            artists: vec![artist.to_string()],
        }
    }

    /// The same song under two thumbnail tags is one song. This is what the
    /// videoId key is for.
    #[test]
    fn a_youtube_track_is_keyed_by_its_video_id() {
        assert_eq!(track_key(Path::new("ytmusic:abc123")), "abc123");
        assert_eq!(
            track_key(Path::new("ytmusic:abc123:urlhex_deadbeef")),
            "abc123"
        );
    }

    /// The bug this key replaces: a non-YouTube path used to key as the empty
    /// string, and the caller dropped every such track. A blended queue would
    /// have silently lost all SoundCloud and local entries.
    #[test]
    fn a_non_youtube_track_keeps_its_whole_path_as_key() {
        for path in [
            "soundcloud:12345",
            "/home/me/music/song.flac",
            "C:\\Users\\me\\song.mp3",
            "ytmusic:", // malformed: prefix but no id
        ] {
            assert_eq!(track_key(Path::new(path)), path, "path: {path}");
            assert!(!track_key(Path::new(path)).is_empty());
        }
    }

    /// Both sources have to be reachable early. Appending B after A means a
    /// listener who stops after ten tracks never hears the second source at
    /// all — which is the entire point of blending.
    #[test]
    fn weave_alternates_rather_than_appends() {
        let a = vec![track("ytmusic:a1", "A", "1"), track("ytmusic:a2", "A", "2")];
        let b = vec![track("ytmusic:b1", "B", "1"), track("ytmusic:b2", "B", "2")];
        let out = weave(&[a, b], &HashSet::new());
        let ids: Vec<String> = out.iter().map(|t| track_key(&t.path)).collect();
        assert_eq!(ids, ["a1", "b1", "a2", "b2"]);
    }

    /// A shorter second source must run out honestly rather than be padded
    /// from the first, which would turn a stated 50/50 into 90/10 without
    /// saying so.
    #[test]
    fn a_shorter_source_runs_out_instead_of_being_padded() {
        let a: Vec<Track> = (0..5)
            .map(|i| track(&format!("ytmusic:a{i}"), "A", "x"))
            .collect();
        let b = vec![track("ytmusic:b0", "B", "y")];
        let out = weave(&[a, b], &HashSet::new());
        assert_eq!(out.len(), 6);
        let from_b = out.iter().filter(|t| t.artist == "B").count();
        assert_eq!(from_b, 1, "B must not be padded up");
        // And B still appears early, not at the end.
        assert_eq!(track_key(&out[1].path), "b0");
    }

    #[test]
    fn weave_drops_repeats_and_excluded_tracks() {
        let a = vec![
            track("ytmusic:same", "A", "1"),
            track("ytmusic:gone", "A", "2"),
        ];
        // Same video id, different thumbnail tag — one song, not two.
        let b = vec![track("ytmusic:same:urlhex_ff", "B", "1")];
        let exclude: HashSet<String> = ["gone".to_string()].into_iter().collect();
        let out = weave(&[a, b], &exclude);
        assert_eq!(out.len(), 1);
        assert_eq!(track_key(&out[0].path), "same");
    }

    #[test]
    fn weave_survives_empty_input() {
        assert!(weave(&[], &HashSet::new()).is_empty());
        assert!(weave(&[vec![], vec![]], &HashSet::new()).is_empty());
    }

    /// A queue is about the artist it mostly contained, not the one it
    /// happened to end on.
    #[test]
    fn seed_artists_are_ranked_by_how_often_they_appear() {
        let seeds = vec![
            track("ytmusic:1", "Charli XCX - Topic", "a"),
            track("ytmusic:2", "Charli XCX - Topic", "b"),
            track("ytmusic:3", "PinkPantheress - Topic", "c"),
            track("ytmusic:4", "One Hit Guest", "d"),
        ];
        let got = seed_artists(&seeds);
        assert_eq!(got[0], "Charli XCX", "most frequent artist must come first");
        assert!(got.contains(&"PinkPantheress".to_string()));
        // "- Topic" is stripped, or MusicBrainz would never match.
        assert!(got.iter().all(|n| !n.contains("Topic")));
    }

    /// Two identical queues must expand the same artists, or the blend cannot
    /// be reasoned about or reported on.
    #[test]
    fn seed_artist_order_is_stable_for_ties() {
        let seeds = vec![
            track("ytmusic:1", "Zara Larsson", "a"),
            track("ytmusic:2", "Artemas", "b"),
        ];
        let first = seed_artists(&seeds);
        assert_eq!(first, seed_artists(&seeds));
        assert_eq!(first, ["Artemas", "Zara Larsson"], "ties break by name");
    }

    /// A continuation seeded only from the tail drifts to whatever song
    /// happened to end the playlist. First and last must always be in.
    #[test]
    fn seeds_are_spread_across_the_queue() {
        let ids: Vec<String> = (0..10).map(|i| format!("v{i}")).collect();
        let got = spread_seeds(&ids, 4);
        assert_eq!(got, ["v0", "v3", "v6", "v9"]);
        assert_eq!(got.first().unwrap(), "v0");
        assert_eq!(got.last().unwrap(), "v9");
    }

    #[test]
    fn spreading_handles_queues_shorter_than_the_sample() {
        let ids = vec!["a".to_string(), "b".to_string()];
        assert_eq!(spread_seeds(&ids, 4), ["a", "b"]);
        assert_eq!(spread_seeds(&ids, 1), ["a"]);
        assert!(spread_seeds(&[], 4).is_empty());
        assert!(spread_seeds(&ids, 0).is_empty());
    }

    /// A queue that repeats one track must not yield four identical seeds —
    /// that would fetch the same radio four times and interleave it with
    /// itself.
    #[test]
    fn repeated_tracks_do_not_become_repeated_seeds() {
        let ids: Vec<String> = vec!["same".into(), "same".into(), "same".into(), "other".into()];
        let got = spread_seeds(&ids, 4);
        assert_eq!(got.len(), 2, "got {got:?}");
        assert!(got.contains(&"other".to_string()));
    }

    #[test]
    fn seeds_without_a_usable_artist_are_skipped_rather_than_queried() {
        let seeds = vec![
            track("ytmusic:1", "", "a"),
            track("ytmusic:2", "   ", "b"),
            track("ytmusic:3", "Real Artist", "c"),
        ];
        assert_eq!(seed_artists(&seeds), ["Real Artist"]);
    }
}
