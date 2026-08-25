//! Several mixes for several tastes.
//!
//! The home screen already had a "Made for you" shelf, and it did the thing
//! this replaces: it found the listener's most-played *genre* and showed random
//! albums carrying that tag. Two tracks under one genre label can be nothing
//! alike, and two that belong together often carry different labels, so a genre
//! shelf can only ever offer more of the same word.
//!
//! # Telling one direction from another without listening to anything
//!
//! Making several mixes is easy; making several *different* mixes is the
//! problem. Anchoring on the five most-played tracks usually produces five
//! views of one taste, because the most-played tracks tend to be similar to
//! each other — that is why they are the most-played.
//!
//! The trick here is to let the radios answer it. Fetch a radio for each
//! candidate anchor, then measure how much the resulting track sets overlap.
//! Two anchors whose radios share most of their tracks *are* the same
//! direction, whatever their titles say; two whose radios barely touch are
//! genuinely different corners. That is a real measurement on real data, and it
//! needs no audio model, no genre tags and no extra requests — the radios had
//! to be fetched anyway, since they are the mixes.

use std::collections::HashSet;

use reader::models::Track;
use serde::{Deserialize, Serialize};

use crate::recommend::track_key;

/// Anchors to try. Each costs one radio request, paced, so this is the wall
/// clock of a refresh more than anything else.
const ANCHORS_TRIED: usize = 8;
/// Mixes to keep. More than a handful stops being a choice and starts being a
/// list to scroll past.
const MAX_MIXES: usize = 4;
/// Above this share of tracks in common, two radios are the same direction.
///
/// Jaccard, so 0.25 means a quarter of the union is shared. Chosen to be
/// forgiving: YouTube radios for two genuinely different anchors still share
/// the odd crossover track, and rejecting a real direction costs the listener a
/// whole mix while admitting a near-duplicate only costs one tile.
const SAME_DIRECTION: f32 = 0.25;
/// A mix shorter than this is not worth a tile.
const MIN_MIX_LEN: usize = 8;
/// Tracks kept per mix.
pub const MIX_LEN: usize = 30;

/// One generated mix, as persisted between launches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Mix {
    /// Stable across regenerations with the same anchor, so the UI can keep a
    /// tile in place instead of reshuffling the shelf under the cursor.
    pub id: String,
    /// What the listener sees. Built from the artists inside, not from a genre.
    pub name: String,
    // No cover field: the thumbnail is already encoded in the first track's
    // path, and every surface in the app decodes it the same way through
    // `utils::jellyfin_image`. Storing a second copy here would duplicate that
    // decoding in a crate that has no business doing UI.
    pub tracks: Vec<Track>,
}

/// Everything the home screen needs, plus when it was made.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MixSet {
    pub mixes: Vec<Mix>,
    /// Unix seconds. 0 means "never generated".
    pub generated: u64,
    /// Feature version of the vectors these were built from, or 0 when they
    /// came from the radio path instead.
    ///
    /// Without this a set stays "fresh" for a day even after the vectors
    /// underneath it were corrected — which is exactly what happened: the
    /// spectrogram fix landed, every vector was recomputed, and the shelf kept
    /// serving mixes built from the wrong ones because they were only an hour
    /// old. A mix set has no other way to know what it was made from.
    #[serde(default)]
    pub feature_version: u8,
    /// Relay version this set was fetched at, or 0 when it was built here.
    ///
    /// Two jobs. It is what a device sends back to ask "is there anything
    /// newer", so a phone on mobile data pays a few hundred bytes to learn
    /// there is not. And a non-zero value marks the set as somebody else's
    /// work, which is the only thing standing between a phone and the
    /// following: a set built from audio carries a `feature_version` the
    /// phone cannot match, [`MixSet::is_stale`] therefore calls it stale the
    /// instant it arrives, and the phone replaces good mixes with radio ones.
    #[serde(default)]
    pub relay_version: u64,
}

/// What a device should do about its mixes when it starts.
///
/// This lives here rather than in the home screen because it is the part most
/// likely to be quietly wrong, and a screen cannot be tested. The specific
/// wrongness it guards against is described on [`MixAction::Fetch`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MixAction {
    /// What is already held is current. Do nothing.
    Keep,
    /// Ask the relay, saying which version is already held so it can answer
    /// "nothing new" in a few hundred bytes instead of fifty kilobytes.
    ///
    /// A device with no vectors always asks, and asks *before* consulting
    /// [`MixSet::is_stale`] -- because a set built from audio carries a
    /// feature version such a device can never match, so `is_stale` calls it
    /// stale the moment it arrives. Left to that, a phone would replace real
    /// measurements with radio guesses on every single launch, and the mixes
    /// on the phone would never once match the mixes on the desktop.
    Fetch { have: u64 },
    /// Build here, from vectors if there are any and from radios if not.
    Build,
}

/// Decide, given what this device has and what it can do.
///
/// `vectors_version` is [`reader::vectors::FEATURE_VERSION`] when this device
/// has analysed audio and 0 when it has not -- which is also what says whether
/// it authors mix sets or reads them. That is a fact about the device rather
/// than a setting, so there is nothing for anyone to configure wrongly: a
/// phone has no ONNX runtime and never publishes, and a desktop never
/// overwrites its own measurements with a copy of them.
///
/// Call it again with `relay_configured = false` when the relay turns out to
/// have nothing, or to be out of reach.
pub fn decide(
    current: &MixSet,
    now_secs: u64,
    vectors_version: u8,
    relay_configured: bool,
) -> MixAction {
    if vectors_version == 0 && relay_configured {
        return MixAction::Fetch {
            have: current.relay_version,
        };
    }
    if current.is_stale(now_secs, vectors_version) {
        MixAction::Build
    } else {
        MixAction::Keep
    }
}

/// How often to rebuild. A day: long enough that the shelf is a fixture the
/// listener can return to and recognise, short enough to follow a taste that
/// is moving.
pub const REFRESH_SECS: u64 = 24 * 60 * 60;

/// How long to wait before trying again after a run that produced nothing.
///
/// An empty result has to be recorded, or the attempt is not remembered and
/// every visit to the home screen re-fires the whole paced burst — eight
/// requests, several seconds, forever, for exactly the listener who has too
/// little history for it to work. But a day is too long to wait for someone
/// who is a few plays short, so a fruitless run is retried within the hour.
pub const RETRY_SECS: u64 = 60 * 60;

impl MixSet {
    /// `vectors_version` is what the caller has available right now:
    /// [`reader::vectors::FEATURE_VERSION`] when there are vectors, 0 when
    /// there are none. A mismatch means these mixes describe different data.
    pub fn is_stale(&self, now_secs: u64, vectors_version: u8) -> bool {
        if self.feature_version != vectors_version {
            return true;
        }
        // 0 means never generated, and must not be read as "just now" — which
        // is what an age subtraction says when the clock is also near zero.
        if self.generated == 0 {
            return true;
        }
        let age = now_secs.saturating_sub(self.generated);
        if self.mixes.is_empty() {
            age >= RETRY_SECS
        } else {
            age >= REFRESH_SECS
        }
    }
}

/// Share of tracks two lists have in common, as Jaccard: shared over union.
///
/// Jaccard rather than raw overlap count, because a radio that happens to be
/// long would otherwise look similar to everything.
fn overlap(a: &HashSet<String>, b: &HashSet<String>) -> f32 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let shared = a.intersection(b).count() as f32;
    let union = a.union(b).count() as f32;
    shared / union
}

/// How many of a mix's tracks decide its name.
const NAME_FROM_TOP: usize = 10;

/// Artists among the first `window` tracks, most frequent first.
///
/// Ties break by name, so regenerating an unchanged mix keeps its title.
fn artist_counts(tracks: &[Track], window: usize) -> Vec<(String, usize)> {
    let mut counts: Vec<(String, usize)> = Vec::new();
    for t in tracks.iter().take(window) {
        let name = scrobble::similar::clean_artist(&t.artist);
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
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    counts
}

/// Name a mix after the artists actually inside it, avoiding names already
/// spent on an earlier mix.
///
/// Two names, not one: a single name reads as that artist's own radio, which is
/// not what this is. Two says "this corner", which is what it means.
///
/// `taken` matters more than it looks. Measured on a real history, the four
/// mixes came out as "Addison Rae & Ariana Grande", "Charli xcx & Addison Rae"
/// and "Ariana Grande & Addison Rae" — three tiles sharing a name and two of
/// them the same pair reordered. Their contents were genuinely different
/// (pairwise overlap 0.11 to 0.20), but a shelf that reads like that looks
/// broken regardless of what is inside. So each mix is named after the most
/// frequent artist nobody has claimed yet, falling back to the plain top two
/// only when everything is taken.
fn name_for(tracks: &[Track], taken: &HashSet<String>) -> String {
    // Only the most typical tracks get a say. Tracks arrive in typicality
    // order, and an artist cap flattens the frequencies across a full mix — so
    // counting all thirty made the name essentially alphabetical. Measured:
    // a mix of The Weeknd, Pastel Ghost and Paramore came out titled
    // "Charli XCX & Katy Perry", neither of whom was anywhere near the front.
    let mut counts = artist_counts(tracks, NAME_FROM_TOP);
    // If nothing in the typical window is still unclaimed, look at the whole
    // mix before settling for a title another tile already carries. A repeated
    // title is worse than a slightly less typical one.
    if counts
        .iter()
        .all(|(n, _)| taken.contains(&n.to_lowercase()))
    {
        counts = artist_counts(tracks, tracks.len());
    }
    let unclaimed: Vec<&String> = counts
        .iter()
        .map(|(n, _)| n)
        .filter(|n| !taken.contains(&n.to_lowercase()))
        .collect();
    // Prefer unclaimed names; if only one is left, pair it with the mix's own
    // top artist rather than repeating another tile's whole title.
    let picked: Vec<&String> = if unclaimed.len() >= 2 {
        unclaimed.into_iter().take(2).collect()
    } else if let Some(first) = unclaimed.into_iter().next() {
        // The unclaimed name leads. Putting the filler first would print the
        // shared artist at the head of every remaining tile, which is the exact
        // shelf this function exists to avoid.
        std::iter::once(first)
            .chain(
                counts
                    .iter()
                    .map(|(n, _)| n)
                    .filter(|n| *n != first)
                    .take(1),
            )
            .collect()
    } else {
        counts.iter().map(|(n, _)| n).take(2).collect()
    };
    match (picked.first(), picked.get(1)) {
        (Some(a), Some(b)) => format!("{a} & {b}"),
        (Some(a), None) => (*a).clone(),
        _ => "Mix".to_string(),
    }
}

/// Pick the radios that represent genuinely different directions and turn them
/// into mixes.
///
/// `candidates` is `(anchor id, that anchor's radio)`, best anchor first.
/// Anchors are taken in order and kept only when their radio does not already
/// look like one that was kept — so the strongest taste gets its mix, and each
/// later one has to earn its place by being different.
pub fn distinct_mixes(candidates: &[(String, Vec<Track>)]) -> Vec<Mix> {
    let mut kept: Vec<(HashSet<String>, Mix)> = Vec::new();
    let mut named: HashSet<String> = HashSet::new();
    for (anchor, tracks) in candidates {
        if kept.len() >= MAX_MIXES {
            break;
        }
        if tracks.len() < MIN_MIX_LEN {
            continue;
        }
        let keys: HashSet<String> = tracks.iter().map(|t| track_key(&t.path)).collect();
        if kept
            .iter()
            .any(|(prev, _)| overlap(prev, &keys) >= SAME_DIRECTION)
        {
            continue;
        }
        let tracks: Vec<Track> = tracks.iter().take(MIX_LEN).cloned().collect();
        let name = name_for(&tracks, &named);
        for part in name.split(" & ") {
            named.insert(part.to_lowercase());
        }
        kept.push((
            keys,
            Mix {
                id: format!("mix:{anchor}"),
                name,
                tracks,
            },
        ));
    }
    kept.into_iter().map(|(_, m)| m).collect()
}

/// Gap between radio requests.
///
/// A burst of InnerTube calls is what trips YouTube's bot gate — the Android
/// engine learned this the hard way, where roughly a hundred resolves in five
/// seconds got every one of them answered with "Sign in to confirm you're not
/// a bot". This runs in the background where nobody is waiting, so there is no
/// reason to hurry.
const REQUEST_SPACING: std::time::Duration = std::time::Duration::from_millis(600);

/// Build the mix shelf from the listener's most-played tracks.
///
/// `anchor_ids` are YouTube video ids, most-played first — the caller owns the
/// history, this owns the fetching. `now_secs` is passed in rather than read
/// from the clock so the result is reproducible in a test.
pub async fn generate(anchor_ids: &[String], cookies: &str, now_secs: u64) -> MixSet {
    let mut candidates: Vec<(String, Vec<Track>)> = Vec::new();
    for anchor in anchor_ids.iter().take(ANCHORS_TRIED) {
        match crate::ytmusic::mix::start_mix(anchor, cookies).await {
            Ok(tracks) if !tracks.is_empty() => {
                // Compilations and hour-long uploads look plausible to any
                // similarity measure because they contain a bit of everything.
                // They have to go before the selection, not after.
                let tracks: Vec<Track> = tracks
                    .into_iter()
                    .filter(|t| reader::candidates::reject(&t.title).is_none())
                    .collect();
                candidates.push((anchor.clone(), tracks));
            }
            Ok(_) => {}
            Err(e) => tracing::debug!("mix anchor {anchor} failed: {e}"),
        }
        tokio::time::sleep(REQUEST_SPACING).await;
    }
    MixSet {
        mixes: distinct_mixes(&candidates),
        generated: now_secs,
        // Built from radios, not from audio: anything analysed later should
        // supersede this rather than wait out the refresh interval.
        feature_version: 0,
        // Built here, so no relay version. A device that receives a set
        // stamps this itself from what the relay reported.
        relay_version: 0,
    }
}

/// What is known about a track besides its vector: artist, title, cover URL.
pub type Labels = std::collections::HashMap<String, (String, String, String)>;

/// Read the label sidecar, tolerating the earlier two-field shape.
///
/// The first version stored artist and title only. Parsing that file strictly
/// as three fields yields an empty map, and an empty map means every mix loses
/// its names silently — the sort of failure that looks like the feature simply
/// stopped working. Old entries come back with an empty cover, which the
/// backfill in the analysis job then fills in.
pub fn load_labels(path: &std::path::Path) -> Labels {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    if let Ok(full) = serde_json::from_str::<Labels>(&text) {
        return full;
    }
    serde_json::from_str::<std::collections::HashMap<String, (String, String)>>(&text)
        .map(|old| {
            old.into_iter()
                .map(|(id, (a, t))| (id, (a, t, String::new())))
                .collect()
        })
        .unwrap_or_default()
}

/// Tracks by one artist inside a single mix.
///
/// A taste direction genuinely can be mostly one artist — that is what a sound
/// is — but thirty tracks by one of them is that artist's discography, not a
/// mix. Measured on the real library, one direction held 82 tracks that were
/// overwhelmingly Charli XCX.
const MAX_PER_ARTIST: usize = 3;

/// Name a mix after what the model says it is.
///
/// Better than naming it after two of its artists, which was the first
/// attempt: those names repeat across tiles, say nothing about the sound, and
/// go stale the moment the mix is regenerated with slightly different members.
/// A style is what the listener is actually choosing between.
///
/// `taken` holds the styles earlier tiles already claimed, so a shelf never
/// shows the same word twice — the next-strongest style is used instead.
fn style_name(centroid: &[f32], taken: &HashSet<String>) -> String {
    let mut ranked: Vec<(usize, f32)> = centroid.iter().copied().enumerate().collect();
    ranked.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });
    for (index, _) in ranked.iter().take(12) {
        let sub = reader::styles::subgenre(*index);
        if sub.is_empty() || taken.contains(&sub.to_lowercase()) {
            continue;
        }
        return sub.to_string();
    }
    // Everything in the top dozen is spoken for — fall back to the broad genre
    // rather than repeating a tile's title.
    ranked
        .first()
        .map(|(i, _)| reader::styles::genre(*i).to_string())
        .filter(|g| !g.is_empty())
        .unwrap_or_else(|| "Mix".to_string())
}

/// Mixes built from analysed audio.
///
/// This is what the radio-overlap path in [`generate`] was standing in for.
/// Overlap between two YouTube radios is a proxy for "these are the same kind
/// of music"; the vectors are the thing itself. On the real library the
/// difference was not subtle — the proxy produced four barely distinguishable
/// pop tiles, the vectors produced Evanescence/Flyleaf/Wisp,
/// Weeknd/Pastel Ghost/Paramore, Sabrina Carpenter/Chappell Roan/Clairo and
/// Charli XCX.
///
/// It also costs nothing: the tracks are already known and the vectors already
/// say how they group, so there is not a single request.
pub fn from_vectors(
    store: &reader::vectors::VectorStore,
    labels: &Labels,
    now_secs: u64,
    seed: u64,
) -> MixSet {
    let (ids, vectors) = store.matrix();
    if vectors.len() < MIN_MIX_LEN {
        return MixSet {
            mixes: Vec::new(),
            generated: now_secs,
            feature_version: reader::vectors::FEATURE_VERSION,
            relay_version: 0,
        };
    }

    let k = reader::taste::best_k(&vectors, MAX_MIXES, seed);
    let clusters = reader::taste::cluster(&vectors, k, seed);
    let mut named: HashSet<String> = HashSet::new();
    let mut mixes = Vec::new();

    for cluster in &clusters {
        // Members arrive most-typical-first, so taking from the front means a
        // mix opens with what defines it.
        let mut per_artist: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        // The same song reaches the library under several uploads — a topic
        // channel and a VEVO video carry different ids and the same music.
        // Keyed on artist and title, because the ids genuinely differ.
        let mut seen_songs: HashSet<String> = HashSet::new();
        let mut tracks: Vec<Track> = Vec::new();
        for &i in &cluster.members {
            if tracks.len() >= MIX_LEN {
                break;
            }
            let id = &ids[i];
            // A vector with no label would render as a raw video id. Those
            // come from tracks analysed before labels were recorded; skipping
            // them costs one entry out of a few hundred and keeps every tile
            // readable.
            let Some((artist, title, cover)) = labels.get(id).cloned() else {
                continue;
            };
            // "- Topic" is a YouTube channel suffix, not part of anyone's
            // name, and it was showing on every row.
            let artist = scrobble::similar::clean_artist(&artist);
            let song = song_key(&artist, &title);
            if !seen_songs.insert(song) {
                continue;
            }
            let key = artist.to_lowercase();
            let seen = per_artist.entry(key).or_insert(0);
            if *seen >= MAX_PER_ARTIST {
                continue;
            }
            *seen += 1;
            tracks.push(track_from(id, &artist, &title, &cover));
        }
        if tracks.len() < MIN_MIX_LEN {
            continue;
        }
        let name = style_name(&cluster.centroid, &named);
        named.insert(name.to_lowercase());
        mixes.push(Mix {
            // Keyed by the cluster's most typical track, so the tile keeps its
            // identity across a regeneration that finds the same direction.
            id: format!("mix:{}", ids[cluster.members[0]]),
            name,
            tracks,
        });
    }

    MixSet {
        mixes,
        generated: now_secs,
        feature_version: reader::vectors::FEATURE_VERSION,
        relay_version: 0,
    }
}

/// Identity of a song across uploads: artist and title, stripped of the
/// decoration that differs between a topic channel and a music video.
fn song_key(artist: &str, title: &str) -> String {
    let clean = |s: &str| -> String {
        let lower = s.to_lowercase();
        // Cut the bracketed suffixes an upload adds — "(Official Music
        // Video)", "[OFFICIAL VIDEO]", "(Lyrics)" — then keep letters and
        // digits only, so punctuation and spacing cannot split one song in two.
        let head = lower.split(['(', '[']).next().unwrap_or(&lower).to_string();
        head.chars().filter(|c| c.is_alphanumeric()).collect()
    };
    // A VEVO upload often repeats the artist inside the title. Comparing the
    // title alone would then miss the match, so both halves are normalised and
    // the artist is removed from the title if it is echoed there.
    //
    // The channel suffix is stripped here rather than relying on the caller
    // having done it: "AllyNicholasVEVO" and "Ally Nicholas - Topic" are the
    // same artist, and a key that depends on which one arrived is not a key.
    let a = clean(&scrobble::similar::clean_artist(artist));
    let mut t = clean(title);
    if !a.is_empty() && t.starts_with(&a) {
        t = t[a.len()..].to_string();
    }
    format!("{a}|{t}")
}

/// A playable track from what the vector store and its labels know.
///
/// The cover URL rides in the path, the way every other surface in the app
/// expects it. Building the path without it — which is what this did — leaves
/// every tile and every row as a grey placeholder, since that is where the
/// artwork is decoded from.
fn track_from(id: &str, artist: &str, title: &str, cover: &str) -> Track {
    let path = if cover.is_empty() {
        format!("{}:{id}", crate::ytmusic::SOURCE_PREFIX)
    } else {
        format!(
            "{}:{id}:{}",
            crate::ytmusic::SOURCE_PREFIX,
            crate::ytmusic::search::encode_url_tag(cover)
        )
    };
    Track {
        path: std::path::PathBuf::from(path),
        album_id: String::new(),
        title: title.to_string(),
        artist: artist.to_string(),
        album: String::new(),
        duration: 0,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn track(id: &str, artist: &str) -> Track {
        Track {
            path: PathBuf::from(format!("ytmusic:{id}")),
            album_id: String::new(),
            title: format!("song {id}"),
            artist: artist.to_string(),
            album: String::new(),
            duration: 200,
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

    /// `n` tracks starting at `from`, so two radios can be made to overlap by a
    /// controlled amount.
    fn radio(from: usize, n: usize, artist: &str) -> Vec<Track> {
        (from..from + n)
            .map(|i| track(&format!("v{i}"), artist))
            .collect()
    }

    #[test]
    fn overlap_is_share_of_the_union() {
        let a: HashSet<String> = ["1", "2", "3", "4"].iter().map(|s| s.to_string()).collect();
        let b: HashSet<String> = ["3", "4", "5", "6"].iter().map(|s| s.to_string()).collect();
        // 2 shared of 6 in the union.
        assert!((overlap(&a, &b) - 2.0 / 6.0).abs() < 1e-6);
        assert_eq!(overlap(&a, &a), 1.0);
        assert_eq!(overlap(&a, &HashSet::new()), 0.0);
    }

    /// The whole point: two anchors whose radios are nearly the same music must
    /// not become two tiles, however different their anchor tracks look.
    #[test]
    fn two_anchors_with_the_same_radio_yield_one_mix() {
        let candidates = vec![
            ("a".to_string(), radio(0, 20, "A")),
            // Nineteen of twenty in common — the same direction.
            ("b".to_string(), radio(1, 20, "A")),
        ];
        let mixes = distinct_mixes(&candidates);
        assert_eq!(
            mixes.len(),
            1,
            "got {:?}",
            mixes.iter().map(|m| &m.id).collect::<Vec<_>>()
        );
        assert_eq!(
            mixes[0].id, "mix:a",
            "the stronger anchor must be the one kept"
        );
    }

    #[test]
    fn anchors_with_separate_radios_each_get_a_mix() {
        let candidates = vec![
            ("a".to_string(), radio(0, 20, "A")),
            ("b".to_string(), radio(100, 20, "B")),
            ("c".to_string(), radio(200, 20, "C")),
        ];
        let mixes = distinct_mixes(&candidates);
        assert_eq!(mixes.len(), 3);
        assert_eq!(
            mixes.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(),
            ["A", "B", "C"]
        );
    }

    /// A little crossover is normal between two real directions; rejecting a
    /// whole mix for it costs the listener more than keeping it.
    #[test]
    fn a_few_shared_tracks_do_not_merge_two_directions() {
        let mut b = radio(100, 18, "B");
        b.extend(radio(0, 2, "A")); // 2 of 38 in the union
        let candidates = vec![("a".to_string(), radio(0, 20, "A")), ("b".to_string(), b)];
        assert_eq!(distinct_mixes(&candidates).len(), 2);
    }

    #[test]
    fn a_mix_is_named_after_its_two_commonest_artists() {
        let mut tracks = radio(0, 6, "Charli XCX - Topic");
        tracks.extend(radio(50, 4, "PinkPantheress - Topic"));
        tracks.extend(radio(90, 1, "Someone Else"));
        assert_eq!(
            name_for(&tracks, &HashSet::new()),
            "Charli XCX & PinkPantheress"
        );
    }

    /// Regenerating an unchanged mix must not rename it, or the shelf looks
    /// like it changed when nothing did.
    #[test]
    fn naming_is_stable_when_two_artists_tie() {
        let mut tracks = radio(0, 3, "Zara Larsson");
        tracks.extend(radio(50, 3, "Artemas"));
        assert_eq!(name_for(&tracks, &HashSet::new()), "Artemas & Zara Larsson");
        assert_eq!(
            name_for(&tracks, &HashSet::new()),
            name_for(&tracks, &HashSet::new())
        );
    }

    /// The measured failure: three of four tiles carried "Addison Rae" and two
    /// were the same pair reordered. Contents differed, the shelf still read
    /// as broken.
    #[test]
    fn no_two_mixes_on_the_shelf_share_a_name() {
        // Four directions that all lean on one very present artist.
        let candidates: Vec<(String, Vec<Track>)> = (0..4)
            .map(|i| {
                let mut tracks = radio(i * 100, 12, "Everywhere Artist");
                tracks.extend(radio(i * 100 + 50, 8, &format!("Local Artist {i}")));
                (format!("a{i}"), tracks)
            })
            .collect();
        let mixes = distinct_mixes(&candidates);
        assert_eq!(mixes.len(), 4);
        let names: Vec<&str> = mixes.iter().map(|m| m.name.as_str()).collect();
        let unique: HashSet<&str> = names.iter().copied().collect();
        assert_eq!(unique.len(), names.len(), "duplicate titles: {names:?}");
        // And the shared artist must not headline every one of them.
        let leading = names
            .iter()
            .filter(|n| n.starts_with("Everywhere Artist"))
            .count();
        assert!(
            leading <= 1,
            "shared artist leads {leading} tiles: {names:?}"
        );
    }

    #[test]
    fn a_mix_with_one_artist_is_named_after_it() {
        assert_eq!(
            name_for(&radio(0, 5, "Solo Act"), &HashSet::new()),
            "Solo Act"
        );
        assert_eq!(name_for(&[], &HashSet::new()), "Mix");
    }

    /// A radio that came back nearly empty is not a mix.
    #[test]
    fn short_radios_are_not_offered_as_mixes() {
        let candidates = vec![
            ("short".to_string(), radio(0, MIN_MIX_LEN - 1, "A")),
            ("ok".to_string(), radio(100, MIN_MIX_LEN, "B")),
        ];
        let mixes = distinct_mixes(&candidates);
        assert_eq!(mixes.len(), 1);
        assert_eq!(mixes[0].id, "mix:ok");
    }

    #[test]
    fn no_more_than_the_cap_is_returned() {
        let candidates: Vec<(String, Vec<Track>)> = (0..10)
            .map(|i| (format!("a{i}"), radio(i * 100, 20, &format!("A{i}"))))
            .collect();
        assert_eq!(distinct_mixes(&candidates).len(), MAX_MIXES);
    }

    #[test]
    fn a_mix_is_capped_in_length() {
        let mixes = distinct_mixes(&[("a".to_string(), radio(0, MIX_LEN + 20, "A"))]);
        assert_eq!(mixes[0].tracks.len(), MIX_LEN);
    }

    #[test]
    fn no_candidates_means_no_mixes() {
        assert!(distinct_mixes(&[]).is_empty());
        assert!(distinct_mixes(&[("a".into(), vec![])]).is_empty());
    }

    /// A direction genuinely can be mostly one artist — that is what a sound
    /// is — but a mix that is thirty tracks by one of them is a discography.
    /// On the real library one direction held 82 tracks that were
    /// overwhelmingly a single artist.
    #[test]
    fn one_artist_cannot_fill_a_whole_mix() {
        let mut store = reader::vectors::VectorStore::new(4);
        let mut labels = std::collections::HashMap::new();
        // Two directions, one of them dominated by a single artist.
        for i in 0..20 {
            let id = format!("hog{i}");
            let mut v = vec![1.0f32, 0.05, 0.0, 0.0];
            let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            v.iter_mut().for_each(|x| *x /= n);
            store.insert(id.clone(), v).unwrap();
            labels.insert(
                id,
                (
                    "One Artist".to_string(),
                    format!("track {i}"),
                    String::new(),
                ),
            );
        }
        for i in 0..20 {
            let id = format!("var{i}");
            let mut v = vec![0.0f32, 0.0, 1.0, 0.05];
            let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
            v.iter_mut().for_each(|x| *x /= n);
            store.insert(id.clone(), v).unwrap();
            labels.insert(
                id,
                (format!("Artist {i}"), format!("song {i}"), String::new()),
            );
        }

        let set = from_vectors(&store, &labels, 1_000, 42);
        for mix in &set.mixes {
            let mut counts: std::collections::HashMap<&str, usize> =
                std::collections::HashMap::new();
            for t in &mix.tracks {
                *counts.entry(t.artist.as_str()).or_insert(0) += 1;
            }
            for (artist, n) in counts {
                assert!(
                    n <= MAX_PER_ARTIST,
                    "{artist} appears {n} times in {}",
                    mix.name
                );
            }
        }
    }

    /// Nothing analysed yet is the normal state before the listener opts in,
    /// and it must not produce an empty tile or a panic.
    #[test]
    fn an_unanalysed_library_yields_no_mixes() {
        let store = reader::vectors::VectorStore::new(4);
        let set = from_vectors(&store, &std::collections::HashMap::new(), 500, 42);
        assert!(set.mixes.is_empty());
        // Still recorded as a run, or the caller retries it forever.
        assert_eq!(set.generated, 500);
    }

    /// The exact pair a listener reported: one song, two uploads, both in the
    /// same mix.
    #[test]
    fn one_song_under_two_uploads_is_one_song() {
        let a = song_key(
            "AllyNicholasVEVO",
            "Ally Nicholas - Fall Into (Official Music Video)",
        );
        let b = song_key("Ally Nicholas", "Fall Into");
        assert_eq!(
            a, b,
            "
{a}
{b}"
        );
    }

    #[test]
    fn different_songs_by_one_artist_stay_apart() {
        assert_ne!(song_key("Wisp", "Sword"), song_key("Wisp", "See you soon"));
        assert_ne!(
            song_key("Paramore", "Decode"),
            song_key("Paramore", "Monster")
        );
    }

    /// Upload decoration must not create two entries for one recording.
    #[test]
    fn bracketed_upload_decoration_is_ignored() {
        let plain = song_key("Paramore", "Still Into You");
        assert_eq!(
            plain,
            song_key("Paramore", "Paramore: Still Into You [OFFICIAL VIDEO]")
        );
        assert_eq!(plain, song_key("Paramore", "Still Into You (Lyrics)"));
    }

    fn a_set(feature_version: u8, relay_version: u64, generated: u64) -> MixSet {
        MixSet {
            mixes: vec![Mix {
                id: "m".into(),
                name: "Shoegaze".into(),
                tracks: Vec::new(),
            }],
            generated,
            feature_version,
            relay_version,
        }
    }

    const DAY: u64 = 24 * 60 * 60;

    /// The bug this whole arrangement exists to prevent.
    ///
    /// A phone has no vectors, so a set built from audio -- feature version 2
    /// -- can never match what the phone can verify, and `is_stale` therefore
    /// calls it stale from the second it arrives. If the decision ran through
    /// staleness, the phone would throw away the desktop's measured mixes and
    /// rebuild worse ones from YouTube radio, on every single launch, forever.
    #[test]
    fn a_phone_asks_the_relay_instead_of_rebuilding_over_what_it_was_given() {
        let from_the_desktop = a_set(2, 5, 1_000);
        // Old enough that staleness would certainly fire.
        let much_later = 1_000 + 30 * DAY;
        assert!(
            from_the_desktop.is_stale(much_later, 0),
            "precondition: without the relay this set reads as stale on a phone"
        );
        assert_eq!(
            decide(&from_the_desktop, much_later, 0, true),
            MixAction::Fetch { have: 5 },
            "a device with no vectors must ask, not rebuild"
        );
    }

    /// The version travels so the answer can be "nothing new" rather than
    /// fifty kilobytes of what the caller already has.
    #[test]
    fn a_device_says_which_version_it_already_holds() {
        assert_eq!(
            decide(&a_set(2, 9, 1_000), 1_100, 0, true),
            MixAction::Fetch { have: 9 }
        );
        // Nothing yet: ask for anything at all.
        assert_eq!(
            decide(&MixSet::default(), 1_100, 0, true),
            MixAction::Fetch { have: 0 }
        );
    }

    /// The desktop is the author. If it read from the relay it would fetch
    /// back a copy of its own work and then never rebuild it again.
    #[test]
    fn a_device_that_can_analyse_never_reads_from_the_relay() {
        let stale = a_set(2, 0, 1_000);
        assert_eq!(decide(&stale, 1_000 + 2 * DAY, 2, true), MixAction::Build);
        let fresh = a_set(2, 0, 1_000);
        assert_eq!(decide(&fresh, 1_100, 2, true), MixAction::Keep);
    }

    /// With no relay configured -- which is everyone, by default -- nothing
    /// about this changes from how it worked before there was one.
    #[test]
    fn without_a_relay_the_old_behaviour_stands() {
        assert_eq!(
            decide(&a_set(0, 0, 1_000), 1_100, 0, false),
            MixAction::Keep
        );
        assert_eq!(
            decide(&a_set(0, 0, 1_000), 1_000 + 2 * DAY, 0, false),
            MixAction::Build
        );
        // Analysed since these were made: rebuild regardless of age.
        assert_eq!(
            decide(&a_set(0, 0, 1_000), 1_100, 2, false),
            MixAction::Build
        );
    }

    /// When the relay has nothing, or cannot be reached from wherever the
    /// phone currently is, the caller asks again with the relay ruled out. A
    /// worse shelf beats an empty one.
    #[test]
    fn a_silent_relay_falls_back_to_building_something() {
        assert_eq!(
            decide(&MixSet::default(), 1_100, 0, false),
            MixAction::Build,
            "nothing held and nowhere to ask: build"
        );
    }

    /// Naming a mix after two of its artists said nothing about the sound and
    /// repeated across tiles. The model already knows what it is.
    #[test]
    fn a_mix_is_named_after_its_strongest_style() {
        let shoegaze = reader::styles::STYLES
            .iter()
            .position(|s| *s == "Rock---Shoegaze")
            .unwrap();
        let mut centroid = vec![0.01f32; 400];
        centroid[shoegaze] = 0.9;
        assert_eq!(style_name(&centroid, &HashSet::new()), "Shoegaze");
    }

    /// Two directions can share a strongest style; the shelf still must not
    /// show one word twice.
    #[test]
    fn a_style_already_on_the_shelf_is_not_used_again() {
        let shoegaze = reader::styles::STYLES
            .iter()
            .position(|s| *s == "Rock---Shoegaze")
            .unwrap();
        let mut centroid = vec![0.01f32; 400];
        centroid[shoegaze] = 0.9;
        centroid[(shoegaze + 1) % 400] = 0.8;
        let taken: HashSet<String> = ["shoegaze".to_string()].into_iter().collect();
        let second = style_name(&centroid, &taken);
        assert_ne!(second.to_lowercase(), "shoegaze");
        assert!(!second.is_empty());
    }

    /// A file written before the field existed — which is what is sitting on
    /// disk right now for anyone who used the previous build. It must read
    /// back as "unknown provenance" and therefore rebuild, not fail to parse
    /// and not silently claim to be current.
    #[test]
    fn a_mix_file_from_before_this_field_reads_as_unknown_provenance() {
        let legacy = r#"{"mixes":[],"generated":1700000000}"#;
        let set: MixSet = serde_json::from_str(legacy).expect("legacy file must still parse");
        assert_eq!(set.feature_version, 0);
        assert!(
            set.is_stale(1_700_000_001, reader::vectors::FEATURE_VERSION),
            "an old file must not pass as current"
        );
    }

    /// The measured failure: the spectrogram fix landed, every vector was
    /// recomputed, and the shelf kept serving mixes built from the old ones
    /// because they were only an hour old. Age is not the only way to be out
    /// of date.
    #[test]
    fn mixes_built_from_different_vectors_are_stale_however_recent() {
        let just_made = MixSet {
            mixes: vec![Mix {
                id: "mix:a".into(),
                name: "A".into(),
                tracks: radio(0, 10, "A"),
            }],
            generated: 1_000_000,
            feature_version: 1,
            relay_version: 0,
        };
        assert!(
            just_made.is_stale(1_000_001, 2),
            "a set from older vectors must rebuild immediately"
        );
        assert!(
            !just_made.is_stale(1_000_001, 1),
            "same vectors, still fresh"
        );
    }

    /// Mixes from the radio path carry version 0, so the moment any audio has
    /// been analysed they give way to it rather than holding the shelf for a
    /// day.
    #[test]
    fn radio_mixes_give_way_once_vectors_exist() {
        let from_radio = MixSet {
            mixes: vec![Mix {
                id: "mix:a".into(),
                name: "A".into(),
                tracks: radio(0, 10, "A"),
            }],
            generated: 1_000_000,
            feature_version: 0,
            relay_version: 0,
        };
        assert!(
            !from_radio.is_stale(1_000_001, 0),
            "no vectors yet, keep them"
        );
        assert!(from_radio.is_stale(1_000_001, 2), "vectors exist now");
    }

    /// The set is written to disk and read back on the next launch, and the
    /// detail page renders straight from it — so a Track field that does not
    /// survive serde would show up as an empty tracklist rather than an error.
    #[test]
    fn a_mix_set_survives_being_written_and_read_back() {
        let original = MixSet {
            mixes: distinct_mixes(&[
                ("a".to_string(), radio(0, 20, "Artist A")),
                ("b".to_string(), radio(100, 20, "Artist B")),
            ]),
            generated: 1_700_000_000,
            feature_version: reader::vectors::FEATURE_VERSION,
            relay_version: 0,
        };
        assert_eq!(original.mixes.len(), 2);

        let json = serde_json::to_string(&original).expect("serialise");
        let back: MixSet = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back, original);
        // The detail page needs the tracks themselves, not just the count.
        assert_eq!(back.mixes[0].tracks.len(), MIX_LEN.min(20));
        assert_eq!(
            back.mixes[0].tracks[0].title,
            original.mixes[0].tracks[0].title
        );
        assert_eq!(
            back.mixes[0].tracks[0].path,
            original.mixes[0].tracks[0].path
        );
    }

    /// A run that produced nothing must still count as a run. Before this, an
    /// empty result was never recorded, so the shelf was permanently stale and
    /// re-fired eight paced requests on every visit to the home screen.
    #[test]
    fn an_empty_result_is_remembered_and_retried_within_the_hour() {
        let empty = MixSet {
            mixes: Vec::new(),
            generated: 1_000_000,
            feature_version: 0,
            relay_version: 0,
        };
        assert!(
            !empty.is_stale(1_000_000 + RETRY_SECS - 1, 0),
            "must not re-fire immediately"
        );
        assert!(
            empty.is_stale(1_000_000 + RETRY_SECS, 0),
            "but must try again before a whole day"
        );
    }

    /// The shelf is a fixture the listener returns to, so it must not rebuild
    /// on every launch — but it must not calcify either.
    #[test]
    fn a_mix_set_goes_stale_after_a_day() {
        let fresh = MixSet {
            mixes: vec![Mix {
                id: "mix:a".into(),
                name: "A".into(),
                tracks: radio(0, 10, "A"),
            }],
            generated: 1_000_000,
            feature_version: reader::vectors::FEATURE_VERSION,
            relay_version: 0,
        };
        let v = reader::vectors::FEATURE_VERSION;
        assert!(!fresh.is_stale(1_000_000 + REFRESH_SECS - 1, v));
        assert!(fresh.is_stale(1_000_000 + REFRESH_SECS, v));
        // Never generated is always stale.
        assert!(MixSet::default().is_stale(0, 0));
    }
}
