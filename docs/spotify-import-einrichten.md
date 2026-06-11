# Spotify-Import einrichten (eigene App)

Um deine Spotify-Playlists nach YouTube Music zu klonen — **ohne** 100-Track-Limit
und schnell — verbindest du Kopuz einmal mit einer eigenen Spotify-App. Dauert
~3 Minuten, ist kostenlos, und du machst es **nur einmal**.

> **Warum?** Spotify hat den anonymen Zugang stark gedrosselt (HTTP 429), wodurch
> URL-Importe auf ~100 Tracks gedeckelt und langsam werden. Mit deiner eigenen
> App lädt Kopuz die volle Playlist zuverlässig.

Du brauchst nur ein normales (auch kostenloses) Spotify-Konto.

---

## Schritt 1 — App erstellen

1. Öffne **https://developer.spotify.com/dashboard** und melde dich an.
2. Klick auf **Create app**.
3. Felder ausfüllen:
   - **App name:** `Kopuz` (oder beliebig)
   - **App description:** irgendwas, z. B. `Import meiner Spotify-Playlists` (Pflichtfeld)
   - **Website:** leer lassen (optional)
   - **Redirect URIs:** **exakt** das hier eintragen und auf **Add** klicken:
     ```
     http://127.0.0.1:8898/callback
     ```
     ⚠️ Genau so — gleicher Port, `/callback` am Ende, kein `https`. Sonst
     schlägt die Anmeldung fehl.
   - **Which API/SDKs are you planning to use?** → **Web API** anhaken.
   - Die **Terms of Service** anhaken.
4. **Save** klicken.

---

## Schritt 2 — Client-ID kopieren

1. In der erstellten App auf **Settings** gehen.
2. Die **Client ID** kopieren (sieht aus wie `a1b2c3d4...`).
   - Ein **Client Secret** brauchst du **nicht** — Kopuz nutzt PKCE (sicherer,
     kein Secret nötig).

---

## Schritt 3 — In Kopuz verbinden

1. In Kopuz das **Spotify-Import**-Fenster öffnen (auf der Playlists-Seite der
   YouTube-Music-Ansicht → **Import from Spotify**).
2. Den Bereich **Connect Spotify** öffnen, die **Client ID** einfügen und auf
   **Connect** klicken.
3. Es öffnet sich der Browser → mit deinem Spotify-Konto **einmal autorisieren**.
4. Zurück in Kopuz: fertig — dein Konto ist verbunden.

Ab jetzt kannst du:
- deine eigenen Playlists direkt auswählen und importieren, **oder**
- eine **Playlist-URL** einfügen — Kopuz nutzt dann automatisch deinen
  verbundenen Zugang und holt **alle** Tracks (kein 100-Limit).

---

## Probleme?

| Symptom | Lösung |
|---|---|
| „INVALID_CLIENT" / Redirect-Fehler | Die Redirect URI muss **exakt** `http://127.0.0.1:8898/callback` sein (Schritt 1). |
| Import hängt bei ~100 Tracks | Account nicht verbunden — verbinde Spotify (Schritt 3). Ohne Verbindung limitiert Spotify selbst. |
| „Cannot bind 127.0.0.1:8898" | Ein anderes Programm belegt den Port 8898 — schließ es und versuch es erneut. |
| Browser öffnet nicht | Öffne die angezeigte URL manuell und autorisiere dort. |

Deine Client-ID wird **nur lokal** in deiner Kopuz-Konfiguration gespeichert.
