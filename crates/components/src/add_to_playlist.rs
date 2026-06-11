//! App-wide "add this track to a playlist" host.
//!
//! A single [`AddToPlaylistHost`] is mounted once near the app root and
//! watches the [`AddToPlaylistState`] context signal. Anything that wants the
//! picker — a track row, the now-playing bar's right-click menu — just drops a
//! [`Track`] into that signal; the host pops the [`PlaylistModal`] and runs the
//! correct add/create against whichever backend the track belongs to (local
//! path-based playlists, or Jellyfin / Subsonic / YouTube Music server
//! playlists). Centralizing it keeps the per-backend mutation logic in one
//! place instead of copy-pasted into every call site.

use config::{AppConfig, MusicService, MusicSource};
use dioxus::prelude::*;
use reader::PlaylistStore;
use reader::models::Track;

use crate::playlist_modal::PlaylistModal;

/// Drop a [`Track`] here to open the add-to-playlist picker for it.
#[derive(Clone, Copy)]
pub struct AddToPlaylistState(pub Signal<Option<Track>>);

/// Convenience for call sites that have the context in scope.
pub fn request_add_to_playlist(track: Track) {
    if let Some(state) = try_consume_context::<AddToPlaylistState>() {
        let mut sig = state.0;
        sig.set(Some(track));
    }
}

/// The YT/Jellyfin/Subsonic server track id is segment 1 of the synthetic
/// path (`<service>:<id>:…`); local tracks use the real filesystem path.
fn server_track_id(track: &Track) -> Option<String> {
    track
        .path
        .to_str()
        .and_then(|s| s.split(':').nth(1))
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
}

#[component]
pub fn AddToPlaylistHost(
    config: Signal<AppConfig>,
    playlist_store: Signal<PlaylistStore>,
) -> Element {
    let state = use_context::<AddToPlaylistState>();
    let mut pending = state.0;

    let Some(track) = pending.read().clone() else {
        return rsx! {};
    };

    // SoundCloud (and any non-server) tracks can't be stored server-side —
    // YT/Jellyfin/Subsonic only accept their own catalog ids. So the modal
    // still lists the ACTIVE backend's playlists (so you can drop a SoundCloud
    // song into a YouTube Music playlist), but the add is routed to a local
    // overlay keyed by that playlist id. The playlist view merges the overlay
    // back in, so the song shows + plays inside the YT playlist anyway.
    let is_external = server::soundcloud::is_soundcloud_path(&track.path.to_string_lossy());
    let is_server = config.read().active_source == MusicSource::Server;

    // Attach an external track to a (server or local) playlist via the overlay.
    let mut add_external = move |playlist_id: String, track: Track| {
        let mut store = playlist_store.write();
        let entry = store.external_tracks.entry(playlist_id).or_default();
        if !entry.contains(&track.path) {
            entry.push(track.path.clone());
        }
    };

    let mut add_local = move |playlist_id: String, track: Track| {
        let mut store = playlist_store.write();
        if let Some(pl) = store.playlists.iter_mut().find(|p| p.id == playlist_id)
            && !pl.tracks.contains(&track.path)
        {
            pl.tracks.push(track.path.clone());
        }
    };
    let mut create_local = move |name: String, track: Track| {
        playlist_store.write().playlists.push(reader::models::Playlist {
            id: uuid::Uuid::new_v4().to_string(),
            name,
            tracks: vec![track.path.clone()],
            cover_path: None,
        });
    };

    // Server add/create: dispatch on the active service. Optimistically
    // reflects the change in the local jellyfin_playlists store so the UI
    // updates without a full resync.
    let mut server_add = move |playlist_id: String, track: Track| {
        let Some(item_id) = server_track_id(&track) else {
            return;
        };
        {
            let mut store = playlist_store.write();
            if let Some(pl) = store.jellyfin_playlists.iter_mut().find(|p| p.id == playlist_id)
                && !pl.tracks.contains(&item_id)
            {
                pl.tracks.push(item_id.clone());
            }
        }
        spawn(async move {
            let conf = config.peek().clone();
            let Some(server) = conf.server.as_ref() else {
                return;
            };
            let Some(token) = server.access_token.as_ref() else {
                return;
            };
            match server.service {
                MusicService::Jellyfin => {
                    let Some(user_id) = server.user_id.as_ref() else {
                        return;
                    };
                    let remote = server::jellyfin::JellyfinClient::new(
                        &server.url,
                        Some(token),
                        &conf.device_id,
                        Some(user_id),
                    );
                    let _ = remote.add_to_playlist(&playlist_id, &item_id).await;
                }
                MusicService::Subsonic | MusicService::Custom => {
                    let Some(user_id) = server.user_id.as_ref() else {
                        return;
                    };
                    let remote =
                        server::subsonic::SubsonicClient::new(&server.url, user_id, token);
                    let _ = remote.add_to_playlist(&playlist_id, &item_id).await;
                }
                MusicService::YtMusic => {
                    let yt = server::ytmusic::YouTubeMusicClient::with_cookies(token.clone());
                    let _ = yt.add_to_playlist(&playlist_id, &item_id).await;
                }
            }
        });
    };
    let mut server_create = move |name: String, track: Track| {
        let Some(item_id) = server_track_id(&track) else {
            return;
        };
        spawn(async move {
            let conf = config.peek().clone();
            let Some(server) = conf.server.as_ref() else {
                return;
            };
            let Some(token) = server.access_token.as_ref() else {
                return;
            };
            match server.service {
                MusicService::Jellyfin => {
                    let Some(user_id) = server.user_id.as_ref() else {
                        return;
                    };
                    let remote = server::jellyfin::JellyfinClient::new(
                        &server.url,
                        Some(token),
                        &conf.device_id,
                        Some(user_id),
                    );
                    let _ = remote.create_playlist(&name, &[item_id.as_str()]).await;
                }
                MusicService::Subsonic | MusicService::Custom => {
                    let Some(user_id) = server.user_id.as_ref() else {
                        return;
                    };
                    let remote =
                        server::subsonic::SubsonicClient::new(&server.url, user_id, token);
                    let _ = remote.create_playlist(&name, &[item_id.as_str()]).await;
                }
                MusicService::YtMusic => {
                    let yt = server::ytmusic::YouTubeMusicClient::with_cookies(token.clone());
                    let _ = yt.create_playlist(&name, "", &[item_id.as_str()]).await;
                }
            }
        });
    };

    let track_for_add = track.clone();
    let track_for_create = track.clone();

    rsx! {
        PlaylistModal {
            playlist_store,
            is_jellyfin: is_server,
            on_close: move |_| pending.set(None),
            on_add_to_playlist: move |playlist_id: String| {
                // Resolve the playlist's name for the confirmation toast.
                let pl_name = {
                    let store = playlist_store.peek();
                    store
                        .jellyfin_playlists
                        .iter()
                        .find(|p| p.id == playlist_id)
                        .map(|p| p.name.clone())
                        .or_else(|| {
                            store
                                .playlists
                                .iter()
                                .find(|p| p.id == playlist_id)
                                .map(|p| p.name.clone())
                        })
                        .unwrap_or_default()
                };
                if is_external {
                    // Can't live in the server catalog → store in the overlay
                    // for this playlist id (works for both server and local).
                    add_external(playlist_id, track_for_add.clone());
                } else if is_server {
                    server_add(playlist_id, track_for_add.clone());
                } else {
                    add_local(playlist_id, track_for_add.clone());
                }
                crate::toast::show_toast(i18n::t_with(
                    "added_to_playlist_toast",
                    &[("name", pl_name)],
                ));
                pending.set(None);
            },
            on_create_playlist: move |name: String| {
                if is_external {
                    // A brand-new playlist holding only an external track is a
                    // local playlist (no point creating an empty server one).
                    create_local(name.clone(), track_for_create.clone());
                } else if is_server {
                    server_create(name.clone(), track_for_create.clone());
                } else {
                    create_local(name.clone(), track_for_create.clone());
                }
                crate::toast::show_toast(i18n::t_with(
                    "added_to_playlist_toast",
                    &[("name", name)],
                ));
                pending.set(None);
            },
        }
    }
}
