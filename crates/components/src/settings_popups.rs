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
    /// No sign-in: browse/search/play public tracks only.
    Anonymous,
}

impl YtAuthMethod {
    /// Windows opens on the paste flow (the only signed-in path
    /// there); other platforms keep the one-click browser sign-in.
    pub fn default_for_platform() -> Self {
        if cfg!(target_os = "windows") {
            Self::PasteCookies
        } else {
            Self::BrowserSignin
        }
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
    let windows = cfg!(target_os = "windows");
    use_effect(move || {
        if cfg!(target_os = "windows") && *yt_auth.peek() == YtAuthMethod::BrowserSignin {
            yt_auth.set(YtAuthMethod::PasteCookies);
        }
    });
    let mut firefox_busy = use_signal(|| false);
    let mut firefox_error = use_signal(|| None::<String>);

    match server_service() {
        MusicService::YtMusic => {
            let method = yt_auth();
            rsx! {
                // Auth method selector (browser sign-in hidden on Windows).
                div { class: "flex flex-col gap-2 mb-2",
                    if !windows {
                        label { class: "flex items-center gap-2 text-sm text-white cursor-pointer",
                            input {
                                r#type: "radio",
                                name: "yt-auth-method",
                                checked: method == YtAuthMethod::BrowserSignin,
                                onchange: move |_| yt_auth.set(YtAuthMethod::BrowserSignin),
                            }
                            span { "{i18n::t(\"yt_auth_browser\")}" }
                        }
                    }
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
                            "Pick which browser kopuz should use for the YouTube Music sign-in window. It opens in an isolated profile (a fresh, separate session) — your normal browsing is untouched. Make sure the browser is installed."
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
