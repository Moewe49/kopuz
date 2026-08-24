//! The whole recommendation pipeline, end to end, on this machine's real
//! listening history.
//!
//! Four pieces that each pass their own tests are not a feature until they run
//! together on real data, so this wires them up and prints what a listener
//! would actually be shown:
//!
//!   history -> embed -> cluster into tastes
//!           -> ListenBrainz for candidate artists
//!           -> title filter -> embed -> rank by style
//!
//! The candidate source is deliberately not YouTube search. The first version
//! of this tool asked for "<seed title> similar songs" and every suggestion it
//! produced was a re-upload of the seed — "Sympathy is a knife (official lyric
//! video)" scoring 0.967 against a cluster whose first member was Sympathy is
//! a knife. Search answers "what matches these words", which is the wrong
//! question. ListenBrainz answers "who else do these listeners play".
//!
//! Vectors are cached between runs, because the cost is downloading and
//! decoding thirty seconds of audio, not the arithmetic.
//!
//! Run: cargo run -p embed --release --example build_mixes -- [seeds] [per_mix]

use embed::Embedder;
use reader::vectors::VectorStore;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

const WINDOW_START: &str = "45";
const WINDOW_SECS: &str = "30";
/// Below this a cluster is a leftovers bin, not a taste worth a mix.
const MIN_MIX_MEMBERS: usize = 3;
/// Related artists to pull per taste direction. The net is meant to be wide
/// and cheap; the audio model does the discriminating.
const ARTISTS_PER_MIX: usize = 8;
/// Candidates to audition per artist. More is wasted work — they are all by
/// the same artist, so they cluster tightly anyway.
const TRACKS_PER_ARTIST: usize = 2;
/// A candidate this close to a seed is that seed under another upload, not a
/// discovery. Calibrated against the listener's own favourites, which topped
/// out at 0.905 between two different tracks they both love.
const SAME_TRACK: f32 = 0.97;

fn config_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("APPDATA").map(|p| PathBuf::from(p).join("temidaradev/kopuz/config"))
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config/temidaradev/kopuz"))
    }
}

/// Decode one window to 16 kHz mono f32. The window starts inside the track:
/// the opening seconds are an intro, and an intro is not what a track sounds
/// like.
fn decode(url: &str) -> Option<Vec<f32>> {
    let ffmpeg = std::env::var("KOPUZ_FFMPEG").unwrap_or_else(|_| "ffmpeg".into());
    let out = std::process::Command::new(ffmpeg)
        .args([
            "-v",
            "error",
            "-ss",
            WINDOW_START,
            "-t",
            WINDOW_SECS,
            "-i",
            url,
            "-f",
            "f32le",
            "-acodec",
            "pcm_f32le",
            "-ac",
            "1",
            "-ar",
            "16000",
            "-",
        ])
        .output()
        .ok()?;
    if !out.status.success() || out.stdout.is_empty() {
        return None;
    }
    Some(
        out.stdout
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect(),
    )
}

/// Artist, title and stream URL for one video id. Anonymous; the URL is never
/// printed, because it carries a signature.
async fn resolve(id: &str) -> Option<(String, String, String)> {
    use server::ytmusic::clients::VISIONOS;
    use server::ytmusic::innertube::{self, PlayerExtras, visitor_id};

    let visitor = visitor_id(None).await.ok()?;
    let json = innertube::player(
        VISIONOS,
        id,
        None,
        PlayerExtras {
            content_pot: None,
            visitor_data: Some(&visitor),
            signature_timestamp: None,
        },
    )
    .await
    .ok()?;

    let artist = json
        .pointer("/videoDetails/author")
        .and_then(|v| v.as_str())
        .unwrap_or("?")
        .to_string();
    let title = json
        .pointer("/videoDetails/title")
        .and_then(|v| v.as_str())
        .unwrap_or("?")
        .to_string();
    let url = json
        .pointer("/streamingData/adaptiveFormats")
        .and_then(|v| v.as_array())?
        .iter()
        .filter(|f| {
            f.get("mimeType")
                .and_then(|m| m.as_str())
                .is_some_and(|m| m.starts_with("audio/"))
                && f.get("url").is_some()
                && !f.get("audioTrack").is_some_and(|t| {
                    !t.get("audioIsDefault")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                })
        })
        .max_by_key(|f| f.get("bitrate").and_then(|v| v.as_u64()).unwrap_or(0))
        .and_then(|f| f.get("url"))
        .and_then(|v| v.as_str())?
        .to_string();
    Some((artist, title, url))
}

async fn vector_for(embedder: &mut Embedder, id: &str) -> Option<(Vec<f32>, String, String)> {
    let (artist, title, url) = resolve(id).await?;
    let pcm = decode(&url)?;
    let v = embedder.vectors(&pcm).ok()?;
    Some((v.style, artist, title))
}

/// Letters and digits only, lowercased — enough to tell "Steve Lacy - Topic"
/// and "Steve Lacy" apart from "Mac DeMarco".
fn name_key(s: &str) -> String {
    scrobble::similar::clean_artist(s)
        .to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect()
}

/// YouTube search on an artist name is fuzzy, so check that the track it
/// returned is actually by the artist that was asked for.
///
/// This catches gross mismatches, not every one: searching for the rapper
/// Noname returns the band No Name, and after normalisation the two names are
/// identical. Nothing short of matching on MusicBrainz ids would separate
/// those, and YouTube search does not accept ids.
fn artist_matches(query: &str, found: &str) -> bool {
    let (q, f) = (name_key(query), name_key(found));
    !q.is_empty() && !f.is_empty() && (f.contains(&q) || q.contains(&f))
}

fn label(id: &str, meta: &HashMap<String, (String, String)>) -> String {
    meta.get(id)
        .map(|(a, t)| format!("{a} — {t}"))
        .unwrap_or_else(|| id.to_string())
}

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let n_seeds: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(24);
    let per_mix: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(6);

    let model = std::env::var("KOPUZ_MODEL").unwrap_or_else(|_| "discogs-effnet.onnx".into());
    let mut embedder = match Embedder::open(&model) {
        Ok(e) => e,
        Err(e) => {
            eprintln!(
                "{e}\nSet KOPUZ_MODEL, or download it from:\n  {}",
                embed::model::MODEL_URL
            );
            return;
        }
    };

    let Some(dir) = config_dir() else {
        eprintln!("no config dir");
        return;
    };
    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("config.json")).unwrap_or_default())
            .unwrap_or_default();
    let counts: HashMap<String, u64> = cfg
        .get("listen_counts")
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    // Keys look like `ytmusic:<id>:urlhex_...`; only the id is wanted, and only
    // from tracks played often enough to mean something.
    let mut seeds: Vec<(String, u64)> = counts
        .iter()
        .filter(|&(_, &n)| n >= 3)
        .filter_map(|(k, &n)| {
            let mut parts = k.split(':');
            if parts.next()? != "ytmusic" {
                return None;
            }
            Some((parts.next()?.to_string(), n))
        })
        .collect();
    seeds.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    seeds.truncate(n_seeds);
    println!("{} seed tracks from the history\n", seeds.len());

    // ---- vectors, cached between runs -----------------------------------
    let store_path = dir.join("style_vectors.bin");
    let meta_path = dir.join("style_meta.json");
    let mut store = VectorStore::load(&store_path, 400).unwrap_or_else(|e| {
        eprintln!("vector store unreadable ({e}), starting fresh");
        VectorStore::new(400)
    });
    let mut meta: HashMap<String, (String, String)> =
        serde_json::from_str(&std::fs::read_to_string(&meta_path).unwrap_or_default())
            .unwrap_or_default();

    for (id, plays) in &seeds {
        if store.contains(id) && meta.contains_key(id) {
            continue;
        }
        match vector_for(&mut embedder, id).await {
            Some((v, artist, title)) => {
                println!("  embedded {artist} — {title}  ({plays} plays)");
                let _ = store.insert(id.clone(), v);
                meta.insert(id.clone(), (artist, title));
            }
            None => println!("  skipped {id} (no audio)"),
        }
        tokio::time::sleep(std::time::Duration::from_millis(600)).await;
    }

    let mut ids = Vec::new();
    let mut vectors = Vec::new();
    for (id, _) in &seeds {
        if let Some(v) = store.get(id) {
            ids.push(id.clone());
            vectors.push(v.to_vec());
        }
    }
    if vectors.len() < 4 {
        println!("\nonly {} vectors — not enough to cluster", vectors.len());
        return;
    }

    // Everything already in the history, so discovery cannot recommend it back.
    let heard_ids: HashSet<String> = ids.iter().cloned().collect();
    let heard_artists: HashSet<String> = ids
        .iter()
        .filter_map(|id| meta.get(id))
        .map(|(a, _)| scrobble::similar::clean_artist(a).to_lowercase())
        .collect();

    // ---- taste directions ------------------------------------------------
    let k = reader::taste::best_k(&vectors, 6, 42);
    let clusters = reader::taste::cluster(&vectors, k, 42);
    println!(
        "\n{} vectors -> {} taste direction(s)",
        vectors.len(),
        clusters.len()
    );

    let http = reqwest::Client::new();
    let yt = server::ytmusic::YouTubeMusicClient::with_cookies(String::new());

    for (n, c) in clusters.iter().enumerate() {
        if c.members.len() < MIN_MIX_MEMBERS {
            println!(
                "\n--- direction {} skipped: only {} track(s) ---",
                n + 1,
                c.members.len()
            );
            continue;
        }
        println!(
            "\n=== direction {} — {} tracks, cohesion {:.3} ===",
            n + 1,
            c.members.len(),
            c.cohesion
        );
        for &i in c.members.iter().take(4) {
            println!("   {}", label(&ids[i], &meta));
        }

        // ---- who else do these listeners play ---------------------------
        let mut related: Vec<String> = Vec::new();
        for &i in c.members.iter().take(3) {
            let Some((artist, _)) = meta.get(&ids[i]).cloned() else {
                continue;
            };
            let mbid = match scrobble::similar::artist_mbid(&http, &artist).await {
                scrobble::similar::Lookup::Found(id) => id,
                scrobble::similar::Lookup::NotFound => {
                    println!("   (no MusicBrainz entry for {artist})");
                    continue;
                }
                scrobble::similar::Lookup::Unavailable => {
                    println!("   (MusicBrainz unavailable for {artist} — not the same as absent)");
                    continue;
                }
            };
            for a in scrobble::similar::similar_artists(&http, &mbid, 40).await {
                let key = a.name.to_lowercase();
                if heard_artists.contains(&key) || related.iter().any(|r| r.to_lowercase() == key) {
                    continue;
                }
                related.push(a.name);
            }
        }
        related.truncate(ARTISTS_PER_MIX);
        if related.is_empty() {
            println!("   (no related artists found)");
            continue;
        }
        println!("   candidates from: {}", related.join(", "));

        // ---- audition them ----------------------------------------------
        let mut ranked: Vec<(f32, String)> = Vec::new();
        let mut filtered = 0usize;
        let mut duplicates = 0usize;
        let mut mismatched = 0usize;
        for artist in &related {
            let Ok(found) = yt.search_tracks(artist).await else {
                continue;
            };
            let mut taken = 0usize;
            for t in found {
                if taken >= TRACKS_PER_ARTIST {
                    break;
                }
                if reader::candidates::reject(&t.title).is_some() {
                    filtered += 1;
                    continue;
                }
                let path = t.path.to_string_lossy().to_string();
                let Some(cid) = path.split(':').nth(1).map(str::to_string) else {
                    continue;
                };
                if heard_ids.contains(&cid) {
                    continue;
                }
                let vec = match store.get(&cid) {
                    Some(v) => v.to_vec(),
                    None => match vector_for(&mut embedder, &cid).await {
                        Some((v, a, ti)) => {
                            let _ = store.insert(cid.clone(), v.clone());
                            meta.insert(cid.clone(), (a, ti));
                            tokio::time::sleep(std::time::Duration::from_millis(600)).await;
                            v
                        }
                        None => continue,
                    },
                };
                // The same song under a different upload is not a discovery.
                // Checked in style space rather than by title, because the
                // titles differ ("... (official lyric video)") while the audio
                // does not.
                if c.members
                    .iter()
                    .any(|&i| embed::similarity(&vec, &vectors[i]) >= SAME_TRACK)
                {
                    duplicates += 1;
                    continue;
                }
                if let Some((found_artist, _)) = meta.get(&cid)
                    && !artist_matches(artist, found_artist)
                {
                    mismatched += 1;
                    continue;
                }
                taken += 1;
                ranked.push((embed::similarity(&vec, &c.centroid), label(&cid, &meta)));
            }
        }
        ranked.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        println!(
            "   ---- suggested — dropped {filtered} non-music, {duplicates} same-track, {mismatched} wrong-artist ----"
        );
        for (score, name) in ranked.iter().take(per_mix) {
            println!("   {score:.3}  {name}");
        }
        if ranked.is_empty() {
            println!("   (nothing survived)");
        }

        let _ = store.save(&store_path);
        let _ = std::fs::write(&meta_path, serde_json::to_string(&meta).unwrap_or_default());
    }

    println!(
        "\nstore: {} vectors at {}",
        store.len(),
        store_path.display()
    );
}
