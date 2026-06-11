# Set up Spotify import (your own app)

To clone your Spotify playlists into YouTube Music — **without** the 100-track
cap and fast — connect Kopuz to your own Spotify app once. Takes ~3 minutes,
it's free, and you only do it **once**.

> **Why?** Spotify heavily rate-limits anonymous access now (HTTP 429), which
> caps URL imports at ~100 tracks and slows them down. With your own app, Kopuz
> pages the full playlist reliably.

You only need a normal (free is fine) Spotify account.

---

## Step 1 — Create an app

1. Open **https://developer.spotify.com/dashboard** and log in.
2. Click **Create app**.
3. Fill in:
   - **App name:** `Kopuz` (anything)
   - **App description:** anything, e.g. `Import my Spotify playlists` (required)
   - **Website:** leave blank (optional)
   - **Redirect URIs:** enter **exactly** this and click **Add**:
     ```
     http://127.0.0.1:8898/callback
     ```
     ⚠️ Exactly like that — same port, `/callback` at the end, `http` not
     `https`. Otherwise sign-in fails.
   - **Which API/SDKs are you planning to use?** → check **Web API**.
   - Check the **Terms of Service**.
4. Click **Save**.

---

## Step 2 — Copy the Client ID

1. Open the app's **Settings**.
2. Copy the **Client ID** (looks like `a1b2c3d4...`).
   - You do **not** need a Client Secret — Kopuz uses PKCE (more secure, no
     secret required).

---

## Step 3 — Connect in Kopuz

1. Open the **Spotify import** dialog in Kopuz (on the Playlists page of the
   YouTube Music view → **Import from Spotify**).
2. Open the **Connect Spotify** section, paste the **Client ID**, click
   **Connect**.
3. A browser opens → **authorize once** with your Spotify account.
4. Back in Kopuz: done — your account is connected.

From now on you can:
- pick your own playlists and import them directly, or
- paste a **playlist URL** — Kopuz then uses your connected account and fetches
  **all** tracks (no 100 cap).

---

## Troubleshooting

| Symptom | Fix |
|---|---|
| "INVALID_CLIENT" / redirect error | The redirect URI must be **exactly** `http://127.0.0.1:8898/callback` (Step 1). |
| Import stops at ~100 tracks | Account not connected — connect Spotify (Step 3). Without it, Spotify itself caps you. |
| "Cannot bind 127.0.0.1:8898" | Another program is using port 8898 — close it and retry. |
| Browser doesn't open | Open the shown URL manually and authorize there. |

Your Client ID is stored **only locally** in your Kopuz config.
