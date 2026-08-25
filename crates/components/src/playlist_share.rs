//! Send a playlist to another Kopuz user, and take one in.
//!
//! One dialog, two halves, because the two directions are the same thought:
//! the left turns a playlist into a code, the right turns a code back into a
//! playlist. Splitting them across two entry points would mean explaining twice
//! where the code comes from.
//!
//! The code is self-contained — see [`reader::share`] for why it carries the
//! playlist rather than pointing at it. The consequence lands here: a long
//! string in a box, so the UI says up front how long it is and whether it still
//! fits in a chat message, instead of letting the user find out when Discord
//! truncates it.
//!
//! Importing is deliberately allowed to half-succeed. A code can name a track
//! that this user's backend cannot hold — a SoundCloud id in a YouTube Music
//! playlist, a local rip that no longer matches anything. Refusing the whole
//! playlist over one such track would be worse than importing the rest and
//! saying plainly what didn't make it.

use config::{AppConfig, MusicService, MusicSource};
use dioxus::prelude::*;
use hooks::use_player_controller::PlayerController;
use reader::PlaylistStore;
use reader::share::{self, Jam, SharedPlaylist, SharedTrack};

use crate::toast::show_toast;
use crate::track_row::copy_to_clipboard;

/// A Discord message caps at 2000 characters. Codes are pasted into chat far
/// more often than anywhere else, so that is the line worth warning about.
const CHAT_MESSAGE_LIMIT: usize = 2000;

/// What the recipient ends up with, so the summary can be honest about the
/// difference between "imported" and "all of it".
#[derive(Default)]
struct ImportOutcome {
    added: usize,
    /// Named in the code, but nothing on this user's backend could hold it.
    skipped: usize,
}

impl ImportOutcome {
    fn summary(&self, name: &str) -> String {
        if self.skipped == 0 {
            format!("Imported \"{name}\" — {} tracks", self.added)
        } else {
            format!(
                "Imported \"{name}\" — {} tracks, {} couldn't be found",
                self.added, self.skipped,
            )
        }
    }
}

/// The moment currently playing, as a pasteable jam code.
///
/// `None` when nothing is queued — there is no moment to share.
fn current_jam(ctrl: &PlayerController, now_secs: u64) -> Option<String> {
    let queue = ctrl.queue.peek();
    if queue.is_empty() {
        return None;
    }
    let tracks: Vec<SharedTrack> = queue
        .iter()
        .map(|t| share::shared_track(&t.path.to_string_lossy(), &t.title, &t.artist, t.duration))
        .collect();
    let index = (*ctrl.current_queue_index.peek()).min(tracks.len() - 1);
    // Named after what is playing, so the receiver sees something meaningful
    // in the preview rather than the word "Jam".
    let name = queue
        .get(index)
        .map(|t| format!("{} - {}", t.artist, t.title))
        .unwrap_or_else(|| "Jam".to_string());
    Some(share::encode_jam(&Jam {
        playlist: SharedPlaylist { name, tracks },
        index,
        position_ms: (*ctrl.current_song_progress.peek()).saturating_mul(1000),
        sent_at: now_secs,
    }))
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// What a pasted code turned out to be.
enum Pasted {
    Playlist(SharedPlaylist),
    Jam(Jam),
    Bad(String),
}

fn read_pasted(raw: &str) -> Option<Pasted> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    // Checked by prefix rather than by trying both decoders: a jam pasted into
    // this box should say "that is a jam", not "damaged playlist link".
    if trimmed.contains(share::JAM_PREFIX) {
        return Some(match share::decode_jam(trimmed) {
            Ok(j) => Pasted::Jam(j),
            Err(e) => Pasted::Bad(e),
        });
    }
    Some(match share::decode(trimmed) {
        Ok(p) => Pasted::Playlist(p),
        Err(e) => Pasted::Bad(e),
    })
}

/// Turn one of the user's playlists into the shareable form.
///
/// Server playlists store bare catalog ids, so the source prefix has to be put
/// back on. Local playlists store real paths, which cannot travel — the library
/// is consulted for the title/artist that can.
fn to_shared(
    name: String,
    track_keys: &[String],
    is_ytmusic: bool,
    library: &reader::Library,
) -> SharedPlaylist {
    let tracks = track_keys
        .iter()
        .map(|key| {
            if is_ytmusic && !key.contains(':') {
                // A bare YouTube catalog id — portable as-is.
                return share::shared_track(&format!("ytmusic:{key}"), "", "", 0);
            }
            let meta = library
                .tracks
                .iter()
                .chain(library.jellyfin_tracks.iter())
                .find(|t| t.path.to_string_lossy() == *key);
            match meta {
                Some(t) => share::shared_track(key, &t.title, &t.artist, t.duration),
                // Not in the library (a stale entry, or a source this build
                // doesn't index). A portable path still travels on its own; a
                // local path becomes a title we can at least try to match.
                None => share::shared_track(key, &file_stem(key), "", 0),
            }
        })
        .collect();
    SharedPlaylist { name, tracks }
}

/// Last resort title for a local file with no library entry: its filename.
fn file_stem(path: &str) -> String {
    std::path::Path::new(path)
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

/// The YouTube video id inside a portable path, if that's what this is.
fn yt_id(track: &SharedTrack) -> Option<String> {
    track
        .path
        .as_deref()?
        .strip_prefix("ytmusic:")
        .and_then(|rest| rest.split(':').next())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

#[component]
pub fn PlaylistShareModal(
    config: Signal<AppConfig>,
    playlist_store: Signal<PlaylistStore>,
    library: Signal<reader::Library>,
    on_close: EventHandler<()>,
    on_imported: EventHandler<()>,
) -> Element {
    let is_server = config.read().active_source == MusicSource::Server;
    let is_ytmusic = matches!(
        config.read().server.as_ref().map(|s| s.service),
        Some(MusicService::YtMusic)
    );

    // Left half: which playlist to hand over.
    let mut selected = use_signal(|| None::<String>);
    // Right half: what was pasted, and what it turned out to be.
    let mut pasted = use_signal(String::new);
    let mut importing = use_signal(|| false);

    // The playlists on offer are the ones the user is actually looking at.
    let options: Vec<(String, String, usize)> = {
        let store = playlist_store.read();
        if is_server {
            store
                .jellyfin_playlists
                .iter()
                .map(|p| (p.id.clone(), p.name.clone(), p.tracks.len()))
                .collect()
        } else {
            store
                .playlists
                .iter()
                .map(|p| (p.id.clone(), p.name.clone(), p.tracks.len()))
                .collect()
        }
    };

    // Encode on the fly: cheap, and it keeps the length readout honest as the
    // selection changes.
    let code = selected.read().as_ref().and_then(|id| {
        let store = playlist_store.read();
        let lib = library.read();
        let shared = if is_server {
            let pl = store.jellyfin_playlists.iter().find(|p| p.id == *id)?;
            to_shared(pl.name.clone(), &pl.tracks, is_ytmusic, &lib)
        } else {
            let pl = store.playlists.iter().find(|p| p.id == *id)?;
            let keys: Vec<String> = pl
                .tracks
                .iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect();
            to_shared(pl.name.clone(), &keys, false, &lib)
        };
        (!shared.tracks.is_empty()).then(|| share::encode(&shared))
    });

    // Decode as they type so the paste box can show what they're about to get
    // — and say why a bad code is bad before they press anything.
    let preview = read_pasted(&pasted.read());

    let ctrl = try_consume_context::<PlayerController>();

    // Join: load the sender's queue and land where they are *now*, not where
    // they were when the code was made.
    let mut do_join = move |jam: Jam| {
        let Some(ctrl) = ctrl else {
            show_toast("Playback isn't ready yet".to_string());
            return;
        };
        let (at, position_ms) = share::catch_up(&jam, now_secs());
        let (tracks, index, on_anchor, dropped) = crate::jam::jam_tracks(&jam, at);
        if tracks.is_empty() {
            show_toast("Nothing in that jam can be played here".to_string());
            return;
        }
        // If the track they were on cannot play here, the next one starts from
        // its beginning rather than from a position that belongs to a
        // different song.
        let secs = if on_anchor { position_ms / 1000 } else { 0 };
        let mut ctrl = ctrl;
        ctrl.play_queue_at(tracks, index, secs);
        pasted.set(String::new());
        show_toast(if dropped == 0 {
            "Joined the jam".to_string()
        } else {
            format!("Joined the jam — {dropped} tracks couldn't be played here")
        });
        on_close.call(());
    };

    let mut do_import = move |decoded: SharedPlaylist| {
        importing.set(true);
        spawn(async move {
            let outcome = import_playlist(
                decoded.clone(),
                config,
                playlist_store,
                is_server,
                is_ytmusic,
            )
            .await;
            importing.set(false);
            match outcome {
                Ok(o) => {
                    show_toast(o.summary(&decoded.name));
                    pasted.set(String::new());
                    on_imported.call(());
                }
                Err(e) => show_toast(format!("Import failed: {e}")),
            }
        });
    };

    rsx! {
        div {
            class: "fixed inset-0 bg-black/80 flex items-center justify-center z-50 p-4",
            // Dismissal is blocked while an import runs, matching
            // `SpotifyImportModal`. The import task is bound to this scope, so
            // unmounting mid-flight cancels it silently — the tracks that were
            // already added stay, and nothing says so.
            onclick: move |_| if !*importing.read() { on_close.call(()) },
            div {
                class: "bg-neutral-900 rounded-xl border border-white/10 w-full max-w-3xl max-h-[85vh] overflow-hidden flex flex-col",
                onclick: move |e| e.stop_propagation(),

                div {
                    class: "flex items-center justify-between px-6 py-4 border-b border-white/10",
                    div {
                        h2 { class: "text-white font-semibold", "Share playlists & jams" }
                        p { class: "text-xs text-white/40 mt-0.5",
                            "The code carries the music itself — no account, no server, nothing to expire. A jam code also carries where you are."
                        }
                    }
                    button {
                        class: "text-white/40 hover:text-white transition-colors px-2 disabled:opacity-30",
                        disabled: *importing.read(),
                        onclick: move |_| if !*importing.read() { on_close.call(()) },
                        i { class: "fa-solid fa-xmark" }
                    }
                }

                div {
                    class: "grid md:grid-cols-2 divide-y md:divide-y-0 md:divide-x divide-white/10 overflow-y-auto",

                    // ── Left: hand one over ───────────────────────────────
                    div {
                        class: "p-6 flex flex-col gap-3",
                        div {
                            class: "flex items-center gap-2 text-white/90 text-sm font-medium",
                            i { class: "fa-solid fa-share-nodes text-indigo-300" }
                            "Send a playlist"
                        }

                        if options.is_empty() {
                            p { class: "text-white/40 text-sm py-6 text-center",
                                "No playlists here yet."
                            }
                        } else {
                            select {
                                class: "w-full bg-white/5 border border-white/10 rounded-lg px-3 py-2 text-white text-sm focus:outline-none focus:border-indigo-400",
                                onchange: move |e| {
                                    let v = e.value();
                                    selected.set((!v.is_empty()).then_some(v));
                                },
                                option { value: "", "Choose a playlist…" }
                                for (id, name, count) in options.iter() {
                                    option { value: "{id}", "{name} ({count})" }
                                }
                            }
                        }

                        // Sending the moment, not the list. Sits under the
                        // playlist picker because it is the same gesture — turn
                        // something you have into a code — and separating it
                        // into its own dialog would mean explaining the code
                        // twice.
                        if let Some(jam_code) = ctrl.and_then(|c| current_jam(&c, now_secs())) {
                            div {
                                class: "rounded-lg border border-violet-400/25 bg-violet-500/5 px-3 py-2.5 flex items-center justify-between gap-3",
                                div {
                                    div { class: "text-white/90 text-sm font-medium",
                                        i { class: "fa-solid fa-tower-broadcast mr-1.5 text-violet-300" }
                                        "Send this moment"
                                    }
                                    p { class: "text-white/40 text-[11px] mt-0.5",
                                        "They land where you are, even if they paste it later."
                                    }
                                }
                                button {
                                    class: "shrink-0 px-3 py-1.5 rounded-lg bg-violet-600 hover:bg-violet-500 text-white text-sm font-medium transition-colors",
                                    onclick: move |_| {
                                        copy_to_clipboard(&jam_code);
                                        show_toast("Jam code copied".to_string());
                                    },
                                    i { class: "fa-solid fa-copy mr-1.5" }
                                    "Copy"
                                }
                            }
                        }

                        if let Some(code) = code.clone() {
                            {
                                let len = code.len();
                                let fits = len <= CHAT_MESSAGE_LIMIT;
                                rsx! {
                                    textarea {
                                        class: "w-full h-32 bg-black/40 border border-white/10 rounded-lg px-3 py-2 text-white/70 text-[11px] font-mono resize-none focus:outline-none focus:border-indigo-400 break-all",
                                        readonly: true,
                                        value: "{code}",
                                    }
                                    div {
                                        class: "flex items-center justify-between gap-2",
                                        span {
                                            class: if fits { "text-[11px] text-white/40" } else { "text-[11px] text-amber-300" },
                                            if fits {
                                                "{len} characters — fits in a chat message"
                                            } else {
                                                "{len} characters — too long for Discord, send it as a file"
                                            }
                                        }
                                        button {
                                            class: "px-3 py-1.5 rounded-lg bg-indigo-600 hover:bg-indigo-500 text-white text-sm font-medium transition-colors",
                                            onclick: move |_| {
                                                copy_to_clipboard(&code);
                                                show_toast("Playlist code copied".to_string());
                                            },
                                            i { class: "fa-solid fa-copy mr-1.5" }
                                            "Copy"
                                        }
                                    }
                                }
                            }
                        } else if selected.read().is_some() {
                            p { class: "text-white/40 text-sm", "That playlist is empty." }
                        }
                    }

                    // ── Right: take one in ────────────────────────────────
                    div {
                        class: "p-6 flex flex-col gap-3",
                        div {
                            class: "flex items-center gap-2 text-white/90 text-sm font-medium",
                            i { class: "fa-solid fa-inbox text-emerald-300" }
                            "Take one in"
                        }

                        textarea {
                            class: "w-full h-32 bg-white/5 border border-white/10 rounded-lg px-3 py-2 text-white text-[11px] font-mono resize-none placeholder:text-white/30 focus:outline-none focus:border-emerald-400 break-all",
                            placeholder: "Paste a kopuz:pl:… or kopuz:jam:… code here",
                            value: "{pasted}",
                            oninput: move |e| pasted.set(e.value()),
                        }

                        match preview {
                            Some(Pasted::Jam(jam)) => {
                                let count = jam.playlist.tracks.len();
                                let (at, position_ms) = share::catch_up(&jam, now_secs());
                                let where_ = jam
                                    .playlist
                                    .tracks
                                    .get(at)
                                    .map(|t| format!("{} — {}", t.artist, t.title))
                                    .unwrap_or_default();
                                let clock = format!(
                                    "{}:{:02}",
                                    position_ms / 60_000,
                                    (position_ms / 1000) % 60
                                );
                                rsx! {
                                    div {
                                        class: "rounded-lg bg-violet-500/10 border border-violet-400/25 px-3 py-2",
                                        div { class: "text-white text-sm font-medium",
                                            i { class: "fa-solid fa-tower-broadcast mr-1.5 text-violet-300" }
                                            "Jam — {count} tracks"
                                        }
                                        div { class: "text-violet-200/70 text-xs mt-0.5",
                                            "Joins at {clock} in {where_}"
                                        }
                                    }
                                    button {
                                        class: "w-full px-4 py-2 rounded-lg bg-violet-600 hover:bg-violet-500 text-white text-sm font-semibold transition-colors",
                                        onclick: move |_| do_join(jam.clone()),
                                        i { class: "fa-solid fa-play mr-1.5" }
                                        "Join the jam"
                                    }
                                }
                            }
                            Some(Pasted::Playlist(decoded)) => {
                                let name = decoded.name.clone();
                                let count = decoded.tracks.len();
                                let busy = *importing.read();
                                rsx! {
                                    div {
                                        class: "rounded-lg bg-emerald-500/10 border border-emerald-400/25 px-3 py-2",
                                        div { class: "text-white text-sm font-medium", "{name}" }
                                        div { class: "text-emerald-200/70 text-xs", "{count} tracks" }
                                    }
                                    button {
                                        class: "w-full px-4 py-2 rounded-lg bg-emerald-600 hover:bg-emerald-500 text-white text-sm font-semibold transition-colors disabled:opacity-50",
                                        disabled: busy,
                                        onclick: move |_| do_import(decoded.clone()),
                                        if busy {
                                            i { class: "fa-solid fa-circle-notch fa-spin mr-1.5" }
                                            "Adding…"
                                        } else {
                                            i { class: "fa-solid fa-plus mr-1.5" }
                                            "Add to my playlists"
                                        }
                                    }
                                }
                            }
                            Some(Pasted::Bad(e)) => rsx! {
                                div {
                                    class: "rounded-lg bg-rose-500/10 border border-rose-400/25 px-3 py-2 text-rose-200 text-xs",
                                    i { class: "fa-solid fa-triangle-exclamation mr-1.5" }
                                    "{e}"
                                }
                            },
                            None => rsx! {
                                p { class: "text-white/40 text-xs",
                                    "Whoever sent it can copy the code from the left-hand side."
                                }
                            },
                        }
                    }
                }
            }
        }
    }
}

/// Put a decoded playlist into whichever backend the user is on.
///
/// A YouTube Music playlist is created empty and filled in a second call: the
/// create endpoint quietly drops ids it doesn't like, which reads as "the
/// playlist imported but is empty". The add endpoint is the one the Spotify
/// import already trusts for bulk.
async fn import_playlist(
    decoded: SharedPlaylist,
    config: Signal<AppConfig>,
    mut playlist_store: Signal<PlaylistStore>,
    is_server: bool,
    is_ytmusic: bool,
) -> Result<ImportOutcome, String> {
    let mut outcome = ImportOutcome::default();

    // Resolve every track to something this backend can store. Tracks that
    // travelled as metadata get matched the way the Spotify import matches
    // them; ones that travelled as an id are already usable.
    let mut yt_ids: Vec<String> = Vec::new();
    let mut local_paths: Vec<String> = Vec::new();
    for track in &decoded.tracks {
        if let Some(id) = yt_id(track) {
            yt_ids.push(id);
            continue;
        }
        if let Some(path) = track.path.clone() {
            // A SoundCloud id — playable, but no server can hold it.
            local_paths.push(path);
            continue;
        }
        let matched = server::spotify::matcher::match_external_to_yt(
            &track.title,
            &[track.artist.clone()],
            track.duration,
        )
        .await;
        match matched.and_then(|t| {
            t.path
                .to_string_lossy()
                .strip_prefix("ytmusic:")
                .and_then(|rest| rest.split(':').next())
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty())
        }) {
            Some(id) => yt_ids.push(id),
            None => outcome.skipped += 1,
        }
    }

    if yt_ids.is_empty() && local_paths.is_empty() {
        return Err("none of those tracks could be found".to_string());
    }

    if is_server && is_ytmusic {
        let (token, _) = {
            let conf = config.peek().clone();
            let server = conf.server.clone().ok_or("no server configured")?;
            let token = server.access_token.clone().ok_or("not signed in")?;
            (token, server)
        };
        let yt = server::ytmusic::YouTubeMusicClient::with_cookies(token);
        let id = yt.create_playlist(&decoded.name, "", &[]).await?;
        if !yt_ids.is_empty() {
            let refs: Vec<&str> = yt_ids.iter().map(|s| s.as_str()).collect();
            yt.add_videos_to_playlist(&id, &refs).await?;
        }
        outcome.added = yt_ids.len();

        // SoundCloud tracks can't live on YouTube, so they ride in the same
        // local overlay the add-to-playlist path uses. They play; they just
        // don't sync.
        if !local_paths.is_empty() {
            let mut store = playlist_store.write();
            let entry = store.external_tracks.entry(id.clone()).or_default();
            for p in &local_paths {
                entry.push(std::path::PathBuf::from(p));
            }
            outcome.added += local_paths.len();
        }
        playlist_store
            .write()
            .jellyfin_playlists
            .push(reader::models::JellyfinPlaylist {
                id,
                name: decoded.name.clone(),
                tracks: yt_ids,
                image_tag: None,
                cover_path: None,
            });
    } else {
        // Local playlist: portable paths are playable directly.
        let tracks: Vec<std::path::PathBuf> = yt_ids
            .iter()
            .map(|id| std::path::PathBuf::from(format!("ytmusic:{id}")))
            .chain(local_paths.iter().map(std::path::PathBuf::from))
            .collect();
        outcome.added = tracks.len();
        playlist_store
            .write()
            .playlists
            .push(reader::models::Playlist {
                id: uuid::Uuid::new_v4().to_string(),
                name: decoded.name.clone(),
                tracks,
                cover_path: None,
            });
    }

    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_partial_import_says_so_rather_than_claiming_success() {
        let full = ImportOutcome {
            added: 12,
            skipped: 0,
        };
        assert_eq!(full.summary("Techno"), "Imported \"Techno\" — 12 tracks");

        let partial = ImportOutcome {
            added: 10,
            skipped: 2,
        };
        assert!(
            partial.summary("Techno").contains("2 couldn't be found"),
            "a partial import must not read like a complete one: {}",
            partial.summary("Techno"),
        );
    }

    #[test]
    fn server_playlists_regain_the_source_prefix_they_dont_store() {
        // Server playlists hold bare catalog ids; the code needs a real path.
        let lib = reader::Library::default();
        let shared = to_shared(
            "Techno".into(),
            &["abc123".to_string(), "def456".to_string()],
            true,
            &lib,
        );
        assert_eq!(shared.tracks[0].path.as_deref(), Some("ytmusic:abc123"));
        assert!(shared.tracks.iter().all(|t| t.is_portable()));
    }

    #[test]
    fn a_local_file_travels_as_metadata_not_as_a_path() {
        let lib = reader::Library::default();
        let shared = to_shared(
            "Rips".into(),
            &[r"C:\Users\someone\Music\Song Name.mp3".to_string()],
            false,
            &lib,
        );
        assert_eq!(shared.tracks[0].path, None, "sender's path must not travel");
        assert_eq!(shared.tracks[0].title, "Song Name");
    }

    #[test]
    fn yt_id_reads_both_path_shapes() {
        // Codes carry `ytmusic:<id>`; the app's own tracks carry extra segments.
        let bare = SharedTrack {
            path: Some("ytmusic:abc123".into()),
            title: String::new(),
            artist: String::new(),
            duration: 0,
        };
        let full = SharedTrack {
            path: Some("ytmusic:abc123:urlhex_00".into()),
            ..bare.clone()
        };
        assert_eq!(yt_id(&bare).as_deref(), Some("abc123"));
        assert_eq!(yt_id(&full).as_deref(), Some("abc123"));

        let sc = SharedTrack {
            path: Some("soundcloud:9f2a".into()),
            ..bare.clone()
        };
        assert_eq!(yt_id(&sc), None, "SoundCloud must not be taken for YouTube");
    }

    /// The paste box has to tell the two token kinds apart before decoding, or
    /// a jam reads as a damaged playlist. Both codes are built here rather
    /// than written out, so the test cannot drift from the format.
    #[test]
    fn the_paste_box_tells_the_two_code_kinds_apart() {
        let playlist = SharedPlaylist {
            name: "x".into(),
            tracks: vec![SharedTrack {
                path: Some("ytmusic:aaaaaaaaaaa".into()),
                title: "t".into(),
                artist: "a".into(),
                duration: 100,
            }],
        };
        let jam_code = share::encode_jam(&Jam {
            playlist: playlist.clone(),
            index: 0,
            position_ms: 0,
            sent_at: 1_000,
        });
        let list_code = share::encode(&playlist);

        assert!(matches!(read_pasted(&jam_code), Some(Pasted::Jam(_))));
        assert!(matches!(read_pasted(&list_code), Some(Pasted::Playlist(_))));
        assert!(matches!(read_pasted("nonsense"), Some(Pasted::Bad(_))));
        assert!(read_pasted("   ").is_none());
    }
}
