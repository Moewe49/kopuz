//! Spotify search view for the unified search bar (source = Spotify).
//!
//! Spotify's audio is DRM-protected, so nothing here streams from Spotify:
//! this searches Spotify's catalogue and plays each hit through its YouTube
//! Music equivalent, using the same matcher the playlist importer uses. Album
//! and editorial-playlist coverage is where Spotify's catalogue is worth
//! reaching for — you find the release on Spotify and listen through YT.
//!
//! `/v1/search` needs a bearer token, so this section only appears once the
//! user has connected their (free, self-registered) Spotify app — the same
//! connection the import dialog sets up. Opening a result, on the other hand,
//! uses the anonymous embed/pathfinder path, so full track lists work with no
//! quota attached.

use dioxus::prelude::*;
use hooks::use_player_controller::PlayerController;
use reader::models::Track;
use server::spotify::{
    self,
    matcher::CloneEvent,
    search::{SearchEntity, SearchResults, SearchTrack},
};

use crate::add_to_playlist::request_add_to_playlist;
use crate::toast::show_toast;

/// Results per type. Spotify caps this at 50; 24 keeps three full card rows
/// without making the songs list endless.
const SEARCH_LIMIT: u32 = 24;

/// True when the account connection Spotify search needs is in place.
pub fn is_connected(config: &config::AppConfig) -> bool {
    !config.spotify_client_id.trim().is_empty() && !config.spotify_refresh_token.trim().is_empty()
}

/// Fetch a usable access token, persisting the refresh token when Spotify
/// rotates it. Rotation is why this must go through the shared provider in
/// `spotify::auth` — two independent refreshers would invalidate each other's
/// token and silently disconnect the account.
async fn token(mut config: Signal<config::AppConfig>) -> Result<String, String> {
    let (client_id, refresh) = {
        let c = config.peek();
        (
            c.spotify_client_id.trim().to_string(),
            c.spotify_refresh_token.trim().to_string(),
        )
    };
    let fresh = spotify::auth::access_token(&client_id, &refresh).await?;
    if let Some(rotated) = fresh.rotated_refresh {
        config.write().spotify_refresh_token = rotated;
    }
    Ok(fresh.access_token)
}

/// What the view is currently doing on top of showing results.
#[derive(Clone, PartialEq)]
enum Busy {
    No,
    /// Matching a single track before playing it.
    Track(String),
    /// Matching a whole playlist/album: `(done, total)`.
    Entity(usize, usize),
    /// Cloning the open entity into the YT Music account.
    Importing(String),
}

#[component]
pub fn SpotifySearchView(search_query: Signal<String>) -> Element {
    let config = use_context::<Signal<config::AppConfig>>();
    // The playlist/album the user drilled into, if any.
    let open_entity = use_signal(|| None::<SearchEntity>);
    let busy = use_signal(|| Busy::No);

    let results = use_resource(move || async move {
        let q = search_query.read().trim().to_string();
        if q.is_empty() {
            return Ok(SearchResults::default());
        }
        let tok = token(config).await?;
        spotify::search::search(&tok, &q, SEARCH_LIMIT).await
    });

    if !is_connected(&config.read()) {
        return rsx! {
            div { class: "max-w-2xl mt-6 rounded-lg bg-emerald-500/10 border border-emerald-400/30 text-emerald-100 text-sm p-4",
                i { class: "fa-brands fa-spotify mr-2 text-emerald-400" }
                "{i18n::t(\"spotify_search_needs_account\")}"
            }
        };
    }

    if let Some(entity) = open_entity.read().clone() {
        return rsx! {
            SpotifyEntityDetail { entity, open_entity, busy }
        };
    }

    let query_empty = search_query.read().trim().is_empty();

    rsx! {
        div { class: "flex-1 min-h-0 overflow-y-auto pb-24",
            BusyBanner { busy }
            match &*results.read_unchecked() {
                _ if query_empty => rsx! {
                    div { class: "text-slate-500 text-sm mt-8 px-1",
                        i { class: "fa-brands fa-spotify text-emerald-400 mr-2" }
                        "{i18n::t(\"spotify_search_hint\")}"
                    }
                },
                None => rsx! {
                    div { class: "flex items-center gap-2 text-slate-400 text-sm mt-8 px-1",
                        i { class: "fa-solid fa-arrows-rotate fa-spin" }
                        "{i18n::t(\"spotify_searching\")}"
                    }
                },
                Some(Err(e)) => rsx! {
                    div { class: "max-w-2xl mt-6 rounded-lg bg-rose-500/10 border border-rose-400/30 text-rose-200 text-sm p-4 break-words",
                        "{e}"
                    }
                },
                Some(Ok(res)) if res.is_empty() => rsx! {
                    div { class: "text-slate-500 text-sm mt-8 px-1", "{i18n::t(\"no_results\")}" }
                },
                Some(Ok(res)) => rsx! {
                    p { class: "text-[11px] text-slate-500 mb-5 px-1",
                        i { class: "fa-solid fa-circle-info mr-1.5" }
                        "{i18n::t(\"spotify_playback_note\")}"
                    }
                    if !res.tracks.is_empty() {
                        section { class: "mb-10",
                            h2 { class: "text-lg font-bold text-white mb-3 px-1", "{i18n::t(\"tracks\")}" }
                            div { class: "flex flex-col gap-1 max-w-3xl",
                                for track in res.tracks.iter() {
                                    SpotifyTrackRow { key: "{track.id}", track: track.clone(), busy }
                                }
                            }
                        }
                    }
                    EntityShelf {
                        title: i18n::t("playlists"),
                        entities: res.playlists.clone(),
                        open_entity,
                    }
                    EntityShelf {
                        title: i18n::t("albums"),
                        entities: res.albums.clone(),
                        open_entity,
                    }
                },
            }
        }
    }
}

/// Sticky progress line for the long-running actions (matching, importing).
#[component]
fn BusyBanner(busy: Signal<Busy>) -> Element {
    let label = match &*busy.read() {
        Busy::No => return rsx! {},
        Busy::Track(title) => {
            i18n::t_with("spotify_finding_match", &[("title", title.clone())])
        }
        Busy::Entity(done, total) => i18n::t_with(
            "spotify_matching",
            &[("done", done.to_string()), ("total", total.to_string())],
        ),
        Busy::Importing(_) => i18n::t("spotify_creating"),
    };
    rsx! {
        div { class: "sticky top-0 z-10 mb-4 max-w-3xl flex items-center gap-2 rounded-lg bg-emerald-500/10 border border-emerald-400/30 text-emerald-100 text-sm px-4 py-2.5",
            i { class: "fa-solid fa-arrows-rotate fa-spin" }
            span { class: "truncate", "{label}" }
        }
    }
}

#[component]
fn SpotifyTrackRow(track: SearchTrack, busy: Signal<Busy>) -> Element {
    let ctrl = use_context::<PlayerController>();
    let cover = track.cover_url.clone();
    let duration = format_duration(track.duration_secs);
    let artists = track.artists.join(", ");

    let play = {
        let track = track.clone();
        move |_| play_matched(track.clone(), busy, ctrl, PostMatch::Play)
    };
    let add = {
        let track = track.clone();
        move |e: Event<MouseData>| {
            e.stop_propagation();
            play_matched(track.clone(), busy, ctrl, PostMatch::AddToPlaylist);
        }
    };

    rsx! {
        div {
            class: "group flex items-center gap-3 px-3 py-2 rounded-lg hover:bg-white/5 transition-colors cursor-pointer",
            onclick: play,
            div {
                class: "w-10 h-10 rounded bg-white/5 shrink-0 overflow-hidden flex items-center justify-center",
                if let Some(c) = cover {
                    img { src: "{c}", class: "w-full h-full object-cover", loading: "lazy" }
                } else {
                    i { class: "fa-brands fa-spotify text-emerald-400/50" }
                }
            }
            div { class: "flex flex-col min-w-0 flex-1",
                span { class: "text-sm text-white/90 truncate", "{track.title}" }
                span { class: "text-xs text-slate-400 truncate", "{artists}" }
            }
            span { class: "text-xs text-slate-500 tabular-nums shrink-0", "{duration}" }
            button {
                class: "w-8 h-8 rounded-full text-white/40 hover:text-white hover:bg-white/10 opacity-0 group-hover:opacity-100 transition-all shrink-0",
                title: i18n::t("add_to_playlist"),
                onclick: add,
                i { class: "fa-solid fa-plus text-xs" }
            }
        }
    }
}

/// A horizontally scrolling row of playlist/album cards.
#[component]
fn EntityShelf(
    title: String,
    entities: Vec<SearchEntity>,
    open_entity: Signal<Option<SearchEntity>>,
) -> Element {
    if entities.is_empty() {
        return rsx! {};
    }
    rsx! {
        section { class: "mb-10",
            h2 { class: "text-lg font-bold text-white mb-3 px-1", "{title}" }
            div {
                class: "flex items-start gap-4 pb-3 pt-1 scrollbar-hide scroll-smooth -mx-2 px-2",
                style: "overflow-x: auto; overflow-y: hidden;",
                for entity in entities.iter() {
                    EntityCard { key: "{entity.id}", entity: entity.clone(), open_entity }
                }
            }
        }
    }
}

#[component]
fn EntityCard(entity: SearchEntity, open_entity: Signal<Option<SearchEntity>>) -> Element {
    let cover = entity.cover_url.clone();
    let subtitle = entity.subtitle.clone();
    let open = {
        let entity = entity.clone();
        move |_| {
            let mut open_entity = open_entity;
            open_entity.set(Some(entity.clone()));
        }
    };
    rsx! {
        button {
            class: "w-40 shrink-0 text-left group cursor-pointer",
            onclick: open,
            div { class: "w-40 h-40 rounded-lg bg-white/5 overflow-hidden mb-2 flex items-center justify-center transition-transform group-hover:scale-[1.03]",
                if let Some(c) = cover {
                    img { src: "{c}", class: "w-full h-full object-cover", loading: "lazy" }
                } else {
                    i { class: "fa-brands fa-spotify text-2xl text-emerald-400/40" }
                }
            }
            span { class: "block text-sm text-white/90 truncate", "{entity.name}" }
            span { class: "block text-xs text-slate-400 truncate", "{subtitle}" }
        }
    }
}

/// The drilled-into playlist/album: its real track list (fetched anonymously,
/// in full) plus play-all and import.
#[component]
fn SpotifyEntityDetail(
    entity: SearchEntity,
    open_entity: Signal<Option<SearchEntity>>,
    busy: Signal<Busy>,
) -> Element {
    let config = use_context::<Signal<config::AppConfig>>();
    let ctrl = use_context::<PlayerController>();

    let kind = entity.kind;
    let id = entity.id.clone();
    let detail = use_resource(move || {
        let id = id.clone();
        async move { spotify::embed::fetch_public(kind, &id).await }
    });

    let cover = entity.cover_url.clone();
    let subtitle = entity.subtitle.clone();
    let is_busy = *busy.read() != Busy::No;

    let close = move |_| {
        let mut open_entity = open_entity;
        open_entity.set(None);
    };

    // Both actions need the fetched track list; until it lands the buttons
    // simply do nothing (they're already disabled while anything is running).
    let loaded = move || match &*detail.read_unchecked() {
        Some(Ok(playlist)) => Some(playlist.clone()),
        _ => None,
    };

    let play_all = move |_| {
        if let Some(playlist) = loaded() {
            play_whole(playlist, busy, ctrl);
        }
    };

    let import = {
        let name = entity.name.clone();
        move |_| {
            if let Some(playlist) = loaded() {
                import_whole(playlist, name.clone(), config, busy);
            }
        }
    };

    rsx! {
        div { class: "flex-1 min-h-0 overflow-y-auto pb-24",
            BusyBanner { busy }
            button {
                class: "text-xs font-bold tracking-widest uppercase text-white/60 hover:text-white cursor-pointer transition-colors mb-5",
                onclick: close,
                i { class: "fa-solid fa-chevron-left mr-2" }
                "{i18n::t(\"back\")}"
            }
            div { class: "flex items-end gap-5 mb-6",
                div { class: "w-32 h-32 rounded-lg bg-white/5 overflow-hidden shrink-0 flex items-center justify-center",
                    if let Some(c) = cover {
                        img { src: "{c}", class: "w-full h-full object-cover" }
                    } else {
                        i { class: "fa-brands fa-spotify text-3xl text-emerald-400/40" }
                    }
                }
                div { class: "min-w-0",
                    h1 { class: "text-2xl md:text-3xl font-bold text-white truncate", "{entity.name}" }
                    p { class: "text-sm text-slate-400 truncate", "{subtitle}" }
                    div { class: "flex gap-2 mt-4",
                        button {
                            class: "px-5 py-2 rounded-full bg-emerald-500 hover:bg-emerald-400 disabled:opacity-40 disabled:cursor-not-allowed text-black text-sm font-bold transition-colors cursor-pointer",
                            disabled: is_busy,
                            onclick: play_all,
                            i { class: "fa-solid fa-play mr-2 text-xs" }
                            "{i18n::t(\"play\")}"
                        }
                        button {
                            class: "px-5 py-2 rounded-full bg-white/10 hover:bg-white/20 disabled:opacity-40 disabled:cursor-not-allowed text-white text-sm font-bold transition-colors cursor-pointer",
                            disabled: is_busy,
                            onclick: import,
                            i { class: "fa-solid fa-download mr-2 text-xs" }
                            "{i18n::t(\"spotify_import_button\")}"
                        }
                    }
                }
            }
            match &*detail.read_unchecked() {
                None => rsx! {
                    div { class: "flex items-center gap-2 text-slate-400 text-sm px-1",
                        i { class: "fa-solid fa-arrows-rotate fa-spin" }
                        "{i18n::t(\"spotify_fetching\")}"
                    }
                },
                Some(Err(e)) => rsx! {
                    div { class: "max-w-2xl rounded-lg bg-rose-500/10 border border-rose-400/30 text-rose-200 text-sm p-4 break-words", "{e}" }
                },
                Some(Ok(playlist)) => rsx! {
                    div { class: "flex flex-col gap-1 max-w-3xl",
                        for (idx, t) in playlist.tracks.iter().enumerate() {
                            div { key: "{idx}", class: "flex items-center gap-3 px-3 py-2 rounded-lg",
                                span { class: "w-6 text-right text-xs text-slate-500 tabular-nums shrink-0", "{idx + 1}" }
                                div { class: "flex flex-col min-w-0 flex-1",
                                    span { class: "text-sm text-white/90 truncate", "{t.title}" }
                                    span { class: "text-xs text-slate-400 truncate", "{t.artists.join(\", \")}" }
                                }
                                span { class: "text-xs text-slate-500 tabular-nums shrink-0",
                                    "{format_duration(t.duration_secs)}"
                                }
                            }
                        }
                    }
                },
            }
        }
    }
}

/// What to do with a track once its YouTube match is known.
#[derive(Clone, Copy)]
enum PostMatch {
    Play,
    AddToPlaylist,
}

/// Resolve one Spotify track to its YouTube Music equivalent, then act on it.
/// Takes ~a second (one anonymous YT search), hence the busy banner.
fn play_matched(
    track: SearchTrack,
    mut busy: Signal<Busy>,
    mut ctrl: PlayerController,
    then: PostMatch,
) {
    if *busy.peek() != Busy::No {
        return;
    }
    busy.set(Busy::Track(track.title.clone()));
    spawn(async move {
        let matched = spotify::matcher::match_external_to_yt(
            &track.title,
            &track.artists,
            track.duration_secs,
        )
        .await;
        busy.set(Busy::No);
        match matched {
            Some(yt) => match then {
                PostMatch::Play => ctrl.play_queue_linear(vec![yt]),
                PostMatch::AddToPlaylist => request_add_to_playlist(yt),
            },
            None => show_toast(i18n::t_with(
                "spotify_no_match",
                &[("title", track.title.clone())],
            )),
        }
    });
}

/// Match a whole Spotify playlist/album and play the result. Sequentially
/// unreachable tracks (nothing confident on YouTube) are dropped rather than
/// blocking the rest.
fn play_whole(
    playlist: spotify::SpotifyPlaylist,
    mut busy: Signal<Busy>,
    mut ctrl: PlayerController,
) {
    if *busy.peek() != Busy::No {
        return;
    }
    let total = playlist.tracks.len();
    if total == 0 {
        return;
    }
    busy.set(Busy::Entity(0, total));
    spawn(async move {
        let tracks: Vec<Track> =
            spotify::matcher::match_playlist_to_tracks(&playlist.tracks, |ev| {
                if let CloneEvent::Matching { done, total, .. } = ev {
                    busy.set(Busy::Entity(done, total));
                }
            })
            .await;
        busy.set(Busy::No);
        if tracks.is_empty() {
            show_toast(i18n::t("no_results"));
            return;
        }
        ctrl.play_queue_linear(tracks);
    });
}

/// Clone the open playlist/album into the signed-in YouTube Music account —
/// the same operation the import dialog performs, without the URL round-trip.
fn import_whole(
    playlist: spotify::SpotifyPlaylist,
    name: String,
    config: Signal<config::AppConfig>,
    mut busy: Signal<Busy>,
) {
    if *busy.peek() != Busy::No {
        return;
    }
    let Some(cookies) = config
        .peek()
        .server
        .as_ref()
        .and_then(|s| s.access_token.clone())
        .filter(|c| !c.is_empty())
    else {
        show_toast(i18n::t("spotify_needs_yt_login"));
        return;
    };
    busy.set(Busy::Importing(name));
    spawn(async move {
        let result = spotify::matcher::import_playlist(cookies, &playlist, |ev| match ev {
            CloneEvent::Matching { done, total, .. } => busy.set(Busy::Entity(done, total)),
            CloneEvent::CreatingPlaylist => busy.set(Busy::Importing(String::new())),
            CloneEvent::Adding { done, total } => busy.set(Busy::Entity(done, total)),
        })
        .await;
        busy.set(Busy::No);
        match result {
            Ok(report) => show_toast(i18n::t_with(
                "spotify_done",
                &[
                    ("matched", report.matched.to_string()),
                    ("total", report.total.to_string()),
                ],
            )),
            Err(e) => show_toast(e),
        }
    });
}

fn format_duration(secs: u64) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}
