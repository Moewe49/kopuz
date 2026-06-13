//! First-run welcome. A fresh install opens to an empty Local library with no
//! server configured, which leaves a new user with nothing to do and no
//! signpost. This one-time modal explains the two ways to get music in and
//! points at Settings. Shown once (gated by `config.onboarded`), then never
//! again.

use dioxus::prelude::*;

#[component]
pub fn OnboardingModal(
    /// Dismiss without going anywhere.
    on_close: EventHandler,
    /// Jump to Settings → Media servers.
    on_open_settings: EventHandler,
) -> Element {
    rsx! {
        div {
            class: "fixed inset-0 bg-black/80 flex items-center justify-center z-50 p-4",
            onclick: move |_| on_close.call(()),
            div {
                class: "bg-neutral-900 rounded-2xl border border-white/10 w-full max-w-md p-7 shadow-2xl",
                onclick: move |e| e.stop_propagation(),

                h2 { class: "text-2xl font-black text-white mb-1", "{i18n::t(\"onboarding_title\")}" }
                p { class: "text-sm text-white/60 mb-6", "{i18n::t(\"onboarding_subtitle\")}" }

                div { class: "space-y-3 mb-7",
                    div { class: "flex items-start gap-3 rounded-xl bg-white/5 p-3",
                        i { class: "fa-solid fa-folder-open text-indigo-300 text-lg mt-0.5 shrink-0" }
                        div {
                            p { class: "text-white font-semibold text-sm", "{i18n::t(\"onboarding_local_title\")}" }
                            p { class: "text-white/50 text-xs", "{i18n::t(\"onboarding_local_desc\")}" }
                        }
                    }
                    div { class: "flex items-start gap-3 rounded-xl bg-white/5 p-3",
                        i { class: "fa-brands fa-youtube text-rose-400 text-lg mt-0.5 shrink-0" }
                        div {
                            p { class: "text-white font-semibold text-sm", "{i18n::t(\"onboarding_stream_title\")}" }
                            p { class: "text-white/50 text-xs", "{i18n::t(\"onboarding_stream_desc\")}" }
                        }
                    }
                }

                div { class: "flex gap-2",
                    button {
                        class: "flex-1 px-4 py-2.5 rounded-lg bg-indigo-500 hover:bg-indigo-400 text-white text-sm font-semibold transition-colors",
                        onclick: move |_| on_open_settings.call(()),
                        "{i18n::t(\"onboarding_open_settings\")}"
                    }
                    button {
                        class: "px-4 py-2.5 rounded-lg bg-white/10 hover:bg-white/20 text-white text-sm transition-colors",
                        onclick: move |_| on_close.call(()),
                        "{i18n::t(\"onboarding_later\")}"
                    }
                }
            }
        }
    }
}
