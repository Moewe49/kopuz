//! The page shape shared by every "here is a list of tracks" screen.
//!
//! Discover's playlist detail and the generated-mix detail are the same page:
//! a back link, a title with a play-everything button, and numbered rows that
//! start the queue where they were clicked. The only real difference is where
//! the tracks come from — one fetches them, the other already has them — and
//! that difference belongs to the caller, not to the markup.
//!
//! Written as one component rather than copied, because the alternative is
//! what happened to the home shelves: `render_albums_row` exists verbatim in
//! two files, so every visual change has to be made twice and they drift.

use dioxus::prelude::*;
use reader::models::Track;

/// One row: index, play button, cover, title, artist.
///
/// Nothing moves on hover. The first version swapped the index for a play icon
/// in the same slot, which made the whole row shift as the pointer crossed it —
/// and left no way to see which track was actually playing. The play button
/// now sits beside the cover and stays there; the index is replaced only for
/// the track that is playing, and only by an indicator of the same width.
#[component]
pub fn TrackListRow(
    track: Track,
    index: usize,
    /// The track playing right now, so this row can say so.
    is_current: bool,
    on_play: EventHandler<Track>,
) -> Element {
    let thumbnail = utils::jellyfin_image::track_cover_url_with_album_fallback(
        &track.path.to_string_lossy(),
        &track.album_id,
        "",
        None,
        96,
        80,
    );
    let title = track.title.clone();
    let artist = track.artist.clone();
    let track_for_click = track.clone();
    rsx! {
        button {
            class: if is_current {
                "group flex items-center gap-3 px-3 py-2 rounded-lg bg-indigo-500/10 transition-colors text-left cursor-pointer w-full"
            } else {
                "group flex items-center gap-3 px-3 py-2 rounded-lg hover:bg-white/5 transition-colors text-left cursor-pointer w-full"
            },
            onclick: move |_| on_play.call(track_for_click.clone()),

            // Same width either way, so the row never shifts.
            if is_current {
                i { class: "w-6 text-center fa-solid fa-volume-high text-indigo-300 text-xs" }
            } else {
                span { class: "w-6 text-right text-white/30 text-xs tabular-nums", "{index}" }
            }

            i { class: "w-6 text-center fa-solid fa-play text-white/50 group-hover:text-white text-xs transition-colors" }

            if let Some(url) = thumbnail {
                img {
                    src: "{url}",
                    class: "w-11 h-11 object-cover rounded bg-white/5",
                    loading: "lazy",
                    decoding: "async",
                }
            } else {
                div { class: "w-11 h-11 rounded bg-white/5" }
            }
            div { class: "min-w-0 flex-1",
                p {
                    class: if is_current {
                        "text-sm text-indigo-200 font-medium truncate"
                    } else {
                        "text-sm text-white font-medium truncate"
                    },
                    "{title}"
                }
                p { class: "text-xs text-white/50 truncate", "{artist}" }
            }
        }
    }
}

/// A full track-list screen.
///
/// `loading` and `error` exist for the callers that fetch. A caller that
/// already holds its tracks passes `false` and `None` and the states simply
/// never render — cheaper than a second component that differs only by their
/// absence.
#[component]
pub fn TrackListPage(
    /// Small uppercase label above the title — "Playlist", "Mix", whatever the
    /// caller means. A prop rather than a fixed string so this needs no new
    /// key in twenty-one locale files.
    eyebrow: String,
    title: String,
    tracks: Vec<Track>,
    loading: bool,
    error: Option<String>,
    on_back: EventHandler<()>,
    /// Play the whole list from the top.
    on_play_all: EventHandler<()>,
    /// Play from one row onwards.
    on_play_from: EventHandler<Track>,
) -> Element {
    let count = tracks.len();
    // Which row to mark, taken from the player rather than passed in: every
    // caller would otherwise have to compute the same thing, and get it
    // subtly different.
    let now_playing: Option<std::path::PathBuf> =
        try_consume_context::<hooks::use_player_controller::PlayerController>().and_then(|ctrl| {
            let idx = *ctrl.current_queue_index.peek();
            ctrl.queue.peek().get(idx).map(|t| t.path.clone())
        });
    rsx! {
        div { class: "p-6 md:p-10 max-w-[1600px] mx-auto",
            button {
                class: "inline-flex items-center gap-2 text-white/70 hover:text-white text-sm cursor-pointer mb-6 group",
                onclick: move |_| on_back.call(()),
                i { class: "fa-solid fa-chevron-left text-xs transition-transform group-hover:-translate-x-0.5" }
                span { "{i18n::t(\"back\")}" }
            }
            div { class: "flex items-end gap-6 mb-8",
                div { class: "min-w-0",
                    p { class: "text-[10px] font-bold tracking-widest uppercase text-white/40 mb-2", "{eyebrow}" }
                    h1 { class: "text-3xl md:text-5xl font-black text-white break-words", "{title}" }
                    if !loading {
                        p { class: "text-sm text-white/50 mt-3",
                            "{i18n::t_with(\"playlist_track_count\", &[(\"count\", count.to_string())])}"
                        }
                    }
                }
                button {
                    class: "shrink-0 inline-flex items-center gap-3 bg-white text-black px-8 py-3 rounded-full font-bold hover:bg-white/90 hover:scale-105 active:scale-95 transition-all cursor-pointer disabled:opacity-40 disabled:cursor-default",
                    disabled: loading || count == 0,
                    onclick: move |_| on_play_all.call(()),
                    i { class: "fa-solid fa-play text-[10px]" }
                    span { class: "text-sm", "{i18n::t(\"start_listening\")}" }
                }
            }

            if loading {
                div { class: "flex justify-center py-24",
                    i { class: "fa-solid fa-arrows-rotate fa-spin text-2xl text-white/60" }
                }
            } else if let Some(err) = error {
                div { class: "py-12 text-rose-400 text-sm",
                    "{i18n::t_with(\"discover_failed\", &[(\"error\", err.clone())])}"
                }
            } else {
                div { class: "flex flex-col",
                    for (idx, track) in tracks.iter().enumerate() {
                        TrackListRow {
                            key: "{idx}",
                            track: track.clone(),
                            index: idx + 1,
                            is_current: now_playing.as_ref() == Some(&track.path),
                            on_play: move |t: Track| on_play_from.call(t),
                        }
                    }
                }
            }
        }
    }
}
