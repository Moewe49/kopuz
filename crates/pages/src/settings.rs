#[cfg(not(target_os = "android"))]
use crate::theme_editor::ThemeEditorPage;
use ::server::provider::ProviderClient;

#[cfg(not(target_os = "android"))]
fn theme_editor_section(config: Signal<AppConfig>) -> Element {
    rsx! {
        section {
            h2 {
                class: "text-lg font-semibold text-white/80 mb-4 border-b border-white/5 pb-2",
                "{i18n::t(\"theme_editor\")}"
            }
            ThemeEditorPage { config, embedded: true }
        }
    }
}

#[cfg(target_os = "android")]
fn theme_editor_section(_config: Signal<AppConfig>) -> Element {
    rsx! {}
}
use components::settings_items::{
    BackBehaviorSelector, ChannelModeSelector, DiscordPresencePausedSettings,
    DiscordPresenceSettings, EqualizerPanel, LanguageSelector, LastFmSettings,
    MultiDirectoryPicker, MusicBrainzSettings, RadioRegistryDropdown, ServerSettings, SettingItem,
    ThemeSelector, ToggleSetting,
};
use components::settings_popups::{AddRegistryPopup, AddServerPopup, LoginPopup, YtAuthMethod};
use config::{AppConfig, ArtistPhotoSource, Browser, FetchStrategy, MusicService, OfflineQuality};
use dioxus::prelude::*;
use hooks::use_player_controller::PlayerController;

async fn validate(cookies: &str) -> bool {
    ::server::ytmusic::YouTubeMusicClient::with_cookies(cookies.to_string())
        .validate_cookies()
        .await
        .is_ok()
}

async fn try_resume(seed: Option<String>) -> Option<String> {
    if let Some(c) = &seed
        && validate(c).await
    {
        return seed;
    }
    if let Some(c) = &seed
        && let Ok(Some(rotated)) =
            ::server::ytmusic::verify_session_keepalive::tick(c).await
        && validate(&rotated).await
    {
        return Some(rotated);
    }
    None
}

/// Sentinel `Err` from [`ensure_signed_in`] meaning "no valid session and no
/// way to refresh silently — the caller must run the interactive login window".
const NEEDS_LOGIN: &str = "__yt_needs_login__";

/// Adopt a freshly obtained YT cookie header as the active + saved session.
/// Shared by the silent-refresh path and the interactive-login "Done" handler.
fn persist_yt_session(
    mut config: Signal<AppConfig>,
    mut error: Signal<Option<String>>,
    cookies: String,
    browser: Browser,
    manual: bool,
) {
    let yt_user_id =
        ::server::ytmusic::derive_user_id(&cookies).unwrap_or_else(|| "me".to_string());
    {
        let mut cfg = config.write();
        let saved_id = cfg.server.as_ref().and_then(|s| s.id.clone());
        if let Some(srv) = cfg.server.as_mut() {
            srv.access_token = Some(cookies.clone());
            srv.user_id = Some(yt_user_id);
            if !manual {
                srv.yt_browser = Some(browser);
            }
        }
        if let Some(id) = saved_id
            && let Some(saved) = cfg.servers.iter_mut().find(|s| s.id == id)
        {
            if manual {
                saved.yt_saved_cookies = Some(cookies);
            } else {
                saved.yt_browser = Some(browser);
            }
        }
    }
    error.set(None);
}

async fn ensure_signed_in(
    config_cookies: Option<String>,
    browser: Browser,
    server_id: &str,
    manual: bool,
) -> Result<String, String> {
    if let Some(c) = try_resume(config_cookies).await {
        return Ok(c);
    }

    // Manual-cookie sessions have no browser to fall back to — when
    // the pasted session dies, the only fix is fresh cookies.
    if manual {
        return Err(i18n::t("yt_manual_session_expired"));
    }

    let profile = ::server::ytmusic::isolated_profile::profile_dir(server_id);

    // Silent refresh: drive the persistent managed profile HEADLESS via CDP.
    // If we're still signed in there, this rolls the rotating cookies forward
    // and returns fresh ones with no visible window — the everyday path.
    if profile.is_dir()
        && let Ok(c) = ::server::ytmusic::cdp::fetch_cookies(
            browser,
            &profile,
            true,
            std::time::Duration::from_secs(25),
        )
        .await
        && validate(&c).await
    {
        return Ok(c);
    }

    // Not signed in (first run, or logged out). The caller has to run the
    // interactive login: a NORMAL browser window (driving it over CDP here
    // makes Google reject the sign-in as an "insecure browser"), then a
    // headless CDP extract once the user confirms they're done.
    Err(NEEDS_LOGIN.to_string())
}

#[component]
pub fn Settings(config: Signal<AppConfig>) -> Element {
    let mut ctrl = use_context::<PlayerController>();
    let crossfade_label = if config.read().crossfade_seconds == 0 {
        i18n::t("crossfade_off")
    } else {
        format!("{}s", config.read().crossfade_seconds)
    };
    let mut show_add_server = use_signal(|| false);
    let mut show_login = use_signal(|| false);

    let mut server_name = use_signal(|| String::new());
    let mut server_url = use_signal(|| String::new());
    let mut server_service = use_signal(|| MusicService::Jellyfin);
    let yt_browser = use_signal(|| {
        config
            .peek()
            .server
            .as_ref()
            .and_then(|s| s.yt_browser)
            .unwrap_or(config::Browser::Chrome)
    });
    // YT auth method for the add-server popup. Windows opens on the
    // cookie-paste flow (its only signed-in path); elsewhere the
    // one-click browser sign-in stays the default.
    let yt_auth = use_signal(YtAuthMethod::default_for_platform);
    let mut yt_pasted_cookies = use_signal(String::new);

    let mut username = use_signal(|| String::new());
    let mut password = use_signal(|| String::new());

    let mut error = use_signal(|| Option::<String>::None);
    let mut login_error = use_signal(|| Option::<String>::None);
    let mut is_loading = use_signal(|| false);

    let mut show_add_registry = use_signal(|| false);
    let mut registry_url = use_signal(|| String::new());
    let mut registry_error = use_signal(|| Option::<String>::None);
    let mut registry_loading = use_signal(|| false);
    let mut registry_toggle_error = use_signal(|| Option::<String>::None);

    let handle_add_registry = move |_| {
        let url = registry_url().trim().to_string();
        if url.is_empty() {
            registry_error.set(Some(i18n::t("radio_registry_empty_path").to_string()));
            return;
        }

        if config.read().radio_registries.iter().any(|r| r.url == url) {
            registry_error.set(Some(i18n::t("radio_registry_exists").to_string()));
            return;
        }

        registry_loading.set(true);
        registry_error.set(None);

        spawn(async move {
            let mut temp_registry = radio::registry::StationRegistry::new();
            match temp_registry.import_registry(&url).await {
                Ok(_) => {
                    let mut current_config = config.write();
                    if !current_config.radio_registries.iter().any(|r| r.url == url) {
                        current_config.radio_registries.push(config::RegistryEntry {
                            url,
                            enabled: true,
                            is_default: false,
                        });
                    }
                    registry_url.set(String::new());
                    registry_error.set(None);
                    show_add_registry.set(false);
                }
                Err(e) => {
                    registry_error.set(Some(i18n::t_with(
                        "radio_registry_import_failed",
                        &[("error", e.to_string())],
                    )));
                }
            }
            registry_loading.set(false);
        });
    };

    // Interactive-login modal state (managed-browser auto-login). When the
    // silent refresh can't get a session, we open a normal browser window for
    // the user to sign in, then wait for them to click "Done".
    let mut yt_login_open = use_signal(|| false);
    let mut yt_login_busy = use_signal(|| false);
    let mut yt_login_pid = use_signal(|| None::<u32>);
    let mut yt_login_ctx = use_signal(|| None::<(Browser, String)>);

    let ytmusic_auto_login = move || {
        // Prefer the browser already saved on the active server entry
        // (set during a previous successful sign-in); fall back to the
        // settings popup's selector for first-time setup.
        let (browser, existing, server_id, manual) = {
            let cfg = config.peek();
            let srv = cfg.server.as_ref();
            (
                srv.and_then(|s| s.yt_browser).unwrap_or(*yt_browser.peek()),
                srv.and_then(|s| s.access_token.clone()).filter(|t| !t.is_empty()),
                srv.and_then(|s| s.id.clone()).unwrap_or_default(),
                srv.map(|s| s.yt_manual).unwrap_or(false),
            )
        };
        let mut report = move |msg: String| {
            error.set(Some(msg.clone()));
            ctrl.playback_error.set(Some(msg));
        };
        spawn(async move {
            match ensure_signed_in(existing, browser, &server_id, manual).await {
                Ok(cookies) => persist_yt_session(config, error, cookies, browser, manual),
                Err(e) if e == NEEDS_LOGIN => {
                    // Open a normal (non-automated) browser window for the
                    // one-time sign-in, then pop the "Done" modal. The headless
                    // CDP extract happens when the user confirms.
                    let profile = ::server::ytmusic::isolated_profile::profile_dir(&server_id);
                    match ::server::ytmusic::cdp::spawn_login_window(browser, &profile).await {
                        Ok(pid) => {
                            yt_login_pid.set(Some(pid));
                            yt_login_ctx.set(Some((browser, server_id.clone())));
                            yt_login_open.set(true);
                        }
                        Err(err) => report(format!("Could not open the sign-in browser: {err}")),
                    }
                }
                Err(e) if manual => report(e),
                Err(e) => report(format!("YT Music sign-in failed ({browser}): {e}")),
            }
        });
    };

    // "Done — I've signed in" handler: close the login window, then read the
    // now-signed-in cookies headlessly via CDP and adopt the session.
    let on_login_done = move |_| {
        let pid = yt_login_pid.write().take();
        let Some((browser, server_id)) = yt_login_ctx.peek().clone() else {
            return;
        };
        yt_login_busy.set(true);
        error.set(None);
        spawn(async move {
            if let Some(pid) = pid {
                ::server::ytmusic::cdp::kill_pid(pid).await;
            }
            // Give the browser a moment to release the profile lock.
            utils::sleep(std::time::Duration::from_secs(2)).await;
            let profile = ::server::ytmusic::isolated_profile::profile_dir(&server_id);
            match ::server::ytmusic::cdp::fetch_cookies(
                browser,
                &profile,
                true,
                std::time::Duration::from_secs(60),
            )
            .await
            {
                Ok(c) if validate(&c).await => {
                    persist_yt_session(config, error, c, browser, false);
                    yt_login_open.set(false);
                }
                Ok(_) => error.set(Some(i18n::t("yt_login_not_done").to_string())),
                Err(e) => error.set(Some(format!("Reading the session failed: {e}"))),
            }
            yt_login_busy.set(false);
        });
    };
    let on_login_cancel = move |_| {
        if let Some(pid) = yt_login_pid.write().take() {
            spawn(async move { ::server::ytmusic::cdp::kill_pid(pid).await });
        }
        yt_login_open.set(false);
        yt_login_busy.set(false);
    };

    let handle_add_server = move |_| {
        let selected_service = server_service();
        let is_ytmusic = selected_service == MusicService::YtMusic;

        if !is_ytmusic && !server_url().starts_with("http") {
            error.set(Some(i18n::t("invalid_server_url").to_string()));
            return;
        }

        // Snapshot the synchronous inputs so the async block doesn't have
        // to re-read signals (which it could, but this keeps the data
        // flow obvious).
        let name_input = server_name();
        let url_input = server_url();

        spawn(async move {
            let display_name = if name_input.is_empty() {
                format!("Local {}", selected_service.display_name())
            } else {
                name_input
            };

            let effective_url = if is_ytmusic {
                "https://music.youtube.com".to_string()
            } else {
                url_input
            };

            let method = *yt_auth.peek();
            let is_anon = is_ytmusic && method == YtAuthMethod::Anonymous;
            let is_paste = is_ytmusic && method == YtAuthMethod::PasteCookies;
            let is_oauth = is_ytmusic && method == YtAuthMethod::OAuth;

            // The paste flow validates BEFORE the popup closes so a bad
            // paste gets immediate inline feedback instead of a dead
            // server entry.
            let mut pasted_header = None;
            if is_paste {
                let raw = yt_pasted_cookies.peek().clone();
                let header = match ::server::ytmusic::manual_cookies::sanitize_header(&raw) {
                    Ok(h) => h,
                    Err(e) => {
                        error.set(Some(e));
                        return;
                    }
                };
                if !validate(&header).await {
                    error.set(Some(i18n::t("yt_paste_invalid")));
                    return;
                }
                pasted_header = Some(header);
            }
            // OAuth: the device flow already ran in the popup and stashed the
            // `oauth:<access>` sentinel in yt_pasted_cookies (+ the refresh
            // token in config). Require it to be present.
            if is_oauth {
                let sentinel = yt_pasted_cookies.peek().clone();
                if !sentinel.starts_with("oauth:") {
                    error.set(Some(i18n::t("yt_oauth_required")));
                    return;
                }
                pasted_header = Some(sentinel);
            }

            let mut new_server = config::MusicServer::new_with_service(
                display_name,
                effective_url,
                selected_service,
            );
            new_server.yt_anonymous = is_anon;
            new_server.yt_manual = is_paste || is_oauth;
            if is_anon {
                // Mark anonymous mode at the server level. Empty access
                // token + yt_anonymous=true is what get_stream /
                // discover etc. read as "no cookies, public surfaces
                // only".
                new_server.access_token = Some(String::new());
            }
            if let Some(header) = &pasted_header {
                new_server.access_token = Some(header.clone());
                new_server.user_id = Some(
                    ::server::ytmusic::derive_user_id(header).unwrap_or_else(|| "me".to_string()),
                );
            }
            // Persist the chosen browser on the active server too (not just the
            // saved-list entry), so the sign-in flow knows which browser to use.
            let uses_browser = is_ytmusic && method == YtAuthMethod::BrowserSignin;
            new_server.yt_browser = uses_browser.then(|| *yt_browser.peek());

            let saved = config::SavedServer {
                id: new_server.id.clone().unwrap_or_default(),
                name: new_server.name.clone(),
                url: new_server.url.clone(),
                service: new_server.service,
                yt_browser: uses_browser.then(|| *yt_browser.peek()),
                yt_anonymous: is_anon,
                yt_manual: is_paste || is_oauth,
                yt_saved_cookies: pasted_header,
            };
            {
                let mut cfg = config.write();
                cfg.add_saved_server(saved);
                cfg.server = Some(new_server);
            }

            server_name.set(String::new());
            server_url.set(String::new());
            server_service.set(MusicService::Jellyfin);
            yt_pasted_cookies.set(String::new());
            error.set(None);
            show_add_server.set(false);

            if uses_browser {
                ytmusic_auto_login();
            } else if !is_ytmusic {
                show_login.set(true);
            }
            // Anonymous + pasted-cookie YT need no further setup — the
            // server entry is already active and playable.
        });
    };

    let handle_switch_server = move |id: String| {
        let server = {
            let cfg = config.read();
            cfg.find_saved_server(&id).cloned()
        };
        if let Some(saved) = server {
            let is_ytmusic = saved.service == MusicService::YtMusic;
            let is_anon = is_ytmusic && saved.yt_anonymous;
            let manual_cookies = (is_ytmusic && saved.yt_manual)
                .then(|| saved.yt_saved_cookies.clone())
                .flatten();
            let manual_user_id = manual_cookies
                .as_deref()
                .and_then(::server::ytmusic::derive_user_id);
            let active = config::MusicServer {
                name: saved.name,
                url: saved.url,
                service: saved.service,
                // Anonymous YT keeps an empty (non-None) token so the
                // backend treats it as anon rather than "needs sign-in".
                // Manual-cookie servers restore their persisted session.
                access_token: if is_anon {
                    Some(String::new())
                } else {
                    manual_cookies
                },
                user_id: manual_user_id,
                id: Some(saved.id),
                // Carry the saved browser choice over so the sign-in
                // launch hits the binary the user picked, not whatever
                // the popup's default selector happens to be.
                yt_browser: saved.yt_browser,
                yt_anonymous: is_anon,
                yt_manual: saved.yt_manual,
            };
            config.write().server = Some(active);
            if is_ytmusic && !is_anon {
                // For manual servers this revalidates/rotates the
                // restored cookies; it never launches a browser.
                ytmusic_auto_login();
            } else if !is_ytmusic {
                show_login.set(true);
            }
            // Anonymous YT is immediately active — no sign-in launch.
        }
    };

    let handle_delete_saved = move |id: String| {
        let was_ytmusic = config
            .peek()
            .find_saved_server(&id)
            .map(|s| s.service == MusicService::YtMusic)
            .unwrap_or(false);
        config.write().remove_saved_server(&id);
        if was_ytmusic {
            let _ = ::server::ytmusic::isolated_profile::delete_profile(&id);
        }
    };

    let handle_login = move |_| {
        if username().is_empty() || password().is_empty() {
            login_error.set(Some(i18n::t("username_and_password_required").to_string()));
            return;
        }

        if let Some(server) = &config.read().server {
            let service = server.service;
            let server_url = server.url.clone();
            let device_id = config.read().device_id.clone();
            let user = username();
            let pass = password();

            is_loading.set(true);
            login_error.set(None);

            spawn(async move {
                let remote = ProviderClient::new(service, server_url, device_id);
                let result = remote.login(&user, &pass).await;

                is_loading.set(false);

                match result {
                    Ok(session) => {
                        if let Some(server) = config.write().server.as_mut() {
                            server.access_token = Some(session.access_token);
                            server.user_id = Some(session.user_id);
                        }
                        username.set(String::new());
                        password.set(String::new());
                        login_error.set(None);
                        show_login.set(false);
                    }
                    Err(e) => {
                        login_error.set(Some(i18n::t_with(
                            "login_failed",
                            &[("error", e.to_string())],
                        )));
                    }
                }
            });
        }
    };

    rsx! {
        div { class: if cfg!(target_os = "android") { "px-4 pt-2 pb-28 w-full" } else { "p-8 w-full" },
            if !cfg!(target_os = "android") {
                h1 { class: "text-3xl font-bold text-white mb-6", "{i18n::t(\"settings\")}" }
            }

            div { class: "space-y-8",
                section {
                    h2 {
                        class: "text-lg font-semibold text-white/80 mb-4 border-b border-white/5 pb-2",
                        "{i18n::t(\"general\")}"
                    }

                    div { class: "space-y-4",
                        SettingItem {
                            title: i18n::t("language").to_string(),
                            control: rsx! {
                                LanguageSelector {
                                    current_language: config.read().language.clone(),
                                    on_change: move |lang: String| {
                                        config.write().language = lang.clone();
                                        i18n::set_locale(&lang);
                                    }
                                }
                            }
                        }

                        SettingItem {
                            title: i18n::t("appearance").to_string(),
                            control: rsx! {
                                ThemeSelector {
                                    current_theme: config.read().theme.clone(),
                                    on_change: move |theme| {
                                        config.write().theme = theme;
                                    }
                                }
                            }
                        }

                        if !cfg!(target_arch = "wasm32") {
                            SettingItem {
                                title: i18n::t("music_directory").to_string(),
                                    control: rsx! {
                                    MultiDirectoryPicker {
                                        current_paths: config.read().music_directory.clone(),
                                        on_add: move |path| {
                                            let mut config = config.write();
                                            if !config.music_directory.contains(&path) {
                                                config.music_directory.push(path);
                                            }
                                        },
                                        on_remove: move |index| {
                                            let mut config = config.write();
                                            if index < config.music_directory.len() {
                                                config.music_directory.remove(index);
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        RadioRegistryDropdown {
                            registries: config.read().radio_registries.clone(),
                            error: registry_toggle_error,
                            on_toggle: move |index: usize| {
                                let (is_enabling, url) = {
                                    let cfg = config.read();
                                    let entry = cfg.radio_registries.get(index);
                                    (
                                        entry.map(|e| !e.enabled).unwrap_or(false),
                                        entry.map(|e| e.url.clone()).unwrap_or_default(),
                                    )
                                };

                                if is_enabling && !url.is_empty() {
                                    registry_toggle_error.set(None);
                                    spawn(async move {
                                        let mut temp_registry = radio::registry::StationRegistry::new();
                                        match temp_registry.import_registry(&url).await {
                                            Ok(_) => {
                                                let mut cfg = config.write();
                                                if let Some(entry) = cfg
                                                .radio_registries
                                                .iter_mut()
                                                .find(|entry| entry.url == url)
                                                {
                                                    entry.enabled = true;
                                                }
                                                registry_toggle_error.set(None);
                                            }
                                            Err(e) => {
                                                registry_toggle_error.set(Some(i18n::t_with("radio_registry_enable_failed", &[("error", e.to_string())])));
                                            }
                                        }
                                    });
                                } else {
                                    let mut cfg = config.write();
                                    if let Some(entry) = cfg.radio_registries.get_mut(index) {
                                        entry.enabled = false;
                                    }
                                    registry_toggle_error.set(None);
                                }
                            },
                            on_add: move |_| show_add_registry.set(true),
                            on_delete: move |index: usize| {
                                let mut cfg = config.write();
                                if index < cfg.radio_registries.len()
                                    && !cfg.radio_registries[index].is_default
                                {
                                    cfg.radio_registries.remove(index);
                                }
                            }
                        }

                        SettingItem {
                            title: i18n::t("media_servers").to_string(),
                            control: rsx! {
                                ServerSettings {
                                    active: config.read().server.clone(),
                                    servers: config.read().servers.clone(),
                                    on_add: move |_| show_add_server.set(true),
                                    on_delete: handle_delete_saved,
                                    on_switch: handle_switch_server,
                                    on_login: move |_| {
                                        let is_ytmusic = config
                                            .read()
                                            .server
                                            .as_ref()
                                            .map(|s| s.service == MusicService::YtMusic)
                                            .unwrap_or(false);
                                        if is_ytmusic {
                                            ytmusic_auto_login();
                                        } else {
                                            show_login.set(true);
                                        }
                                    },
                                }
                            }
                        }
                        SettingItem {
                            title: i18n::t("reduce_animations").to_string(),
                            control: rsx! {
                                ToggleSetting {
                                    enabled: config.read().reduce_animations,
                                    on_change: move |val| config.write().reduce_animations = val,
                                }
                            }
                        }
                        if !cfg!(target_arch = "wasm32") {
                            SettingItem {
                                title: i18n::t("auto_check_updates").to_string(),
                                control: rsx! {
                                    ToggleSetting {
                                        enabled: config.read().auto_check_updates,
                                        on_change: move |val| config.write().auto_check_updates = val,
                                    }
                                }
                            }
                        }
                        if !cfg!(target_arch = "wasm32") {
                            SettingItem {
                                title: i18n::t("show_source_toggle").to_string(),
                                    control: rsx! {
                                    ToggleSetting {
                                        enabled: config.read().show_source_toggle,
                                        on_change: move |val| config.write().show_source_toggle = val,
                                    }
                                }
                            }
                        }
                        if cfg!(any(target_os = "linux", target_os = "windows")) {
                            SettingItem {
                                title: i18n::t("titlebar_mode").to_string(),
                                control: rsx! {
                                    {
                                        let current_mode = config.read().titlebar_mode;
                                        rsx! {
                                            select {
                                                class: "bg-stone-800 text-white rounded-lg px-3 py-2 text-sm border border-white/10 focus:outline-none focus:border-indigo-500",
                                                onchange: move |evt| {
                                                    config.write().titlebar_mode = match evt.value().as_str() {
                                                        "system" => config::TitlebarMode::System,
                                                        "off" => config::TitlebarMode::Off,
                                                        _ => config::TitlebarMode::Custom,
                                                    };
                                                },
                                                option {
                                                    value: "custom",
                                                    selected: current_mode == config::TitlebarMode::Custom,
                                                    "{i18n::t(\"titlebar_custom\")}"
                                                }
                                                option {
                                                    value: "system",
                                                    selected: current_mode == config::TitlebarMode::System,
                                                    "{i18n::t(\"titlebar_system\")}"
                                                }
                                                option {
                                                    value: "off",
                                                    selected: current_mode == config::TitlebarMode::Off,
                                                    "{i18n::t(\"titlebar_off\")}"
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        SettingItem {
                            title: i18n::t("ui_style").to_string(),
                            control: rsx! {
                                {
                                    let current_style = config.read().ui_style;
                                    rsx! {
                                        select {
                                            class: "bg-stone-800 text-white rounded-lg px-3 py-2 text-sm border border-white/10 focus:outline-none focus:border-indigo-500",
                                            onchange: move |evt| {
                                                config.write().ui_style = match evt.value().as_str() {
                                                    "modern" => config::UiStyle::Modern,
                                                    _ => config::UiStyle::Normal,
                                                };
                                            },
                                            option {
                                                value: "normal",
                                                selected: current_style == config::UiStyle::Normal,
                                                "{i18n::t(\"ui_normal\")}"
                                            }
                                            option {
                                                value: "modern",
                                                selected: current_style == config::UiStyle::Modern,
                                                "{i18n::t(\"ui_modern\")}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        SettingItem {
                            title: i18n::t("player_bar_position").to_string(),
                            control: rsx! {
                                {
                                    let current_position = config.read().player_bar_position;
                                    rsx! {
                                        select {
                                            class: "bg-stone-800 text-white rounded-lg px-3 py-2 text-sm border border-white/10 focus:outline-none focus:border-indigo-500",
                                            onchange: move |evt| {
                                                config.write().player_bar_position = match evt.value().as_str() {
                                                    "top" => config::PlayerBarPosition::Top,
                                                    _ => config::PlayerBarPosition::Bottom,
                                                };
                                            },
                                            option {
                                                value: "bottom",
                                                selected: current_position == config::PlayerBarPosition::Bottom,
                                                "{i18n::t(\"position_bottom\")}"
                                            }
                                            option {
                                                value: "top",
                                                selected: current_position == config::PlayerBarPosition::Top,
                                                "{i18n::t(\"position_top\")}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        SettingItem {
                            title: i18n::t("back_behavior").to_string(),
                            control: rsx! {
                                BackBehaviorSelector {
                                    current: config.read().back_behavior,
                                    on_change: move |val| config.write().back_behavior = val,
                                }
                            }
                        }
                        if !cfg!(target_arch = "wasm32") {
                            section {
                                h2 {
                                    class: "text-lg font-semibold text-white/80 mb-4 border-b border-white/5 pb-2",
                                    "{i18n::t(\"connectivity\")}"
                                }
                                div {
                                    class: "space-y-4",
                                    if !cfg!(target_os = "android") {
                                        SettingItem {
                                            title: i18n::t("discord_presence").to_string(),
                                            control: rsx! {
                                                DiscordPresenceSettings {
                                                    enabled: config.read().discord_presence.unwrap_or(true),
                                                    on_change: move |val| config.write().discord_presence = Some(val),
                                                }
                                            }
                                        }
                                        SettingItem {
                                            title: i18n::t("discord_presence_paused").to_string(),
                                            control: rsx! {
                                                DiscordPresencePausedSettings {
                                                    enabled: config.read().discord_presence_paused.unwrap_or(true),
                                                    on_change: move |val| config.write().discord_presence_paused = Some(val),
                                                }
                                            }
                                        }
                                    }
                                    SettingItem {
                                        title: i18n::t("listenbrainz").to_string(),
                                        control: rsx! {
                                            MusicBrainzSettings {
                                                current: config.read().musicbrainz_token.clone(),
                                                on_save: move |token: String| {
                                                    config.write().musicbrainz_token = token;
                                                },
                                            }
                                        }
                                    }
                                    SettingItem {
                                        title: i18n::t("lastfm").to_string(),
                                        control: rsx! {
                                            LastFmSettings {
                                                api_key: config.read().lastfm_api_key.clone(),
                                                api_secret: config.read().lastfm_api_secret.clone(),
                                                session_key: config.read().lastfm_session_key.clone(),

                                                on_api_key_save: move |value: String| {
                                                    config.write().lastfm_api_key = value;
                                                },

                                                on_api_secret_save: move |value: String| {
                                                    config.write().lastfm_api_secret = value;
                                                },

                                                on_session_key_save: move |value: String| {
                                                    config.write().lastfm_session_key = value;
                                                },
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                if config.read().server.is_some() {
                    section {
                        h2 {
                            class: "text-lg font-semibold text-white/80 mb-4 border-b border-white/5 pb-2",
                            "{i18n::t(\"offline_downloads\")}"
                        }
                        div { class: "space-y-4",
                            SettingItem {
                                title: i18n::t("download_quality").to_string(),
                                control: rsx! {
                                    select {
                                        class: "bg-stone-800 text-white rounded-lg px-3 py-2 text-sm border border-white/10 focus:outline-none focus:border-indigo-500",
                                        onchange: move |evt| {
                                            config.write().offline_quality = OfflineQuality::from_value_str(&evt.value());
                                        },
                                        for q in OfflineQuality::ALL {
                                            option {
                                                value: q.value_str(),
                                                selected: *q == config.read().offline_quality,
                                                "{q.label()}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                section {
                    h2 {
                        class: "text-lg font-semibold text-white/80 mb-4 border-b border-white/5 pb-2",
                        "{i18n::t(\"metadata\")}"
                    }
                    div { class: "space-y-4",
                        SettingItem {
                            title: i18n::t("auto_fetch_covers").to_string(),
                            control: rsx! {
                                ToggleSetting {
                                    enabled: config.read().auto_fetch_covers,
                                    on_change: move |val| config.write().auto_fetch_covers = val,
                                }
                            }
                        }
                        SettingItem {
                            title: i18n::t("prefer_local_lyrics").to_string(),
                            control: rsx! {
                                ToggleSetting {
                                    enabled: config.read().prefer_local_lyrics,
                                    on_change: move |val| config.write().prefer_local_lyrics = val,
                                }
                            }
                        }
                        SettingItem {
                            title: i18n::t("cover_fetch_strategy").to_string(),
                            control: rsx! {
                                {
                                    let current = config.read().cover_fetch_strategy;
                                    rsx! {
                                        select {
                                            class: "bg-stone-800 text-white rounded-lg px-3 py-2 text-sm border border-white/10 focus:outline-none focus:border-indigo-500",
                                            onchange: move |evt| {
                                                config.write().cover_fetch_strategy = match evt.value().as_str() {
                                                    "lastfm_first" => FetchStrategy::LastFmFirst,
                                                    "musicbrainz_only" => FetchStrategy::MusicBrainzOnly,
                                                    "lastfm_only" => FetchStrategy::LastFmOnly,
                                                    _ => FetchStrategy::MusicBrainzFirst,
                                                };
                                            },
                                            option {
                                                value: "musicbrainz_first",
                                                selected: current == FetchStrategy::MusicBrainzFirst,
                                                "{i18n::t(\"musicbrainz_first\")}"
                                            }
                                            option {
                                                value: "lastfm_first",
                                                selected: current == FetchStrategy::LastFmFirst,
                                                "{i18n::t(\"lastfm_first\")}"
                                            }
                                            option {
                                                value: "musicbrainz_only",
                                                selected: current == FetchStrategy::MusicBrainzOnly,
                                                "{i18n::t(\"musicbrainz_only\")}"
                                            }
                                            option {
                                                value: "lastfm_only",
                                                selected: current == FetchStrategy::LastFmOnly,
                                                "{i18n::t(\"lastfm_only\")}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        SettingItem {
                            title: i18n::t("artist_photo_source").to_string(),
                            control: rsx! {
                                {
                                    let current = config.read().artist_photo_source;
                                    rsx! {
                                        select {
                                            class: "bg-stone-800 text-white rounded-lg px-3 py-2 text-sm border border-white/10 focus:outline-none focus:border-indigo-500",
                                            onchange: move |evt| {
                                                config.write().artist_photo_source = match evt.value().as_str() {
                                                    "artist_photo" => ArtistPhotoSource::ArtistPhoto,
                                                    _ => ArtistPhotoSource::AlbumCover,
                                                };
                                            },
                                            option {
                                                value: "album_cover",
                                                selected: current == ArtistPhotoSource::AlbumCover,
                                                "{i18n::t(\"album_cover\")}"
                                            }
                                            option {
                                                value: "artist_photo",
                                                selected: current == ArtistPhotoSource::ArtistPhoto,
                                                "{i18n::t(\"artist_photo\")}"
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                section {
                    h2 {
                        class: "text-lg font-semibold text-white/80 mb-4 border-b border-white/5 pb-2",
                        "{i18n::t(\"player_settings\")}"
                    }

                    div { class: "space-y-4",
                        SettingItem {
                            title: i18n::t("crossfade").to_string(),
                            control: rsx! {
                                div { class: "flex items-center gap-3 min-w-[220px]",
                                    input {
                                        r#type: "range",
                                        min: "0",
                                        max: "12",
                                        step: "1",
                                        value: format!("{}", config.read().crossfade_seconds),
                                        class: "w-40",
                                        style: "accent-color: var(--color-indigo-500);",
                                        oninput: move |evt| {
                                            if let Ok(value) = evt.value().parse::<u8>() {
                                                config.write().crossfade_seconds = value.min(12);
                                            }
                                        }
                                    }
                                    span {
                                        class: "text-xs font-mono text-white/80 w-16 text-right",
                                        "{crossfade_label}"
                                    }
                                }
                            }
                        }
                        SettingItem {
                            title: i18n::t("volume_scroll_step").to_string(),
                            control: rsx! {
                                div { class: "flex items-center gap-3 min-w-[220px]",
                                    input {
                                        r#type: "range",
                                        min: "1",
                                        max: "50",
                                        step: "1",
                                        value: format!("{}", (config.read().volume_scroll_step * 100.0).round() as i32),
                                        class: "w-40",
                                        style: "accent-color: var(--color-indigo-500);",
                                        oninput: move |evt| {
                                            if let Ok(pct) = evt.value().parse::<i32>() {
                                                let clamped = pct.clamp(1, 50);
                                                config.write().volume_scroll_step = clamped as f32 / 100.0;
                                            }
                                        }
                                    }
                                    span {
                                        class: "text-xs font-mono text-white/80 w-16 text-right",
                                        "{(config.read().volume_scroll_step * 100.0).round() as i32}%"
                                    }
                                }
                            }
                        }
                        SettingItem {
                            title: i18n::t("channel_mode").to_string(),
                            control: rsx! {
                                ChannelModeSelector {
                                    current: config.read().channel_mode,
                                    on_change: move |mode| {
                                        config.write().channel_mode = mode;
                                        ctrl.player.write().set_channel_mode(mode);
                                    }
                                }
                            }
                        }
                        div { class: "py-2",
                            p { class: "text-white font-medium mb-3", "{i18n::t(\"equalizer\")}" }
                            EqualizerPanel {
                                current: config.read().equalizer.clone(),
                                on_preview: move |equalizer: config::EqualizerSettings| {
                                    ctrl.player.write().set_equalizer(equalizer);
                                },
                                on_commit: move |equalizer: config::EqualizerSettings| {
                                    config.write().equalizer = equalizer.clone();
                                    ctrl.player.write().set_equalizer(equalizer);
                                }
                            }
                        }
                    }
                }

                {theme_editor_section(config)}



                if show_add_server() {
                    AddServerPopup {
                        server_name,
                        server_url,
                        server_service,
                        yt_browser,
                        yt_auth,
                        yt_pasted_cookies,
                        error,
                        on_close: move |_| show_add_server.set(false),
                        on_save: handle_add_server
                    }
                }

                if show_add_registry() {
                    AddRegistryPopup {
                        registry_url,
                        error: registry_error,
                        loading: registry_loading,
                        on_close: move |_| show_add_registry.set(false),
                        on_save: handle_add_registry
                    }
                }

                if show_login() {
                    LoginPopup {
                        username,
                        password,
                        service_name: config
                            .read()
                            .server
                            .as_ref()
                            .map(|server| server.service.display_name().to_string())
                            .unwrap_or_else(|| i18n::t("server").to_string()),
                        error: login_error,
                        loading: is_loading,
                        on_close: move |_| {
                            show_login.set(false);
                            username.set(String::new());
                            password.set(String::new());
                            login_error.set(None);
                        },
                        on_save: handle_login
                    }
                }

                if yt_login_open() {
                    div { class: "overlay",
                        div {
                            class: "bg-neutral-900 border border-white/10 rounded-xl p-5 max-w-md w-full mx-4 shadow-2xl",
                            h3 { class: "text-lg font-semibold text-white mb-1",
                                i { class: "fa-solid fa-right-to-bracket mr-2" }
                                "{i18n::t(\"yt_login_title\")}"
                            }
                            p { class: "text-sm text-white/60 mb-4", "{i18n::t(\"yt_login_body\")}" }
                            if let Some(err) = error.read().clone() {
                                p { class: "text-xs text-rose-300 mb-3 break-words", "{err}" }
                            }
                            div { class: "flex justify-end gap-2",
                                button {
                                    class: "px-4 py-2 rounded-lg bg-white/10 hover:bg-white/20 text-white text-sm transition-colors disabled:opacity-50",
                                    disabled: yt_login_busy(),
                                    onclick: on_login_cancel,
                                    "{i18n::t(\"cancel\")}"
                                }
                                button {
                                    class: "px-4 py-2 rounded-lg bg-indigo-500 hover:bg-indigo-400 text-white text-sm font-medium transition-colors disabled:opacity-50",
                                    disabled: yt_login_busy(),
                                    onclick: on_login_done,
                                    if yt_login_busy() {
                                        i { class: "fa-solid fa-arrows-rotate fa-spin mr-1.5" }
                                    }
                                    "{i18n::t(\"yt_login_done\")}"
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
