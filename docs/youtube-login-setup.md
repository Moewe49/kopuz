# Set up YouTube Music login (your own Google client)

> ⛔ **CURRENTLY NOT WORKING — do not follow this guide.** Google disabled OAuth
> against the YouTube Music API in 2025/2026: every request returns HTTP 400
> `INVALID_ARGUMENT` even with a correctly configured personal client (verified;
> the same change broke ytmusicapi and yt-dlp OAuth). **Use the Incognito cookie
> method instead** — see the README's "YouTube Music Setup" section. This guide
> is kept for reference in case Google re-enables OAuth.

To keep you **permanently** signed in to YouTube Music — with no browser kept
open and no cookie copying — Kopuz needs your own Google OAuth client. It sounds
technical, but it takes **~5 minutes** and you only do it **once**.

> **Why is this needed?** Google disabled the old shared sign-in used by tools
> like this one. With your own (free) client it works again — reliably and
> permanently.

You only need a normal Google account. There is **no cost**.

---

## Step 1 — Create a project

1. Open **https://console.cloud.google.com/projectcreate**
2. Enter any **Project name**, e.g. `Kopuz`
3. Click **Create**
4. Make sure your new project is selected in the project picker (top-left).

---

## Step 2 — Enable the YouTube API

1. Open **https://console.cloud.google.com/apis/library/youtube.googleapis.com**
2. Check that your project (`Kopuz`) is selected at the top
3. Click **Enable** and wait a moment

---

## Step 3 — Configure the consent screen

1. Open **https://console.cloud.google.com/auth/overview**
   (the "Google Auth Platform", formerly "OAuth consent screen")
2. If prompted, click **Get started**
3. **App information:**
   - **App name:** `Kopuz` (anything)
   - **User support email:** pick your own email
4. **Audience:** choose **External**
5. **Contact information:** enter your own email
6. Accept the terms → **Create**

---

## Step 4 — Publish the app (IMPORTANT!)

> Without this, your login **expires every 7 days** and you'd have to sign in
> again each week. Publishing keeps you signed in **permanently**.

1. Open **https://console.cloud.google.com/auth/audience**
2. Under **Publishing status** it probably says "Testing".
3. Click **Publish app** and confirm. The status changes to **In production**.

> You do **not** need to request Google verification. This is your private app
> for yourself — the later "Google hasn't verified this app" warning is normal
> and safe (see Step 6).

---

## Step 5 — Create the client ID

1. Open **https://console.cloud.google.com/auth/clients**
2. Click **Create client**
3. **Application type:** **TVs and Limited Input devices**
4. **Name:** `Kopuz` (anything)
5. Click **Create**
6. A dialog shows your **Client ID** and **Client secret**. You'll need both in
   a second — keep the dialog open or copy both somewhere.
   - **Client ID** looks like: `123456789-abcdef….apps.googleusercontent.com`
   - **Client secret** looks like: `GOCSPX-xxxxxxxxxxxxxxxx`

---

## Step 6 — Enter in Kopuz & sign in

1. In Kopuz: **Settings → Media servers → YouTube Music**
2. Choose the **"Sign in with Google"** method
3. Open the collapsible **"One-time setup"** section
4. Paste the **Client ID** and **Client secret** from Step 5
5. Click **"Sign in with Google"**
6. A browser opens with a **code**. Enter it and sign in with **your YouTube
   account**.
   - See **"Google hasn't verified this app"**? → **Advanced** → **Go to Kopuz
     (unsafe)**. This is fine here, because **you** are the app's developer.
7. **Allow** access → back to Kopuz → **Save**.

Done! 🎉 Kopuz now refreshes its own access — no browser, no F12, no signing in
again.

---

## Troubleshooting

| Symptom | Fix |
|---|---|
| "Sign in with Google" is greyed out | Both Client ID **and** Client secret must be filled in. |
| `400 INVALID_ARGUMENT` / library stays empty | YouTube API not enabled (**Step 2**) or wrong project selected. |
| Works, but signed out after ~1 week | App not published — do **Step 4** (status "In production"). |
| `access_denied` | Your account isn't allowed. Publish the app (Step 4), **or** add yourself under *Auth Platform → Audience → Test users*. |
| "This app is blocked" | Wrong application type — Step 5 must be *TVs and Limited Input devices*, not "Web application". |

Your client ID and secret are stored **only locally** in your Kopuz config and
are never shared.
