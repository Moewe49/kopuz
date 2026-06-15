# Plan: Android PO-token minter (fix song-skipping on the phone)

Status: **investigated + root-caused on a real device (Galaxy S24).** Not yet implemented.
Owner notes: desktop is fully working; this is purely an Android-playback gap.

## The bug (confirmed via `adb logcat -s RustStdoutStderr`)

On Android, every YT song auto-skips. Per-track log, identical each time:

```
[yt-player] resolved <id> itag=251 opus via WEB_REMIX (decipher)
[yt-player] <id> signed-in but non-Premium (itag=251) — needs a content pot, trying ANDROID_VR
[yt-player] ANDROID_VR+pot failed (PO mint: PO token minter not running — select a YouTube Music server) — trying bare clients
[yt-player] <id> no content pot available — using the non-Premium decipher stream; deep seeks may 403
symphonia probe error: range fetch 3054711-3054870 HTTP 403 Forbidden   ← track skipped
```

Root cause: anonymous/non-Premium googlevideo URLs **403 on deep/seek byte-ranges without a
content-bound PO token**. symphonia's format probe reads the file's tail (webm cues) → deep
range → 403 → probe fails → skip. (The one track that played ~1 min is the same URL getting
throttle-cut mid-stream — also no-pot.)

The PO token is minted by `crates/kopuz/src/pot_minter.rs`, which stands up a hidden **wry**
WebView and is gated `#[cfg(not(any(target_arch="wasm32", target_os="android")))]` — i.e.
**desktop-only**. On Android nothing registers a minter, so `botguard::mint_content_pot`
returns "minter not running".

`botguard::mint_content_pot(video_id)` itself is platform-agnostic and **already runs on
Android** (player::resolve calls it). We only need to **register an Android minter** that drives
a headless WebView running the same BgUtils JS.

## ⭐ SIMPLER ALTERNATIVE — TRY THIS FIRST (may make the whole minter port unnecessary)

YouTube's **TV client bypasses the PO-token requirement** (confirmed general knowledge +
matches our earlier download fix: the `tv` / `web_embedded` clients yield clean DASH opus
without a pot, which is why `ytdlp_resolve` downloads pin `player_client=tv,web_embedded`).
The app already ships a TV client: `TVHTML5_SIMPLY_EMBEDDED_PLAYER` (client_id 85) in
`crates/server/src/ytmusic/clients.rs`.

So before building the headless-WebView minter, try the cheap fix: **make the Android playback
resolver get its stream from the TV (TVHTML5) client**, which needs no content pot → no deep-
range 403 → songs play.

Steps:
1. In `crates/server/src/ytmusic/player.rs` `resolve` (the fallback chain that currently does
   WEB_REMIX decipher → "needs a content pot, trying ANDROID_VR" → bare clients), add/prefer a
   `TVHTML5_SIMPLY_EMBEDDED_PLAYER` `/player` attempt **on Android (or whenever the minter is
   unavailable / `botguard::is_available()` is false)**. Use its returned audio stream
   (itag 251/140) URL.
2. Verify the TV-client stream is range-playable by the app's range source: it must be
   `https` progressive DASH (not HLS/m3u8) and need no decipher-with-pot. (Earlier `-F` showed
   the tv client returns `251 … https … opus` — range-friendly.) If the TV `/player` returns a
   `signatureCipher` needing nsig decipher, reuse the existing decipher path.
3. Test on the S24: expect `[yt-player] resolved <id> … via TVHTML5…` and **no**
   `symphonia probe error … 403`. If TV streams play through, **skip the minter port entirely.**

Caveats to check on-device: TV-client streams may have a slightly lower max bitrate or need the
same nsig decipher; confirm they don't themselves 403 on deep ranges (they shouldn't — that's
the whole point of the TV bypass). If for some reason TV is throttled/unavailable, fall back to
the full minter port below.

## Key existing pieces (read these first)

- `crates/server/src/ytmusic/botguard.rs` — the typed channel. `MintRequest { video_id,
  reply: oneshot::Sender<Result<String,String>> }`, `set_minter(tx)`, `mint_content_pot()`
  (15s timeout). **No changes needed here** — Android just needs to call `set_minter`.
- `crates/kopuz/src/pot_minter.rs` — the desktop minter. Reuse its `init_script()` +
  `crates/kopuz/src/bgutils.js` verbatim. The JS defines `window.__kopuzMint(videoId, reqId)`
  which mints and posts `{id, pot}` or `{id, err}` back via `window.ipc.postMessage(...)`.
  It pre-warms the integrity token and caches the `WebPoMinter` (per-track mint is sub-ms).
- `crates/player/src/systemint/android.rs` — the JNI bridge to copy the pattern from
  (`find_app_class`, `attach_current_thread`, `ndk_context::android_context()`, and the
  `Java_com_temidaradev_kopuz_<Class>_native...` callbacks). The new `YtLogin` login WebView
  (`android-src/.../YtLogin.kt` + `launch_login`/`nativeOnYtCookies`) is the closest template.

## Architecture (Android)

Mirror the in-app-login WebView, but **headless + long-lived**, and wire it to the botguard
channel:

1. **Kotlin `android-src/java/com/temidaradev/kopuz/PotMinter.kt`** (object):
   - `init(context, initScript)`: create an offscreen `WebView` (1×1, parked off-screen, never
     attached visibly — like `YtLogin` but never shown). UA = a desktop Chrome string (same as
     `pot_minter.rs` UA). **Inject `initScript` at document-start** (see androidx.webkit note),
     then `loadUrl("https://music.youtube.com/")`. Keep the WebView alive for the app lifetime
     (store in a static).
   - JS→Kotlin bridge: replace wry's `window.ipc.postMessage` with an Android
     `@JavascriptInterface`. Add `addJavascriptInterface(Bridge(), "kopuzIpc")` and in the
     init script the JS should post via `window.kopuzIpc.post(JSON)` — i.e. the Android
     init-script variant rebinds `window.ipc = { postMessage: (s) => window.kopuzIpc.post(s) }`
     (do this in Kotlin/JS, not by editing the shared JS) so `bgutils.js`'s
     `window.ipc.postMessage` keeps working unchanged.
   - `mint(videoId, reqId)`: `webView.evaluateJavascript("window.__kopuzMint && window.__kopuzMint('$videoId', $reqId)", null)` on the UI thread.
   - `Bridge.post(json)` (the `@JavascriptInterface`): parse `{id, pot|err}` and call the Rust
     native `PotMinter.nativeOnPot(reqId, pot, err)`.

2. **Rust JNI (in `crates/player/src/systemint/android.rs`, reusing its helpers):**
   - `pub fn pot_minter_init(script: &str)` → JNI call `PotMinter.init(context, script)`.
   - `pub fn pot_minter_mint(video_id: &str, req_id: u64)` → JNI call `PotMinter.mint(...)`.
   - `extern "system" fn Java_com_temidaradev_kopuz_PotMinter_nativeOnPot(env, _cls, reqId: jlong, pot: JString, err: JString)`
     → look up the pending reply by `reqId` in a `static Mutex<HashMap<u64, oneshot::Sender<Result<String,String>>>>`
     and `send(Ok(pot))` / `send(Err(err))`. (Empty `pot` + non-empty `err` ⇒ Err.)
   - Expose `pot_minter_init`/`pot_minter_mint` through `player::systemint` (cross-platform
     no-op stubs on non-android, like `start_yt_login`).

3. **Driver (small, in `crates/kopuz/src/pot_minter.rs` under an Android cfg branch, OR a new
   `pot_minter_android` module):**
   - When an anon/cookie YT Music server is active (the existing `request()` trigger), once:
     `player::systemint::pot_minter_init(&init_script())` (reuse the desktop `init_script()` —
     move it + `BGUTILS` out of the `#[cfg(not(android))]` block so both targets share it).
   - Create the botguard channel: `let (tx, rx) = mpsc::unbounded_channel(); botguard::set_minter(tx);`
   - Spawn a tokio task: `while let Some(req) = rx.recv().await { let id = next_id(); PENDING.insert(id, req.reply); player::systemint::pot_minter_mint(&sanitize(req.video_id), id); }`
     (sanitize videoId to `[A-Za-z0-9_-]` like desktop `pump()`). The `nativeOnPot` callback
     resolves `req.reply`. botguard's own 15s timeout covers a lost reply.
   - `main.rs`: the `crate::pot_minter::request()` `use_effect` at ~line 1118 is currently
     `#[cfg(not(any(wasm32, android)))]`. Add an Android arm that calls the Android init/driver
     (it must run for **signed-in non-Premium too**, not just anon — the device log shows a
     signed-in cookie session still needs the content pot).

## The androidx.webkit dependency (document-start injection)

The desktop minter injects at **document-start** to capture `window.module.exports.BG` before
the YT page clobbers `window.module`. Plain Android `WebView` can't inject pre-page-JS without
`androidx.webkit`'s `WebViewCompat.addDocumentStartJavaScript(webView, script, setOf("*"))`
(needs `androidx.webkit:webkit:1.12.x` and a `WebViewFeature.isFeatureSupported(DOCUMENT_START_SCRIPT)` guard).

Add the gradle dep in the **`android-patch` Justfile recipe** (the dx-generated project doesn't
include it): after `dx build` scaffolds the project and before `./gradlew assembleDebug`, patch
`target/dx/kopuz/release/android/app/app/build.gradle*` to add
`implementation("androidx.webkit:webkit:1.12.1")` to its `dependencies { }` block (a small
python/sed step, like `patch_manifest.py`). **First task tomorrow: inspect the generated
build.gradle path/format** (Kotlin DSL vs Groovy) so the injection is correct — get this from a
CI run artifact or by reading the dx 0.7 android template. If document-start proves flaky,
fallback: inject in `onPageStarted` (may race the page; test first).

## Implementation order (each step = CI build → `adb install` → logcat)

1. Add androidx.webkit gradle injection to the Justfile; confirm a build still succeeds + the
   dep resolves. (De-risks the unknown first.)
2. `PotMinter.kt` (headless WebView + JS interface + document-start inject) + the Rust JNI
   (`pot_minter_init`/`pot_minter_mint`/`nativeOnPot`) + `player::systemint` exports.
3. Share `init_script()`/`bgutils.js` across targets; add the Android driver + botguard
   registration + the `main.rs` Android trigger.
4. Test on the S24: play the Stateside playlist; in logcat expect
   `[yt-player] … content pot minted` (add a log) and **no** `symphonia probe error … 403`.
   Songs should play through. If the integrity-token negotiation fails in the headless WebView,
   check the BgUtils `requestKey`/CSP/trustedTypes path (WebView2-style Trusted Types differ
   from Android System WebView — the `default` policy in init_script should cover it, but log
   the JS error from `nativeOnPot`'s `err`).

## Risks / unknowns
- Android System WebView may handle Trusted Types / `new Function` differently than wry's
  WebView2/WebKit — the BgUtils VM might need a tweak (the `err` channel will show it).
- `addDocumentStartJavaScript` requires a recent WebView (S24 is fine; older devices may lack
  `DOCUMENT_START_SCRIPT` → guard + fallback).
- Headless WebView lifecycle on Android (must survive Activity backgrounding; keep a static ref
  and ideally create it from the Application/Activity context, not a transient one).

## Current Android state (done before this)
- cdp desktop-only module: Android/iOS stub (compiles).
- ABI: build targets `aarch64-linux-android` (arm64). 32-bit devices unsupported (manganis).
- TLS crash fixed: `[patch] vendor/rustls-platform-verifier` → WebPKI roots (no JNI).
- In-app WebView Google login: **built but Google blocks embedded-WebView sign-in** ("browser
  not secure"); parked. Cookie-paste + the now-working `/verify_session` keepalive is the live
  login path and **stays signed in** (confirmed in logcat).
- Distribution: each CI build uses a fresh debug key → must `adb uninstall` before installing a
  new APK. APK on the GitHub release `v0.7.53` (`Kopuz-android.apk`).
