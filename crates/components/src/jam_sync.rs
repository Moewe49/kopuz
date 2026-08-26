//! The hand on the player: driving a real [`PlayerController`] from the tested
//! sync brain in [`reader::jamlive`], over the relay.
//!
//! Everything hard was decided and tested next door. What is left here is glue,
//! and it is deliberately thin: one loop that, while a jam is joined,
//!
//! 1. reads the shared document (cheap when unchanged), and if it is newer than
//!    what this device applied, [`reconcile`](reader::jamlive::reconcile)s it to
//!    an action and does it to the player;
//! 2. looks at what the player now shows and, if the local listener changed
//!    something, publishes it with compare-and-swap.
//!
//! Because step 2 watches the player's own state rather than intercepting its
//! controls, every existing button — pause, skip, the queue's drag handles,
//! add-to-queue — publishes for free the moment it takes effect. There was no
//! collaborative queue to build; there was a loop to watch one.
//!
//! The one non-obvious guard: a tick that applied a remote change does not then
//! publish. The player takes a beat to settle after a seek or a track change,
//! so reading it back in the same breath would see the old state and mistake
//! the settling for a local edit — and publish the very thing it just received.
//! Skipping the publish for that one tick lets the player catch up first.

use dioxus::prelude::*;
use hooks::use_player_controller::PlayerController;
use reader::jamlive::{self, Action, JamDoc, LocalView};
use reader::models::Track;
use reader::share::{self, SharedTrack};
use relay::jam::JamAccess;

use crate::toast::show_toast;

/// How often the loop reads the jam and reports its own changes. Fast enough
/// that a pause on one side reaches the other within a breath, slow enough not
/// to hammer a relay on the far end of a home connection.
const TICK_MS: u64 = 1_500;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// The live-jam state, shared app-wide as a context so a control anywhere can
/// start, join, leave, or show it. All signals, so it is `Copy`.
#[derive(Clone, Copy)]
pub struct JamState {
    /// `Some` while a jam is joined. The whole access, so the UI can show the
    /// join code to hand on.
    pub access: Signal<Option<JamAccess>>,
    /// Whether this device opened the jam (it can then re-share the code) or
    /// joined one. Both drive it equally — this is for wording, not power.
    pub is_host: Signal<bool>,
    /// A short human line for the panel: "waiting for the other person", an
    /// error, or empty.
    pub status: Signal<String>,
    /// The document both sides last agreed on.
    shared: Signal<JamDoc>,
    /// The document revision already applied to this player.
    applied_rev: Signal<u64>,
    /// The relay's compare-and-swap version this device holds.
    relay_version: Signal<u64>,
    /// Set for the one tick after applying a remote change, so the player is
    /// left to settle before its state is read as a local edit.
    settling: Signal<bool>,
}

impl JamState {
    pub fn in_a_jam(&self) -> bool {
        self.access.peek().is_some()
    }
}

/// Set up the jam state and its loop. Mounted once, near where the player
/// controller is created; returns the context others consume.
pub fn use_jam_sync(ctrl: PlayerController) -> JamState {
    let state = use_context_provider(|| JamState {
        access: Signal::new(None),
        is_host: Signal::new(false),
        status: Signal::new(String::new()),
        shared: Signal::new(JamDoc::default()),
        applied_rev: Signal::new(0),
        relay_version: Signal::new(0),
        settling: Signal::new(false),
    });

    use_effect(move || {
        // One long-lived loop for the app's life. It idles cheaply when no jam
        // is joined, so there is no start/stop plumbing to get wrong.
        dioxus::core::spawn_forever(async move {
            loop {
                utils::sleep(std::time::Duration::from_millis(TICK_MS)).await;
                let Some(access) = state.access.peek().clone() else {
                    continue;
                };
                tick(state, ctrl, &access).await;
            }
        });
    });

    state
}

/// One pass of the loop: follow the far side, then report this side.
async fn tick(mut state: JamState, ctrl: PlayerController, access: &JamAccess) {
    let have = *state.relay_version.peek();
    match relay::client::jam_read(access, have).await {
        Ok(relay::Fetched::Unchanged) => {}
        Ok(relay::Fetched::Value(stored)) => {
            state.relay_version.set(stored.version);
            if let Some(doc) = jamlive::decode(&stored.bytes) {
                let applied = *state.applied_rev.peek();
                let action = jamlive::reconcile(&local_view(ctrl), &doc, applied, now_secs());
                if !matches!(action, Action::Nothing) {
                    apply_action(ctrl, action);
                    state.settling.set(true);
                }
                state.shared.set(doc.clone());
                state.applied_rev.set(doc.rev);
            }
        }
        Ok(relay::Fetched::Missing) | Err(relay::RelayError::JamGone) => {
            end_locally(state, "The jam has ended.");
            return;
        }
        Err(_) => return, // A blip; try again next tick.
    }

    // A tick that just applied a remote change leaves the player to settle
    // rather than reading its half-applied state back as a local edit.
    if *state.settling.peek() {
        state.settling.set(false);
        return;
    }

    let shared = state.shared.peek().clone();
    if let Some(op) = jamlive::local_change(&shared, &local_view(ctrl), now_secs()) {
        let next = jamlive::apply(&shared, op, now_secs());
        let based_on = *state.relay_version.peek();
        match relay::client::jam_write(access, &jamlive::encode(&next), based_on).await {
            Ok(relay::jam::JamWrite::Stored { version }) => {
                state.relay_version.set(version);
                state.shared.set(next.clone());
                state.applied_rev.set(next.rev);
            }
            // Someone wrote first: do nothing now. Next tick reads their version,
            // reconciles it, and re-derives this change on top if it still holds.
            Ok(relay::jam::JamWrite::Conflict { .. }) => {}
            Err(relay::RelayError::JamGone) => end_locally(state, "The jam has ended."),
            Err(_) => {}
        }
    }
}

/// Read the player as the brain needs to see it.
fn local_view(ctrl: PlayerController) -> LocalView {
    let queue = ctrl.queue.peek();
    let index = (*ctrl.current_queue_index.peek()).min(queue.len().saturating_sub(1));
    LocalView {
        playing: *ctrl.is_playing.peek(),
        index,
        position_ms: (*ctrl.current_song_progress.peek()).saturating_mul(1000),
        queue: queue
            .iter()
            .map(|t| {
                share::shared_track(&t.path.to_string_lossy(), &t.title, &t.artist, t.duration)
            })
            .collect(),
    }
}

/// Do one reconciled action to the player.
fn apply_action(mut ctrl: PlayerController, action: Action) {
    match action {
        Action::Nothing => {}
        Action::ReplaceQueue {
            tracks,
            index,
            position_ms,
            playing,
        } => {
            let (playable, mapped) = to_tracks(&tracks, index);
            if playable.is_empty() {
                return;
            }
            ctrl.play_queue_at(playable, mapped, position_ms / 1000);
            if !playing {
                ctrl.pause();
            }
        }
        Action::Follow {
            index,
            position_ms,
            playing,
        } => {
            let queue_len = ctrl.queue.peek().len();
            if index < queue_len && index != *ctrl.current_queue_index.peek() {
                ctrl.play_track(index);
            }
            ctrl.player
                .write()
                .seek(std::time::Duration::from_millis(position_ms));
            if playing && !*ctrl.is_playing.peek() {
                ctrl.resume();
            } else if !playing && *ctrl.is_playing.peek() {
                ctrl.pause();
            }
        }
    }
}

/// Portable shared tracks into playable ones, mapping the target index across
/// any that had to be dropped. The same care [`crate::jam::jam_tracks`] takes
/// for the one-shot join: a dropped track shifts every index after it.
fn to_tracks(tracks: &[SharedTrack], target: usize) -> (Vec<Track>, usize) {
    let mut out = Vec::new();
    let mut mapped = 0usize;
    for (i, t) in tracks.iter().enumerate() {
        let Some(path) = &t.path else {
            continue;
        };
        if i <= target {
            mapped = out.len();
        }
        out.push(Track {
            path: std::path::PathBuf::from(path),
            album_id: String::new(),
            title: t.title.clone(),
            artist: t.artist.clone(),
            album: String::new(),
            duration: t.duration,
            khz: 0,
            bitrate: 0,
            track_number: None,
            disc_number: None,
            musicbrainz_release_id: None,
            musicbrainz_recording_id: None,
            musicbrainz_track_id: None,
            playlist_item_id: None,
            artists: vec![t.artist.clone()],
        });
    }
    let mapped = mapped.min(out.len().saturating_sub(1));
    (out, mapped)
}

fn end_locally(mut state: JamState, why: &str) {
    state.access.set(None);
    state.is_host.set(false);
    state.status.set(String::new());
    show_toast(why.to_string());
}

/// Open a jam from whatever is playing now, and return the code to hand over.
pub async fn start_jam(mut state: JamState, ctrl: PlayerController, config: relay::RelayConfig) {
    if !config.is_configured() {
        state
            .status
            .set("Set up your relay first, in Settings.".to_string());
        return;
    }
    state.status.set("Opening a jam…".to_string());
    let access = match relay::client::jam_open(&config).await {
        Ok(a) => a,
        Err(e) => {
            state.status.set(e.to_string());
            return;
        }
    };
    // Seed the jam with the current queue and playhead, so the person joining
    // lands in the middle of what you are already listening to.
    let view = local_view(ctrl);
    let doc = jamlive::apply(
        &JamDoc::default(),
        jamlive::JamOp::SetQueue {
            tracks: view.queue,
            index: view.index,
            position_ms: view.position_ms,
            playing: view.playing,
        },
        now_secs(),
    );
    match relay::client::jam_write(&access, &jamlive::encode(&doc), 0).await {
        Ok(relay::jam::JamWrite::Stored { version }) => {
            state.relay_version.set(version);
            state.shared.set(doc.clone());
            state.applied_rev.set(doc.rev);
            state.is_host.set(true);
            state.access.set(Some(access));
            state
                .status
                .set("Jam open — send the code to listen together.".to_string());
        }
        _ => state.status.set("Could not start the jam.".to_string()),
    }
}

/// Join a jam from a pasted `kopuz:live:` code. Adopts its queue and playhead.
pub async fn join_jam(mut state: JamState, ctrl: PlayerController, code: String) {
    let Some(access) = relay::jam::decode_join(&code) else {
        state.status.set("That is not a jam code.".to_string());
        return;
    };
    state.status.set("Joining…".to_string());
    match relay::client::jam_read(&access, 0).await {
        Ok(relay::Fetched::Value(stored)) => {
            if let Some(doc) = jamlive::decode(&stored.bytes) {
                let (index, position) = jamlive::catch_up(&doc, now_secs());
                apply_action(
                    ctrl,
                    Action::ReplaceQueue {
                        tracks: doc.queue.clone(),
                        index,
                        position_ms: position,
                        playing: doc.playing,
                    },
                );
                state.relay_version.set(stored.version);
                state.shared.set(doc.clone());
                // Mark applied, so the loop does not immediately re-apply it.
                state.applied_rev.set(doc.rev);
                state.settling.set(true);
                state.is_host.set(false);
                state.access.set(Some(access));
                state.status.set("In the jam.".to_string());
            } else {
                state
                    .status
                    .set("That jam is in a format this app cannot read.".to_string());
            }
        }
        // An empty jam is a real state — the host opened it but nothing is
        // queued yet. Join anyway and wait for them.
        Ok(relay::Fetched::Unchanged) | Ok(relay::Fetched::Missing) => {
            // A fresh jam is at version 0, which a read with have=0 reports as
            // Unchanged. Nothing to adopt yet; join and wait for the host.
            state.relay_version.set(0);
            state.shared.set(JamDoc::default());
            state.applied_rev.set(0);
            state.is_host.set(false);
            state.access.set(Some(access));
            state
                .status
                .set("In the jam — waiting for the host.".to_string());
        }
        Err(relay::RelayError::JamGone) => {
            state.status.set("That jam has ended.".to_string());
        }
        Err(e) => state.status.set(e.to_string()),
    }
}

/// Leave a jam. The other side plays on; only this device steps out.
pub fn leave_jam(mut state: JamState) {
    state.access.set(None);
    state.is_host.set(false);
    state.status.set(String::new());
}
