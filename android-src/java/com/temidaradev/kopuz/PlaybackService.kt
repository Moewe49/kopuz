package com.temidaradev.kopuz

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Handler
import android.os.Looper
import androidx.media3.common.AudioAttributes
import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.common.MediaMetadata
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.session.MediaSession
import androidx.media3.session.MediaSessionService
import org.json.JSONArray

/**
 * Native Android playback via Media3 ExoPlayer inside a MediaSessionService.
 *
 * Kopuz drives the queue + resolves stream URLs in Rust, but the ACTUAL playback
 * must live in a MediaSessionService so it survives the app being backgrounded —
 * the wry/Dioxus event loop (and the old cpal-driven auto-advance) is suspended by
 * Android when the Activity is Stopped. ExoPlayer + MediaSessionService natively
 * handle background playback, gapless auto-advance, next/prev, the media
 * notification/lock-screen, audio focus and becoming-noisy — none of which depend
 * on the UI loop. See docs/android-exoplayer-background-playback-plan.md.
 *
 * Rust → Kotlin: the `cmd*` static methods (called over JNI).
 * Kotlin → Rust: the `native*` external methods (Player.Listener forwards events).
 */
class PlaybackService : MediaSessionService() {

    private var mediaSession: MediaSession? = null

    override fun onCreate() {
        super.onCreate()
        val player = ExoPlayer.Builder(this)
            // USAGE_MEDIA + focus handling → Samsung stops "AudioHardening"-muting us,
            // and calls/other media duck/pause us correctly.
            .setAudioAttributes(
                AudioAttributes.Builder()
                    .setUsage(C.USAGE_MEDIA)
                    .setContentType(C.AUDIO_CONTENT_TYPE_MUSIC)
                    .build(),
                /* handleAudioFocus = */ true
            )
            .setHandleAudioBecomingNoisy(true)
            .build()

        player.addListener(object : Player.Listener {
            override fun onMediaItemTransition(mediaItem: MediaItem?, reason: Int) {
                nativeOnTransition(mediaItem?.mediaId ?: "", player.currentMediaItemIndex)
            }

            override fun onIsPlayingChanged(isPlaying: Boolean) {
                nativeOnState(isPlaying, player.currentPosition)
            }

            override fun onPlaybackStateChanged(state: Int) {
                if (state == Player.STATE_ENDED) nativeOnEnded()
            }

            override fun onPlayerError(error: PlaybackException) {
                nativeOnError(
                    player.currentMediaItem?.mediaId ?: "",
                    error.errorCode
                )
            }
        })

        mediaSession = MediaSession.Builder(this, player).build()
        INSTANCE = this
        // Apply anything queued before the service finished starting.
        pending?.let { it(player) }
        pending = null
    }

    override fun onGetSession(controllerInfo: MediaSession.ControllerInfo): MediaSession? =
        mediaSession

    // Keep playing when the task is swiped away (a real music app doesn't stop).
    override fun onTaskRemoved(rootIntent: Intent?) {
        // no-op: don't stopSelf; playback continues via the foreground service.
    }

    override fun onDestroy() {
        mediaSession?.run {
            player.release()
            release()
        }
        mediaSession = null
        if (INSTANCE === this) INSTANCE = null
        super.onDestroy()
    }

    companion object {
        @Volatile private var INSTANCE: PlaybackService? = null
        // A command that arrived before onCreate finished; replayed once the player exists.
        @Volatile private var pending: ((Player) -> Unit)? = null
        private val main = Handler(Looper.getMainLooper())

        // --- Rust → Kotlin commands (called over JNI) ----------------------

        /** Start the service (if needed) and play `itemsJson` from `startIndex`/`positionMs`. */
        @JvmStatic
        fun cmdPlay(context: Context, itemsJson: String, startIndex: Int, positionMs: Long) {
            val items = parseItems(itemsJson)
            ensureStarted(context)
            onPlayer { p ->
                p.setMediaItems(items, startIndex, positionMs.coerceAtLeast(0))
                p.prepare()
                p.playWhenReady = true
            }
        }

        /** Replace the items AFTER the current one (the rolling look-ahead window). */
        @JvmStatic
        fun cmdSetUpcoming(itemsJson: String) {
            val items = parseItems(itemsJson)
            onPlayer { p ->
                val keep = p.currentMediaItemIndex
                if (p.mediaItemCount > keep + 1) {
                    p.removeMediaItems(keep + 1, p.mediaItemCount)
                }
                if (items.isNotEmpty()) p.addMediaItems(items)
            }
        }

        @JvmStatic fun cmdPause() = onPlayer { it.playWhenReady = false }
        @JvmStatic fun cmdResume() = onPlayer { it.playWhenReady = true }
        @JvmStatic fun cmdNext() = onPlayer { if (it.hasNextMediaItem()) it.seekToNextMediaItem() }
        @JvmStatic fun cmdPrev() = onPlayer { it.seekToPreviousMediaItem() }
        @JvmStatic fun cmdSeek(positionMs: Long) = onPlayer { it.seekTo(positionMs) }
        @JvmStatic fun cmdSetVolume(volume: Float) = onPlayer { it.volume = volume.coerceIn(0f, 1f) }

        @JvmStatic
        fun cmdStop(context: Context) {
            onPlayer { it.stop() }
            try {
                context.stopService(Intent(context, PlaybackService::class.java))
            } catch (_: Exception) {}
        }

        /** Current playback position in ms (or -1 if no player). Polled by Rust for the UI. */
        @JvmStatic
        fun cmdPosition(): Long =
            INSTANCE?.mediaSession?.player?.let {
                if (Looper.myLooper() == Looper.getMainLooper()) it.currentPosition else -1L
            } ?: -1L

        // --- Kotlin → Rust callbacks (implemented in Rust via JNI) ---------

        @JvmStatic external fun nativeOnTransition(mediaId: String, index: Int)
        @JvmStatic external fun nativeOnState(isPlaying: Boolean, positionMs: Long)
        @JvmStatic external fun nativeOnEnded()
        @JvmStatic external fun nativeOnError(mediaId: String, errorCode: Int)

        // --- helpers -------------------------------------------------------

        private fun ensureStarted(context: Context) {
            if (INSTANCE != null) return
            val intent = Intent(context, PlaybackService::class.java)
            try {
                // NOT startForegroundService: that imposes a 5s "call startForeground()"
                // deadline, but ExoPlayer may still be buffering the (network) URL at that
                // point → ForegroundServiceDidNotStartInTime crash. Media3 promotes the
                // service to foreground itself once playback actually starts. First play is
                // always from the foregrounded app, so a plain startService is allowed.
                context.startService(intent)
            } catch (e: Exception) {
                e.printStackTrace()
            }
        }

        /** Run `block` on the ExoPlayer on the main thread; queue it if the service isn't up yet. */
        private fun onPlayer(block: (Player) -> Unit) {
            val inst = INSTANCE
            if (inst == null) {
                // Chain onto any existing pending command so none are lost.
                val prev = pending
                pending = { p -> prev?.invoke(p); block(p) }
                return
            }
            main.post {
                inst.mediaSession?.player?.let(block)
            }
        }

        private fun parseItems(json: String): List<MediaItem> {
            val out = ArrayList<MediaItem>()
            try {
                val arr = JSONArray(json)
                for (i in 0 until arr.length()) {
                    val o = arr.getJSONObject(i)
                    val url = o.optString("url")
                    if (url.isEmpty()) continue
                    val metaBuilder = MediaMetadata.Builder()
                        .setTitle(o.optString("title"))
                        .setArtist(o.optString("artist"))
                        .setAlbumTitle(o.optString("album"))
                    val art = o.optString("artworkUrl")
                    if (art.isNotEmpty()) metaBuilder.setArtworkUri(Uri.parse(art))
                    val dur = o.optLong("durationMs", 0L)
                    out.add(
                        MediaItem.Builder()
                            .setMediaId(o.optString("mediaId"))
                            .setUri(url)
                            .setMediaMetadata(metaBuilder.build())
                            .build()
                    )
                    // dur is carried in metadata by ExoPlayer once loaded; kept in JSON for Rust.
                    if (dur < 0) { /* placeholder to keep dur referenced */ }
                }
            } catch (e: Exception) {
                e.printStackTrace()
            }
            return out
        }
    }
}
