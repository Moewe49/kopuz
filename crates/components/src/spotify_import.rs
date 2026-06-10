//! "Import from Spotify" modal. Two source paths:
//!
//! - **URL tab**: paste any public playlist/album link — fetched
//!   anonymously through the Spotify embed endpoint (≈first 100
//!   tracks, no login required).
//! - **Account tab**: OAuth PKCE with the user's own Spotify app
//!   client id. Lists every playlist (incl. private + Liked Songs)
//!   and clones the selected one, fully paginated.
//!
//! Either way the tracks get matched on YT Music and recreated as a
//! real playlist in the signed-in YT Music account, so it appears in
//! the library — playable and editable like any other playlist.

use dioxus::prelude::*;
use server::spotify::{self, matcher::CloneEvent};

/// Where the import currently stands. One linear state machine keeps
/// the modal honest — no half-loading half-done UI.
#[derive(Clone, PartialEq)]
enum ImportPhase {
    Idle,
    FetchingSource,
    Matching { done: usize, total: usize, current: String },
    Creating,
    Adding { done: usize, total: usize },
    Done { matched: usize, total: usize, unmatched: Vec<String>, name: String },
    Failed(String),
}

#[derive(Clone, Copy, PartialEq)]
enum SourceTab {
    Url,
    Account,
}

#[component]
pub fn SpotifyImportModal(
    config: Signal<config::AppConfig>,
    on_close: EventHandler,
    /// Fired after a successful clone so the playlists page can
    /// refresh its YT list.
    on_imported: EventHandler,
) -> Element {
    let mut tab = use_signal(|| SourceTab::Url);
    let mut url_input = use_signal(String::new);
    let mut client_id_input =
        use_signal(|| config.peek().spotify_client_id.clone());
    let mut phase = use_signal(|| ImportPhase::Idle);
    // (access_token, unix_expiry_secs) for the connected account.
    let access_token = use_signal(|| None::<(String, u64)>);
    let mut account_playlists =
        use_signal(|| None::<Vec<server::spotify::api::PlaylistSummary>>);
    let mut connecting = use_signal(|| false);
    let mut account_error = use_signal(|| None::<String>);

    let yt_cookies = use_memo(move || {
        config
            .read()
            .server
            .as_ref()
            .and_then(|s| s.access_token.clone())
            .filter(|c| !c.is_empty())
    });

    let busy = matches!(
        *phase.read(),
        ImportPhase::FetchingSource
            | ImportPhase::Matching { .. }
            | ImportPhase::Creating
            | ImportPhase::Adding { .. }
    );

    let mut run_import = move |playlist: spotify::SpotifyPlaylist| {
        let Some(cookies) = yt_cookies.peek().clone() else {
            phase.set(ImportPhase::Failed(i18n::t("spotify_needs_yt_login")));
            return;
        };
        spawn(async move {
            let total = playlist.tracks.len();
            phase.set(ImportPhase::Matching { done: 0, total, current: String::new() });
            let result = spotify::matcher::import_playlist(cookies, &playlist, |ev| match ev {
                CloneEvent::Matching { done, total, current } => {
                    phase.set(ImportPhase::Matching { done, total, current });
                }
                CloneEvent::CreatingPlaylist => phase.set(ImportPhase::Creating),
                CloneEvent::Adding { done, total } => {
                    phase.set(ImportPhase::Adding { done, total });
                }
            })
            .await;
            match result {
                Ok(report) => {
                    phase.set(ImportPhase::Done {
                        matched: report.matched,
                        total: report.total,
                        unmatched: report
                            .unmatched
                            .iter()
                            .map(|t| format!("{} — {}", t.title, t.artists.join(", ")))
                            .collect(),
                        name: report.playlist_name,
                    });
                    on_imported.call(());
                }
                Err(e) => phase.set(ImportPhase::Failed(e)),
            }
        });
    };

    let import_from_url = move |_| {
        let input = url_input.peek().clone();
        let Some((kind, id)) = spotify::parse_spotify_url(&input) else {
            phase.set(ImportPhase::Failed(i18n::t("spotify_bad_url")));
            return;
        };
        phase.set(ImportPhase::FetchingSource);
        spawn(async move {
            match spotify::embed::fetch_public(kind, &id).await {
                Ok(playlist) => run_import(playlist),
                Err(e) => phase.set(ImportPhase::Failed(e)),
            }
        });
    };

    // Returns a fresh access token, transparently refreshing through
    // the stored rotating refresh token.
    let ensure_token = move || async move {
        let now = web_time::SystemTime::now()
            .duration_since(web_time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if let Some((tok, exp)) = access_token.peek().clone()
            && exp > now + 30
        {
            return Ok::<String, String>(tok);
        }
        let (client_id, refresh) = {
            let c = config.peek();
            (c.spotify_client_id.clone(), c.spotify_refresh_token.clone())
        };
        if client_id.is_empty() || refresh.is_empty() {
            return Err(i18n::t("spotify_not_connected"));
        }
        let tokens = spotify::auth::refresh_token(&client_id, &refresh).await?;
        let mut access = access_token;
        access.set(Some((tokens.access_token.clone(), now + tokens.expires_in)));
        if let Some(new_refresh) = tokens.refresh_token
            && !new_refresh.is_empty()
        {
            config.write().spotify_refresh_token = new_refresh;
        }
        Ok(tokens.access_token)
    };

    let connect_spotify = move |_| {
        let client_id = client_id_input.peek().trim().to_string();
        if client_id.is_empty() {
            account_error.set(Some(i18n::t("spotify_client_id_missing")));
            return;
        }
        connecting.set(true);
        account_error.set(None);
        spawn(async move {
            let result = async {
                let pending = spotify::auth::begin_pkce(&client_id)?;
                webbrowser::open(&pending.authorize_url)
                    .map_err(|e| format!("Browser: {e}"))?;
                spotify::auth::finish_pkce(pending).await
            }
            .await;
            connecting.set(false);
            match result {
                Ok(tokens) => {
                    let now = web_time::SystemTime::now()
                        .duration_since(web_time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let mut access = access_token;
                    access.set(Some((tokens.access_token.clone(), now + tokens.expires_in)));
                    {
                        let mut c = config.write();
                        c.spotify_client_id = client_id;
                        if let Some(r) = tokens.refresh_token {
                            c.spotify_refresh_token = r;
                        }
                    }
                }
                Err(e) => account_error.set(Some(e)),
            }
        });
    };

    let load_account_playlists = move |_| {
        account_error.set(None);
        spawn(async move {
            match ensure_token().await {
                Ok(token) => match spotify::api::list_user_playlists(&token).await {
                    Ok(list) => account_playlists.set(Some(list)),
                    Err(e) => account_error.set(Some(e)),
                },
                Err(e) => account_error.set(Some(e)),
            }
        });
    };

    let mut import_account_playlist = move |id: Option<String>| {
        phase.set(ImportPhase::FetchingSource);
        spawn(async move {
            let result = async {
                let token = ensure_token().await?;
                match &id {
                    Some(pid) => spotify::api::fetch_playlist(&token, pid).await,
                    None => spotify::api::fetch_liked_songs(&token).await,
                }
            }
            .await;
            match result {
                Ok(playlist) => run_import(playlist),
                Err(e) => phase.set(ImportPhase::Failed(e)),
            }
        });
    };

    let is_connected = !config.read().spotify_refresh_token.is_empty();

    rsx! {
        div {
            class: "fixed inset-0 bg-black/80 flex items-center justify-center z-50 p-4",
            onclick: move |_| if !busy { on_close.call(()) },
            div {
                class: "bg-neutral-900 rounded-xl border border-white/10 w-full max-w-lg p-6 max-h-[85vh] overflow-y-auto",
                onclick: move |e| e.stop_propagation(),

                div { class: "flex items-center justify-between mb-4",
                    h2 { class: "text-xl font-bold text-white",
                        i { class: "fa-brands fa-spotify text-emerald-400 mr-2" }
                        "{i18n::t(\"spotify_import_title\")}"
                    }
                    button {
                        class: "text-white/50 hover:text-white transition-colors",
                        disabled: busy,
                        onclick: move |_| on_close.call(()),
                        i { class: "fa-solid fa-xmark text-lg" }
                    }
                }

                if yt_cookies.read().is_none() {
                    div { class: "rounded-lg bg-amber-500/10 border border-amber-400/30 text-amber-200 text-sm p-3 mb-4",
                        "{i18n::t(\"spotify_needs_yt_login\")}"
                    }
                }

                match phase.read().clone() {
                    ImportPhase::FetchingSource => rsx! {
                        ProgressBlock { icon: "fa-solid fa-cloud-arrow-down", label: i18n::t("spotify_fetching"), detail: String::new(), pct: None }
                    },
                    ImportPhase::Matching { done, total, current } => rsx! {
                        ProgressBlock {
                            icon: "fa-solid fa-magnifying-glass",
                            label: i18n::t_with("spotify_matching", &[("done", done.to_string()), ("total", total.to_string())]),
                            detail: current,
                            pct: Some(if total == 0 { 0.0 } else { done as f64 / total as f64 }),
                        }
                    },
                    ImportPhase::Creating => rsx! {
                        ProgressBlock { icon: "fa-solid fa-list", label: i18n::t("spotify_creating"), detail: String::new(), pct: None }
                    },
                    ImportPhase::Adding { done, total } => rsx! {
                        ProgressBlock {
                            icon: "fa-solid fa-plus",
                            label: i18n::t_with("spotify_adding", &[("done", done.to_string()), ("total", total.to_string())]),
                            detail: String::new(),
                            pct: Some(if total == 0 { 1.0 } else { done as f64 / total as f64 }),
                        }
                    },
                    ImportPhase::Done { matched, total, unmatched, name } => rsx! {
                        div { class: "text-center py-4",
                            i { class: "fa-solid fa-circle-check text-emerald-400 text-4xl mb-3" }
                            p { class: "text-white font-semibold mb-1", "{name}" }
                            p { class: "text-slate-300 text-sm mb-3",
                                {i18n::t_with("spotify_done", &[("matched", matched.to_string()), ("total", total.to_string())])}
                            }
                            if !unmatched.is_empty() {
                                details { class: "text-left text-xs text-slate-400 bg-white/5 rounded-lg p-3 mb-3",
                                    summary { class: "cursor-pointer text-slate-300",
                                        {i18n::t_with("spotify_unmatched", &[("count", unmatched.len().to_string())])}
                                    }
                                    ul { class: "mt-2 space-y-1 list-disc list-inside",
                                        for t in unmatched.iter() {
                                            li { "{t}" }
                                        }
                                    }
                                }
                            }
                            button {
                                class: "px-4 py-2 rounded-lg bg-indigo-500 hover:bg-indigo-400 text-white text-sm font-semibold transition-colors",
                                onclick: move |_| on_close.call(()),
                                "{i18n::t(\"close\")}"
                            }
                        }
                    },
                    ImportPhase::Failed(err) => rsx! {
                        div { class: "py-2",
                            div { class: "rounded-lg bg-rose-500/10 border border-rose-400/30 text-rose-200 text-sm p-3 mb-3 break-words",
                                "{err}"
                            }
                            button {
                                class: "px-4 py-2 rounded-lg bg-white/10 hover:bg-white/20 text-white text-sm transition-colors",
                                onclick: move |_| phase.set(ImportPhase::Idle),
                                "{i18n::t(\"back\")}"
                            }
                        }
                    },
                    ImportPhase::Idle => rsx! {
                        div { class: "flex gap-1 mb-4 bg-white/5 rounded-lg p-1",
                            for (t, label_key) in [(SourceTab::Url, "spotify_tab_url"), (SourceTab::Account, "spotify_tab_account")] {
                                button {
                                    class: if *tab.read() == t {
                                        "flex-1 px-3 py-1.5 rounded-md bg-white/10 text-white text-sm font-semibold"
                                    } else {
                                        "flex-1 px-3 py-1.5 rounded-md text-white/50 hover:text-white text-sm transition-colors"
                                    },
                                    onclick: move |_| tab.set(t),
                                    {i18n::t(label_key)}
                                }
                            }
                        }

                        if *tab.read() == SourceTab::Url {
                            p { class: "text-slate-400 text-xs mb-2", "{i18n::t(\"spotify_url_hint\")}" }
                            input {
                                class: "w-full bg-white/5 border border-white/10 rounded-lg px-3 py-2 text-white text-sm placeholder:text-white/30 focus:outline-none focus:border-indigo-400 mb-3",
                                placeholder: "https://open.spotify.com/playlist/…",
                                value: "{url_input}",
                                oninput: move |e| url_input.set(e.value()),
                            }
                            button {
                                class: "w-full px-4 py-2 rounded-lg bg-emerald-600 hover:bg-emerald-500 text-white text-sm font-semibold transition-colors disabled:opacity-50",
                                disabled: url_input.read().trim().is_empty() || yt_cookies.read().is_none(),
                                onclick: import_from_url,
                                i { class: "fa-solid fa-cloud-arrow-down mr-2" }
                                "{i18n::t(\"spotify_import_button\")}"
                            }
                        } else {
                            if !is_connected {
                                p { class: "text-slate-400 text-xs mb-2", "{i18n::t(\"spotify_connect_hint\")}" }
                                ol { class: "text-slate-400 text-xs mb-3 list-decimal list-inside space-y-1",
                                    li { "developer.spotify.com → Dashboard → Create app" }
                                    li {
                                        "Redirect URI: "
                                        code { class: "bg-white/10 px-1 rounded text-emerald-300", "http://127.0.0.1:8898/callback" }
                                    }
                                    li { "{i18n::t(\"spotify_connect_step3\")}" }
                                }
                                input {
                                    class: "w-full bg-white/5 border border-white/10 rounded-lg px-3 py-2 text-white text-sm placeholder:text-white/30 focus:outline-none focus:border-indigo-400 mb-3",
                                    placeholder: "Client ID",
                                    value: "{client_id_input}",
                                    oninput: move |e| client_id_input.set(e.value()),
                                }
                                button {
                                    class: "w-full px-4 py-2 rounded-lg bg-emerald-600 hover:bg-emerald-500 text-white text-sm font-semibold transition-colors disabled:opacity-50",
                                    disabled: *connecting.read() || client_id_input.read().trim().is_empty(),
                                    onclick: connect_spotify,
                                    if *connecting.read() {
                                        i { class: "fa-solid fa-arrows-rotate fa-spin mr-2" }
                                        "{i18n::t(\"spotify_waiting_browser\")}"
                                    } else {
                                        i { class: "fa-brands fa-spotify mr-2" }
                                        "{i18n::t(\"spotify_connect_button\")}"
                                    }
                                }
                            } else {
                                div { class: "flex items-center justify-between mb-3",
                                    span { class: "text-emerald-300 text-xs",
                                        i { class: "fa-solid fa-link mr-1" }
                                        "{i18n::t(\"spotify_connected\")}"
                                    }
                                    button {
                                        class: "text-xs text-white/40 hover:text-rose-300 transition-colors",
                                        onclick: move |_| {
                                            let mut c = config.write();
                                            c.spotify_refresh_token = String::new();
                                            account_playlists.set(None);
                                        },
                                        "{i18n::t(\"spotify_disconnect\")}"
                                    }
                                }
                                if account_playlists.read().is_none() {
                                    button {
                                        class: "w-full px-4 py-2 rounded-lg bg-white/10 hover:bg-white/20 text-white text-sm font-semibold transition-colors",
                                        onclick: load_account_playlists,
                                        i { class: "fa-solid fa-list mr-2" }
                                        "{i18n::t(\"spotify_load_playlists\")}"
                                    }
                                } else {
                                    div { class: "space-y-1 max-h-72 overflow-y-auto",
                                        button {
                                            class: "w-full flex items-center justify-between px-3 py-2 rounded-lg bg-white/5 hover:bg-white/10 text-left transition-colors disabled:opacity-50",
                                            disabled: yt_cookies.read().is_none(),
                                            onclick: move |_| import_account_playlist(None),
                                            span { class: "text-white text-sm",
                                                i { class: "fa-solid fa-heart text-rose-400 mr-2" }
                                                "{i18n::t(\"spotify_liked_songs\")}"
                                            }
                                            i { class: "fa-solid fa-cloud-arrow-down text-white/40" }
                                        }
                                        for pl in account_playlists.read().clone().unwrap_or_default() {
                                            button {
                                                key: "{pl.id}",
                                                class: "w-full flex items-center justify-between px-3 py-2 rounded-lg bg-white/5 hover:bg-white/10 text-left transition-colors disabled:opacity-50",
                                                disabled: yt_cookies.read().is_none(),
                                                onclick: {
                                                    let id = pl.id.clone();
                                                    move |_| import_account_playlist(Some(id.clone()))
                                                },
                                                div { class: "min-w-0",
                                                    p { class: "text-white text-sm truncate", "{pl.name}" }
                                                    p { class: "text-white/40 text-xs",
                                                        {i18n::t_with("track_count", &[("count", pl.track_count.to_string())])}
                                                    }
                                                }
                                                i { class: "fa-solid fa-cloud-arrow-down text-white/40 shrink-0 ml-2" }
                                            }
                                        }
                                    }
                                }
                            }
                            if let Some(err) = account_error.read().clone() {
                                div { class: "rounded-lg bg-rose-500/10 border border-rose-400/30 text-rose-200 text-xs p-2 mt-3 break-words",
                                    "{err}"
                                }
                            }
                        }
                    },
                }
            }
        }
    }
}

#[component]
fn ProgressBlock(icon: String, label: String, detail: String, pct: Option<f64>) -> Element {
    rsx! {
        div { class: "py-6 text-center",
            i { class: "{icon} text-indigo-300 text-2xl mb-3 fa-fade" }
            p { class: "text-white text-sm font-semibold mb-1", "{label}" }
            if !detail.is_empty() {
                p { class: "text-white/40 text-xs truncate mb-2", "{detail}" }
            }
            if let Some(p) = pct {
                div { class: "w-full bg-white/10 rounded-full h-1.5 mt-3 overflow-hidden",
                    div {
                        class: "bg-indigo-400 h-full rounded-full transition-all",
                        style: "width: {(p * 100.0).clamp(0.0, 100.0)}%",
                    }
                }
            }
        }
    }
}
