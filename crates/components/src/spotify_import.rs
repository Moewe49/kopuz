//! "Import from Spotify": paste a public playlist or album link.
//!
//! The tracks are fetched anonymously through the `open.spotify.com/embed`
//! page — which hands out a web-player token — and then read in full via the
//! web player's pathfinder GraphQL endpoint. No login, no API key, no track
//! cap. They are then matched on YT Music and recreated as a real playlist in
//! the signed-in YT Music account, so the result shows up in the library:
//! playable and editable like any other playlist.
//!
//! There used to be a second path that connected a Spotify account over OAuth,
//! to reach private playlists and Liked Songs. It is gone. Spotify's February
//! 2026 Web API migration made it a dead end for an app like this: Development
//! Mode now requires the app owner to hold Premium, caps a new app at five
//! users, returns playlist contents *only* for playlists the user owns or
//! collaborates on — a bare `403 Forbidden` for everything else — and lists
//! barely any of them to begin with. The anonymous path has none of those
//! limits and reads any public playlist completely, so it is the only one
//! left.

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

#[component]
pub fn SpotifyImportModal(
    config: Signal<config::AppConfig>,
    on_close: EventHandler,
    /// Fired after a successful clone so the playlists page can
    /// refresh its YT list.
    on_imported: EventHandler,
) -> Element {
    let mut url_input = use_signal(String::new);
    let mut phase = use_signal(|| ImportPhase::Idle);

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
            // Anonymous fetch through the embed token. For playlists this goes
            // via the web player's pathfinder GraphQL endpoint, which reads ANY
            // public playlist in full — no login, no owning/following, no
            // ~100-track cap. Albums page through the REST API, then the
            // inlined list as a fallback.
            match spotify::embed::fetch_public(kind, &id).await {
                Ok(playlist) => run_import(playlist),
                Err(e) => phase.set(ImportPhase::Failed(e)),
            }
        });
    };

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
