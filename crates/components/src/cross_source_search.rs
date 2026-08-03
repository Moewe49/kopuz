//! "Not on this source? Try the others."
//!
//! A search that comes back empty on the selected backend is usually not the
//! end of the story — the track exists, just somewhere else (YouTube Music has
//! the obscure single a Jellyfin library doesn't; SoundCloud has the remix
//! YouTube took down; the local library has the rip you made years ago). This
//! renders under an empty result set and offers whatever the *other* reachable
//! sources have for the same query, playable in place.
//!
//! Deliberately only on an empty result set: firing it on every search would
//! bury good local results under noise and spend a network round-trip per
//! keystroke-settled query.

use config::{AppConfig, MusicService, MusicSource};
use dioxus::prelude::*;
use hooks::use_player_controller::PlayerController;
use reader::Library;
use reader::models::Track;

use crate::add_to_playlist::request_add_to_playlist;

/// Hits per fallback source. Short on purpose — this is a rescue list, not a
/// second search page.
const FALLBACK_LIMIT: usize = 8;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FallbackSource {
    YtMusic,
    SoundCloud,
    Local,
}

impl FallbackSource {
    fn label(self) -> String {
        match self {
            FallbackSource::YtMusic => "YouTube Music".to_string(),
            FallbackSource::SoundCloud => "SoundCloud".to_string(),
            FallbackSource::Local => i18n::t("local"),
        }
    }

    fn icon(self) -> &'static str {
        match self {
            FallbackSource::YtMusic => "fa-brands fa-youtube text-red-400/70",
            FallbackSource::SoundCloud => "fa-brands fa-soundcloud text-orange-400/70",
            FallbackSource::Local => "fa-solid fa-folder text-sky-400/70",
        }
    }
}

/// Which sources are worth asking, given what the user is searching *now*.
/// Never includes the active one — repeating the search that just failed would
/// only waste a request.
fn others(config: &AppConfig) -> Vec<FallbackSource> {
    let on_server = config.active_source == MusicSource::Server;
    let service = config.server.as_ref().map(|s| s.service);
    let yt_is_active = on_server && service == Some(MusicService::YtMusic);

    let mut out = Vec::new();
    if !yt_is_active {
        // Anonymous YT Music search — the broadest catalogue we can reach
        // without the user having configured anything.
        out.push(FallbackSource::YtMusic);
    }
    out.push(FallbackSource::SoundCloud);
    if on_server {
        out.push(FallbackSource::Local);
    }
    out
}

/// Name of the source that came up empty, for the heading.
fn active_label(config: &AppConfig) -> String {
    match config.active_source {
        MusicSource::Server => config
            .server
            .as_ref()
            .map(|s| s.service.display_name().to_string())
            .unwrap_or_else(|| i18n::t("server")),
        _ => i18n::t("local"),
    }
}

#[component]
pub fn CrossSourceFallback(query: String, library: Signal<Library>) -> Element {
    let config = use_context::<Signal<AppConfig>>();
    let trimmed = query.trim().to_string();
    if trimmed.is_empty() {
        return rsx! {};
    }
    let sources = others(&config.read());
    let heading = i18n::t_with("search_elsewhere", &[("source", active_label(&config.read()))]);

    rsx! {
        div { class: "mt-8",
            p { class: "text-[11px] font-bold tracking-widest uppercase text-white/40 mb-4",
                "{heading}"
            }
            for source in sources {
                FallbackSection {
                    key: "{source:?}",
                    source,
                    query: trimmed.clone(),
                    library,
                }
            }
        }
    }
}

/// One source's hits. Each section owns its own request so a slow SoundCloud
/// lookup can't hold back results that already arrived.
#[component]
fn FallbackSection(source: FallbackSource, query: String, library: Signal<Library>) -> Element {
    let q = query.clone();
    let results = use_resource(move || {
        let q = q.clone();
        async move {
            match source {
                FallbackSource::YtMusic => remote_hits(
                    server::ytmusic::YouTubeMusicClient::new()
                        .search_tracks(&q)
                        .await
                        .unwrap_or_default(),
                ),
                FallbackSource::SoundCloud => remote_hits(
                    server::soundcloud::search(&q, FALLBACK_LIMIT)
                        .await
                        .unwrap_or_default(),
                ),
                // Local is in memory already — no request, so this resolves
                // immediately and is the first section to appear.
                FallbackSource::Local => local_matches(library, &q),
            }
        }
    });

    // A source with nothing to offer stays silent rather than adding an empty
    // heading for every backend we asked.
    let tracks = match &*results.read_unchecked() {
        Some(t) if !t.is_empty() => t.clone(),
        _ => return rsx! {},
    };

    rsx! {
        section { class: "mb-6",
            div { class: "flex items-center gap-2 mb-2 px-1",
                i { class: "{source.icon()} text-sm" }
                h3 { class: "text-sm font-bold text-white/80", "{source.label()}" }
            }
            div { class: "flex flex-col gap-1 max-w-3xl",
                for (idx, (track, cover)) in tracks.iter().enumerate() {
                    FallbackRow { key: "{idx}", track: track.clone(), cover: cover.clone(), source }
                }
            }
        }
    }
}

/// Remote hits carry their artwork in the path/album id, resolved the same way
/// the SoundCloud and server rows do it.
fn remote_hits(tracks: Vec<Track>) -> Vec<(Track, Option<utils::CoverUrl>)> {
    tracks
        .into_iter()
        .take(FALLBACK_LIMIT)
        .map(|t| {
            let path_str = t.path.to_string_lossy().to_string();
            let cover = utils::map_cover_url(
                utils::jellyfin_image::track_cover_url_with_album_fallback(
                    &path_str,
                    &t.album_id,
                    "",
                    None,
                    80,
                    80,
                ),
            );
            (t, cover)
        })
        .collect()
}

/// Substring match over the on-disk library — the same "does the title or the
/// artist contain this" rule the local search page uses. Each hit carries its
/// album's cover, which is where a local track's artwork lives.
fn local_matches(library: Signal<Library>, query: &str) -> Vec<(Track, Option<utils::CoverUrl>)> {
    let needle = query.to_lowercase();
    let lib = library.read();
    let covers: std::collections::HashMap<&String, &std::path::PathBuf> = lib
        .albums
        .iter()
        .filter_map(|a| a.cover_path.as_ref().map(|c| (&a.id, c)))
        .collect();
    lib.tracks
        .iter()
        .filter(|t| {
            t.title.to_lowercase().contains(&needle) || t.artist.to_lowercase().contains(&needle)
        })
        .take(FALLBACK_LIMIT)
        .map(|t| {
            let cover = covers
                .get(&t.album_id)
                .and_then(|c| utils::format_artwork_thumb_url(Some(*c), 80));
            (t.clone(), cover)
        })
        .collect()
}

#[component]
fn FallbackRow(track: Track, cover: Option<utils::CoverUrl>, source: FallbackSource) -> Element {
    let mut ctrl = use_context::<PlayerController>();

    let track_play = track.clone();
    let track_ctx = track.clone();
    let track_add = track.clone();

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
                    i { class: "{source.icon()}" }
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
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(active: MusicSource, service: Option<MusicService>) -> AppConfig {
        let mut c = AppConfig::default();
        c.active_source = active;
        c.server = service.map(|service| config::MusicServer {
            name: "test".into(),
            url: String::new(),
            service,
            access_token: None,
            user_id: None,
            id: None,
            yt_browser: None,
            yt_anonymous: false,
            yt_manual: false,
        });
        c
    }

    #[test]
    fn never_re_asks_the_source_that_just_came_up_empty() {
        let on_yt = others(&cfg(MusicSource::Server, Some(MusicService::YtMusic)));
        assert!(
            !on_yt.contains(&FallbackSource::YtMusic),
            "searching YT again after YT found nothing is a wasted request",
        );
        assert!(on_yt.contains(&FallbackSource::SoundCloud));
        assert!(on_yt.contains(&FallbackSource::Local));
    }

    #[test]
    fn a_non_youtube_server_still_gets_youtube_offered() {
        let on_jellyfin = others(&cfg(MusicSource::Server, Some(MusicService::Jellyfin)));
        assert!(on_jellyfin.contains(&FallbackSource::YtMusic));
        assert!(on_jellyfin.contains(&FallbackSource::SoundCloud));
    }

    #[test]
    fn local_search_does_not_offer_local_again() {
        let on_local = others(&cfg(MusicSource::Local, None));
        assert!(!on_local.contains(&FallbackSource::Local));
        assert!(on_local.contains(&FallbackSource::YtMusic));
    }
}
