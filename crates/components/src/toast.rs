//! Tiny transient toast: a one-line confirmation that fades after a few
//! seconds. Used for "added to playlist" and similar quick feedback.

use dioxus::prelude::*;

/// App-wide current toast message; `None` = nothing shown. Provided once in
/// main, set via [`show_toast`], rendered by [`ToastHost`].
#[derive(Clone, Copy)]
pub struct ToastState(pub Signal<Option<String>>);

/// Show `msg` for ~3 seconds. No-ops if the context isn't wired.
pub fn show_toast(msg: impl Into<String>) {
    if let Some(state) = try_consume_context::<ToastState>() {
        let mut sig = state.0;
        let msg = msg.into();
        sig.set(Some(msg.clone()));
        // `spawn_forever`, not `spawn`: a plain `spawn` binds the future to the
        // calling scope, so dismissing the dialog that raised the toast makes
        // dioxus cancel the pending sleep — and the toast then stays on screen
        // forever. That is exactly what happened after a playlist import: the
        // share modal unmounts, the timer dies with it, "imported" never
        // clears. ROOT outlives every caller, and the future only touches a
        // `Copy` signal, so it is safe to leave there.
        dioxus::core::spawn_forever(async move {
            utils::sleep(std::time::Duration::from_secs(3)).await;
            // Only clear if our message is still the one showing.
            if sig.peek().as_deref() == Some(msg.as_str()) {
                sig.set(None);
            }
        });
    }
}

#[component]
pub fn ToastHost() -> Element {
    let state = use_context::<ToastState>();
    let msg = state.0.read().clone();
    rsx! {
        if let Some(msg) = msg {
            div {
                class: "fixed bottom-28 left-1/2 -translate-x-1/2 z-[60] flex items-center gap-2 \
                        px-4 py-2.5 rounded-full bg-neutral-900/95 border border-white/10 \
                        text-white text-sm shadow-xl backdrop-blur pointer-events-none",
                i { class: "fa-solid fa-circle-check text-emerald-400" }
                span { "{msg}" }
            }
        }
    }
}
