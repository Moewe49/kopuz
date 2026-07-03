use crate::server::download_manager::{DownloadQueue, DownloadStatus, queue_downloads_into};
use ::server::jellyfin::JellyfinClient;
use ::server::subsonic::SubsonicClient;
use config::{AppConfig, MusicService};
use dioxus::prelude::*;
use reader::{Library, PlaylistStore};

#[component]
pub fn JellyfinPlaylists(
    playlist_store: Signal<PlaylistStore>,
    library: Signal<Library>,
    config: Signal<AppConfig>,
    mut selected_playlist_id: Signal<Option<String>>,
    #[props(default)] refresh_trigger: Signal<u64>,
) -> Element {
    let is_offline = use_context::<Signal<bool>>();
    let mut last_fetch_key = use_signal(|| None::<String>);
    let mut fetch_request_id = use_signal(|| 0u64);
    let mut yt_refresh_nonce: Signal<u64> = use_signal(|| 0);
    let mut show_spotify_import = use_signal(|| false);
    let mut yt_is_syncing = use_signal(|| false);
    let mut yt_synced_so_far: Signal<usize> = use_signal(|| 0);
    let mut yt_sync_error = use_signal(|| None::<String>);
    let download_queue = use_context::<Signal<DownloadQueue>>();

    // Auto-refresh: while the playlists page is open, re-sync the playlist list
    // every few minutes so new imports and server-side changes appear without a
    // manual refresh. Cheap — this is just the playlist list, not each
    // playlist's tracks (those load on open). Pauses while offline or syncing.
    use_future(move || async move {
        loop {
            utils::sleep(std::time::Duration::from_secs(180)).await;
            if !*is_offline.peek() && !*yt_is_syncing.peek() {
                let next = *yt_refresh_nonce.peek() + 1;
                yt_refresh_nonce.set(next);
            }
        }
    });

    use_effect(move || {
        let yt_nonce = *yt_refresh_nonce.read();
        let trigger = *refresh_trigger.read();

        let fetch_context = {
            let conf = config.peek();
            conf.server.as_ref().and_then(|server| {
                if let (Some(token), Some(user_id)) = (&server.access_token, &server.user_id) {
                    Some((
                        server.service,
                        server.url.clone(),
                        token.clone(),
                        user_id.clone(),
                        conf.device_id.clone(),
                    ))
                } else {
                    None
                }
            })
        };
        let is_ytmusic = fetch_context
            .as_ref()
            .map(|(s, _, _, _, _)| *s == MusicService::YtMusic)
            .unwrap_or(false);

        if is_ytmusic
            && yt_nonce == 0
            && trigger == 0
            && library.peek().last_yt_playlists_sync_at.is_some()
        {
            return;
        }

        // Build a "server identity" key (without trigger) to detect server changes
        let server_key = fetch_context
            .as_ref()
            .map(|(service, url, _, user_id, _)| format!("{service:?}|{url}|{user_id}"));

        // Build the full fetch key that also includes the trigger
        let fetch_key = fetch_context
            .as_ref()
            .map(|(service, url, token, user_id, _)| {
                // Include the refresh nonce so an explicit/auto refresh always
                // produces a distinct key and actually re-fetches (the dedup
                // below otherwise treats an unchanged token+trigger as "already
                // fetched" and silently skips).
                format!("{service:?}|{url}|{user_id}|{token}|{trigger}|{yt_nonce}")
            });

        // peek() rather than read() — we already control re-firing
        // via yt_refresh_nonce / refresh_trigger above.
        let has_cached = {
            let store = playlist_store.peek();
            !store.jellyfin_playlists.is_empty()
        };
        let last_key = last_fetch_key.peek().clone();

        // Extract the server-identity part of the last fetch key (everything before the last |)
        let last_server_key = last_key.as_ref().and_then(|k| {
            let parts: Vec<&str> = k.splitn(5, '|').collect();
            if parts.len() >= 3 {
                Some(format!("{}", &parts[..3].join("|")))
            } else {
                None
            }
        });

        // Skip if same key (already fetched this exact state)
        if last_key.as_ref() == fetch_key.as_ref() {
            return;
        }

        // If server identity is the same and we have cached data, only re-fetch
        // on an explicit trigger or refresh nonce (manual or periodic auto).
        if server_key == last_server_key && has_cached && trigger == 0 && yt_nonce == 0 {
            // Update the key so we don't keep hitting this branch, but don't fetch
            last_fetch_key.set(fetch_key.clone());
            return;
        }

        last_fetch_key.set(fetch_key.clone());

        let request_id = *fetch_request_id.read() + 1;
        fetch_request_id.set(request_id);

        let Some((service, url, token, user_id, device_id)) = fetch_context else {
            return;
        };

        spawn(async move {
            let mut server_playlists = Vec::new();

            match service {
                MusicService::Jellyfin => {
                    let remote =
                        JellyfinClient::new(&url, Some(&token), &device_id, Some(&user_id));
                    if let Ok(playlists) = remote.get_playlists().await {
                        for p in playlists {
                            let image_tag = p
                                .image_tags
                                .as_ref()
                                .and_then(|tags| tags.get("Primary"))
                                .cloned();
                            if let Ok(items) = remote.get_playlist_items(&p.id).await {
                                let tracks: Vec<String> =
                                    items.into_iter().map(|item| item.id).collect();
                                server_playlists.push(reader::models::JellyfinPlaylist {
                                    id: p.id.clone(),
                                    name: p.name.clone(),
                                    tracks,
                                    image_tag,
                                    cover_path: None,
                                });
                            } else {
                                server_playlists.push(reader::models::JellyfinPlaylist {
                                    id: p.id.clone(),
                                    name: p.name.clone(),
                                    tracks: vec![],
                                    image_tag,
                                    cover_path: None,
                                });
                            }
                        }
                    }
                }
                MusicService::Subsonic | MusicService::Custom => {
                    let remote = SubsonicClient::new(&url, &user_id, &token);
                    if let Ok(playlists) = remote.get_playlists().await {
                        for p in playlists {
                            let tracks = remote
                                .get_playlist_entries(&p.id)
                                .await
                                .unwrap_or_default()
                                .into_iter()
                                .map(|song| song.id)
                                .collect();
                            server_playlists.push(reader::models::JellyfinPlaylist {
                                id: p.id,
                                name: p.name,
                                tracks,
                                image_tag: None,
                                cover_path: None,
                            });
                        }
                    }
                }
                MusicService::YtMusic => {
                    eprintln!("[yt-playlists] sync starting");
                    yt_is_syncing.set(true);
                    yt_synced_so_far.set(0);
                    yt_sync_error.set(None);
                    let yt =
                        ::server::ytmusic::YouTubeMusicClient::with_cookies(token.clone());
                    let list_result = yt.list_playlists().await;
                    if *fetch_request_id.read() != request_id {
                        return;
                    }
                    eprintln!(
                        "[yt-playlists] list_playlists → {}",
                        list_result
                            .as_ref()
                            .map(|v| format!("{} entries", v.len()))
                            .unwrap_or_else(|e| format!("ERR {e}"))
                    );
                    let summaries = match list_result {
                        Ok(s) => s,
                        Err(e) => {
                            // Surface the real reason (expired session, etc.)
                            // instead of a silently empty list.
                            yt_sync_error.set(Some(e));
                            yt_is_syncing.set(false);
                            return;
                        }
                    };
                    let total = summaries.len();
                    yt_synced_so_far.set(0);

                    {
                        let mut store_write = playlist_store.write();
                        let mut seeded: Vec<reader::models::JellyfinPlaylist> = summaries
                            .iter()
                            .map(|s| {
                                let image_tag = s
                                    .thumbnail_url
                                    .as_ref()
                                    .map(|u| utils::jellyfin_image::encode_cover_url(u));
                                let existing_cover_path = store_write
                                    .jellyfin_playlists
                                    .iter()
                                    .find(|e| e.id == s.id)
                                    .and_then(|e| e.cover_path.clone());
                                reader::models::JellyfinPlaylist {
                                    id: s.id.clone(),
                                    name: s.title.clone(),
                                    tracks: Vec::new(),
                                    image_tag,
                                    cover_path: existing_cover_path,
                                }
                            })
                            .collect();
                        for existing in store_write.jellyfin_playlists.drain(..) {
                            if !seeded.iter().any(|s| s.id == existing.id) {
                                seeded.push(existing);
                            }
                        }
                        store_write.jellyfin_playlists = seeded;
                    }

                    let mut accumulated: Vec<reader::models::Track> = Vec::new();
                    let mut seen_paths: std::collections::HashSet<std::path::PathBuf> =
                        std::collections::HashSet::new();
                    for (i, summary) in summaries.into_iter().enumerate() {
                        if *fetch_request_id.read() != request_id {
                            return;
                        }
                        yt_synced_so_far.set(i + 1);
                        let tracks = match yt.get_playlist_entries(&summary.id).await {
                            Ok(t) => t,
                            Err(e) => {
                                eprintln!("[yt-playlists] {} → ERR {e}", summary.id);
                                Vec::new()
                            }
                        };
                        let track_ids: Vec<String> = tracks
                            .iter()
                            .filter_map(|t| {
                                t.path
                                    .to_string_lossy()
                                    .split(':')
                                    .nth(1)
                                    .map(|s| s.to_string())
                            })
                            .collect();
                        {
                            let mut store_write = playlist_store.write();
                            if let Some(entry) = store_write
                                .jellyfin_playlists
                                .iter_mut()
                                .find(|e| e.id == summary.id)
                            {
                                entry.tracks = track_ids;
                            }
                        }
                        for t in tracks {
                            if seen_paths.insert(t.path.clone()) {
                                accumulated.push(t);
                            }
                        }
                    }

                    if *fetch_request_id.read() != request_id {
                        return;
                    }
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let mut lib = library.write();
                    let mut existing: std::collections::HashSet<std::path::PathBuf> = lib
                        .jellyfin_tracks
                        .iter()
                        .map(|t| t.path.clone())
                        .collect();
                    for t in accumulated {
                        if existing.insert(t.path.clone()) {
                            lib.jellyfin_tracks.push(t);
                        }
                    }
                    lib.last_yt_playlists_sync_at = Some(now);
                    yt_is_syncing.set(false);
                    yt_synced_so_far.set(total);
                    return;
                }
            }

            if *fetch_request_id.read() != request_id {
                return;
            }

            let mut store_write = playlist_store.write();
            // Preserve any locally-set cover_path when replacing server data
            for p in &mut server_playlists {
                if let Some(existing) = store_write.jellyfin_playlists.iter().find(|e| e.id == p.id)
                {
                    p.cover_path = existing.cover_path.clone();
                }
            }
            store_write.jellyfin_playlists = server_playlists;
        });
    });

    let jellyfin_playlists = use_memo(move || {
        let store_ref = playlist_store.read();
        let offline = *is_offline.read();
        let conf = config.read();
        if offline {
            store_ref
                .jellyfin_playlists
                .iter()
                .filter(|p| {
                    !p.tracks.is_empty()
                        && p.tracks.iter().all(|tid| {
                            if let Some(path_str) = conf.offline_tracks.get(tid) {
                                std::path::Path::new(path_str).exists()
                            } else {
                                false
                            }
                        })
                })
                .cloned()
                .collect()
        } else {
            store_ref.jellyfin_playlists.clone()
        }
    });

    let playlists = jellyfin_playlists.read().clone();
    let is_ytmusic_active = config
        .read()
        .server
        .as_ref()
        .map(|s| s.service == MusicService::YtMusic)
        .unwrap_or(false);

    rsx! {
        div {
            if is_ytmusic_active {
                {
                    let syncing = *yt_is_syncing.read();
                    let done = *yt_synced_so_far.read();
                    // Total is the number of tiles seeded into the
                    // store; while syncing this is the target count,
                    // after syncing it's the final count.
                    let total = playlists.len();
                    let remaining = total.saturating_sub(done);
                    rsx! {
                        div {
                            class: "flex items-center justify-between gap-3 mb-3 px-2 text-xs text-slate-400",
                            div {
                                class: "flex items-center gap-2",
                                if syncing {
                                    i { class: "fa-solid fa-arrows-rotate fa-spin text-indigo-300" }
                                    span { "Loading tracks — {done} / {total} playlists ({remaining} left)" }
                                } else if total > 0 {
                                    i { class: "fa-solid fa-check text-emerald-400" }
                                    span { "{total} playlists synced" }
                                }
                            }
                            div {
                                class: "flex items-center gap-2",
                                button {
                                    class: "px-3 py-1 rounded bg-emerald-600/20 hover:bg-emerald-600/35 text-emerald-300 transition-colors",
                                    onclick: move |_| show_spotify_import.set(true),
                                    i { class: "fa-brands fa-spotify mr-1" }
                                    "{i18n::t(\"spotify_import_from\")}"
                                }
                                button {
                                    class: "px-3 py-1 rounded bg-white/5 hover:bg-white/10 text-white/80 transition-colors disabled:opacity-50",
                                    disabled: syncing,
                                    onclick: move |_| {
                                        let next = *yt_refresh_nonce.peek() + 1;
                                        yt_refresh_nonce.set(next);
                                    },
                                    i { class: "fa-solid fa-arrows-rotate mr-1" }
                                    "Refresh"
                                }
                            }
                        }
                    }
                }
            }

            if let Some(err) = yt_sync_error.read().clone() {
                div {
                    class: "mx-2 mb-3 rounded-lg bg-rose-500/10 border border-rose-400/30 text-rose-200 text-xs p-3",
                    i { class: "fa-solid fa-triangle-exclamation mr-1" }
                    "{err}"
                }
            }

            if *show_spotify_import.read() {
                components::spotify_import::SpotifyImportModal {
                    config: config,
                    on_close: move |_| show_spotify_import.set(false),
                    on_imported: move |_| {
                        // Refetch the YT playlist list so the clone
                        // shows up in the library immediately.
                        let next = *yt_refresh_nonce.peek() + 1;
                        yt_refresh_nonce.set(next);
                    },
                }
            }

            if playlists.is_empty() {
                {
                    // Anonymous YT has no library playlists by design —
                    // show a sign-in prompt rather than the generic
                    // "no playlists found" empty state.
                    let yt_anon = config
                        .read()
                        .server
                        .as_ref()
                        .map(|s| {
                            s.service == config::MusicService::YtMusic && s.yt_anonymous
                        })
                        .unwrap_or(false);
                    rsx! {
                        div { class: "flex flex-col items-center justify-center h-64 text-slate-500 text-center px-6",
                            if yt_anon {
                                i { class: "fa-solid fa-right-to-bracket text-4xl mb-4 opacity-50" }
                                p { "{i18n::t(\"yt_anon_playlists\")}" }
                            } else {
                                i { class: "fa-regular fa-folder-open text-4xl mb-4 opacity-50" }
                                p { "{i18n::t(\"no_playlists_found\")}" }
                            }
                        }
                    }
                }
            } else {
                div { class: "grid grid-cols-2 md:grid-cols-3 lg:grid-cols-4 gap-6",
                    {playlists.into_iter().map(|playlist| {
                        let cover_url = {
                            let conf = config.peek();
                            if let Some(server) = &conf.server {
                                if let Some(path) = &playlist.cover_path {
                                    utils::format_artwork_url(Some(path))
                                } else if let Some(tag) = &playlist.image_tag {
                                    utils::map_cover_url(Some(utils::jellyfin_image::jellyfin_image_url(
                                        &server.url,
                                        &playlist.id,
                                        Some(tag.as_str()),
                                        server.access_token.as_deref(),
                                        384,
                                        80,
                                    )))
                                } else if let Some(first_track_id) = playlist.tracks.first() {
                                    let lib = library.peek();
                                    lib.jellyfin_tracks
                                        .iter()
                                        .find(|t| t.path.to_string_lossy().contains(first_track_id.as_str()))
                                        .and_then(|t| {
                                            let path_str = t.path.to_string_lossy();
                                            utils::map_cover_url(utils::jellyfin_image::track_cover_url_with_album_fallback(
                                                &path_str,
                                                &t.album_id,
                                                &server.url,
                                                server.access_token.as_deref(),
                                                384,
                                                80,
                                            ))
                                        })
                                } else {
                                    None
                                }
                            } else {
                                None
                            }
                        };

                        let playlist_id_nav = playlist.id.clone();
                        let track_requests_dl: Vec<(String, String, String)> = {
                            let lib = library.peek();
                            playlist.tracks.iter().map(|tid| {
                                let meta = lib.jellyfin_tracks.iter()
                                    .find(|t| t.path.to_string_lossy().contains(tid.as_str()));
                                (
                                    tid.clone(),
                                    meta.map(|t| t.title.clone()).unwrap_or_default(),
                                    meta.map(|t| t.artist.clone()).unwrap_or_default(),
                                )
                            }).collect()
                        };
                        // SoundCloud overlay tracks attached to this playlist
                        // (they can't live in the server's track list).
                        let sc_requests = crate::server::download_manager::soundcloud_requests_for(
                            &playlist_store.read(),
                            &playlist.id,
                        );
                        let is_dl = {
                            let q = download_queue.read();
                            playlist.tracks.iter().map(|t| t.as_str())
                                .chain(sc_requests.iter().map(|(id, _, _)| id.as_str()))
                                .any(|tid| q.items.iter().any(|i| i.id == tid && matches!(i.status, DownloadStatus::Queued | DownloadStatus::Downloading)))
                        };

                        let all_downloaded = {
                            let conf = config.read();
                            let offline = |key: &str| {
                                conf.offline_tracks
                                    .get(key)
                                    .map(|p| std::path::Path::new(p).exists())
                                    .unwrap_or(false)
                            };
                            (!playlist.tracks.is_empty() || !sc_requests.is_empty())
                                && playlist.tracks.iter().all(|tid| offline(tid))
                                // SC offline key = the hex permalink (path segment 1).
                                && sc_requests
                                    .iter()
                                    .all(|(id, _, _)| offline(id.split(':').nth(1).unwrap_or(id)))
                        };
                        // Count SoundCloud overlay tracks attached to this YT
                        // playlist so the overview total matches the detail.
                        let total_tracks = playlist.tracks.len()
                            + playlist_store
                                .read()
                                .external_tracks
                                .get(&playlist.id)
                                .map(|v| v.len())
                                .unwrap_or(0);

                        rsx! {
                            div {
                                key: "{playlist.id}",
                                class: "bg-white/5 border border-white/5 rounded-2xl p-6 hover:bg-white/10 transition-all cursor-pointer group relative",
                                onclick: move |_| selected_playlist_id.set(Some(playlist_id_nav.clone())),
                                div {
                                    class: "mb-4 w-full aspect-square rounded-xl flex items-center justify-center overflow-hidden transition-all bg-white/5",
                                    if let Some(url) = cover_url {
                                        img {
                                            src: "{url}",
                                            class: "w-full h-full object-cover",
                                            decoding: "async", loading: "lazy"
                                        }
                                    } else {
                                        div {
                                            class: "w-full h-full flex items-center justify-center",
                                            style: "background: color-mix(in srgb, var(--color-indigo-500), transparent 80%); color: var(--color-indigo-400)",
                                            i { class: "fa-solid fa-server text-2xl" }
                                        }
                                    }
                                }
                                h3 { class: "text-xl font-bold text-white mb-1 truncate", "{playlist.name}" }
                                p { class: "text-sm text-slate-400", "Server • {total_tracks} tracks" }

                                button {
                                    class: "absolute top-4 right-4 w-8 h-8 rounded-full bg-black/40 border border-white/10 flex items-center justify-center text-white/60 hover:text-white hover:border-white/30 transition-colors opacity-0 group-hover:opacity-100",
                                    title: if all_downloaded { "Remove downloads" } else { "Download playlist for offline playback" },
                                    disabled: is_dl,
                                    onclick: {
                                        let playlist_name_dl = playlist.name.clone();
                                        let playlist_id_dl2 = playlist.id.clone();
                                        let known_ids = {
                                            let mut ids = playlist.tracks.clone();
                                            ids.extend(sc_requests.iter().map(|(id, _, _)| id.clone()));
                                            ids
                                        };
                                        move |evt: Event<MouseData>| {
                                            evt.stop_propagation();
                                            if all_downloaded {
                                                crate::server::download_manager::delete_downloads(known_ids.clone(), config, download_queue);
                                                return;
                                            }
                                            // Group all of a playlist's downloads under
                                            // <downloads>/<Playlist Name>/.
                                            let subdir = Some(playlist_name_dl.clone());
                                            let is_yt = matches!(
                                                config.peek().server.as_ref().map(|s| s.service),
                                                Some(MusicService::YtMusic)
                                            );
                                            if !is_yt {
                                                let mut requests = track_requests_dl.clone();
                                                requests.extend(sc_requests.iter().cloned());
                                                if !requests.is_empty() {
                                                    queue_downloads_into(requests, subdir, config, download_queue);
                                                }
                                                return;
                                            }
                                            // YT: always fetch the CURRENT entries — the
                                            // cached list goes stale when songs are added
                                            // from other pages, which made this button
                                            // claim "already downloaded" for fresh songs.
                                            // spawn_forever: navigating away must not
                                            // cancel the fetch.
                                            let pid = playlist_id_dl2.clone();
                                            let fallback = track_requests_dl.clone();
                                            let sc = sc_requests.clone();
                                            dioxus::core::spawn_forever(async move {
                                                let cookies = config.peek().server.as_ref().and_then(|s| s.access_token.clone());
                                                let mut live: Vec<(String, String, String)> = Vec::new();
                                                if let Some(cookies) = cookies {
                                                    let yt = ::server::ytmusic::YouTubeMusicClient::with_cookies(cookies);
                                                    if let Ok(tracks) = yt.get_playlist_entries(&pid).await {
                                                        live = tracks.iter().filter_map(|t| {
                                                            let vid = t.path.to_string_lossy().split(':').nth(1).filter(|s| !s.is_empty()).map(|s| s.to_string())?;
                                                            Some((vid, t.title.clone(), t.artist.clone()))
                                                        }).collect();
                                                    }
                                                }
                                                let mut requests = if live.is_empty() { fallback } else { live };
                                                requests.extend(sc);
                                                if !requests.is_empty() {
                                                    queue_downloads_into(requests, subdir, config, download_queue);
                                                }
                                            });
                                        }
                                    },
                                    if is_dl {
                                        i { class: "fa-solid fa-spinner fa-spin text-xs" }
                                    } else if all_downloaded {
                                        i { class: "fa-solid fa-trash text-xs" }
                                    } else {
                                        i { class: "fa-solid fa-download text-xs" }
                                    }
                                }
                            }
                        }
                    })}
                }
            }
        }
    }
}

pub use JellyfinPlaylists as ServerPlaylists;
