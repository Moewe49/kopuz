//! Sleep timer — pauses playback after a chosen duration. The state
//! lives in an app-level context so the countdown survives switching
//! between the modern and normal bottombar styles.

use dioxus::prelude::*;
use hooks::use_player_controller::PlayerController;

/// Unix-seconds deadline; `None` = timer off. Provided once in main.
#[derive(Clone, Copy)]
pub struct SleepTimerState(pub Signal<Option<u64>>);

const CHOICES_MIN: [u64; 5] = [15, 30, 45, 60, 90];

fn now_secs() -> u64 {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[component]
pub fn SleepTimerButton() -> Element {
    let mut state = use_context::<SleepTimerState>().0;
    let mut ctrl = use_context::<PlayerController>();
    let mut open = use_signal(|| false);

    // Watchdog: while a deadline is set, check every 5s and pause
    // playback once it passes. Cheap enough to just always run while
    // the bar is mounted.
    use_future(move || async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            let Some(deadline) = *state.peek() else {
                continue;
            };
            if now_secs() >= deadline {
                state.set(None);
                if *ctrl.is_playing.peek() {
                    ctrl.toggle();
                }
            }
        }
    });

    let deadline = *state.read();
    let remaining_min = deadline
        .map(|d| d.saturating_sub(now_secs()).div_ceil(60))
        .filter(|m| *m > 0);

    rsx! {
        div { class: "relative",
            button {
                class: if deadline.is_some() {
                    "w-7 h-7 flex items-center justify-center text-indigo-300 hover:text-indigo-200 transition-colors"
                } else {
                    "w-7 h-7 flex items-center justify-center text-slate-500 hover:text-white transition-colors"
                },
                title: i18n::t("sleep_timer").to_string(),
                onclick: move |_| {
                    let c = *open.read();
                    open.set(!c);
                },
                i { class: "fa-solid fa-moon text-[10px]" }
                if let Some(min) = remaining_min {
                    span { class: "absolute -top-1 -right-1 text-[8px] font-bold text-indigo-300",
                        "{min}"
                    }
                }
            }
            if *open.read() {
                div {
                    class: "absolute bottom-9 right-0 bg-neutral-900 border border-white/10 rounded-lg shadow-xl p-1.5 z-50 min-w-32",
                    p { class: "text-[10px] uppercase tracking-wider text-white/40 px-2 py-1",
                        "{i18n::t(\"sleep_timer\")}"
                    }
                    for min in CHOICES_MIN {
                        button {
                            key: "{min}",
                            class: "w-full text-left px-2 py-1.5 rounded text-xs text-white/80 hover:bg-white/10 transition-colors",
                            onclick: move |_| {
                                state.set(Some(now_secs() + min * 60));
                                open.set(false);
                            },
                            {i18n::t_with("sleep_timer_minutes", &[("min", min.to_string())])}
                        }
                    }
                    if deadline.is_some() {
                        button {
                            class: "w-full text-left px-2 py-1.5 rounded text-xs text-rose-300 hover:bg-white/10 transition-colors",
                            onclick: move |_| {
                                state.set(None);
                                open.set(false);
                            },
                            {i18n::t("sleep_timer_off")}
                        }
                    }
                }
            }
        }
    }
}
