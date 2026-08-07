use config::{AppConfig, MusicSource};
use dioxus::prelude::*;
use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

/// Which platform the unified search bar queries. `Server` follows the
/// configured media server (YouTube Music / Jellyfin / Subsonic); `Local`
/// the on-disk library; `SoundCloud` the native SoundCloud search.
///
/// Lives in `config` because it is persisted: re-exported here so the rest of
/// the UI keeps referring to it as `search_bar::SearchSource`.
pub use config::SearchSource;

/// App-wide selected search source, driven by the dropdown in [`SearchBar`]
/// and read by the search page to decide which results view to render. The
/// authoritative value is `config.search_source`; this signal mirrors it so
/// components can react without every one of them writing config.
#[derive(Clone, Copy)]
pub struct SearchSourceState(pub Signal<SearchSource>);

#[component]
pub fn SearchBar(search_query: Signal<String>) -> Element {
    let debounce_gen = use_hook(|| Arc::new(AtomicU64::new(0))).clone();
    let source_state = try_consume_context::<SearchSourceState>();
    let config = try_consume_context::<Signal<AppConfig>>();

    rsx! {
        div {
            class: "flex items-center gap-2 max-w-2xl mb-8",
            div {
                class: "relative flex-1",
                i { class: "fa-solid fa-magnifying-glass absolute left-4 top-1/2 -translate-y-1/2 text-slate-500" }
                input {
                    r#type: "text",
                    placeholder: "{i18n::t(\"search_placeholder\")}",
                    class: "w-full bg-white/5 border border-white/10 rounded-full py-3 pl-12 pr-4 text-white focus:outline-none focus:border-white/20 transition-colors",
                    oninput: move |evt| {
                        let value = evt.value();
                        let tick = debounce_gen.fetch_add(1, Ordering::Relaxed) + 1;
                        let debounce_gen = debounce_gen.clone();
                        spawn(async move {
                            tokio::time::sleep(Duration::from_millis(150)).await;
                            if debounce_gen.load(Ordering::Relaxed) == tick {
                                search_query.set(value);
                            }
                        });
                    },
                    onkeydown: move |e| e.stop_propagation()
                }
            }
            // Source dropdown — only when the context is wired (the search
            // page) and the config context is available for labels/availability.
            if let (Some(state), Some(config)) = (source_state, config) {
                SourceDropdown { source: state.0, config }
            }
        }
    }
}

#[component]
fn SourceDropdown(source: Signal<SearchSource>, config: Signal<AppConfig>) -> Element {
    let server_label = config
        .read()
        .server
        .as_ref()
        .map(|s| s.service.display_name().to_string());
    let soundcloud_ok = server::soundcloud::is_available();
    let current = *source.read();

    let value_str = match current {
        SearchSource::Local => "local",
        SearchSource::Server => "server",
        SearchSource::SoundCloud => "soundcloud",
    };

    rsx! {
        div {
            class: "relative shrink-0",
            i { class: "fa-solid fa-chevron-down absolute right-3 top-1/2 -translate-y-1/2 text-slate-500 text-xs pointer-events-none" },
            select {
                class: "appearance-none bg-white/5 border border-white/10 rounded-full py-3 pl-4 pr-9 text-white text-sm focus:outline-none focus:border-white/20 transition-colors cursor-pointer",
                value: "{value_str}",
                onchange: move |e| {
                    let picked = match e.value().as_str() {
                        "local" => SearchSource::Local,
                        "server" => SearchSource::Server,
                        "soundcloud" => SearchSource::SoundCloud,
                        _ => return,
                    };
                    source.set(picked);
                    // ONE write, not three. Each `config.write()` marks the
                    // signal dirty and re-runs every subscriber mid-change, so
                    // splitting this left observers seeing a half-applied state
                    // (a new active_source with source_explicitly_set still
                    // false), which is exactly what let the choice snap back.
                    let mut cfg = config;
                    let mut c = cfg.write();
                    c.search_source = picked;
                    match picked {
                        // Local/Server also drive the app-wide backend so the
                        // sidebar toggle and this dropdown can't disagree.
                        SearchSource::Local => {
                            c.active_source = MusicSource::Local;
                            c.source_explicitly_set = true;
                        }
                        SearchSource::Server => {
                            c.active_source = MusicSource::Server;
                            c.source_explicitly_set = true;
                        }
                        // Overlay sources render their own view and leave the
                        // configured backend alone.
                        SearchSource::SoundCloud => {}
                    }
                },
                onkeydown: move |e| e.stop_propagation(),
                // `selected` makes the <select> properly controlled — without it
                // the webview drops back to the first option (Local) on re-render.
                option { value: "local", selected: current == SearchSource::Local, "{i18n::t(\"local\")}" }
                if let Some(label) = server_label {
                    option { value: "server", selected: current == SearchSource::Server, "{label}" }
                }
                if soundcloud_ok {
                    option { value: "soundcloud", selected: current == SearchSource::SoundCloud, "SoundCloud" }
                }
            }
        }
    }
}
