//! Deciding which search results are worth judging at all.
//!
//! A ranker sorts whatever it is given. Point it at a catalogue full of hour
//! long compilations, live uploads and reaction videos and it will sort those
//! very carefully — measured on a live pool, an EDM megamix titled
//! "Beautiful Female Vocal Mix ♫ Top 30 Songs" scored higher than every real
//! track in the list. That is not a ranking failure, it is a pool failure, and
//! it has to be fixed before the ranking, not after.
//!
//! Everything here works on the title, because that is all a search result
//! reliably carries. It is a blunt instrument on purpose: throwing away a good
//! track costs one candidate out of hundreds, while letting a sixty-minute mix
//! through costs the credibility of the whole list.

/// Why a candidate was rejected — kept so the reason can be logged or shown
/// rather than a track silently vanishing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// A compilation, megamix or "top N" upload — many tracks in one file.
    Compilation,
    /// Long-form: an hour of lofi, a full album, a sleep loop.
    LongForm,
    /// A live recording, tour footage or a concert upload.
    Live,
    /// Commentary, reaction, tutorial — someone talking over or about music.
    NotMusic,
    /// A pitched or slowed edit; the audio no longer represents the track.
    AlteredAudio,
}

impl Rejection {
    pub fn as_str(self) -> &'static str {
        match self {
            Rejection::Compilation => "compilation",
            Rejection::LongForm => "long-form",
            Rejection::Live => "live",
            Rejection::NotMusic => "not music",
            Rejection::AlteredAudio => "altered audio",
        }
    }
}

/// Words that only appear in titles of things that are not a single studio
/// track. Matched on word boundaries against a lowercased title.
const COMPILATION: &[&str] = &[
    "compilation",
    "megamix",
    "mixtape",
    "playlist",
    "best of",
    "greatest hits",
    "top 10",
    "top 20",
    "top 30",
    "top 50",
    "top 100",
    "mashup",
    // A bare "mix" is a compilation marker, and the word boundary makes it
    // safe against "remix" on its own — the "e" in front blocks the match.
    "mix",
];
/// Music made to be background rather than listened to. These phrases only
/// occur on hour-long uploads, never on a single track.
const FUNCTIONAL: &[&str] = &[
    "study music",
    "sleep music",
    "relaxing music",
    "background music",
    "workout music",
    "gaming music",
    "focus music",
    "meditation",
    "to relax",
    "to study",
    "for sleep",
    "for studying",
    "white noise",
];
const LONG_FORM: &[&str] = &[
    "full album",
    "full ep",
    "full set",
    "full concert",
    "loop",
    "continuous",
    "non stop",
    "nonstop",
    "radio show",
    "dj set",
    "livestream",
];
const LIVE: &[&str] = &[
    "live at",
    "live from",
    "live in",
    "live session",
    "concert",
    "tour",
    "unplugged",
    "acoustic session",
    "live performance",
];
const NOT_MUSIC: &[&str] = &[
    "reaction",
    "review",
    "interview",
    "behind the scenes",
    "making of",
    "tutorial",
    "how to",
    "explained",
    "documentary",
    "podcast",
    "trailer",
    "teaser",
    "announcement",
    "karaoke",
    "instrumental version",
    "backing track",
];
const ALTERED: &[&str] = &[
    "sped up",
    "speed up",
    "slowed",
    "reverb",
    "nightcore",
    "daycore",
    "8d audio",
    "bass boosted",
    "pitched",
];

/// Whole-word search, so "tourniquet" does not match "tour" and "shower" does
/// not match "hour" — a substring check rejects real tracks by accident.
fn has_phrase(haystack: &str, needle: &str) -> bool {
    let mut from = 0;
    while let Some(pos) = haystack[from..].find(needle) {
        let start = from + pos;
        let end = start + needle.len();
        let before_ok = start == 0
            || !haystack[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric());
        let after_ok = end >= haystack.len()
            || !haystack[end..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
        from = start + needle.len().max(1);
        if from >= haystack.len() {
            break;
        }
    }
    false
}

/// "(Radio Mix)", "(Club Mix)", "(Extended Mix)" name a real released version
/// of one track, not a compilation. "Remix" needs no exception — the word
/// boundary already keeps it out.
const MIX_IS_A_VERSION: &[&str] = &[
    "radio mix",
    "club mix",
    "extended mix",
    "original mix",
    "vip mix",
    "dub mix",
    "vocal mix",
    "instrumental mix",
    "album mix",
];

/// A parenthesised "(FULL)" on a concert or festival upload means the whole
/// set, which is the same problem as an hour-long compilation.
fn full_marker(t: &str) -> bool {
    t.contains("(full)") || t.contains("[full]")
}

/// "Top Chinese Songs Remix of 2025" carries no single keyword that is safe on
/// its own — "top" and "songs" are both ordinary words. Together they are not.
fn plural_songs_roundup(t: &str) -> bool {
    has_phrase(t, "songs")
        && [
            "top", "best", "hits", "popular", "greatest", "viral", "trending",
        ]
        .iter()
        .any(|w| has_phrase(t, w))
}

/// "hour" and "minutes" are only long-form markers with a number in front:
/// "10 Hours", "60 Minutes Loop". Bare, they are ordinary song titles —
/// "Golden Hour", "Rush Hour", "The Hours" — and rejecting those is the
/// failure mode that matters.
fn stated_duration(t: &str) -> bool {
    for unit in ["hour", "hours", "minutes", "min", "mins"] {
        let mut from = 0;
        while let Some(pos) = t[from..].find(unit) {
            let start = from + pos;
            let after_ok = t[start + unit.len()..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric());
            // Walk back over whitespace; a digit before it means a duration.
            let before = t[..start].trim_end();
            if after_ok
                && before
                    .chars()
                    .next_back()
                    .is_some_and(|c| c.is_ascii_digit())
            {
                return true;
            }
            from = start + unit.len();
            if from >= t.len() {
                break;
            }
        }
    }
    false
}

/// A bare "live" cannot be a keyword — "Live Forever", "Live Your Life" and
/// "Long Live" are studio tracks. Inside brackets it almost always is one:
/// "(Live)", "(Live From Mexico)", "[Live Cannibalism]".
fn bracketed_live(t: &str) -> bool {
    ["(live", "[live", "- live ", "- live)"]
        .iter()
        .any(|m| t.contains(m))
        || t.ends_with("- live")
}

/// `None` when the candidate looks like a single studio track.
pub fn reject(title: &str) -> Option<Rejection> {
    let t = title.to_lowercase();
    if bracketed_live(&t) {
        return Some(Rejection::Live);
    }
    if stated_duration(&t) || full_marker(&t) {
        return Some(Rejection::LongForm);
    }
    if FUNCTIONAL.iter().any(|w| t.contains(w)) {
        return Some(Rejection::LongForm);
    }
    if plural_songs_roundup(&t) {
        return Some(Rejection::Compilation);
    }
    // Checked before the keyword sweep so a named version survives the bare
    // "mix" entry in COMPILATION.
    let mix_is_a_version = MIX_IS_A_VERSION.iter().any(|w| t.contains(w));
    for (words, why) in [
        (COMPILATION, Rejection::Compilation),
        (LONG_FORM, Rejection::LongForm),
        (NOT_MUSIC, Rejection::NotMusic),
        (ALTERED, Rejection::AlteredAudio),
        (LIVE, Rejection::Live),
    ] {
        if words
            .iter()
            .any(|w| has_phrase(&t, w) && !(*w == "mix" && mix_is_a_version))
        {
            return Some(why);
        }
    }
    None
}

/// True when a candidate is worth spending a download and an inference on.
pub fn is_playable_track(title: &str) -> bool {
    reject(title).is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact title that beat every real track in the measured pool.
    #[test]
    fn the_megamix_that_topped_the_live_ranking_is_rejected() {
        // The title is a compilation AND functional background music, so both
        // labels are defensible; what the test is for is that it never reaches
        // the ranker again.
        assert!(
            reject("Beautiful Female Vocal Mix ♫ Top 30 Songs: EDM, NCS, Gaming Music").is_some()
        );
    }

    /// Every category needs at least one real example, taken from the pool that
    /// was actually searched rather than invented.
    #[test]
    fn each_category_catches_what_it_is_for() {
        for (title, want) in [
            ("Sleep Music 10 Hours", Rejection::LongForm),
            (
                "BACH: Cello Suite No. 1 Prelude (60 Minutes Loop)",
                Rejection::LongForm,
            ),
            (
                "Cannibal Corpse - Hammer Smashed Face [Live Cannibalism]",
                Rejection::Live,
            ),
            (
                "[4K] Taylor Swift - Anti-Hero (From The Eras Tour)",
                Rejection::Live,
            ),
            ("Producer REACTION to Charli xcx", Rejection::NotMusic),
            ("Espresso (sped up)", Rejection::AlteredAudio),
            ("Golden Hour - slowed + reverb", Rejection::AlteredAudio),
        ] {
            assert_eq!(reject(title), Some(want), "title: {title}");
        }
    }

    /// The expensive mistake is the other direction — a filter that quietly
    /// eats real music leaves the listener with a thinner pool and no clue why.
    #[test]
    fn ordinary_tracks_survive() {
        for title in [
            "Everything is romantic",
            "Fame is a Gun",
            "b i g f e e l i n g s",
            "So Hot You're Hurting My Feelings",
            "Assumptions",
            "800 db cloud",
            "Gotta Get Up (Interlude)",
            "Guess featuring billie eilish",
        ] {
            assert!(is_playable_track(title), "wrongly rejected: {title}");
        }
    }

    /// Word boundaries, not substrings. Every one of these contains a keyword
    /// inside a longer word and would die under a naive `contains`.
    #[test]
    fn a_keyword_inside_a_longer_word_is_not_a_match() {
        for title in [
            "Tourniquet",       // tour
            "Shower",           // hour
            "Mixed Emotions",   // mix (only as a phrase, not bare)
            "Reviewing Mirror", // review... as a longer word
            "Livewire",         // live
            "Deloused",         // (control)
        ] {
            assert!(is_playable_track(title), "wrongly rejected: {title}");
        }
    }

    /// A bare "live" must NOT be a keyword: these are studio tracks and
    /// rejecting them would be the filter quietly eating real music.
    #[test]
    fn live_as_an_ordinary_word_is_not_a_live_recording() {
        for title in ["Live Forever", "Live Your Life", "Long Live", "Livewire"] {
            assert!(is_playable_track(title), "wrongly rejected: {title}");
        }
    }

    /// A duration needs a number. Without one these are song titles and the
    /// filter must leave them alone.
    #[test]
    fn hour_without_a_number_is_a_song_title() {
        for title in ["Golden Hour", "Rush Hour", "The Hours", "Happy Hour"] {
            assert!(is_playable_track(title), "wrongly rejected: {title}");
        }
        assert_eq!(reject("Lofi Beats 2 Hours"), Some(Rejection::LongForm));
        assert_eq!(reject("Ambient 45 minutes"), Some(Rejection::LongForm));
    }

    /// The three leaks a live search exposed, each with the counter-case that
    /// stops the fix from going too far.
    #[test]
    fn the_leaks_a_live_search_found_are_closed() {
        assert_eq!(
            reject("Chill Lofi Mix [chill lo-fi hip hop beats]"),
            Some(Rejection::Compilation)
        );
        assert_eq!(
            reject("Arctic Monkeys - Rock Werchter 2023 (FULL)"),
            Some(Rejection::LongForm)
        );
        assert_eq!(
            reject("Top Chinese Songs Remix of 2025"),
            Some(Rejection::Compilation)
        );
        assert_eq!(
            reject("3 Hours of Chill Lofi Music for DEEP Sleep or Study Session"),
            Some(Rejection::LongForm)
        );
    }

    /// "mix" is only safe as a keyword because a remix and a named version
    /// still get through. If these ever fail the filter is eating real music.
    #[test]
    fn remixes_and_named_versions_survive_the_mix_keyword() {
        for title in [
            "Tum Mile (Lofi Flip)",
            "Levitating (Remix)",
            "Blinding Lights - Chromatics Remix",
            "Strobe (Radio Mix)",
            "Adagio for Strings (Extended Mix)",
            "Mixed Emotions",
        ] {
            assert!(is_playable_track(title), "wrongly rejected: {title}");
        }
    }

    /// Case must not matter — search results are inconsistently capitalised.
    #[test]
    fn matching_ignores_case() {
        assert!(reject("FULL ALBUM - Some Record").is_some());
        assert!(reject("Live At Wembley").is_some());
    }
}
