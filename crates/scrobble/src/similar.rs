//! Finding artists worth trying, from ListenBrainz.
//!
//! # Why an external list at all
//!
//! The audio model can say how close two tracks are, but it cannot name a
//! track it has never seen. Something has to propose candidates, and YouTube
//! search cannot: asking it for "<track> similar songs" returns that track,
//! measured — every suggestion in the first end-to-end run was a re-upload of
//! the seed.
//!
//! ListenBrainz answers a different question — who do people who listen to
//! this also listen to — and answers it from real listening data rather than
//! genre tags. The list it returns is coarse: for Charli xcx it offers Taylor
//! Swift, Katy Perry and Dua Lipa, which is "popular pop" rather than "sounds
//! like this". That is fine and it is the design: a wide, cheap net, then the
//! audio model as the filter. The listening test confirmed this works — the
//! same unconvincing popularity list produced good suggestions once ranked by
//! style rather than by name.
//!
//! # Cost to the listener
//!
//! Nothing, and no account. Both endpoints used here are anonymous: the
//! MusicBrainz web service and the ListenBrainz *labs* API, which unlike the
//! main ListenBrainz API needs no token. A listener who does connect their
//! ListenBrainz account gets nothing extra from this path.
//!
//! MusicBrainz asks for at most one request per second and a User-Agent that
//! identifies the application. Both are honoured here; the User-Agent names
//! the project, never the user.

use std::time::{Duration, Instant};

/// MusicBrainz requires an identifying User-Agent and will block generic ones.
/// It names the project, not the person running it — a contact address here
/// would send the listener's identity to a service that did not ask for it.
const USER_AGENT: &str = concat!(
    "Kopuz/",
    env!("CARGO_PKG_VERSION"),
    " ( https://github.com/Moewe49/kopuz )"
);

/// MusicBrainz's published limit is one request per second, averaged. Going
/// over it gets the application blocked rather than throttled, so the gate is
/// on the strict side.
const MIN_GAP: Duration = Duration::from_millis(1100);

/// The ListenBrainz algorithm string. It selects how similarity was computed,
/// and the endpoint returns nothing at all if it is omitted or misspelt.
const ALGORITHM: &str =
    "session_based_days_7500_session_300_contribution_5_threshold_10_limit_100_filter_True_skip_30";

/// One artist ListenBrainz thinks is related, with its raw co-listen score.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct SimilarArtist {
    #[serde(rename = "artist_mbid")]
    pub mbid: String,
    pub name: String,
    /// Co-listening strength. Comparable within one response, meaningless
    /// across artists, so it is only ever used for ordering.
    #[serde(default)]
    pub score: u64,
}

/// Wait until at least `MIN_GAP` has passed since the previous MusicBrainz
/// call, whichever task made it.
async fn rate_limit() {
    use std::sync::{Mutex, OnceLock};
    static LAST: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    let cell = LAST.get_or_init(|| Mutex::new(None));

    let wait = {
        let mut guard = match cell.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let now = Instant::now();
        // The stored value is the next free slot, not the last call. Storing
        // the last call instead makes the third concurrent caller compute its
        // wait against a moment already in the future, which clamps to zero
        // and lets it go a full gap early.
        let slot = guard.map(|next| next.max(now)).unwrap_or(now);
        let wait = slot.saturating_duration_since(now);
        // Reserved before the lock is released, so callers queue rather than
        // all deciding at once that they may go.
        *guard = Some(slot + MIN_GAP);
        wait
    };
    if !wait.is_zero() {
        tokio::time::sleep(wait).await;
    }
}

/// The outcome of a name lookup.
///
/// `NotFound` and `Unavailable` have to be distinguishable. MusicBrainz
/// answers a burst of queries with `{"error": "The MusicBrainz web server is
/// currently busy"}`, and collapsing that into "no such artist" was measured
/// to cost a whole taste direction its candidates — Ariana Grande and Artemas
/// were both reported as unknown while both have entries with a perfect match
/// score.
#[derive(Debug, Clone, PartialEq)]
pub enum Lookup {
    Found(String),
    /// The service answered, and there is genuinely no such artist.
    NotFound,
    /// The service did not answer usefully. Trying again later may work.
    Unavailable,
}

impl Lookup {
    pub fn found(self) -> Option<String> {
        match self {
            Lookup::Found(id) => Some(id),
            _ => None,
        }
    }
}

/// How many times to retry a busy MusicBrainz. Their guidance is to back off,
/// not to hammer; three attempts spaced by the rate limiter is enough to ride
/// out the momentary busy responses seen in practice.
const ATTEMPTS: usize = 3;

/// Look up an artist's MusicBrainz id by name, retrying while the service is
/// busy.
pub async fn artist_mbid(client: &reqwest::Client, name: &str) -> Lookup {
    for attempt in 0..ATTEMPTS {
        match artist_mbid_once(client, name).await {
            Lookup::Unavailable if attempt + 1 < ATTEMPTS => {
                // The rate limiter already spaces calls out; widen the gap a
                // little more each time rather than retrying at full speed.
                tokio::time::sleep(MIN_GAP * (attempt as u32 + 1)).await;
            }
            other => return other,
        }
    }
    Lookup::Unavailable
}

async fn artist_mbid_once(client: &reqwest::Client, name: &str) -> Lookup {
    let cleaned = clean_artist(name);
    if cleaned.is_empty() {
        return Lookup::NotFound;
    }
    rate_limit().await;
    // `text()` then `serde_json`, not reqwest's `json()`: the workspace
    // declares reqwest without its `json` feature, so that method exists only
    // because another crate turns it on. Relying on that would make this
    // module compile or not depending on who else is in the build.
    let response = client
        .get("https://musicbrainz.org/ws/2/artist")
        .query(&[
            ("query", format!("artist:\"{cleaned}\"").as_str()),
            ("fmt", "json"),
            ("limit", "1"),
        ])
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await;
    let Ok(response) = response else {
        return Lookup::Unavailable;
    };
    let Ok(text) = response.text().await else {
        return Lookup::Unavailable;
    };
    let Ok(body) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Lookup::Unavailable;
    };
    read_mbid(&body)
}

/// Split out from the request so the three outcomes can be tested without a
/// network.
pub fn read_mbid(body: &serde_json::Value) -> Lookup {
    if body.get("error").is_some() {
        return Lookup::Unavailable;
    }
    match body.pointer("/artists/0/id").and_then(|v| v.as_str()) {
        Some(id) => Lookup::Found(id.to_string()),
        // An `artists` array that is present and empty is a real answer;
        // anything else means the response was not the shape expected.
        None if body.get("artists").is_some() => Lookup::NotFound,
        None => Lookup::Unavailable,
    }
}

/// Artists commonly listened to alongside this one, strongest first.
pub async fn similar_artists(
    client: &reqwest::Client,
    mbid: &str,
    limit: usize,
) -> Vec<SimilarArtist> {
    let response = client
        .get("https://labs.api.listenbrainz.org/similar-artists/json")
        .query(&[("artist_mbids", mbid), ("algorithm", ALGORITHM)])
        .header(reqwest::header::USER_AGENT, USER_AGENT)
        .send()
        .await;
    let Ok(text) = (match response {
        Ok(r) => r.text().await,
        Err(e) => Err(e),
    }) else {
        return Vec::new();
    };
    let Ok(body) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let mut out = parse_similar(&body);
    out.truncate(limit);
    out
}

/// The labs API has returned two shapes over its life: a bare array of
/// artists, and an array whose last element wraps them in `{"data": [...]}`.
/// Both are accepted so a change on their side degrades to fewer suggestions
/// rather than none.
pub fn parse_similar(body: &serde_json::Value) -> Vec<SimilarArtist> {
    let rows = body
        .as_array()
        .and_then(|a| a.last())
        .and_then(|last| last.get("data"))
        .or(Some(body))
        .and_then(|v| v.as_array());
    let Some(rows) = rows else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|r| serde_json::from_value::<SimilarArtist>(r.clone()).ok())
        .filter(|a| !a.name.is_empty())
        .collect()
}

/// Strip the decoration YouTube puts on channel names, so the result has a
/// chance of matching a database entry.
pub fn clean_artist(name: &str) -> String {
    let mut s = name.trim();
    for suffix in [" - Topic", " - Tema", "VEVO", " Official", "Official"] {
        s = s.trim_end_matches(suffix).trim();
    }
    // A quote would break out of the Lucene phrase the query wraps it in.
    s.replace(['"', '\\'], "").trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every artist in this library's history is a "- Topic" channel, so this
    /// is the normal case, not an edge case.
    #[test]
    fn youtube_channel_decoration_is_stripped() {
        assert_eq!(clean_artist("Charli XCX - Topic"), "Charli XCX");
        assert_eq!(clean_artist("PinkPantheress - Topic"), "PinkPantheress");
        assert_eq!(clean_artist("DuaLipaVEVO"), "DuaLipa");
        assert_eq!(clean_artist("  Ravyn Lenae  "), "Ravyn Lenae");
    }

    /// The name goes into a quoted Lucene phrase; a quote inside it would
    /// change the query rather than be searched for.
    #[test]
    fn quotes_cannot_escape_the_query() {
        assert_eq!(clean_artist(r#"Guns "N" Roses"#), "Guns N Roses");
        assert_eq!(clean_artist(r#"back\slash"#), "backslash");
    }

    /// The shape the API returns today.
    #[test]
    fn parses_a_bare_array_of_artists() {
        let body = serde_json::json!([
            {"artist_mbid": "aaa", "name": "Ariana Grande", "score": 3883},
            {"artist_mbid": "bbb", "name": "Lorde", "score": 3667},
        ]);
        let got = parse_similar(&body);
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].name, "Ariana Grande");
        assert_eq!(got[0].score, 3883);
    }

    /// The older wrapped shape, kept so a change on their side costs
    /// suggestions rather than all of them.
    #[test]
    fn parses_the_wrapped_shape_too() {
        let body = serde_json::json!([
            {"type": "header"},
            {"data": [{"artist_mbid": "ccc", "name": "Sia", "score": 3289}]},
        ]);
        let got = parse_similar(&body);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, "Sia");
    }

    /// An error page or an empty body must give an empty list, never a panic:
    /// this runs while the listener is waiting for a mix.
    #[test]
    fn a_broken_response_yields_nothing_rather_than_panicking() {
        assert!(parse_similar(&serde_json::json!({"error": "nope"})).is_empty());
        assert!(parse_similar(&serde_json::json!([])).is_empty());
        assert!(parse_similar(&serde_json::json!(null)).is_empty());
        // Rows missing the fields are dropped, the rest survive.
        let mixed = serde_json::json!([
            {"nonsense": 1},
            {"artist_mbid": "d", "name": "Sia", "score": 1},
        ]);
        assert_eq!(parse_similar(&mixed).len(), 1);
    }

    /// The measured failure: a busy MusicBrainz must not read as "no such
    /// artist". Both of these artists exist with a perfect match score, and
    /// treating the busy response as absence lost a taste direction its
    /// candidates.
    #[test]
    fn a_busy_service_is_not_the_same_as_no_such_artist() {
        let busy = serde_json::json!({
            "error": "The MusicBrainz web server is currently busy. Please try again later."
        });
        assert_eq!(read_mbid(&busy), Lookup::Unavailable);

        let empty = serde_json::json!({"artists": []});
        assert_eq!(read_mbid(&empty), Lookup::NotFound);

        let hit = serde_json::json!({"artists": [{"id": "f4fd", "name": "Ariana Grande"}]});
        assert_eq!(read_mbid(&hit), Lookup::Found("f4fd".into()));

        // A shape nobody expected is not evidence of absence either.
        assert_eq!(read_mbid(&serde_json::json!({})), Lookup::Unavailable);
        assert_eq!(
            read_mbid(&serde_json::json!("nonsense")),
            Lookup::Unavailable
        );
    }

    /// A name that reduces to nothing must be answered without a request —
    /// MusicBrainz counts every call against the rate limit, including the
    /// pointless ones.
    #[test]
    fn a_name_that_reduces_to_nothing_is_recognised() {
        for name in ["", "   ", "\"\"", "Official"] {
            assert!(clean_artist(name).is_empty(), "not empty: {name:?}");
        }
        // A leftover fragment is not empty and is allowed through; it simply
        // finds nothing, which is the correct answer for it.
        assert_eq!(clean_artist(" - Topic"), "- Topic");
    }

    /// Exceeding one request per second gets the application blocked, not
    /// throttled, so the gate has to hold even when callers overlap.
    #[tokio::test]
    async fn the_rate_limit_spaces_out_concurrent_callers() {
        let started = Instant::now();
        let handles: Vec<_> = (0..3).map(|_| tokio::spawn(rate_limit())).collect();
        for h in handles {
            h.await.unwrap();
        }
        // Three calls means two gaps. The third caller is the one that
        // matters: it must wait behind the second, not alongside it.
        let elapsed = started.elapsed();
        assert!(elapsed >= MIN_GAP * 2, "three calls took only {elapsed:?}");
        assert!(
            elapsed < MIN_GAP * 4,
            "three calls took {elapsed:?} — over-waiting"
        );
    }
}
