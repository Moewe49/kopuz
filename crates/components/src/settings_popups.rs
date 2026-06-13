use config::{Browser, MusicService};
use dioxus::prelude::*;

/// How the YouTube Music session gets authenticated.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum YtAuthMethod {
    /// Isolated-profile browser sign-in (Linux/macOS only — Google
    /// renders the login page blank in fresh profiles on Windows and
    /// Chrome's App-Bound Encryption blocks cookie extraction there).
    BrowserSignin,
    /// Paste the Cookie header from a signed-in music.youtube.com tab
    /// (or import it from Firefox). Works on every platform.
    PasteCookies,
    /// Google OAuth device-code flow: sign in once in any browser, then
    /// kopuz holds a long-lived refresh token and mints access tokens
    /// itself — no browser kept open, no cookies to re-paste. The
    /// recommended path for a always-signed-in lightweight player.
    OAuth,
    /// No sign-in: browse/search/play public tracks only.
    Anonymous,
}

impl YtAuthMethod {
    /// Default to the managed-browser auto-login on every desktop platform —
    /// the CDP cookie path works on Windows too now (it bypasses App-Bound
    /// Encryption), so it's the recommended "sign in once, stays alive" route.
    pub fn default_for_platform() -> Self {
        Self::BrowserSignin
    }
}

#[component]
pub fn AddServerPopup(
    server_name: Signal<String>,
    server_url: Signal<String>,
    server_service: Signal<MusicService>,
    /// Selected Chromium-family browser when service is YouTube Music.
    yt_browser: Signal<Browser>,
    /// Selected auth method when service is YouTube Music.
    yt_auth: Signal<YtAuthMethod>,
    /// Raw cookie paste buffer for [`YtAuthMethod::PasteCookies`].
    yt_pasted_cookies: Signal<String>,
    error: Signal<Option<String>>,
    on_close: EventHandler<()>,
    on_save: EventHandler<()>,
) -> Element {
    let _service_value = match server_service() {
        MusicService::Jellyfin => "jellyfin",
        MusicService::Subsonic => "subsonic",
        MusicService::Custom => "custom",
        MusicService::YtMusic => "ytmusic",
    };

    let server_name_optional = i18n::t("server_name_optional").to_string();
    let server_url_placeholder = i18n::t("server_url_placeholder").to_string();
    let custom_manual = i18n::t("custom_manual").to_string();
    let cancel_text = i18n::t("cancel").to_string();
    let save_text = i18n::t("save").to_string();

    rsx! {
        div {
            class: "overlay",
            onclick: move |_| on_close.call(()),

            div {
                class: "popup",
                onclick: |e| e.stop_propagation(),

                h2 { "{i18n::t(\"add_media_server\")}" }

                if let Some(err) = error() {
                    p { class: "error", "{err}" }
                }

                input {
                    placeholder: "{server_name_optional}",
                    value: "{server_name()}",
                    oninput: move |e| server_name.set(e.value()),
                    onkeydown: move |e| e.stop_propagation()
                }

                ServerServiceFields {
                    server_service,
                    server_url,
                    yt_browser,
                    yt_auth,
                    yt_pasted_cookies,
                    server_url_placeholder: server_url_placeholder.clone(),
                }

                select {
                    onchange: move |e| {
                        let service = match e.value().as_str() {
                            "subsonic" => MusicService::Subsonic,
                            "custom" => MusicService::Custom,
                            "ytmusic" => MusicService::YtMusic,
                            _ => MusicService::Jellyfin,
                        };
                        server_service.set(service);
                    },
                    onkeydown: move |e| e.stop_propagation(),
                    option {
                        value: "jellyfin",
                        selected: server_service() == MusicService::Jellyfin,
                        "{i18n::t(\"jellyfin\")}"
                    }
                    option {
                        value: "subsonic",
                        selected: server_service() == MusicService::Subsonic,
                        "{i18n::t(\"subsonic\")}"
                    }
                    option {
                        value: "custom",
                        selected: server_service() == MusicService::Custom,
                        "{custom_manual}"
                    }
                    option {
                        value: "ytmusic",
                        selected: server_service() == MusicService::YtMusic,
                        "YouTube Music"
                    }
                }

                div { class: "actions",
                    button {
                        onclick: move |_| on_close.call(()),
                        "{cancel_text}"
                    }
                    button {
                        onclick: move |_| on_save.call(()),
                        "{save_text}"
                    }
                }
            }
        }
    }
}

#[component]
pub fn LoginPopup(
    username: Signal<String>,
    password: Signal<String>,
    service_name: String,
    error: Signal<Option<String>>,
    loading: Signal<bool>,
    on_close: EventHandler<()>,
    on_save: EventHandler<()>,
) -> Element {
    let cancel_text = i18n::t("cancel").to_string();
    let login_text = i18n::t("login").to_string();
    let username_placeholder = i18n::t("username").to_string();
    let password_placeholder = i18n::t("password").to_string();
    let login_to_service_text =
        i18n::t_with("login_to_service", &[("service", service_name.clone())]);

    rsx! {
        div {
            class: "overlay",
            onclick: move |_| on_close.call(()),

            div {
                class: "popup",
                onclick: |e| e.stop_propagation(),

                h2 { "{login_to_service_text}" }

                if let Some(err) = error() {
                    p { class: "error", "{err}" }
                }

                input {
                    placeholder: "{username_placeholder}",
                    value: "{username()}",
                    oninput: move |e| username.set(e.value()),
                    onkeydown: move |e| e.stop_propagation(),
                    disabled: loading()
                }

                input {
                    r#type: "password",
                    placeholder: "{password_placeholder}",
                    value: "{password()}",
                    oninput: move |e| password.set(e.value()),
                    onkeydown: move |e| e.stop_propagation(),
                    disabled: loading()
                }

                div { class: "actions",
                    button {
                        onclick: move |_| if !loading() { on_close.call(()) },
                        disabled: loading(),
                        "{cancel_text}"
                    }
                    button {
                        onclick: move |_| if !loading() { on_save.call(()) },
                        disabled: loading(),
                        if loading() { "{i18n::t(\"logging_in\")}" } else { "{login_text}" }
                    }
                }
            }
        }
    }
}

#[component]
pub fn AddRegistryPopup(
    registry_url: Signal<String>,
    error: Signal<Option<String>>,
    loading: Signal<bool>,
    on_close: EventHandler<()>,
    on_save: EventHandler<()>,
) -> Element {
    let url_placeholder = i18n::t("radio_registry_url_placeholder").to_string();
    let cancel_text = i18n::t("cancel").to_string();
    let save_text = i18n::t("save").to_string();

    rsx! {
        div {
            class: "overlay",
            onclick: move |_| { if !loading() { on_close.call(()) } },

            div {
                class: "popup",
                onclick: |e| e.stop_propagation(),

                h2 { "{i18n::t(\"add_radio_registry\")}" }

                if let Some(err) = error() {
                    p { class: "error", "{err}" }
                }

                input {
                    placeholder: "{url_placeholder}",
                    value: "{registry_url()}",
                    oninput: move |e| registry_url.set(e.value()),
                    onkeydown: move |e| e.stop_propagation(),
                    disabled: loading()
                }

                div { class: "actions",
                    button {
                        onclick: move |_| if !loading() { on_close.call(()) },
                        disabled: loading(),
                        "{cancel_text}"
                    }
                    button {
                        onclick: move |_| if !loading() { on_save.call(()) },
                        disabled: loading(),
                        if loading() { "{i18n::t(\"saving\")}" } else { "{save_text}" }
                    }
                }
            }
        }
    }
}

#[component]
fn ServerServiceFields(
    server_service: Signal<MusicService>,
    server_url: Signal<String>,
    yt_browser: Signal<Browser>,
    mut yt_auth: Signal<YtAuthMethod>,
    yt_pasted_cookies: Signal<String>,
    server_url_placeholder: String,
) -> Element {
    // The isolated-browser sign-in is dead on Windows for two stacked
    // reasons: Google renders ServiceLogin blank in a fresh profile,
    // and Chrome 127+ App-Bound Encryption blocks decrypting the
    // profile's cookies. Hide that option there; manual cookies are
    // the Windows sign-in path.
    use_effect(move || {
        // OAuth is hidden (broken by Google) — fall back to paste. Browser
        // sign-in now works on Windows via the headless CDP path, so it's no
        // longer forced away there.
        if *yt_auth.peek() == YtAuthMethod::OAuth {
            yt_auth.set(YtAuthMethod::PasteCookies);
        }
    });
    let mut firefox_busy = use_signal(|| false);
    let mut firefox_error = use_signal(|| None::<String>);

    match server_service() {
        MusicService::YtMusic => {
            let method = yt_auth();
            rsx! {
                // Auth method selector. Browser sign-in (auto-login) is shown on
                // all desktop platforms — the CDP refresh path works on Windows.
                div { class: "flex flex-col gap-2 mb-2",
                    label { class: "flex items-center gap-2 text-sm text-white cursor-pointer",
                        input {
                            r#type: "radio",
                            name: "yt-auth-method",
                            checked: method == YtAuthMethod::BrowserSignin,
                            onchange: move |_| yt_auth.set(YtAuthMethod::BrowserSignin),
                        }
                        span { "{i18n::t(\"yt_auth_browser\")}" }
                    }
                    // NOTE: the OAuth ("Sign in with Google") method is hidden —
                    // Google disabled OAuth against the YouTube Music InnerTube
                    // API in 2025/2026 (every browse/playlist call returns HTTP
                    // 400 INVALID_ARGUMENT even with a correct personal client),
                    // a server-side change that also broke ytmusicapi/yt-dlp
                    // OAuth. The code path stays for if/when Google reopens it.
                    label { class: "flex items-center gap-2 text-sm text-white cursor-pointer",
                        input {
                            r#type: "radio",
                            name: "yt-auth-method",
                            checked: method == YtAuthMethod::PasteCookies,
                            onchange: move |_| yt_auth.set(YtAuthMethod::PasteCookies),
                        }
                        span { "{i18n::t(\"yt_auth_paste\")}" }
                    }
                    label { class: "flex items-center gap-2 text-sm text-white cursor-pointer",
                        input {
                            r#type: "radio",
                            name: "yt-auth-method",
                            checked: method == YtAuthMethod::Anonymous,
                            onchange: move |_| yt_auth.set(YtAuthMethod::Anonymous),
                        }
                        span { "{i18n::t(\"yt_auth_anonymous\")}" }
                    }
                }

                match method {
                    YtAuthMethod::Anonymous => rsx! {
                        p { class: "text-xs text-white/60", "{i18n::t(\"yt_anon_explainer\")}" }
                    },
                    YtAuthMethod::OAuth => rsx! {
                        OAuthSignIn { yt_pasted_cookies }
                    },
                    YtAuthMethod::PasteCookies => rsx! {
                        ol { class: "text-xs text-white/60 list-decimal list-inside space-y-0.5 mb-2",
                            li { "{i18n::t(\"yt_paste_step1\")}" }
                            li { "{i18n::t(\"yt_paste_step2\")}" }
                            li { "{i18n::t(\"yt_paste_step3\")}" }
                        }
                        textarea {
                            class: "w-full h-24 bg-white/5 border border-white/10 rounded-lg px-3 py-2 text-white text-xs font-mono placeholder:text-white/30 focus:outline-none focus:border-indigo-400 resize-y",
                            placeholder: "VISITOR_INFO1_LIVE=…; SID=…; SAPISID=…; …",
                            value: "{yt_pasted_cookies()}",
                            oninput: move |e| yt_pasted_cookies.set(e.value()),
                            onkeydown: move |e| e.stop_propagation(),
                        }
                        div { class: "flex items-center gap-2 mt-1",
                            button {
                                class: "px-3 py-1.5 rounded-lg bg-white/10 hover:bg-white/20 text-white text-xs transition-colors disabled:opacity-50",
                                disabled: *firefox_busy.read(),
                                onclick: move |_| {
                                    firefox_busy.set(true);
                                    firefox_error.set(None);
                                    spawn(async move {
                                        match server::ytmusic::manual_cookies::extract_from_firefox().await {
                                            Ok(header) => yt_pasted_cookies.set(header),
                                            Err(e) => firefox_error.set(Some(e)),
                                        }
                                        firefox_busy.set(false);
                                    });
                                },
                                if *firefox_busy.read() {
                                    i { class: "fa-solid fa-arrows-rotate fa-spin mr-1" }
                                } else {
                                    i { class: "fa-brands fa-firefox-browser mr-1" }
                                }
                                "{i18n::t(\"yt_paste_firefox\")}"
                            }
                            span { class: "text-[10px] text-white/40", "{i18n::t(\"yt_paste_firefox_hint\")}" }
                        }
                        if let Some(err) = firefox_error.read().clone() {
                            p { class: "text-xs text-rose-300 mt-1 break-words", "{err}" }
                        }
                        // Stay signed in automatically — re-read cookies from a
                        // signed-in browser on every start + periodically.
                        if let Some(mut cfg) = try_consume_context::<Signal<config::AppConfig>>() {
                            label { class: "flex items-start gap-2 mt-3 text-xs text-white cursor-pointer",
                                input {
                                    r#type: "checkbox",
                                    class: "mt-0.5",
                                    checked: cfg.read().yt_auto_refresh,
                                    onchange: move |e| cfg.write().yt_auto_refresh = e.checked(),
                                }
                                span {
                                    class: "text-white/80",
                                    "{i18n::t(\"yt_auto_refresh_label\")}"
                                }
                            }
                        }
                    },
                    YtAuthMethod::BrowserSignin => rsx! {
                        p { class: "text-xs text-white/60",
                            "Sign in once: kopuz opens a small browser window (its own private profile — your normal browsing is untouched). After that, kopuz keeps you signed in automatically by refreshing the session invisibly in the background on each start — no window, no re-pasting, until you actually log out. Pick which installed browser to use."
                        }
                        select {
                            onchange: move |e| {
                                if let Some(b) = Browser::from_id(&e.value()) {
                                    yt_browser.set(b);
                                }
                            },
                            onkeydown: move |e| e.stop_propagation(),
                            for browser in Browser::ALL.iter().copied() {
                                option {
                                    value: "{browser.id()}",
                                    selected: yt_browser() == browser,
                                    "{browser.label()}"
                                }
                            }
                        }
                        // Tell the user immediately whether the chosen browser
                        // will actually launch, instead of a dead-end error
                        // after they commit.
                        {
                            #[cfg(not(target_arch = "wasm32"))]
                            {
                                let sel = yt_browser();
                                let installed = ::server::ytmusic::isolated_profile::is_browser_installed(sel);
                                if installed {
                                    rsx! { p { class: "text-xs text-emerald-300 mt-1",
                                        "✓ {sel.label()} found — auto-login will work." } }
                                } else {
                                    rsx! { p { class: "text-xs text-rose-300 mt-1",
                                        "✗ {sel.label()} not found. Install Chrome, Edge, or Brave — or pick an installed one above. (You can also use the cookie-paste method.)" } }
                                }
                            }
                            #[cfg(target_arch = "wasm32")]
                            { rsx! {} }
                        }
                    },
                }

            }
        },
        _ => rsx! {
            input {
                placeholder: "{server_url_placeholder}",
                value: "{server_url()}",
                oninput: move |e| server_url.set(e.value()),
                onkeydown: move |e| e.stop_propagation()
            }
        },
    }
}

/// Google OAuth device-code sign-in. Requests a short user code, opens the
/// verification page, and polls until the user authorizes — then stashes the
/// long-lived refresh token in config and feeds the fresh access token (as the
/// `oauth:<…>` sentinel) into `yt_pasted_cookies` so the surrounding Save flow
/// adopts it exactly like a pasted session. No browser is kept open afterwards.
#[component]
fn OAuthSignIn(yt_pasted_cookies: Signal<String>) -> Element {
    let mut busy = use_signal(|| false);
    let mut error = use_signal(|| None::<String>);
    // (user_code, verification_url) while we wait for the user to authorize.
    let mut user_code = use_signal(|| None::<(String, String)>);
    let mut done = use_signal(|| false);

    // The user's own Google Cloud OAuth client (the shared one is dead — see
    // oauth.rs). Bound to config so it persists and the boot refresh can reuse
    // it. Both fields must be filled before sign-in is possible.
    let cfg_ctx = try_consume_context::<Signal<config::AppConfig>>();
    let client_id = cfg_ctx
        .map(|c| c.read().yt_oauth_client_id.clone())
        .unwrap_or_default();
    let client_secret = cfg_ctx
        .map(|c| c.read().yt_oauth_client_secret.clone())
        .unwrap_or_default();
    let creds_ready = !client_id.trim().is_empty() && !client_secret.trim().is_empty();

    let start = move |_| {
        busy.set(true);
        error.set(None);
        done.set(false);
        user_code.set(None);
        // Snapshot creds at click time.
        let (cid, csec) = cfg_ctx
            .map(|c| {
                let r = c.read();
                (r.yt_oauth_client_id.clone(), r.yt_oauth_client_secret.clone())
            })
            .unwrap_or_default();
        spawn(async move {
            use server::ytmusic::oauth;
            let dc = match oauth::request_device_code(&cid).await {
                Ok(dc) => dc,
                Err(e) => {
                    error.set(Some(e));
                    busy.set(false);
                    return;
                }
            };
            user_code.set(Some((dc.user_code.clone(), dc.verification_url.clone())));
            let _ = webbrowser::open(&dc.verification_url);
            let mut interval = dc.interval.max(2);
            let deadline = dc.expires_in.max(300);
            let mut waited = 0u64;
            loop {
                utils::sleep(std::time::Duration::from_secs(interval)).await;
                waited += interval;
                if waited > deadline {
                    error.set(Some(i18n::t("yt_oauth_timeout").to_string()));
                    break;
                }
                match oauth::poll_once(&cid, &csec, &dc.device_code).await {
                    oauth::PollResult::Pending => continue,
                    oauth::PollResult::SlowDown => interval += 5,
                    oauth::PollResult::Authorized(tok) => {
                        if let Some(mut cfg) = cfg_ctx {
                            cfg.write().yt_oauth_refresh_token = tok.refresh_token.clone();
                        }
                        yt_pasted_cookies.set(oauth::to_sentinel(&tok.access_token));
                        done.set(true);
                        break;
                    }
                    oauth::PollResult::Failed(e) => {
                        error.set(Some(e));
                        break;
                    }
                }
            }
            user_code.set(None);
            busy.set(false);
        });
    };

    rsx! {
        div { class: "flex flex-col gap-2",
            p { class: "text-xs text-white/60", "{i18n::t(\"yt_oauth_explainer\")}" }
            // One-time Google Cloud client setup.
            if let Some(mut cfg) = cfg_ctx {
                details { class: "text-xs text-white/70 bg-white/5 border border-white/10 rounded-lg p-2",
                    summary { class: "cursor-pointer select-none text-white/80",
                        i { class: "fa-solid fa-key mr-1.5" }
                        "{i18n::t(\"yt_oauth_setup_title\")}"
                    }
                    a {
                        class: "flex items-center gap-1.5 text-indigo-300 underline mt-2 font-medium",
                        href: "https://github.com/Moewe49/kopuz/blob/master/docs/youtube-login-setup.md",
                        i { class: "fa-solid fa-book-open" }
                        "{i18n::t(\"yt_oauth_full_guide\")}"
                    }
                    ol { class: "list-decimal list-inside space-y-0.5 mt-2 text-white/55",
                        li { "{i18n::t(\"yt_oauth_setup_step1\")}" }
                        li { "{i18n::t(\"yt_oauth_setup_step2\")}" }
                        li { "{i18n::t(\"yt_oauth_setup_step3\")}" }
                        li { "{i18n::t(\"yt_oauth_setup_step4\")}" }
                    }
                    a {
                        class: "text-indigo-300 underline mt-1 inline-block",
                        href: "https://console.cloud.google.com/auth/clients",
                        "console.cloud.google.com"
                    }
                    input {
                        r#type: "text",
                        class: "w-full mt-2 bg-white/5 border border-white/10 rounded px-2 py-1 text-white text-xs font-mono placeholder:text-white/30 focus:outline-none focus:border-indigo-400",
                        placeholder: "Client ID (…apps.googleusercontent.com)",
                        value: "{cfg.read().yt_oauth_client_id}",
                        oninput: move |e| cfg.write().yt_oauth_client_id = e.value(),
                        onkeydown: move |e| e.stop_propagation(),
                    }
                    input {
                        r#type: "password",
                        class: "w-full mt-1 bg-white/5 border border-white/10 rounded px-2 py-1 text-white text-xs font-mono placeholder:text-white/30 focus:outline-none focus:border-indigo-400",
                        placeholder: "Client secret (GOCSPX-…)",
                        value: "{cfg.read().yt_oauth_client_secret}",
                        oninput: move |e| cfg.write().yt_oauth_client_secret = e.value(),
                        onkeydown: move |e| e.stop_propagation(),
                    }
                }
            }
            if *done.read() {
                p { class: "text-xs text-emerald-300",
                    i { class: "fa-solid fa-circle-check mr-1" }
                    "{i18n::t(\"yt_oauth_done\")}"
                }
            } else if let Some((code, url)) = user_code.read().clone() {
                div { class: "rounded-lg bg-white/5 border border-white/10 p-3 text-center",
                    p { class: "text-[11px] text-white/50 mb-1", "{i18n::t(\"yt_oauth_enter_code\")}" }
                    p { class: "text-2xl font-mono font-bold tracking-[0.2em] text-white select-all my-1",
                        "{code}"
                    }
                    a {
                        class: "text-xs text-indigo-300 underline break-all",
                        href: "{url}",
                        "{url}"
                    }
                    p { class: "text-[11px] text-white/40 mt-2",
                        i { class: "fa-solid fa-arrows-rotate fa-spin mr-1" }
                        "{i18n::t(\"yt_oauth_waiting\")}"
                    }
                }
            } else {
                button {
                    class: "px-3 py-2 rounded-lg bg-indigo-500 hover:bg-indigo-400 text-white text-sm font-medium transition-colors disabled:opacity-40 disabled:cursor-not-allowed self-start",
                    disabled: *busy.read() || !creds_ready,
                    onclick: start,
                    i { class: "fa-brands fa-google mr-2" }
                    "{i18n::t(\"yt_oauth_button\")}"
                }
                if !creds_ready {
                    p { class: "text-[11px] text-amber-300/80", "{i18n::t(\"yt_oauth_need_creds\")}" }
                }
            }
            if let Some(err) = error.read().clone() {
                p { class: "text-xs text-rose-300 break-words", "{err}" }
            }
        }
    }
}
