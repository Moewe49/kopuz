//! The tracklist behind a generated mix.
//!
//! Unlike Discover's playlist detail this fetches nothing: a mix already
//! carries its tracks, because generating one *is* fetching them. That is why
//! neither existing detail view could be reused — `DiscoverPlaylistDetail`
//! looks its tracks up by a YouTube playlist id that a locally built mix does
//! not have, and `PlaylistDetail` resolves against the saved playlist store,
//! where a mix deliberately never appears.
//!
//! What both of them do have in common is the page around the tracks, and that
//! is shared through [`components::track_list_page::TrackListPage`].

use dioxus::prelude::*;
use reader::models::Track;

use crate::server::home::GeneratedMixes;

#[component]
pub fn MixDetail(selected_mix_id: Signal<Option<String>>, on_back: EventHandler<()>) -> Element {
    let mixes = use_context::<GeneratedMixes>().0;
    let mut ctrl = use_context::<hooks::use_player_controller::PlayerController>();

    let mix = selected_mix_id.read().clone().and_then(|id| {
        mixes
            .read()
            .mixes
            .iter()
            .find(|m| m.id == id)
            .map(|m| (m.name.clone(), m.tracks.clone()))
    });

    // A mix can vanish under the listener: the daily refresh replaces the set,
    // and an id that no longer exists is normal rather than an error worth
    // shouting about.
    let Some((name, tracks)) = mix else {
        return rsx! {
            div { class: "flex flex-col items-center justify-center h-full text-white/60 p-12 gap-4",
                p { "{i18n::t(\"playlist_not_found\")}" }
                button {
                    class: "inline-flex items-center gap-2 text-white/70 hover:text-white text-sm cursor-pointer",
                    onclick: move |_| on_back.call(()),
                    i { class: "fa-solid fa-chevron-left text-xs" }
                    span { "{i18n::t(\"back\")}" }
                }
            }
        };
    };

    let all = tracks.clone();
    let from_row = tracks.clone();
    rsx! {
        components::track_list_page::TrackListPage {
            // Reuses the shelf's own heading, so the page the listener lands on
            // is labelled the same as the tile they clicked.
            eyebrow: i18n::t("made_for_you").to_string(),
            title: name,
            tracks: tracks.clone(),
            loading: false,
            error: None,
            on_back: move |_| on_back.call(()),
            on_play_all: move |_| {
                if !all.is_empty() {
                    ctrl.play_queue_linear(all.clone());
                }
            },
            on_play_from: move |t: Track| {
                let mut queue = from_row.clone();
                let start = queue.iter().position(|x| x.path == t.path).unwrap_or(0);
                queue.rotate_left(start);
                ctrl.play_queue_linear(queue);
            },
        }
    }
}
