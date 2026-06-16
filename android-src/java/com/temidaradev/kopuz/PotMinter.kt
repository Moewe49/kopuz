package com.temidaradev.kopuz

import android.annotation.SuppressLint
import android.content.Context
import android.os.Handler
import android.os.Looper
import android.webkit.JavascriptInterface
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.webkit.WebViewCompat
import androidx.webkit.WebViewFeature
import org.json.JSONObject

/**
 * Headless BgUtils PoToken minter for Android.
 *
 * The desktop build mints content PO tokens in a hidden wry WebView; a phone has
 * no such thing, so we host an offscreen System WebView at the music.youtube.com
 * origin (same-origin so BgUtils' WAA fetch isn't CORS-blocked), inject the SAME
 * BgUtils BotGuard JS at document-start, and mint a token per video. Results go
 * back to Rust via [nativeOnPot]. The WebView is created once and held in a
 * static so it survives Activity backgrounding.
 *
 * Document-start injection (so our script runs before YouTube's page clobbers
 * window.module) uses androidx.webkit's WebViewCompat.addDocumentStartJavaScript,
 * the Android analog of wry's init script; window.ipc is rebound to an
 * @JavascriptInterface so the shared BgUtils JS posts results back unchanged.
 */
object PotMinter {
    // Implemented in Rust (player::systemint::android). reqId echoes the request;
    // a non-empty pot is success, otherwise err carries the JS error/stack.
    @JvmStatic external fun nativeOnPot(reqId: Long, pot: String, err: String)

    private const val DESKTOP_UA =
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 " +
            "(KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36"
    private const val ORIGIN = "https://music.youtube.com"

    @Volatile private var webView: WebView? = null

    @JvmStatic
    fun init(context: Context, script: String) {
        if (webView != null) return
        val app = context.applicationContext
        Handler(Looper.getMainLooper()).post { setup(app, script) }
    }

    @SuppressLint("SetJavaScriptEnabled")
    private fun setup(context: Context, script: String) {
        if (webView != null) return
        val web = WebView(context)
        web.settings.apply {
            javaScriptEnabled = true
            domStorageEnabled = true
            userAgentString = DESKTOP_UA
        }
        web.addJavascriptInterface(Bridge(), "kopuzIpc")

        // Rebind window.ipc to our bridge so the shared BgUtils JS (which posts via
        // window.ipc.postMessage) works unchanged, then run the BotGuard script.
        val full = "window.ipc={postMessage:function(s){window.kopuzIpc.post(s);}};\n$script"
        if (WebViewFeature.isFeatureSupported(WebViewFeature.DOCUMENT_START_SCRIPT)) {
            WebViewCompat.addDocumentStartJavaScript(web, full, setOf(ORIGIN))
        } else {
            // Fallback: inject as early as possible (races the page clobbering
            // window.module — best-effort on WebViews without DOCUMENT_START_SCRIPT).
            web.webViewClient = object : WebViewClient() {
                override fun onPageStarted(
                    view: WebView,
                    url: String,
                    favicon: android.graphics.Bitmap?,
                ) {
                    view.evaluateJavascript(full, null)
                }
            }
        }
        webView = web
        web.loadUrl("$ORIGIN/")
    }

    @JvmStatic
    fun mint(videoId: String, reqId: Long) {
        Handler(Looper.getMainLooper()).post {
            val web = webView
            if (web == null) {
                nativeOnPot(reqId, "", "minter webview not initialized")
                return@post
            }
            web.evaluateJavascript(
                "window.__kopuzMint && window.__kopuzMint('$videoId', $reqId)",
                null,
            )
        }
    }

    private class Bridge {
        @JavascriptInterface
        fun post(json: String) {
            try {
                val o = JSONObject(json)
                val id = o.optLong("id", -1L)
                if (id < 0) return
                val pot = o.optString("pot", "")
                val err = o.optString("err", "")
                nativeOnPot(id, pot, err)
            } catch (e: Exception) {
                // Malformed message — ignore.
            }
        }
    }
}
