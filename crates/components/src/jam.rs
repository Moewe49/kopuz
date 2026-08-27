//! Listening to the same thing as someone else.
//!
//! A jam code carries the queue plus where you are in it — see
//! [`reader::share`] for the format, and for why it is one-shot rather than a
//! live session.
//!
//! Both directions live in one control beside the queue. Sending used to be
//! the only thing here and joining lived in the playlist share dialog: three
//! navigations away from the music, under a heading about playlists, which is
//! not where anyone holding a jam code would think to look.

use dioxus::prelude::*;
use hooks::use_player_controller::PlayerController;
use reader::share::{self, Jam, SharedPlaylist, SharedTrack};

use crate::toast::show_toast;
use crate::track_row::copy_to_clipboard;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The current queue and position, as a pasteable code.
///
/// `None` when nothing is queued — there is no moment to hand over.
pub fn current_jam_code(ctrl: &PlayerController) -> Option<String> {
    let queue = ctrl.queue.peek();
    if queue.is_empty() {
        return None;
    }
    let tracks: Vec<SharedTrack> = queue
        .iter()
        .map(|t| share::shared_track(&t.path.to_string_lossy(), &t.title, &t.artist, t.duration))
        .collect();
    let index = (*ctrl.current_queue_index.peek()).min(tracks.len() - 1);
    // Named after what is playing, so the receiver's preview says something
    // recognisable rather than the word "Jam".
    let name = queue
        .get(index)
        .map(|t| format!("{} - {}", t.artist, t.title))
        .unwrap_or_else(|| "Jam".to_string());
    Some(share::encode_jam(&Jam {
        playlist: SharedPlaylist { name, tracks },
        index,
        position_ms: (*ctrl.current_song_progress.peek()).saturating_mul(1000),
        sent_at: now_secs(),
    }))
}

/// Turn the tracks a jam carries into something playable, and map the
/// sender's position onto the result.
///
/// Only tracks that travelled with a portable id can play directly. One that
/// travelled as metadata would need a search before playback could start,
/// which is the wrong trade for something meant to begin immediately.
///
/// Dropping tracks shifts every index after them, so the mapping is done here
/// rather than by the caller: `at` is an index into the jam's own list, and
/// what comes back indexes the playable list. If the track the sender was on
/// is itself unplayable here, the next one that is plays from its start —
/// carrying the position across would drop the listener into the middle of a
/// song the sender had not reached.
pub fn jam_tracks(jam: &Jam, at: usize) -> (Vec<reader::models::Track>, usize, bool, usize) {
    let mut out = Vec::new();
    let mut dropped = 0usize;
    let mut mapped = 0usize;
    let mut landed_on_anchor = false;
    for (i, t) in jam.playlist.tracks.iter().enumerate() {
        let Some(path) = &t.path else {
            dropped += 1;
            continue;
        };
        if i == at {
            mapped = out.len();
            landed_on_anchor = true;
        } else if i < at {
            // Keeps `mapped` pointing at the first playable track at or after
            // the anchor, for the case where the anchor itself is dropped.
            mapped = out.len() + 1;
        }
        out.push(reader::models::Track {
            path: std::path::PathBuf::from(path),
            album_id: String::new(),
            title: t.title.clone(),
            artist: t.artist.clone(),
            album: String::new(),
            duration: t.duration,
            khz: 0,
            bitrate: 0,
            track_number: None,
            disc_number: None,
            musicbrainz_release_id: None,
            musicbrainz_recording_id: None,
            musicbrainz_track_id: None,
            playlist_item_id: None,
            artists: vec![t.artist.clone()],
        });
    }
    let mapped = mapped.min(out.len().saturating_sub(1));
    (out, mapped, landed_on_anchor, dropped)
}

/// Send a moment or join one, from beside the queue.
#[component]
pub fn JamButton() -> Element {
    let ctrl = try_consume_context::<PlayerController>();
    let jam = try_consume_context::<crate::jam_sync::JamState>();
    let config = try_consume_context::<Signal<config::AppConfig>>();
    let mut open = use_signal(|| false);
    let mut pasted = use_signal(String::new);

    let has_queue = ctrl.map(|c| !c.queue.peek().is_empty()).unwrap_or(false);
    let in_a_jam = jam.map(|j| j.in_a_jam()).unwrap_or(false);
    let relay_ready = config
        .map(|c| {
            let conf = c.read();
            !conf.relay_url.trim().is_empty() && !conf.relay_token.trim().is_empty()
        })
        .unwrap_or(false);

    // The invite as plain, selectable text. The copy button is a convenience;
    // this is the guarantee. A webview where the clipboard API is unavailable
    // used to leave the host with no way at all to get the code out — so it is
    // shown, and can always be selected by hand and copied.
    let invite_text =
        use_memo(move || jam.and_then(|j| j.access.read().as_ref().map(relay::jam::encode_join)));

    // Open a live jam from what is playing, and copy the invite to hand over.
    let mut start_live = move |_| {
        let (Some(jam), Some(ctrl), Some(config)) = (jam, ctrl, config) else {
            return;
        };
        let relay = relay::RelayConfig {
            url: relay::normalise_url(&config.peek().relay_url),
            token: config.peek().relay_token.clone(),
        };
        dioxus::core::spawn_forever(crate::jam_sync::start_jam(jam, ctrl, relay));
        open.set(false);
    };

    // Copy the live invite for whoever is already in a jam this device hosts.
    let copy_invite = move |_| {
        if let Some(jam) = jam
            && let Some(access) = jam.access.peek().clone()
        {
            copy_to_clipboard(&relay::jam::encode_join(&access));
            show_toast("Invite copied — send it to listen together".to_string());
        }
    };

    let leave_live = move |_| {
        if let Some(jam) = jam {
            crate::jam_sync::leave_jam(jam);
        }
    };

    // Join a live jam from a pasted `kopuz:live:` code.
    let mut join_live = move || {
        let (Some(jam), Some(ctrl)) = (jam, ctrl) else {
            return;
        };
        let code = pasted.peek().clone();
        dioxus::core::spawn_forever(crate::jam_sync::join_jam(jam, ctrl, code));
        pasted.set(String::new());
        open.set(false);
    };

    // Decoded as they type, so the panel can say where they would land before
    // they commit to anything.
    let preview = {
        let raw = pasted.read();
        (!raw.trim().is_empty()).then(|| share::decode_jam(&raw))
    };

    let mut join = move |jam: Jam| {
        let Some(ctrl) = ctrl else {
            show_toast("Playback is not ready yet".to_string());
            return;
        };
        let (at, position_ms) = share::catch_up(&jam, now_secs());
        let (tracks, index, on_anchor, dropped) = jam_tracks(&jam, at);
        if tracks.is_empty() {
            show_toast("Nothing in that jam can be played here".to_string());
            return;
        }
        // If the track the sender was on cannot play here, the next one starts
        // from its beginning rather than from a position belonging to a
        // different song.
        let secs = if on_anchor { position_ms / 1000 } else { 0 };
        let mut ctrl = ctrl;
        ctrl.play_queue_at(tracks, index, secs);
        pasted.set(String::new());
        open.set(false);
        show_toast(if dropped == 0 {
            "Joined the jam".to_string()
        } else {
            format!("Joined the jam - {dropped} tracks could not be played here")
        });
    };

    rsx! {
        div { class: "relative ml-auto",
            button {
                class: "px-3 py-1.5 rounded-full text-xs font-medium text-violet-200 hover:text-white hover:bg-violet-500/20 transition-colors flex items-center gap-1.5",
                onclick: move |_| {
                    let now = *open.peek();
                    open.set(!now);
                },
                i { class: "fa-solid fa-tower-broadcast" }
                "Jam"
            }

            if open() {
                // Click anywhere else to dismiss. Sits behind the panel, so it
                // never swallows a click meant for the controls inside it.
                div {
                    class: "fixed inset-0 z-40",
                    onclick: move |_| open.set(false),
                }
                div {
                    class: "absolute right-0 top-full mt-2 w-80 z-50 rounded-xl border border-white/10 bg-neutral-900 shadow-2xl p-4 flex flex-col gap-3",
                    onclick: move |e| e.stop_propagation(),

                    // Live jam: two people, both driving, staying in step. When
                    // one is running it takes over the panel; otherwise it is an
                    // offer, above the one-shot share it supersedes.
                    if in_a_jam {
                        div { class: "rounded-lg bg-violet-500/10 border border-violet-400/25 px-3 py-2.5 flex flex-col gap-2",
                            div { class: "flex items-center gap-2 text-violet-200 text-sm font-medium",
                                i { class: "fa-solid fa-tower-broadcast" }
                                "Live jam"
                            }
                            if let Some(jam) = jam {
                                if !jam.status.read().is_empty() {
                                    p { class: "text-violet-200/70 text-[11px]", "{jam.status}" }
                                }
                            }
                            // The invite, shown so it can always be selected and
                            // copied by hand — the copy button is only the quick
                            // path. Read-only, and clicking it selects the lot.
                            if jam.map(|j| *j.is_host.peek()).unwrap_or(false) {
                                if let Some(code) = invite_text() {
                                    textarea {
                                        class: "w-full h-14 bg-black/30 border border-violet-400/20 rounded-lg px-2 py-1.5 text-violet-100/80 text-[10px] font-mono resize-none focus:outline-none focus:border-violet-400 break-all select-all",
                                        readonly: true,
                                        value: "{code}",
                                        // Keystrokes here (Ctrl+A, Ctrl+C) are for
                                        // selecting and copying, not playback.
                                        onkeydown: move |e| e.stop_propagation(),
                                    }
                                }
                            }
                            div { class: "flex gap-2",
                                if jam.map(|j| *j.is_host.peek()).unwrap_or(false) {
                                    button {
                                        class: "flex-1 px-3 py-1.5 rounded-lg bg-violet-600 hover:bg-violet-500 text-white text-xs font-medium transition-colors",
                                        onclick: copy_invite,
                                        i { class: "fa-solid fa-copy mr-1.5" }
                                        "Copy invite"
                                    }
                                }
                                button {
                                    class: "flex-1 px-3 py-1.5 rounded-lg bg-white/10 hover:bg-white/20 text-white/80 text-xs font-medium transition-colors",
                                    onclick: leave_live,
                                    "Leave"
                                }
                            }
                        }
                        div { class: "h-px bg-white/10" }
                    } else {
                        button {
                            class: if relay_ready && has_queue {
                                "w-full px-3 py-2 rounded-lg bg-violet-600 hover:bg-violet-500 text-white text-sm font-medium transition-colors flex items-center justify-center gap-1.5"
                            } else {
                                "w-full px-3 py-2 rounded-lg bg-white/5 text-white/30 text-sm font-medium cursor-default flex items-center justify-center gap-1.5"
                            },
                            disabled: !relay_ready || !has_queue,
                            onclick: start_live,
                            i { class: "fa-solid fa-tower-broadcast" }
                            "Start a live jam"
                        }
                        if !relay_ready {
                            p { class: "text-white/40 text-[11px]",
                                "Set up your own relay in Settings to jam live — you and the other person both playing, both in control."
                            }
                        }
                        div { class: "h-px bg-white/10" }
                    }

                    div {
                        div { class: "text-white text-sm font-semibold", "Listen together" }
                        // Says plainly what this does and does not do. Someone
                        // who expects the two players to stay in step would
                        // otherwise find out by noticing they had drifted.
                        p { class: "text-white/40 text-[11px] mt-0.5",
                            "A code carries the queue and where you are in it. Whoever pastes it lands at that same moment — once. After that you each play on your own."
                        }
                    }

                    button {
                        class: if has_queue {
                            "w-full px-3 py-2 rounded-lg bg-violet-600 hover:bg-violet-500 text-white text-sm font-medium transition-colors"
                        } else {
                            "w-full px-3 py-2 rounded-lg bg-white/5 text-white/30 text-sm font-medium cursor-default"
                        },
                        disabled: !has_queue,
                        onclick: move |_| {
                            let Some(ctrl) = ctrl else { return };
                            match current_jam_code(&ctrl) {
                                Some(code) => {
                                    copy_to_clipboard(&code);
                                    show_toast("Jam code copied".to_string());
                                    open.set(false);
                                }
                                None => show_toast("Nothing playing to share".to_string()),
                            }
                        },
                        i { class: "fa-solid fa-copy mr-1.5" }
                        "Copy this moment"
                    }

                    div { class: "h-px bg-white/10" }

                    textarea {
                        class: "w-full h-16 bg-white/5 border border-white/10 rounded-lg px-2.5 py-2 text-white text-[11px] font-mono resize-none placeholder:text-white/25 focus:outline-none focus:border-violet-400 break-all",
                        placeholder: "or paste a code you were sent",
                        value: "{pasted}",
                        oninput: move |e| pasted.set(e.value()),
                        // Without this the queue view eats the keystrokes as
                        // playback shortcuts.
                        onkeydown: move |e| e.stop_propagation(),
                    }

                    // A live invite and a one-shot code go in the same box; it
                    // tells them apart and offers the right thing. The live one
                    // joins an ongoing session, so it is not a "moment" preview.
                    if relay::jam::looks_like_join(&pasted.read()) {
                        button {
                            class: "w-full px-3 py-2 rounded-lg bg-violet-600 hover:bg-violet-500 text-white text-sm font-semibold transition-colors flex items-center justify-center gap-1.5",
                            onclick: move |_| join_live(),
                            i { class: "fa-solid fa-tower-broadcast" }
                            "Join live jam"
                        }
                    } else {
                    match preview {
                        Some(Ok(jam)) => {
                            let count = jam.playlist.tracks.len();
                            let (at, position_ms) = share::catch_up(&jam, now_secs());
                            let where_ = jam
                                .playlist
                                .tracks
                                .get(at)
                                .filter(|t| !t.title.is_empty())
                                .map(|t| format!("{} — {}", t.artist, t.title))
                                .unwrap_or_else(|| jam.playlist.name.clone());
                            let clock = format!(
                                "{}:{:02}",
                                position_ms / 60_000,
                                (position_ms / 1000) % 60
                            );
                            rsx! {
                                div { class: "rounded-lg bg-violet-500/10 border border-violet-400/25 px-2.5 py-2",
                                    div { class: "text-white text-xs font-medium", "{count} tracks" }
                                    div { class: "text-violet-200/70 text-[11px] mt-0.5",
                                        "You would join at {clock} in {where_}"
                                    }
                                }
                                button {
                                    class: "w-full px-3 py-2 rounded-lg bg-violet-600 hover:bg-violet-500 text-white text-sm font-semibold transition-colors",
                                    onclick: move |_| join(jam.clone()),
                                    i { class: "fa-solid fa-play mr-1.5" }
                                    "Join"
                                }
                            }
                        }
                        Some(Err(e)) => rsx! {
                            div {
                                class: "rounded-lg bg-rose-500/10 border border-rose-400/25 px-2.5 py-2 text-rose-200 text-[11px]",
                                "{e}"
                            }
                        },
                        None => rsx! {},
                    }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn portable(id: &str, secs: u64) -> SharedTrack {
        SharedTrack {
            path: Some(format!("ytmusic:{id}")),
            title: format!("song {id}"),
            artist: "Artist".into(),
            duration: secs,
        }
    }

    fn metadata_only(title: &str) -> SharedTrack {
        SharedTrack {
            path: None,
            title: title.into(),
            artist: "Artist".into(),
            duration: 100,
        }
    }

    fn jam_of(tracks: Vec<SharedTrack>, index: usize) -> Jam {
        Jam {
            playlist: SharedPlaylist {
                name: "j".into(),
                tracks,
            },
            index,
            position_ms: 30_000,
            sent_at: 1_000,
        }
    }

    #[test]
    fn every_playable_track_survives_and_the_index_is_kept() {
        let jam = jam_of(
            vec![portable("a", 100), portable("b", 100), portable("c", 100)],
            1,
        );
        let (tracks, index, on_anchor, dropped) = super::jam_tracks(&jam, 1);
        assert_eq!(tracks.len(), 3);
        assert_eq!(index, 1);
        assert!(on_anchor);
        assert_eq!(dropped, 0);
    }

    /// Dropping a track shifts everything after it. If the mapping is wrong the
    /// listener silently starts on the wrong song, which looks like the jam
    /// simply not working.
    #[test]
    fn dropping_earlier_tracks_shifts_the_index_back() {
        let jam = jam_of(
            vec![
                metadata_only("only on their disk"),
                metadata_only("also unplayable"),
                portable("a", 100),
                portable("b", 100),
            ],
            3,
        );
        let (tracks, index, on_anchor, dropped) = super::jam_tracks(&jam, 3);
        assert_eq!(tracks.len(), 2);
        assert_eq!(dropped, 2);
        assert!(on_anchor);
        assert_eq!(index, 1, "the anchor is the second playable track");
        assert!(tracks[index].path.to_string_lossy().contains('b'));
    }

    /// When the track they were on cannot play here, the next one that can
    /// starts from its beginning — carrying the position across would drop the
    /// listener into the middle of a song the sender had not reached.
    #[test]
    fn an_unplayable_anchor_falls_forward_and_reports_it() {
        let jam = jam_of(
            vec![
                portable("a", 100),
                metadata_only("their local file"),
                portable("c", 100),
            ],
            1,
        );
        let (tracks, index, on_anchor, dropped) = super::jam_tracks(&jam, 1);
        assert_eq!(tracks.len(), 2);
        assert_eq!(dropped, 1);
        assert!(!on_anchor, "caller must know not to apply the position");
        assert_eq!(index, 1, "falls forward to the next playable track");
    }

    #[test]
    fn a_jam_with_nothing_playable_yields_nothing() {
        let jam = jam_of(vec![metadata_only("x"), metadata_only("y")], 0);
        let (tracks, _, _, dropped) = super::jam_tracks(&jam, 0);
        assert!(tracks.is_empty());
        assert_eq!(dropped, 2);
    }
}
