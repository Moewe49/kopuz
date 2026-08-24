//! "Rediscovered" mixes — the no-account, no-network tier of recommendations.
//!
//! Tracks the listener demonstrably likes but has not heard lately, drawn only
//! from what they have already played. No model, no catalogue, no third party,
//! nothing to sign up for. It is the one tier that works for every user on day
//! one, and it is genuinely good, because the hard part of recommendation —
//! knowing what someone likes — is already answered by their own history.
//!
//! ## Why `listen_counts` is trustworthy here
//!
//! It counts COMPLETED plays, not starts. Both increment sites in
//! `use_player_task` fire on the natural end of a track (the crossfade trigger
//! near the end, and the auto-advance past `duration`), and both are guarded by
//! `!skip_in_progress`, so a track the listener skipped is never counted. A
//! play count is therefore evidence of listening rather than merely of
//! selecting — the distinction a naive "plays" counter gets wrong.
//!
//! ## What is deliberately missing
//!
//! Nothing in the stored state carries a timestamp, so "not heard in three
//! months" cannot be expressed. `recently_played` is the only recency signal
//! there is, and it is a short list. Recency here therefore means "not among
//! the last few played" rather than a real age. Recording played-at times is
//! the single change that would most improve this.

use std::collections::{HashMap, HashSet};

use config::PlayRecord;

/// Completed plays before a track counts as liked rather than merely tried.
///
/// Two is noise — an accidental repeat, or a track that happened to follow a
/// favourite twice. Three is a choice.
const MIN_PLAYS: u64 = 3;

/// Tracks per mix. Long enough to be an evening, short enough to stay curated.
const MIX_LEN: usize = 25;

/// Cap per artist, so one heavily-played favourite cannot become the mix.
const MAX_PER_ARTIST: usize = 2;

/// Heard inside this window counts as "in rotation", not as a rediscovery.
/// Six weeks — long enough that a track has genuinely receded, short enough
/// that the pool refreshes.
const RECENT_SECS: u64 = 6 * 7 * 24 * 60 * 60;

/// One entry of a mix. Carries its own metadata, so a track that was never
/// added to the library still displays properly.
#[derive(Debug, Clone, PartialEq)]
pub struct MixEntry {
    /// The track path — playable, and the key back into the history.
    pub path: String,
    pub title: String,
    pub artist: String,
    pub plays: u64,
    /// Unix seconds of the last completed play; 0 when unknown.
    pub last_played: u64,
}

/// A built mix, plus what it was built from — the UI can explain itself, and a
/// test can assert on something other than the track list.
#[derive(Debug, Clone, PartialEq)]
pub struct RediscoverMix {
    pub tracks: Vec<MixEntry>,
    /// Tracks that cleared `MIN_PLAYS`, before recency and artist caps applied.
    pub eligible: usize,
    /// Dropped for having been played recently.
    pub excluded_recent: usize,
}

/// Deterministic shuffle from a caller-supplied seed.
///
/// Deliberately not `rand`: the mix has to be STABLE for a given week, or
/// reopening the screen reshuffles a list the listener is halfway through. The
/// caller passes a week number, so the same week yields the same mix.
fn seeded_order(len: usize, seed: u64) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..len).collect();
    // xorshift64 — plenty for ordering a few hundred items.
    let mut s = seed.max(1);
    let mut next = move || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    for i in (1..idx.len()).rev() {
        let j = (next() % (i as u64 + 1)) as usize;
        idx.swap(i, j);
    }
    idx
}

/// Build a rediscovery mix from the play history.
///
/// Drawn from the history rather than the library on purpose: measured on a
/// real profile, 240 tracks had three or more completed plays but only 33 were
/// in the library — the rest came from search, radio and playlists, which never
/// add to it. Building from the library would have discarded seven eighths of
/// what the listener actually likes, including their most-played tracks.
///
/// `recent` holds keys to hold back; `now_secs` enables the age filter (pass 0
/// to disable it); `seed` fixes the selection for a period.
pub fn build(
    history: &HashMap<String, PlayRecord>,
    recent: &[String],
    now_secs: u64,
    seed: u64,
) -> RediscoverMix {
    let recent: HashSet<&str> = recent.iter().map(|s| s.as_str()).collect();

    let mut eligible = 0usize;
    let mut excluded_recent = 0usize;
    let mut pool: Vec<MixEntry> = Vec::new();

    for (path, rec) in history {
        if rec.plays < MIN_PLAYS {
            continue;
        }
        eligible += 1;

        // Two recency tests, because there are two kinds of evidence.
        //
        // The list is what the app already tracks, and holds BARE IDS for
        // server tracks while paths are full — so both shapes are compared.
        // The timestamp is the better signal where it exists, but records
        // written before `last_played` existed carry 0, and treating those as
        // "played in 1970" would wrongly admit everything.
        let id_segment = path.split(':').nth(1).unwrap_or("");
        let in_recent_list = recent.contains(path.as_str())
            || (!id_segment.is_empty() && recent.contains(id_segment));
        let heard_lately = now_secs > 0
            && rec.last_played > 0
            && now_secs.saturating_sub(rec.last_played) < RECENT_SECS;
        if in_recent_list || heard_lately {
            excluded_recent += 1;
            continue;
        }

        pool.push(MixEntry {
            path: path.clone(),
            title: rec.title.clone(),
            artist: rec.artist.clone(),
            plays: rec.plays,
            last_played: rec.last_played,
        });
    }

    // A HashMap has no order, so sort before seeding or the "same seed, same
    // mix" guarantee would hold only within one process run.
    pool.sort_by(|a, b| b.plays.cmp(&a.plays).then_with(|| a.path.cmp(&b.path)));
    let order = seeded_order(pool.len(), seed);

    let mut per_artist: HashMap<String, usize> = HashMap::new();
    let mut tracks = Vec::with_capacity(MIX_LEN);
    for &i in &order {
        if tracks.len() >= MIX_LEN {
            break;
        }
        let entry = &pool[i];
        let artist = entry.artist.trim().to_lowercase();
        let seen = per_artist.entry(artist).or_insert(0);
        if *seen >= MAX_PER_ARTIST {
            continue;
        }
        *seen += 1;
        tracks.push(entry.clone());
    }

    RediscoverMix {
        tracks,
        eligible,
        excluded_recent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(title: &str, artist: &str, plays: u64, last: u64) -> PlayRecord {
        PlayRecord {
            title: title.to_string(),
            artist: artist.to_string(),
            plays,
            last_played: last,
        }
    }

    fn history(items: &[(&str, PlayRecord)]) -> HashMap<String, PlayRecord> {
        items.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    /// The premise: a track played once is not a preference.
    #[test]
    fn a_track_played_once_is_not_treated_as_a_favourite() {
        let h = history(&[("a", rec("A", "X", 1, 0)), ("b", rec("B", "Y", 5, 0))]);
        let mix = build(&h, &[], 0, 1);
        assert_eq!(mix.eligible, 1);
        assert_eq!(mix.tracks.len(), 1);
        assert_eq!(mix.tracks[0].path, "b");
    }

    /// "Rediscovered" means exactly that — something already in rotation is not
    /// a rediscovery, however much it is loved.
    #[test]
    fn recently_played_favourites_are_held_back() {
        let h = history(&[("a", rec("A", "X", 9, 0)), ("b", rec("B", "Y", 9, 0))]);
        let mix = build(&h, &["a".to_string()], 0, 1);
        assert_eq!(mix.excluded_recent, 1);
        assert_eq!(mix.tracks.len(), 1);
        assert_eq!(mix.tracks[0].path, "b");
    }

    /// Server tracks are recorded as bare ids while history keys are full
    /// paths. Comparing only whole strings would defeat the filter for every
    /// server track — which is most of them.
    #[test]
    fn a_recent_server_id_matches_its_full_path() {
        let h = history(&[("ytmusic:abc123:urlhex_00", rec("T", "X", 7, 0))]);
        let mix = build(&h, &["abc123".to_string()], 0, 1);
        assert_eq!(mix.excluded_recent, 1, "a bare id must match its path");
        assert!(mix.tracks.is_empty());
    }

    /// The timestamp is the better recency signal, and the reason it was added.
    #[test]
    fn a_track_heard_last_week_is_not_a_rediscovery() {
        let now = 10_000_000u64;
        let h = history(&[
            ("fresh", rec("F", "X", 8, now - 7 * 24 * 3600)),
            ("faded", rec("D", "Y", 8, now - RECENT_SECS - 1)),
        ]);
        let mix = build(&h, &[], now, 1);
        assert_eq!(mix.tracks.len(), 1);
        assert_eq!(mix.tracks[0].path, "faded");
    }

    /// Records written before `last_played` existed carry 0. Reading that as
    /// "played in 1970" would wrongly admit every one of them.
    #[test]
    fn a_record_without_a_timestamp_is_not_treated_as_ancient() {
        let h = history(&[("old", rec("O", "X", 6, 0))]);
        let mix = build(&h, &[], 10_000_000, 1);
        assert_eq!(mix.tracks.len(), 1, "no timestamp must not mean excluded");
        assert_eq!(mix.excluded_recent, 0);
    }

    /// One artist in heavy rotation must not become the entire mix.
    #[test]
    fn no_artist_can_dominate_the_mix() {
        let items: Vec<(String, PlayRecord)> = (0..30)
            .map(|i| (format!("t{i}"), rec(&format!("T{i}"), "Same Artist", 5, 0)))
            .collect();
        let h: HashMap<String, PlayRecord> = items.into_iter().collect();
        let mix = build(&h, &[], 0, 42);
        assert_eq!(mix.tracks.len(), MAX_PER_ARTIST);
    }

    /// The mix must not reshuffle every time the screen is opened — the
    /// listener is halfway through it. A HashMap has no order, so this also
    /// guards the sort that makes the seed meaningful across runs.
    #[test]
    fn the_same_seed_yields_the_same_mix() {
        let h: HashMap<String, PlayRecord> = (0..40)
            .map(|i| (format!("t{i}"), rec(&format!("T{i}"), &format!("artist{i}"), 4, 0)))
            .collect();
        let a = build(&h, &[], 0, 7);
        let b = build(&h, &[], 0, 7);
        let other = build(&h, &[], 0, 8);
        assert_eq!(a.tracks, b.tracks, "same seed must be stable");
        assert_ne!(a.tracks, other.tracks, "a new week should differ");
    }

    /// An empty history yields an empty mix rather than a random selection —
    /// suggesting tracks nobody has played is worse than suggesting nothing.
    #[test]
    fn no_history_yields_nothing_rather_than_noise() {
        let mix = build(&HashMap::new(), &[], 0, 1);
        assert!(mix.tracks.is_empty());
        assert_eq!(mix.eligible, 0);
    }
}
