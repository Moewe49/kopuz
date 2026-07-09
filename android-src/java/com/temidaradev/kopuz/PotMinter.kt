package com.temidaradev.kopuz

import android.annotation.SuppressLint
import android.app.Activity
import android.content.Context
import android.os.Handler
import android.os.Looper
import android.util.Log
import android.webkit.ConsoleMessage
import android.webkit.CookieManager
import android.webkit.JavascriptInterface
import android.webkit.WebChromeClient
import android.webkit.WebView
import android.webkit.WebViewClient
import android.widget.FrameLayout
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
 * back to Rust via [nativeOnPot].
 *
 * The WebView is attached to the Activity content view at 1x1 px (NOT shown):
 * an unattached/offscreen Android WebView throttles JS timers and may never
 * complete a page load, so BgUtils' setup never runs. Attaching it (invisibly)
 * keeps the renderer live. Document-start injection uses androidx.webkit's
 * WebViewCompat.addDocumentStartJavaScript; window.ipc is rebound to an
 * @JavascriptInterface so the shared BgUtils JS posts results back unchanged.
 */
object PotMinter {
    private const val TAG = "PotMinter"

    // Implemented in Rust (player::systemint::android). reqId echoes the request;
    // a non-empty pot is success, otherwise err carries the JS error/stack.
    @JvmStatic external fun nativeOnPot(reqId: Long, pot: String, err: String)

    private const val DESKTOP_UA =
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 " +
            "(KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36"
    // www.youtube.com (NOT music.youtube.com) + anonymous: signed-in
    // music.youtube.com now serves a strict CSP with no `unsafe-eval` that blocks
    // the BotGuard `new Function`; anonymous www.youtube.com still allows eval.
    // Verified on desktop. The shared BgUtils script's Function/Trusted-Types
    // patch does the rest.
    private const val ORIGIN = "https://www.youtube.com"

    @Volatile private var webView: WebView? = null

    @JvmStatic
    fun init(context: Context, script: String, cookies: String) {
        if (webView != null) return
        Handler(Looper.getMainLooper()).post { setup(context, script, cookies) }
    }

    @SuppressLint("SetJavaScriptEnabled")
    private fun setup(context: Context, script: String, cookies: String) {
        if (webView != null) return
        val web = WebView(context)
        web.settings.apply {
            javaScriptEnabled = true
            domStorageEnabled = true
            userAgentString = DESKTOP_UA
        }

        // Load ANONYMOUSLY: signing in makes YouTube serve the strict CSP that
        // blocks the BotGuard eval, so we must NOT inject the user's YT cookies.
        // (Verified on desktop: anonymous www.youtube.com loads directly with no
        // consent-wall redirect, unlike music.youtube.com.) `cookies` is now unused.
        val cm = CookieManager.getInstance()
        cm.setAcceptCookie(true)
        cm.setAcceptThirdPartyCookies(web, true)

        web.addJavascriptInterface(Bridge(), "kopuzIpc")

        // Surface page console + load lifecycle to logcat so on-device debugging
        // can see BgUtils / Trusted-Types failures.
        web.webChromeClient = object : WebChromeClient() {
            override fun onConsoleMessage(m: ConsoleMessage): Boolean {
                Log.i(TAG, "console: ${m.message()} @${m.sourceId()}:${m.lineNumber()}")
                return true
            }
        }

        // Rebind window.ipc to our bridge so the shared BgUtils JS (which posts via
        // window.ipc.postMessage) works unchanged, then run the BotGuard script.
        val full = "window.ipc={postMessage:function(s){window.kopuzIpc.post(s);}};\n$script"
        if (WebViewFeature.isFeatureSupported(WebViewFeature.DOCUMENT_START_SCRIPT)) {
            WebViewCompat.addDocumentStartJavaScript(web, full, setOf(ORIGIN))
            web.webViewClient = object : WebViewClient() {
                override fun onPageFinished(view: WebView, url: String) {
                    Log.i(TAG, "page loaded: $url")
                }
                override fun onReceivedError(
                    view: WebView,
                    request: android.webkit.WebResourceRequest,
                    error: android.webkit.WebResourceError,
                ) {
                    if (request.isForMainFrame) Log.w(TAG, "load error: ${error.description}")
                }
            }
        } else {
            // Fallback: inject as early as possible (races the page clobbering
            // window.module — best-effort on WebViews without DOCUMENT_START_SCRIPT).
            Log.w(TAG, "DOCUMENT_START_SCRIPT unsupported — onPageStarted fallback")
            web.webViewClient = object : WebViewClient() {
                override fun onPageStarted(
                    view: WebView,
                    url: String,
                    favicon: android.graphics.Bitmap?,
                ) {
                    view.evaluateJavascript(full, null)
                }
                override fun onPageFinished(view: WebView, url: String) {
                    Log.i(TAG, "page loaded (fallback): $url")
                }
            }
        }

        // Attach invisibly so the renderer stays live (an unattached WebView
        // stalls JS). 1x1 px in the Activity content; never visible.
        val activity = context as? Activity
        if (activity != null) {
            activity.addContentView(web, FrameLayout.LayoutParams(1, 1))
        } else {
            Log.w(TAG, "context is not an Activity — WebView left detached (may stall)")
        }

        webView = web
        web.loadUrl("$ORIGIN/")
        Log.i(TAG, "minter webview created, loading $ORIGIN")
    }

    @JvmStatic
    fun mint(videoId: String, reqId: Long) {
        Handler(Looper.getMainLooper()).post {
            val web = webView
            if (web == null) {
                nativeOnPot(reqId, "", "minter webview not initialized")
                return@post
            }
            // If __kopuzMint isn't defined yet (page still loading / BgUtils not
            // ready), report back immediately so the resolver falls back fast
            // instead of stalling on the 15s mint timeout. When it IS ready the
            // real pot/err arrives later via the bridge.
            web.evaluateJavascript(
                "(window.__kopuzMint ? (window.__kopuzMint('$videoId', $reqId), 'ok') : 'notready')",
            ) { result ->
                if (result != null && result.contains("notready")) {
                    nativeOnPot(reqId, "", "minter not ready")
                }
            }
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
