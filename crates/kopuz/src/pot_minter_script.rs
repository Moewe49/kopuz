//! Shared BgUtils minter JavaScript — used by BOTH the desktop wry minter
//! (`pot_minter.rs`) and the Android System WebView minter (`pot_minter_android.rs`
//! + `android-src/.../PotMinter.kt`), so the two emit byte-identical BotGuard JS.
//!
//! The script negotiates YouTube's BotGuard integrity token once (BG.Challenge →
//! snapshot → GenerateIT WAA fetch → WebPoMinter) and then mints a content PO
//! token per `videoId` locally via `mintAsWebsafeString`. The host (wry IPC on
//! desktop, an `@JavascriptInterface` on Android) receives results over
//! `window.ipc.postMessage`; the Android side rebinds `window.ipc` to its bridge
//! before injecting this, so the body below is platform-agnostic.

const BGUTILS: &str = include_str!("bgutils.js");

/// The document-start init script: installs a permissive Trusted-Types policy
/// (Chromium/WebView2/Android-WebView enforce it on the BotGuard VM's
/// `new Function`), loads BgUtils, and exposes `window.__kopuzMint(videoId, reqId)`.
pub fn init_script() -> String {
    // `PoToken.generate` does the whole BotGuard dance — VM load, snapshot, and
    // the GenerateIT WAA round-trip (jnn-pa) — on *every* call (~1 s). The
    // integrity token it negotiates is reusable for `estimatedTtlSecs` (hours)
    // and content pots are minted from it locally. So we split: negotiate the
    // integrity token + WebPoMinter once (and refresh near expiry), then per
    // track only `mintAsWebsafeString(videoId)` runs — sub-ms, no network.
    format!(
        r#"
window.module = {{ exports: {{}} }}; window.exports = window.module.exports;
// Chromium (WebView2) enforces Trusted Types on `new Function`/`eval`; the
// BotGuard VM does unwrapped `new Function(...)` internally, so a permissive
// `default` policy is needed to let those through. WebKit (Linux/macOS) ignores
// this (no enforcement). Best-effort: a pre-existing default policy throws.
try {{
  if (window.trustedTypes && window.trustedTypes.createPolicy) {{
    // A permissive `default` policy is the first line — but YouTube's page later
    // installs its OWN restrictive default policy (you can't replace one), so by
    // BotGuard-run time `new Function(str)` is blocked again. Second line: keep
    // our OWN named policy and PATCH `window.Function` (and its
    // `prototype.constructor`) so every `new Function(str)` the BotGuard VM does
    // pre-wraps the string in OUR trusted policy — bypassing whatever default
    // policy is in force. WebKit (Linux/macOS) has no enforcement; harmless there.
    try {{
      window.trustedTypes.createPolicy('default', {{
        createHTML: function(s) {{ return s; }},
        createScript: function(s) {{ return s; }},
        createScriptURL: function(s) {{ return s; }}
      }});
    }} catch (e) {{}}

    var __ttp = window.trustedTypes.createPolicy('kopuz-eval', {{ createScript: function(s) {{ return s; }} }});
    var __RF = window.Function;
    var __wrap = function() {{
      var a = Array.prototype.slice.call(arguments);
      if (a.length && typeof a[a.length - 1] === 'string') {{
        try {{ a[a.length - 1] = __ttp.createScript(a[a.length - 1]); }} catch (e) {{}}
      }}
      return __RF.apply(this, a);
    }};
    __wrap.prototype = __RF.prototype;
    try {{ Object.defineProperty(__RF.prototype, 'constructor', {{ value: __wrap, configurable: true, writable: true }}); }} catch (e) {{}}
    window.Function = __wrap;
  }}
}} catch (e) {{}}
{BGUTILS}
window.__KOPUZ_BGX = (window.module && window.module.exports) || null;
window.__kopuzMinter = null;
window.__kopuzMinterExp = 0;
window.__kopuzMinting = null;

window.__kopuzEnsureMinter = function() {{
  var now = Date.now();
  if (window.__kopuzMinter && now < window.__kopuzMinterExp) return Promise.resolve(window.__kopuzMinter);
  if (window.__kopuzMinting) return window.__kopuzMinting;
  window.__kopuzMinting = (async function() {{
    var X = window.__KOPUZ_BGX;
    if (!X || !X.BG) throw new Error('no BG');
    var BG = X.BG;
    var bgConfig = {{
      fetch: function(u, o) {{ return fetch(u, o); }},
      globalObj: window, identifier: '', requestKey: "O43z0dpjhgX20SCx4KAo"
    }};
    var ch = await BG.Challenge.create(bgConfig);
    if (!ch) throw new Error('null challenge');
    var js = ch.interpreterJavascript && ch.interpreterJavascript.privateDoNotAccessOrElseSafeScriptWrappedValue;
    if (js) {{
      var src = js;
      if (window.trustedTypes && window.trustedTypes.createPolicy) {{
        var pol = window.trustedTypes.createPolicy('kopuz-bg', {{ createScript: function(s){{ return s; }} }});
        src = pol.createScript(js);
      }}
      new Function(src)();
    }}
    var botguard = await BG.BotGuardClient.create({{ program: ch.program, globalName: ch.globalName, globalObj: window }});
    var sig = [];
    var botguardResponse = await botguard.snapshot({{ webPoSignalOutput: sig }});
    var itUrl = X.buildURL('GenerateIT', bgConfig.useYouTubeAPI);
    var itResp = await bgConfig.fetch(itUrl, {{
      method: 'POST', headers: X.getHeaders(),
      body: JSON.stringify([bgConfig.requestKey, botguardResponse])
    }});
    var it = await itResp.json();
    var itData = {{ integrityToken: it[0], estimatedTtlSecs: it[1], mintRefreshThreshold: it[2], websafeFallbackToken: it[3] }};
    var minter = await BG.WebPoMinter.create(itData, sig);
    var ttl = (itData.estimatedTtlSecs > 0) ? itData.estimatedTtlSecs : 21600;
    window.__kopuzMinter = minter;
    window.__kopuzMinterExp = Date.now() + Math.floor(ttl * 0.8) * 1000;
    return minter;
  }})();
  window.__kopuzMinting.then(function() {{ window.__kopuzMinting = null; }},
    function() {{ window.__kopuzMinting = null; window.__kopuzMinter = null; window.__kopuzMinterExp = 0; }});
  return window.__kopuzMinting;
}};

window.__kopuzMint = async function(videoId, reqId) {{
  function send(o) {{ o.id = reqId; try {{ window.ipc.postMessage(JSON.stringify(o)); }} catch(e) {{}} }}
  try {{
    var minter = await window.__kopuzEnsureMinter();
    var pot = await minter.mintAsWebsafeString(videoId);
    // Report the visitor_data of the session that produced the integrity
    // token. The player request has to present THIS one: a pot minted under
    // one anonymous session and presented under another is not a valid pot,
    // and YouTube answers that with "Sign in to confirm you're not a bot".
    var vd = '';
    try {{ vd = (window.ytcfg && window.ytcfg.get && window.ytcfg.get('VISITOR_DATA')) || ''; }} catch (e) {{}}
    send({{pot: (pot || '') + '', vd: vd + ''}});
  }} catch (e) {{
    window.__kopuzMinter = null; window.__kopuzMinterExp = 0;
    send({{err: (e && e.stack) ? e.stack : ('' + e)}});
  }}
}};

// Pre-warm the integrity token as soon as the origin is live, so even the
// first track doesn't pay the negotiation. Best-effort, backs off, then stops.
(function warm(n) {{
  if (!window.__KOPUZ_BGX || !window.__KOPUZ_BGX.BG) {{ if (n > 0) setTimeout(function() {{ warm(n - 1); }}, 500); return; }}
  window.__kopuzEnsureMinter().catch(function() {{ if (n > 0) setTimeout(function() {{ warm(n - 1); }}, 2000); }});
}})(20);
"#
    )
}
