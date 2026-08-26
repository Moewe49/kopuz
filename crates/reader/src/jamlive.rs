//! The shared document two people drive, and the rules for keeping a player in
//! step with it. Pure and tested — the part most likely to be subtly wrong is
//! the part kept furthest from the untestable player.
//!
//! The relay ([`crate` does not know it — it lives in the `relay` crate]) is a
//! dumb byte store with compare-and-swap. Everything about *what a jam is*
//! lives here: the queue, where the playhead sits, and — the two hard bits —
//! how a local change becomes an edit to publish, and how an incoming edit
//! becomes the smallest set of things to do to the player.
//!
//! # Two decisions worth stating
//!
//! **The playhead syncs on events, not continuously.** Play, pause, seek, skip
//! and a track change each carry a position and re-align both listeners; between
//! them, each plays on its own clock and drifts by seconds over a song. This is
//! deliberate: continuous position sync across two machines needs their clocks
//! agreed to the millisecond, and they are not. Aligning on the events people
//! actually take is honest, cheap, and close enough for listening together.
//!
//! **A fresh joiner catches up; an ongoing follower does not extrapolate.** When
//! someone joins, [`catch_up`] rolls the playhead forward by the time since the
//! last event, assuming play continued — the same trick the one-shot share used,
//! so they do not land an hour behind. Someone already in the jam polls often
//! enough that the last event is seconds old, so they just follow it.

use crate::share::SharedTrack;
use serde::{Deserialize, Serialize};

/// The whole of a live jam, as it travels between devices.
#[derive(Clone, PartialEq, Debug, Default, Serialize, Deserialize)]
pub struct JamDoc {
    pub queue: Vec<SharedTrack>,
    /// Which track in `queue` is current.
    pub index: usize,
    /// Position within that track at the moment of the last transport event.
    pub position_ms: u64,
    pub playing: bool,
    /// Unix seconds of the last transport event, so a joiner can roll the
    /// playhead forward to now. Not touched by a pure queue edit.
    pub event_at: u64,
    /// Bumped on every change. This is the app's own revision, distinct from
    /// the relay's compare-and-swap version: it is what tells a follower "this
    /// is genuinely new" rather than a re-fetch of what it already applied.
    pub rev: u64,
}

/// A local change, before it is folded into the document. Transport actions —
/// play, pause, seek, skip — all collapse to one shape because to everyone else
/// they are the same thing: the playhead is now here, playing or not.
#[derive(Clone, Debug, PartialEq)]
pub enum JamOp {
    /// Replace the whole queue and playhead. What opening a jam from an
    /// existing queue does, and what a big change (shuffle, load a playlist)
    /// becomes.
    SetQueue {
        tracks: Vec<SharedTrack>,
        index: usize,
        position_ms: u64,
        playing: bool,
    },
    /// Add one track to the end.
    AddTrack(SharedTrack),
    /// Remove the track at a position.
    RemoveAt(usize),
    /// Move a track from one position to another (a reorder).
    Move { from: usize, to: usize },
    /// The playhead moved: play/pause/seek/skip, all the same to the other side.
    Transport {
        index: usize,
        position_ms: u64,
        playing: bool,
    },
}

/// Fold a local change into the document, producing the next one to publish.
///
/// `now` stamps `event_at` for transport changes only — a queue edit does not
/// disturb where the playhead is or when it last moved, so a reorder must not
/// look to a joiner like a seek.
pub fn apply(doc: &JamDoc, op: JamOp, now: u64) -> JamDoc {
    let mut next = doc.clone();
    next.rev = doc.rev.saturating_add(1);
    match op {
        JamOp::SetQueue {
            tracks,
            index,
            position_ms,
            playing,
        } => {
            next.queue = tracks;
            next.index = clamp_index(index, next.queue.len());
            next.position_ms = position_ms;
            next.playing = playing;
            next.event_at = now;
        }
        JamOp::AddTrack(track) => {
            next.queue.push(track);
            // A track appended after the current one does not move the playhead.
        }
        JamOp::RemoveAt(at) => {
            if at < next.queue.len() {
                next.queue.remove(at);
                // Keep the same song playing under the listener's feet: an index
                // at or after the removed slot shifts back by one, and the
                // current one is never left pointing past the end.
                if at < next.index || (at == next.index && next.index == next.queue.len()) {
                    next.index = next.index.saturating_sub(1);
                }
                next.index = clamp_index(next.index, next.queue.len());
            }
        }
        JamOp::Move { from, to } => {
            let len = next.queue.len();
            if from < len && to < len && from != to {
                let track = next.queue.remove(from);
                next.queue.insert(to, track);
                // The playing track must stay the playing track wherever it
                // slid to, so its index follows the move rather than the slot.
                next.index = index_after_move(next.index, from, to);
            }
        }
        JamOp::Transport {
            index,
            position_ms,
            playing,
        } => {
            next.index = clamp_index(index, next.queue.len());
            next.position_ms = position_ms;
            next.playing = playing;
            next.event_at = now;
        }
    }
    next
}

/// Where the playhead really is now, assuming play continued since the last
/// event. For a joiner, so they do not land where the host *was* when they last
/// touched anything. Rolls through the queue if enough time has passed to have
/// finished the current track.
///
/// Returns the index and position to start at. A paused jam does not move.
pub fn catch_up(doc: &JamDoc, now: u64) -> (usize, u64) {
    if doc.queue.is_empty() {
        return (0, 0);
    }
    let mut index = clamp_index(doc.index, doc.queue.len());
    if !doc.playing {
        return (index, doc.position_ms);
    }
    let elapsed_ms = now.saturating_sub(doc.event_at).saturating_mul(1000);
    let mut position = doc.position_ms.saturating_add(elapsed_ms);
    while index < doc.queue.len() {
        let duration_ms = doc.queue[index].duration.saturating_mul(1000);
        if duration_ms == 0 || position < duration_ms {
            break;
        }
        if index + 1 == doc.queue.len() {
            position = duration_ms;
            break;
        }
        position -= duration_ms;
        index += 1;
    }
    (index, position)
}

/// What the local player currently shows, as the reconciler needs to see it.
///
/// Carries the whole tracks, not just their ids: when the local user adds a
/// song, its title and duration exist only here, so publishing must be able to
/// read them off this rather than reconstruct from an id the shared document
/// has never seen.
#[derive(Clone, Debug, PartialEq)]
pub struct LocalView {
    pub playing: bool,
    pub index: usize,
    pub position_ms: u64,
    pub queue: Vec<SharedTrack>,
}

impl LocalView {
    fn ids(&self) -> Vec<&str> {
        self.queue
            .iter()
            .filter_map(|t| t.path.as_deref())
            .collect()
    }
}

/// What the player should do to match an incoming document.
#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    /// Nothing changed that this device has not already applied.
    Nothing,
    /// The queue itself changed: rebuild it, landing on this track and position.
    ReplaceQueue {
        tracks: Vec<SharedTrack>,
        index: usize,
        position_ms: u64,
        playing: bool,
    },
    /// The queue is the same; follow the playhead to here. Carries the whole
    /// target — track, position and play state — because a transport event is
    /// atomic: a pause is a seek to the pause point *and* a stop, and returning
    /// only one of the two would drop the other. The adapter applies each part
    /// only where it differs from what the player already shows.
    Follow {
        index: usize,
        position_ms: u64,
        playing: bool,
    },
}

/// How far the local playhead may sit from the document's before a seek is
/// worth it. Below this is ordinary drift and network slop; forcing a seek for
/// it would make playback stutter for no audible gain.
pub const DRIFT_TOLERANCE_MS: u64 = 2_500;

/// Decide what the player must do to follow `incoming`, given what it shows now.
///
/// `prev_rev` is the document revision this device has already applied. When
/// `incoming.rev` is not newer, there is nothing to do — this is the common
/// case on a poll and must be cheap. Otherwise it returns the *narrowest*
/// action that fits, so following a pause does not rebuild the queue and
/// following a reorder does not reset the playhead.
pub fn reconcile(local: &LocalView, incoming: &JamDoc, prev_rev: u64, now: u64) -> Action {
    if incoming.rev <= prev_rev {
        return Action::Nothing;
    }
    let incoming_ids: Vec<&str> = incoming
        .queue
        .iter()
        .filter_map(|t| t.path.as_deref())
        .collect();
    if incoming_ids != local.ids() {
        let (index, position_ms) = catch_up(incoming, now);
        return Action::ReplaceQueue {
            tracks: incoming.queue.clone(),
            index,
            position_ms,
            playing: incoming.playing,
        };
    }

    let (target_index, target_pos) = catch_up(incoming, now);
    let index_moved = target_index != local.index;
    let seeked = local.position_ms.abs_diff(target_pos) > DRIFT_TOLERANCE_MS;
    let play_changed = incoming.playing != local.playing;
    if index_moved || seeked || play_changed {
        Action::Follow {
            index: target_index,
            position_ms: target_pos,
            playing: incoming.playing,
        }
    } else {
        // Same track, position within ordinary drift, same play state: leave the
        // player alone. This is the common outcome and must stay cheap.
        Action::Nothing
    }
}

/// What the local player did since the last agreed document, as an op to
/// publish — or `None` when nothing worth sending changed.
///
/// This is the send half, kept pure for the same reason as [`reconcile`]: it is
/// the logic that decides what goes on the wire, and getting it wrong means
/// either a jam that does not follow or one that publishes on every tick. The
/// adapter that owns the player just calls this and, on `Some`, does the write.
///
/// `shared` is the document both sides last agreed on. Normal playback advances
/// the position by about a poll interval each time; that is expected and must
/// not publish. A seek is a jump away from where continued play would have put
/// the head, and that does.
pub fn local_change(shared: &JamDoc, local: &LocalView, now: u64) -> Option<JamOp> {
    let shared_ids: Vec<&str> = shared
        .queue
        .iter()
        .filter_map(|t| t.path.as_deref())
        .collect();
    if shared_ids != local.ids() {
        // Any change to the queue's contents or order. The whole queue rather
        // than a minimal edit: correct for add, remove and reorder alike, at the
        // cost of the other side rebuilding its queue — acceptable while queues
        // are small, and the current track keeps playing across it.
        return Some(JamOp::SetQueue {
            tracks: local.queue.clone(),
            index: local.index,
            position_ms: local.position_ms,
            playing: local.playing,
        });
    }
    if local.index != shared.index || local.playing != shared.playing {
        return Some(JamOp::Transport {
            index: local.index,
            position_ms: local.position_ms,
            playing: local.playing,
        });
    }
    // A seek: the head is somewhere continued play would not have carried it.
    let expected = if shared.playing {
        shared
            .position_ms
            .saturating_add(now.saturating_sub(shared.event_at).saturating_mul(1000))
    } else {
        shared.position_ms
    };
    if local.position_ms.abs_diff(expected) > DRIFT_TOLERANCE_MS {
        return Some(JamOp::Transport {
            index: local.index,
            position_ms: local.position_ms,
            playing: local.playing,
        });
    }
    None
}

fn clamp_index(index: usize, len: usize) -> usize {
    if len == 0 { 0 } else { index.min(len - 1) }
}

/// Where an index lands after the track at `from` is moved to `to`. If the
/// index *is* the moved track it goes with it; otherwise it shifts only if the
/// move crossed over it.
fn index_after_move(index: usize, from: usize, to: usize) -> usize {
    if index == from {
        to
    } else if from < index && to >= index {
        index - 1
    } else if from > index && to <= index {
        index + 1
    } else {
        index
    }
}

/// Serialise for the wire. Its own function so the one format lives in one place.
pub fn encode(doc: &JamDoc) -> Vec<u8> {
    serde_json::to_vec(doc).unwrap_or_default()
}

/// Read one back. `None` for bytes that are not a document — an older or newer
/// app, or something else entirely under the key.
pub fn decode(bytes: &[u8]) -> Option<JamDoc> {
    serde_json::from_slice(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str, secs: u64) -> SharedTrack {
        SharedTrack {
            path: Some(format!("ytmusic:{id}")),
            title: id.to_string(),
            artist: "artist".to_string(),
            duration: secs,
        }
    }

    fn doc(ids: &[(&str, u64)], index: usize, playing: bool) -> JamDoc {
        JamDoc {
            queue: ids.iter().map(|(id, d)| track(id, *d)).collect(),
            index,
            position_ms: 0,
            playing,
            event_at: 1_000,
            rev: 1,
        }
    }

    fn view(doc: &JamDoc) -> LocalView {
        LocalView {
            playing: doc.playing,
            index: doc.index,
            position_ms: doc.position_ms,
            queue: doc.queue.clone(),
        }
    }

    #[test]
    fn adding_a_track_leaves_the_playhead_alone() {
        let d = doc(&[("a", 100), ("b", 100)], 1, true);
        let after = apply(&d, JamOp::AddTrack(track("c", 100)), 5_000);
        assert_eq!(after.queue.len(), 3);
        assert_eq!(after.index, 1, "the playing track did not move");
        assert_eq!(
            after.event_at, d.event_at,
            "a queue add is not a transport event"
        );
        assert_eq!(after.rev, 2);
    }

    #[test]
    fn removing_a_track_before_the_playhead_keeps_the_same_song_playing() {
        let d = doc(&[("a", 100), ("b", 100), ("c", 100)], 2, true);
        let after = apply(&d, JamOp::RemoveAt(0), 5_000);
        assert_eq!(after.index, 1, "c is still playing, now at index 1");
        assert_eq!(after.queue[after.index].path.as_deref(), Some("ytmusic:c"));
    }

    #[test]
    fn reordering_carries_the_playing_track_with_it() {
        let d = doc(&[("a", 100), ("b", 100), ("c", 100)], 0, true);
        // Move the playing track (a) to the end.
        let after = apply(&d, JamOp::Move { from: 0, to: 2 }, 5_000);
        assert_eq!(
            after
                .queue
                .iter()
                .filter_map(|t| t.path.as_deref())
                .collect::<Vec<_>>(),
            vec!["ytmusic:b", "ytmusic:c", "ytmusic:a"]
        );
        assert_eq!(after.index, 2, "a is still current, now at the end");
    }

    #[test]
    fn reordering_around_the_playhead_shifts_it() {
        let d = doc(&[("a", 100), ("b", 100), ("c", 100)], 1, true);
        // Move c (after the playhead) to the front (before it): b shifts back.
        let after = apply(&d, JamOp::Move { from: 2, to: 0 }, 5_000);
        assert_eq!(after.queue[after.index].path.as_deref(), Some("ytmusic:b"));
        assert_eq!(after.index, 2);
    }

    #[test]
    fn a_joiner_rolls_forward_through_a_finished_track() {
        // a is 200s long, playing from 0, and 250s have passed: we are 50s into b.
        let mut d = doc(&[("a", 200), ("b", 200)], 0, true);
        d.event_at = 1_000;
        let (index, position) = catch_up(&d, 1_250);
        assert_eq!(index, 1);
        assert_eq!(position, 50_000);
    }

    #[test]
    fn a_paused_jam_does_not_roll_forward() {
        let mut d = doc(&[("a", 200)], 0, false);
        d.position_ms = 30_000;
        d.event_at = 1_000;
        assert_eq!(catch_up(&d, 9_999), (0, 30_000));
    }

    #[test]
    fn an_already_applied_revision_is_nothing_to_do() {
        let d = doc(&[("a", 100)], 0, true);
        assert_eq!(reconcile(&view(&d), &d, d.rev, 1_000), Action::Nothing);
    }

    #[test]
    fn a_changed_queue_is_a_replace() {
        let local = doc(&[("a", 100)], 0, true);
        let mut incoming = doc(&[("a", 100), ("b", 100)], 0, true);
        incoming.rev = 2;
        match reconcile(&view(&local), &incoming, 1, 1_000) {
            Action::ReplaceQueue { tracks, .. } => assert_eq!(tracks.len(), 2),
            other => panic!("expected a replace, got {other:?}"),
        }
    }

    #[test]
    fn a_pause_from_the_other_side_carries_the_pause_point() {
        let local = doc(&[("a", 100), ("b", 100)], 0, true);
        let mut incoming = local.clone();
        incoming.playing = false;
        incoming.position_ms = 30_000;
        incoming.rev = 2;
        incoming.event_at = 1_000;
        // A pause is a stop AND a seek to where it stopped — both, in one Follow.
        assert_eq!(
            reconcile(&view(&local), &incoming, 1, 1_000),
            Action::Follow {
                index: 0,
                position_ms: 30_000,
                playing: false
            }
        );
    }

    #[test]
    fn a_skip_from_the_other_side_follows_not_rebuilds() {
        let local = doc(&[("a", 100), ("b", 100)], 0, true);
        let mut incoming = local.clone();
        incoming.index = 1;
        incoming.playing = false; // paused so catch_up does not roll it on
        incoming.rev = 2;
        incoming.event_at = 1_000;
        assert_eq!(
            reconcile(&view(&local), &incoming, 1, 1_000),
            Action::Follow {
                index: 1,
                position_ms: 0,
                playing: false
            }
        );
    }

    #[test]
    fn ordinary_drift_is_tolerated_but_a_real_seek_is_not() {
        let mut local = doc(&[("a", 300)], 0, false);
        local.position_ms = 10_000;
        let mut incoming = local.clone();
        incoming.rev = 2;

        // Within tolerance: nothing.
        incoming.position_ms = 10_000 + DRIFT_TOLERANCE_MS - 1;
        assert_eq!(
            reconcile(&view(&local), &incoming, 1, 1_000),
            Action::Nothing
        );

        // A real seek, past tolerance: correct it.
        incoming.position_ms = 60_000;
        assert_eq!(
            reconcile(&view(&local), &incoming, 1, 1_000),
            Action::Follow {
                index: 0,
                position_ms: 60_000,
                playing: false
            }
        );
    }

    #[test]
    fn a_document_survives_the_wire() {
        let d = doc(&[("a", 100), ("b", 200)], 1, true);
        assert_eq!(decode(&encode(&d)), Some(d));
        assert_eq!(decode(b"not a document"), None);
    }

    // --- the send half ---

    #[test]
    fn steady_playback_publishes_nothing() {
        let shared = doc(&[("a", 300)], 0, true); // event_at 1_000, pos 0
        let mut local = view(&shared);
        // Two seconds later, the head has advanced two seconds. Expected, quiet.
        local.position_ms = 2_000;
        assert_eq!(local_change(&shared, &local, 1_002), None);
    }

    #[test]
    fn a_local_pause_is_published() {
        let shared = doc(&[("a", 300)], 0, true);
        let mut local = view(&shared);
        local.playing = false;
        local.position_ms = 2_000;
        assert_eq!(
            local_change(&shared, &local, 1_002),
            Some(JamOp::Transport {
                index: 0,
                position_ms: 2_000,
                playing: false
            })
        );
    }

    #[test]
    fn a_local_seek_is_published_but_drift_is_not() {
        let shared = doc(&[("a", 300)], 0, true);
        let mut local = view(&shared);
        // Within tolerance of expected (2s): quiet.
        local.position_ms = 2_000 + DRIFT_TOLERANCE_MS - 1;
        assert_eq!(local_change(&shared, &local, 1_002), None);
        // A real jump forward: published.
        local.position_ms = 120_000;
        assert!(matches!(
            local_change(&shared, &local, 1_002),
            Some(JamOp::Transport {
                position_ms: 120_000,
                ..
            })
        ));
    }

    #[test]
    fn adding_a_track_locally_publishes_the_whole_queue_with_its_title() {
        let shared = doc(&[("a", 100)], 0, true);
        let mut local = view(&shared);
        local.queue.push(SharedTrack {
            path: Some("ytmusic:new".into()),
            title: "A Real Title".into(),
            artist: "Someone".into(),
            duration: 180,
        });
        match local_change(&shared, &local, 1_000) {
            Some(JamOp::SetQueue { tracks, .. }) => {
                assert_eq!(tracks.len(), 2);
                // The new track's metadata survives — the whole reason LocalView
                // carries tracks and not ids.
                assert_eq!(tracks[1].title, "A Real Title");
                assert_eq!(tracks[1].duration, 180);
            }
            other => panic!("expected a queue publish, got {other:?}"),
        }
    }

    /// The round trip that must converge: A changes something, B applies it,
    /// and B then has nothing of its own to publish. If it did, the two would
    /// publish back and forth forever.
    #[test]
    fn applying_a_change_leaves_the_follower_with_nothing_to_send() {
        let shared = doc(&[("a", 300), ("b", 300)], 0, true);
        // A pauses.
        let a_local = LocalView {
            playing: false,
            index: 0,
            position_ms: 5_000,
            queue: shared.queue.clone(),
        };
        let op = local_change(&shared, &a_local, 1_005).expect("A publishes a pause");
        let published = apply(&shared, op, 1_005);

        // B receives it and reconciles. B was still playing, roughly in step at
        // ~5s; the pause carries the point, so B follows to it and stops.
        let b_before = LocalView {
            playing: true,
            index: 0,
            position_ms: 5_000,
            queue: shared.queue.clone(),
        };
        let action = reconcile(&b_before, &published, shared.rev, 1_005);
        assert_eq!(
            action,
            Action::Follow {
                index: 0,
                position_ms: 5_000,
                playing: false
            }
        );

        // B applies it: now B looks like the published doc.
        let b_after = LocalView {
            playing: false,
            index: 0,
            position_ms: 5_000,
            queue: published.queue.clone(),
        };
        // And B, agreeing with the published doc, has nothing to send back.
        assert_eq!(
            local_change(&published, &b_after, 1_005),
            None,
            "convergence: the follower must not echo the change back"
        );
    }
}
