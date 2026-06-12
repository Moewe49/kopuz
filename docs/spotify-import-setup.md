# Spotify import

Kopuz can clone Spotify playlists into YouTube Music. There are two paths — for
the common case you need to **set up nothing**.

---

## Path 1 — By playlist URL (recommended, no login)

Works for **any public playlist** — anyone's, any size:

1. In Spotify, open the playlist → **Share → Copy link**.
2. In Kopuz, open the **Spotify import** dialog (Playlists page of the YouTube
   Music view → **Import from Spotify**).
3. On the **URL** tab, paste the link and click **Import**.

Kopuz fetches **all** tracks — no 100-track cap, no sign-in, no API key. This
works because Kopuz uses the same internal endpoint the Spotify web player
itself uses.

> Requirement: you must be **signed in to YouTube Music** in Kopuz, since the
> cloned playlist lands in your YT Music account.

---

## Path 2 — Connect your account (only for *private* playlists & Liked Songs)

Only needed to import **your own private** playlists or your **Liked Songs** —
those aren't reachable by URL. Public playlists do **not** need this (see Path 1).

### Step 1 — Create an app

1. Open **https://developer.spotify.com/dashboard** and log in.
2. Click **Create app**.
3. Fill in:
   - **App name:** `Kopuz` (anything)
   - **App description:** anything (required)
   - **Redirect URIs:** enter **exactly** this and click **Add**:
     ```
     http://127.0.0.1:8898/callback
     ```
     ⚠️ Exactly like that — same port, `/callback` at the end, `http` not `https`.
   - **Which API/SDKs are you planning to use?** → check **Web API**.
   - Check the **Terms of Service**.
4. Click **Save**.

### Step 2 — Copy the Client ID

1. Open the app's **Settings**.
2. Copy the **Client ID**. You do **not** need a Client Secret — Kopuz uses PKCE.

### Step 3 — Connect in Kopuz

1. In the Spotify import dialog, open the **Account** tab.
2. Paste the **Client ID** → **Connect** → authorize once in the browser.
3. Done — now you can pick your own playlists and Liked Songs directly.

---

## Troubleshooting

| Symptom | Fix |
|---|---|
| Import fails instantly | Are you signed in to **YouTube Music** in Kopuz? That's required (it's the clone target). |
| "No tracks found" on a URL | The playlist is **private** — either make it public or import it via Path 2 (connect account). |
| "INVALID_CLIENT" / redirect error (Path 2) | The redirect URI must be **exactly** `http://127.0.0.1:8898/callback`. |
| "Cannot bind 127.0.0.1:8898" (Path 2) | Another program is using port 8898 — close it and retry. |

Your Client ID is stored **only locally** in your Kopuz config.
