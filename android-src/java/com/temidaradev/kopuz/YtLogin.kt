package com.temidaradev.kopuz

import android.app.Dialog
import android.content.Context
import android.os.Handler
import android.os.Looper
import android.view.ViewGroup
import android.webkit.CookieManager
import android.webkit.WebView
import android.webkit.WebViewClient

/**
 * In-app YouTube/Google sign-in for Android.
 *
 * Desktop drives a real browser over CDP to mint + refresh the rotating YT
 * cookies; a phone has no such browser, so we host a WebView ourselves, let the
 * user sign in, and read the resulting cookies straight out of Android's
 * CookieManager. Those cookies feed the exact same `persist_yt_session` path the
 * desktop browser-login uses, and the in-app HTTP keepalive then keeps them warm.
 *
 * Gotcha handled here: Google refuses sign-in inside an *embedded* WebView
 * ("this browser or app may not be secure") when it detects the `; wv` WebView
 * UA marker. We override the User-Agent with a plain desktop-Chrome string so the
 * standard web sign-in proceeds — and that's also the WEB_REMIX context the app's
 * YT backend speaks, so the cookies we capture are exactly the ones it needs.
 */
object YtLogin {
    // Implemented in Rust (player::systemint::android). Empty string = cancelled.
    @JvmStatic external fun nativeOnYtCookies(cookies: String)

    @Volatile private var finished = false

    private const val DESKTOP_UA =
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 " +
            "(KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36"
    private const val SIGNIN_URL =
        "https://accounts.google.com/ServiceLogin?continue=https%3A%2F%2Fmusic.youtube.com%2F"

    @JvmStatic
    fun start(context: Context) {
        Handler(Looper.getMainLooper()).post { showDialog(context) }
    }

    private fun showDialog(context: Context) {
        finished = false

        val web = WebView(context)
        web.settings.apply {
            javaScriptEnabled = true
            domStorageEnabled = true
            userAgentString = DESKTOP_UA
        }

        val cookies = CookieManager.getInstance()
        cookies.setAcceptCookie(true)
        cookies.setAcceptThirdPartyCookies(web, true)

        val dialog = Dialog(context, android.R.style.Theme_Black_NoTitleBar_Fullscreen)
        dialog.setContentView(
            web,
            ViewGroup.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT,
            ),
        )
        dialog.setCancelable(true)
        dialog.setOnDismissListener {
            web.destroy()
            // If the user backed out before we captured a session, report a cancel
            // so the Rust side stops waiting.
            if (!finished) {
                finished = true
                nativeOnYtCookies("")
            }
        }

        web.webViewClient = object : WebViewClient() {
            override fun onPageFinished(view: WebView, url: String) {
                if (finished) return
                cookies.flush()
                val jar = cookies.getCookie("https://music.youtube.com") ?: ""
                // We're signed in once the persistent YT auth cookie is present.
                // (__Secure-3PSID is the youtube.com session cookie; __Secure-3PAPISID
                // is what the app hashes for SAPISIDHASH auth.)
                val signedIn = url.contains("music.youtube.com") &&
                    jar.contains("__Secure-3PSID") &&
                    jar.contains("__Secure-3PAPISID")
                if (signedIn) {
                    finished = true
                    nativeOnYtCookies(jar)
                    dialog.dismiss()
                }
            }
        }

        dialog.show()
        web.loadUrl(SIGNIN_URL)
    }
}
