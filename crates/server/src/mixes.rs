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
    pub fn is_stale(&self, now_secs: u64) -> bool {
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
    let mut counts: Vec<(String, usize)> = Vec::new();
    for t in tracks {
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
    // Ties break by name, so regenerating an unchanged mix keeps its title.
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
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
        };
        assert!(
            !empty.is_stale(1_000_000 + RETRY_SECS - 1),
            "must not re-fire immediately"
        );
        assert!(
            empty.is_stale(1_000_000 + RETRY_SECS),
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
        };
        assert!(!fresh.is_stale(1_000_000 + REFRESH_SECS - 1));
        assert!(fresh.is_stale(1_000_000 + REFRESH_SECS));
        // Never generated is always stale.
        assert!(MixSet::default().is_stale(0));
    }
}
