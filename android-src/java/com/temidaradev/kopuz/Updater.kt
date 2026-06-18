package com.temidaradev.kopuz

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.provider.Settings
import androidx.core.content.FileProvider
import java.io.File

/**
 * In-app updater. Rust downloads the new release APK and calls [install] over
 * JNI; we hand it to Android's package installer via a FileProvider content URI.
 *
 * A sideloaded app can never update itself silently — Android always shows its
 * own "Update / Install" confirmation, and on Android 8+ the app first needs the
 * per-app "install unknown apps" grant. If that grant is missing we deep-link the
 * user to the settings page to enable it (next tap on Update then proceeds).
 */
object Updater {
    @JvmStatic
    fun install(context: Context, apkPath: String) {
        try {
            val app = context.applicationContext

            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O
                && !app.packageManager.canRequestPackageInstalls()
            ) {
                val grant = Intent(
                    Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES,
                    Uri.parse("package:" + app.packageName)
                ).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
                context.startActivity(grant)
                return
            }

            val file = File(apkPath)
            val uri = FileProvider.getUriForFile(
                app, app.packageName + ".fileprovider", file
            )
            val intent = Intent(Intent.ACTION_VIEW).apply {
                setDataAndType(uri, "application/vnd.android.package-archive")
                addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            }
            context.startActivity(intent)
        } catch (e: Exception) {
            e.printStackTrace()
        }
    }
}
