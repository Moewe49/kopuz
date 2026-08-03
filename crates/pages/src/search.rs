use components::search_bar::{SearchBar, SearchSource, SearchSourceState};
use components::soundcloud_search::SoundCloudSearchView;
use components::spotify_search::SpotifySearchView;
use config::{AppConfig, MusicSource};
use dioxus::prelude::*;
use player::player;
use reader::Library;

use crate::local::search::LocalSearch;
use crate::server::search::ServerSearch;

#[component]
pub fn Search(
    library: Signal<Library>,
    config: Signal<AppConfig>,
    playlist_store: Signal<reader::PlaylistStore>,
    search_query: Signal<String>,
    player: Signal<player::Player>,
    is_playing: Signal<bool>,
    current_playing: Signal<u64>,
    current_song_cover_url: Signal<String>,
    current_song_title: Signal<String>,
    current_song_artist: Signal<String>,
    current_song_duration: Signal<u64>,
    current_song_progress: Signal<u64>,
    queue: Signal<Vec<reader::models::Track>>,
    current_queue_index: Signal<usize>,
    on_select_album: EventHandler<String>,
    on_select_playlist: EventHandler<(String, String)>,
    on_open_artist: EventHandler<(String, String)>,
    on_search_artist: EventHandler<String>,
) -> Element {
    // Unified search source — driven by the dropdown in the search bar.
    // SoundCloud is an explicit overlay; for Local/Server we follow the
    // active backend so the sidebar toggle keeps working in lockstep (the
    // dropdown sets active_source too, so the two never disagree).
    let source = use_context::<SearchSourceState>().0;
    let is_soundcloud = *source.read() == SearchSource::SoundCloud;
    let is_spotify = *source.read() == SearchSource::Spotify;
    let is_server = config.read().active_source == MusicSource::Server;

    rsx! {
        if is_soundcloud {
            // SoundCloud-only view: render the shared bar (with dropdown) plus
            // the auto-searching SoundCloud results.
            div {
                class: "p-8 absolute inset-0 flex flex-col",
                SearchBar { search_query }
                SoundCloudSearchView { search_query }
            }
        } else if is_spotify {
            // Spotify is an overlay source like SoundCloud: its own results
            // view, playback routed through the YT Music match.
            div {
                class: "p-8 absolute inset-0 flex flex-col",
                SearchBar { search_query }
                SpotifySearchView { search_query }
            }
        } else if is_server {
            ServerSearch {
                library,
                config,
                playlist_store,
                search_query,
                player,
                is_playing,
                current_playing,
                current_song_cover_url,
                current_song_title,
                current_song_artist,
                current_song_duration,
                current_song_progress,
                queue,
                current_queue_index,
                on_select_album,
                on_select_playlist,
                on_open_artist,
                on_search_artist,
            }
        } else {
            LocalSearch {
                library,
                config,
                playlist_store,
                search_query,
                player,
                is_playing,
                current_playing,
                current_song_cover_url,
                current_song_title,
                current_song_artist,
                current_song_duration,
                current_song_progress,
                queue,
                current_queue_index,
                on_select_album,
            }
        }
    }
}
