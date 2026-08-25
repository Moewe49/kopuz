//! Handing someone the moment you are listening to.
//!
//! A jam code carries the queue plus where you are in it — see
//! [`reader::share`] for the format and for why it is one-shot rather than a
//! live session.
//!
//! This is the button. It lives beside the queue rather than in the share
//! dialog, because that is where the thing being shared is: the share dialog
//! is reached from the playlists page, and navigating away from what you are
//! listening to in order to share what you are listening to is the wrong
//! gesture.

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

/// Copies the current moment to the clipboard.
///
/// Disabled with nothing playing, rather than hidden: a control that appears
/// and vanishes is harder to find again than one that is simply greyed out.
#[component]
pub fn JamButton() -> Element {
    let ctrl = try_consume_context::<PlayerController>();
    let has_queue = ctrl.map(|c| !c.queue.peek().is_empty()).unwrap_or(false);

    rsx! {
        button {
            class: if has_queue {
                "ml-auto px-3 py-1.5 rounded-full text-xs font-medium text-violet-200 hover:text-white hover:bg-violet-500/20 transition-colors flex items-center gap-1.5"
            } else {
                "ml-auto px-3 py-1.5 rounded-full text-xs font-medium text-white/25 cursor-default flex items-center gap-1.5"
            },
            disabled: !has_queue,
            title: "Copy a code that drops someone into this exact moment",
            onclick: move |_| {
                let Some(ctrl) = ctrl else { return };
                match current_jam_code(&ctrl) {
                    Some(code) => {
                        copy_to_clipboard(&code);
                        show_toast("Jam code copied — they'll land where you are".to_string());
                    }
                    None => show_toast("Nothing playing to share".to_string()),
                }
            },
            i { class: "fa-solid fa-tower-broadcast" }
            "Jam"
        }
    }
}
