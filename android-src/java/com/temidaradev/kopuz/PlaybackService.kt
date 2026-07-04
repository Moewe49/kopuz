package com.temidaradev.kopuz

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.Uri
import android.os.Build
import android.os.Handler
import android.os.Looper
import androidx.core.app.NotificationCompat
import androidx.core.app.ServiceCompat
import androidx.media3.common.AudioAttributes
import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.common.MediaMetadata
import androidx.media3.common.PlaybackException
import androidx.media3.common.Player
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.session.MediaSession
import androidx.media3.session.MediaSessionService
import androidx.media3.session.MediaStyleNotificationHelper
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
        createNotificationChannel()
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
                refreshNotification()
            }

            override fun onIsPlayingChanged(isPlaying: Boolean) {
                nativeOnState(isPlaying, player.currentPosition, knownDuration(player))
                refreshNotification()
            }

            override fun onPlaybackStateChanged(state: Int) {
                if (state == Player.STATE_ENDED) nativeOnEnded()
                refreshNotification()
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

        // Report the live position ~2×/s from the main thread (ExoPlayer.position
        // is main-thread-only) so Rust can cache it for the progress bar.
        positionTicker.post(positionTick)
    }

    private val positionTicker = Handler(Looper.getMainLooper())
    private val positionTick = object : Runnable {
        override fun run() {
            mediaSession?.player?.let {
                nativeOnState(it.isPlaying, it.currentPosition, knownDuration(it))
            }
            positionTicker.postDelayed(this, 500)
        }
    }

    override fun onGetSession(controllerInfo: MediaSession.ControllerInfo): MediaSession? =
        mediaSession

    // CRITICAL for background playback: this app drives ExoPlayer directly (no
    // MediaController connects), so Media3's automatic foreground promotion
    // never triggered — the process stayed CACHED and Samsung froze it after a
    // few songs (engine thread suspended → look-ahead stopped refilling →
    // playback died). We foreground the service OURSELVES, immediately, so the
    // OS keeps the process (and the native engine thread) alive in the
    // background. startForeground here also satisfies the 5s deadline of
    // startForegroundService BEFORE ExoPlayer buffers the network URL.
    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        goForeground()
        return super.onStartCommand(intent, flags, startId)
    }

    private fun goForeground() {
        try {
            ServiceCompat.startForeground(
                this,
                NOTIF_ID,
                buildNotification(),
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q)
                    ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK
                else 0,
            )
        } catch (e: Exception) {
            e.printStackTrace()
        }
    }

    /** Re-post the notification so its title/state track the current song. */
    private fun refreshNotification() {
        if (INSTANCE == null) return
        try {
            val mgr = getSystemService(NotificationManager::class.java)
            mgr?.notify(NOTIF_ID, buildNotification())
        } catch (_: Exception) {}
    }

    private fun buildNotification(): android.app.Notification {
        val meta = mediaSession?.player?.mediaMetadata
        val open = packageManager.getLaunchIntentForPackage(packageName)?.let {
            PendingIntent.getActivity(
                this, 0, it,
                PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
            )
        }
        val b = NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(android.R.drawable.ic_media_play)
            .setContentTitle(meta?.title ?: "Kopuz")
            .setContentText(meta?.artist ?: "")
            .setOngoing(true)
            .setContentIntent(open)
            .setVisibility(NotificationCompat.VISIBILITY_PUBLIC)
        mediaSession?.let { b.setStyle(MediaStyleNotificationHelper.MediaStyle(it)) }
        return b.build()
    }

    private fun createNotificationChannel() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val ch = NotificationChannel(
                CHANNEL_ID,
                "Playback",
                NotificationManager.IMPORTANCE_LOW,
            ).apply { setShowBadge(false) }
            getSystemService(NotificationManager::class.java)?.createNotificationChannel(ch)
        }
    }

    // Keep playing when the task is swiped away (a real music app doesn't stop).
    override fun onTaskRemoved(rootIntent: Intent?) {
        // no-op: don't stopSelf; playback continues via the foreground service.
    }

    override fun onDestroy() {
        positionTicker.removeCallbacks(positionTick)
        ServiceCompat.stopForeground(this, ServiceCompat.STOP_FOREGROUND_REMOVE)
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
        private const val NOTIF_ID = 1001
        private const val CHANNEL_ID = "kopuz_playback"

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

        /** Append freshly-resolved look-ahead items to the end of the playlist. The
         *  engine only ever sends NEW tracks (it tracks how far it has resolved), so
         *  a plain append keeps the rolling window without disturbing played items. */
        @JvmStatic
        fun cmdSetUpcoming(itemsJson: String) {
            val items = parseItems(itemsJson)
            if (items.isEmpty()) return
            onPlayer { p -> p.addMediaItems(items) }
        }

        /** Replace everything AFTER the currently-playing item with `itemsJson`
         *  (used when shuffle is toggled). The current item is untouched, so it
         *  keeps playing with no re-buffer; only the upcoming tail changes. An
         *  empty list just trims the stale upcoming items. */
        @JvmStatic
        fun cmdReplaceUpcoming(itemsJson: String) {
            val items = parseItems(itemsJson)
            onPlayer { p ->
                val from = p.currentMediaItemIndex + 1
                if (from in 1 until p.mediaItemCount) {
                    p.removeMediaItems(from, p.mediaItemCount)
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
        @JvmStatic external fun nativeOnState(isPlaying: Boolean, positionMs: Long, durationMs: Long)
        @JvmStatic external fun nativeOnEnded()
        @JvmStatic external fun nativeOnError(mediaId: String, errorCode: Int)

        /** Real media duration once ExoPlayer knows it, else 0. Track metadata
         *  often lacks a duration (e.g. YT search results), so this feeds the
         *  Rust side the authoritative value for the progress bar + seek max. */
        private fun knownDuration(p: Player): Long =
            p.duration.let { if (it == C.TIME_UNSET || it < 0) 0L else it }

        // --- helpers -------------------------------------------------------

        private fun ensureStarted(context: Context) {
            if (INSTANCE != null) return
            val intent = Intent(context, PlaybackService::class.java)
            try {
                // startForegroundService (NOT plain startService): the service's
                // onStartCommand calls startForeground() IMMEDIATELY (before
                // ExoPlayer buffers the network URL), so it satisfies the 5s
                // deadline AND the process becomes a real foreground service.
                // Plain startService left it CACHED → Samsung froze it after a
                // few songs and playback died. First play is always from the
                // foregrounded app, so starting the FGS is allowed.
                androidx.core.content.ContextCompat.startForegroundService(context, intent)
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
