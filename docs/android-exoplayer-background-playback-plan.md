# Android background playback — the real fix: native Media3 ExoPlayer + MediaSessionService

## Why the current approach can't work

Kopuz plays audio on Android with **cpal** (native AudioTrack) but drives the **queue + auto-advance + next/prev** from the **Dioxus `use_future` loop**, which runs on the wry/winit event loop. On Android, winit **suspends** that loop when the Activity is Stopped (the SurfaceView is destroyed by design — this is not configurable). So:

- The current song keeps playing (cpal is a native thread) …
- … but "next song when this ends", and the notification's next/back buttons, are driven by the suspended loop → they stop. Once silence sets in, the idle process gets frozen ("sleep mode").

A native heartbeat that only pokes the Looper (`wakeMainThread`) does **not** revive the suspended executor (verified on-device). An adversarial review also rejected a cpal-based "native gapless swap" hack (real-time-audio glitch risk, desktop-engine refactor risk, and it still wouldn't survive a true process freeze).

## What every real app (and Tauri) does

The canonical Android pattern (Android docs, Media3): **keeping the player in the UI breaks the moment the app is backgrounded — put the player + queue inside a `MediaSessionService`.** Tauri (same wry engine as Kopuz) has this *exact* bug; the ecosystem fix is `tauri-plugin-native-audio` = **Media3 ExoPlayer + MediaSessionService + foreground service**.

So: **on Android, do playback with ExoPlayer inside a MediaSessionService.** ExoPlayer natively handles, with zero UI-loop involvement: background playback, playlist + gapless **auto-advance**, **next/prev**, **lock-screen/notification** (Media3 builds it), audio focus, becoming-noisy, and proper `USAGE_MEDIA` attributes (no Samsung AudioHardening muting).

Rust stops being the Android *audio engine* and becomes the **queue model + URL resolver + UI**. cpal playback is disabled on Android only; desktop keeps cpal untouched.

## Architecture

```
Dioxus UI (Rust)  ──JNI──►  PlaybackService (Kotlin, MediaSessionService + ExoPlayer)
  - owns full queue model         - owns ExoPlayer + MediaSession + foreground notif
  - resolves YT stream URLs        - plays a short window of MediaItems (current + next)
  - reflects state in Signals      - auto-advances / next / prev NATIVELY (works backgrounded)
        ▲                                   │
        └──────── JNI callbacks ◄───────────┘  (onMediaItemTransition / onIsPlayingChanged /
                                                onPlayerError / position)
```

- **Rust → Kotlin (commands):** `native_play(items: [{url,title,artist,album,artworkUrl,videoId,durationMs}], startIndex, positionMs)`, `native_set_upcoming(items)` (append/replace the tail), `native_pause/resume/seek/stop/setVolume`.
- **Kotlin → Rust (JNI callbacks):** `onTransition(newVideoId, newIndex)`, `onStateChanged(playing, positionMs)`, `onPlayerError(videoId, code)`, `onNextExhausted()` (ExoPlayer is about to run out of buffered items → ask Rust for more).

### YT URL resolution (the one non-trivial bit)

YT stream URLs are resolved in Rust (googlevideo + decipher + PO token) and **expire**. So don't hand ExoPlayer the whole queue — hand it a **rolling window**:

1. On play: Rust resolves the **current** track's URL (+ builds a `MediaItem` with metadata) and the **next** track's URL, passes both to ExoPlayer, which plays index 0 and auto-advances to the next gaplessly.
2. `onMediaItemTransition` (ExoPlayer, native, fires in background) → JNI → Rust: (a) update `current_queue_index` + metadata Signals **on the Dioxus thread when it next ticks** (reconcile, don't re-drive playback), (b) resolve the **following** track's URL on a background thread and `native_set_upcoming` it so ExoPlayer never runs dry.
3. `next/prev` from the notification → MediaSession → `ExoPlayer.seekToNext/Previous` (native) → `onMediaItemTransition` → same sync path.
4. `onPlayerError` (usually an expired 403 URL) → JNI → Rust re-resolves that videoId and replaces the item (mirrors today's `ReResolver`).
5. End of queue: if autoradio is on, Rust (on resume, or via a background resolver thread) resolves the radio continuation and `native_set_upcoming`s it. Preferably run the "resolve next / autoradio" on a **dedicated OS-thread tokio runtime** (like the desktop `systemint` pattern) so it works while the Dioxus loop is suspended.

### Signal-safety

The ExoPlayer callbacks arrive on Kotlin threads. JNI handlers must **never** touch Dioxus Signals directly. They push into a thread-safe channel/atomic breadcrumb; the Dioxus `use_future` loop reconciles Signals (index, metadata, playing) when it next ticks (foreground/resume). This is the same reconcile seam the earlier design used, but now playback correctness never depends on it — ExoPlayer already did the right thing.

## Implementation checklist

1. **Gradle:** add Media3 deps (`androidx.media3:media3-exoplayer`, `media3-session`, `media3-exoplayer-hls`/`-dash` if needed) via `patch_gradle.py`.
2. **Kotlin `PlaybackService`** extends `androidx.media3.session.MediaSessionService`; build `ExoPlayer` + `MediaSession` in `onCreate`; `onGetSession` returns it. Media3 auto-manages the foreground notification (can replace the hand-rolled `MediaSessionHelper` notification). Add a `Player.Listener` that forwards transition/state/error to Rust JNI (`Java_com_temidaradev_kopuz_PlaybackService_native*`).
3. **Kotlin command surface** (`@JvmStatic` on a companion / a bound `MediaController`) for play/setUpcoming/pause/resume/seek/stop, called from Rust JNI.
4. **Rust `player::systemint::android` (new `exoplayer.rs`):** JNI wrappers for the commands + `extern "system"` callbacks that push breadcrumbs into a thread-safe queue the hooks crate drains.
5. **Rust player abstraction:** on `target_os="android"`, route `PlayerController` playback through the ExoPlayer commands instead of cpal `player.play()`. Keep the Rust queue model + resolve; delete the cpal decode path on Android (or leave it dormant). Reconcile UI from ExoPlayer callbacks.
6. **Resolver thread:** dedicated OS-thread tokio runtime that resolves upcoming/next/radio URLs off the Dioxus loop and feeds `native_set_upcoming`.
7. **Remove/disable** the Android cpal stream, the background heartbeat (obsolete), and hand notification building to Media3.
8. **Desktop/iOS untouched** — everything above is `#[cfg(target_os="android")]` and a Kotlin service; desktop keeps cpal + the Dioxus driver.

## Effort / risk

Substantial (new Kotlin service, JNI command+callback bridge, Android playback re-route, rolling-window URL resolver) but **low-risk architecturally** — it's the documented, battle-tested pattern; ExoPlayer owns the hard parts. Desktop is not touched. Expect a few on-device iterations for the URL-window timing and reconcile.

## References
- Android: Background playback with a MediaSessionService — https://developer.android.com/media/media3/session/background-playback
- Android 15/16 background audio hardening — https://developer.android.com/about/versions/17/changes/bg-audio
- Tauri same-bug report — https://github.com/tauri-apps/tauri/issues/12650
- tauri-plugin-native-audio (ExoPlayer + MediaSessionService for a wry app) — https://github.com/uvarov-frontend/tauri-plugin-native-audio
