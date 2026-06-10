//! SoundCloud search view for the unified search bar (source = SoundCloud).
//!
//! Searching shells out to yt-dlp, so we auto-search the *settled* query (the
//! search bar already debounces keystrokes). Each row plays through the
//! SoundCloud→yt-dlp resolve path, can be added to a playlist (via the
//! app-wide picker), and can be sent to the yt-dlp Downloads page.

use dioxus::prelude::*;
use hooks::use_player_controller::PlayerController;
use kopuz_route::Route;
use reader::models::Track;

use crate::NavigationController;
use crate::add_to_playlist::request_add_to_playlist;

/// Set by a SoundCloud row's download button; the Downloads (yt-dlp) page
/// reads it on open and prefills its URL field. Provided once in main.
#[derive(Clone, Copy)]
pub struct YtdlpPrefillUrl(pub Signal<Option<String>>);

fn cover_for(track: &Track) -> Option<utils::CoverUrl> {
    let path_str = track.path.to_string_lossy();
    utils::map_cover_url(utils::jellyfin_image::track_cover_url_with_album_fallback(
        &path_str,
        &track.album_id,
        "",
        None,
        80,
        80,
    ))
}

/// Auto-searching SoundCloud results view. Drives off the shared, debounced
/// `search_query` signal so picking "SoundCloud" in the search dropdown shows
/// results without an extra click.
#[component]
pub fn SoundCloudSearchView(search_query: Signal<String>) -> Element {
    // Re-runs whenever the (debounced) query changes. yt-dlp is slow-ish, so
    // one resolve per settled query is the right cadence.
    let results = use_resource(move || async move {
        let q = search_query.read().trim().to_string();
        if q.is_empty() {
            return Ok(Vec::new());
        }
        server::soundcloud::search(&q, 30).await
    });

    if !server::soundcloud::is_available() {
        return rsx! {
            div { class: "max-w-2xl mt-6 rounded-lg bg-amber-500/10 border border-amber-400/30 text-amber-200 text-sm p-4",
                i { class: "fa-brands fa-soundcloud mr-2" }
                "{i18n::t(\"soundcloud_needs_ytdlp\")}"
            }
        };
    }

    let query_empty = search_query.read().trim().is_empty();

    rsx! {
        div { class: "flex-1 min-h-0 overflow-y-auto pb-24",
            match &*results.read_unchecked() {
                _ if query_empty => rsx! {
                    div { class: "text-slate-500 text-sm mt-8 px-1",
                        i { class: "fa-brands fa-soundcloud text-orange-400 mr-2" }
                        "{i18n::t(\"soundcloud_hint\")}"
                    }
                },
                None => rsx! {
                    div { class: "flex items-center gap-2 text-slate-400 text-sm mt-8 px-1",
                        i { class: "fa-solid fa-arrows-rotate fa-spin" }
                        "{i18n::t(\"soundcloud_searching\")}"
                    }
                },
                Some(Err(e)) => rsx! {
                    div { class: "max-w-2xl mt-6 rounded-lg bg-rose-500/10 border border-rose-400/30 text-rose-200 text-sm p-4 break-words",
                        "{e}"
                    }
                },
                Some(Ok(tracks)) if tracks.is_empty() => rsx! {
                    div { class: "text-slate-500 text-sm mt-8 px-1", "{i18n::t(\"no_results\")}" }
                },
                Some(Ok(tracks)) => rsx! {
                    div { class: "flex flex-col gap-1 max-w-3xl",
                        for (idx, track) in tracks.iter().enumerate() {
                            SoundCloudRow { key: "{idx}", track: track.clone() }
                        }
                    }
                },
            }
        }
    }
}

#[component]
fn SoundCloudRow(track: Track) -> Element {
    let mut ctrl = use_context::<PlayerController>();
    let nav = use_context::<NavigationController>();
    let prefill = try_consume_context::<YtdlpPrefillUrl>();
    let cover = cover_for(&track);

    let track_play = track.clone();
    let track_ctx = track.clone();
    let track_add = track.clone();
    let track_dl = track.clone();

    rsx! {
        div {
            class: "group flex items-center gap-3 px-3 py-2 rounded-lg hover:bg-white/5 transition-colors cursor-pointer",
            onclick: move |_| ctrl.play_queue_linear(vec![track_play.clone()]),
            oncontextmenu: move |e| {
                e.prevent_default();
                request_add_to_playlist(track_ctx.clone());
            },
            div {
                class: "w-10 h-10 rounded bg-white/5 shrink-0 overflow-hidden flex items-center justify-center",
                if let Some(c) = &cover {
                    img { src: "{c.as_ref()}", class: "w-full h-full object-cover", loading: "lazy" }
                } else {
                    i { class: "fa-brands fa-soundcloud text-orange-400/50" }
                }
            }
            div { class: "flex flex-col min-w-0 flex-1",
                span { class: "text-sm text-white/90 truncate", "{track.title}" }
                span { class: "text-xs text-slate-400 truncate", "{track.artist}" }
            }
            button {
                class: "w-8 h-8 rounded-full text-white/40 hover:text-white hover:bg-white/10 opacity-0 group-hover:opacity-100 transition-all shrink-0",
                title: i18n::t("add_to_playlist"),
                onclick: move |e| {
                    e.stop_propagation();
                    request_add_to_playlist(track_add.clone());
                },
                i { class: "fa-solid fa-plus text-xs" }
            }
            button {
                class: "w-8 h-8 rounded-full text-white/40 hover:text-white hover:bg-white/10 opacity-0 group-hover:opacity-100 transition-all shrink-0",
                title: i18n::t("soundcloud_download"),
                onclick: move |e| {
                    e.stop_propagation();
                    let path = track_dl.path.to_string_lossy().to_string();
                    if let Some(url) = server::soundcloud::permalink_from_path(&path)
                        && let Some(p) = prefill
                    {
                        let mut sig = p.0;
                        sig.set(Some(url));
                        let mut route = nav.current_route;
                        route.set(Route::Ytdlp);
                    }
                },
                i { class: "fa-solid fa-download text-xs" }
            }
        }
    }
}
