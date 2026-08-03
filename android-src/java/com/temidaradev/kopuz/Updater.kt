package com.temidaradev.kopuz

import android.app.PendingIntent
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageInstaller
import android.net.Uri
import android.os.Build
import android.provider.Settings
import android.util.Log
import androidx.core.content.ContextCompat
import androidx.core.content.FileProvider
import java.io.File

/**
 * In-app updater. Rust downloads the new release APK and calls [install] over JNI.
 *
 * Installs through the **PackageInstaller session API** rather than firing an
 * `ACTION_VIEW` intent at the APK file. Both end in the same single system
 * confirmation (a non-privileged app can never swap itself silently), but the
 * session API:
 *
 *  - is a real *update* of the existing package — same `applicationId`, same
 *    signature, so app data (server config, YouTube cookies, playlists) is kept
 *    instead of the user landing back in onboarding;
 *  - reports the outcome back, so a failure shows up in logcat with a reason
 *    instead of the tap appearing to do nothing;
 *  - skips the "open with…" / file-handler detour the VIEW intent goes through.
 *
 * `ACTION_VIEW` remains as a fallback if the session can't be created at all.
 *
 * On Android 8+ the app needs the per-app "install unknown apps" grant first. If
 * it's missing we deep-link to that settings page; the staged APK is kept on
 * disk, so the next Update tap installs immediately without re-downloading.
 */
object Updater {
    private const val TAG = "KopuzUpdater"
    private const val ACTION_INSTALL_RESULT = "com.temidaradev.kopuz.INSTALL_RESULT"

    @JvmStatic
    fun install(context: Context, apkPath: String) {
        val app = context.applicationContext
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O &&
                !app.packageManager.canRequestPackageInstalls()
            ) {
                Log.i(TAG, "install-unknown-apps not granted — sending the user to settings")
                val grant = Intent(
                    Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES,
                    Uri.parse("package:" + app.packageName),
                ).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                context.startActivity(grant)
                return
            }

            val file = File(apkPath)
            if (!file.isFile || file.length() == 0L) {
                Log.w(TAG, "staged APK missing or empty: $apkPath")
                return
            }
            if (!installViaSession(app, file)) {
                installViaViewIntent(context, app, file)
            }
        } catch (e: Exception) {
            Log.e(TAG, "install failed", e)
        }
    }

    /** Stream the APK into a PackageInstaller session and commit it. */
    private fun installViaSession(app: Context, file: File): Boolean {
        val installer = app.packageManager.packageInstaller
        val sessionId: Int
        val session: PackageInstaller.Session
        try {
            val params = PackageInstaller.SessionParams(
                PackageInstaller.SessionParams.MODE_FULL_INSTALL,
            ).apply {
                setAppPackageName(app.packageName)
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                    // Skip the extra "app will be updated" step where the OS
                    // allows it; the commit still shows the install confirmation.
                    setRequireUserAction(PackageInstaller.SessionParams.USER_ACTION_NOT_REQUIRED)
                }
            }
            sessionId = installer.createSession(params)
            session = installer.openSession(sessionId)
        } catch (e: Exception) {
            Log.w(TAG, "cannot open install session (${e.message}) — falling back to VIEW intent")
            return false
        }
        return try {
            session.openWrite("kopuz-update", 0, file.length()).use { out ->
                file.inputStream().use { input -> input.copyTo(out) }
                session.fsync(out)
            }
            registerResultReceiver(app)
            session.commit(resultSender(app, sessionId).intentSender)
            session.close()
            Log.i(TAG, "install session $sessionId committed")
            true
        } catch (e: Exception) {
            Log.w(TAG, "session install failed (${e.message}) — falling back to VIEW intent")
            try {
                session.abandon()
            } catch (_: Exception) {
            }
            false
        }
    }

    private fun resultSender(app: Context, sessionId: Int): PendingIntent {
        val intent = Intent(ACTION_INSTALL_RESULT).setPackage(app.packageName)
        return PendingIntent.getBroadcast(
            app,
            sessionId,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_MUTABLE,
        )
    }

    @Volatile private var receiverRegistered = false

    /**
     * The system reports the session outcome here. The important branch is
     * [PackageInstaller.STATUS_PENDING_USER_ACTION]: the OS hands back the
     * confirmation dialog for us to launch — that is the one unavoidable tap.
     */
    private fun registerResultReceiver(app: Context) {
        if (receiverRegistered) return
        receiverRegistered = true
        val receiver = object : BroadcastReceiver() {
            override fun onReceive(context: Context, intent: Intent) {
                val status = intent.getIntExtra(
                    PackageInstaller.EXTRA_STATUS,
                    PackageInstaller.STATUS_FAILURE,
                )
                val message = intent.getStringExtra(PackageInstaller.EXTRA_STATUS_MESSAGE) ?: ""
                when (status) {
                    PackageInstaller.STATUS_PENDING_USER_ACTION -> {
                        val confirm = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                            intent.getParcelableExtra(Intent.EXTRA_INTENT, Intent::class.java)
                        } else {
                            @Suppress("DEPRECATION")
                            intent.getParcelableExtra<Intent>(Intent.EXTRA_INTENT)
                        }
                        if (confirm != null) {
                            confirm.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                            context.applicationContext.startActivity(confirm)
                        } else {
                            Log.w(TAG, "pending user action without a confirmation intent")
                        }
                    }
                    PackageInstaller.STATUS_SUCCESS ->
                        Log.i(TAG, "update installed — app data kept")
                    else ->
                        Log.w(TAG, "install failed: status=$status $message")
                }
            }
        }
        ContextCompat.registerReceiver(
            app,
            receiver,
            IntentFilter(ACTION_INSTALL_RESULT),
            ContextCompat.RECEIVER_NOT_EXPORTED,
        )
    }

    /** Pre-session fallback: hand the APK to whatever handles package archives. */
    private fun installViaViewIntent(context: Context, app: Context, file: File) {
        val uri = FileProvider.getUriForFile(app, app.packageName + ".fileprovider", file)
        val intent = Intent(Intent.ACTION_VIEW).apply {
            setDataAndType(uri, "application/vnd.android.package-archive")
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        }
        context.startActivity(intent)
    }
}
