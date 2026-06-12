# Spotify-Import

Kopuz kann Spotify-Playlists nach YouTube Music klonen. Es gibt zwei Wege —
für den normalen Fall brauchst du **gar nichts einzurichten**.

---

## Weg 1 — Per Playlist-URL (empfohlen, kein Login nötig)

Für **jede öffentliche Playlist** — egal von wem, egal wie groß:

1. In Spotify die Playlist öffnen → **Teilen → Link kopieren**.
2. In Kopuz das **Spotify-Import**-Fenster öffnen (Playlists-Seite der
   YouTube-Music-Ansicht → **Import from Spotify**).
3. Im Tab **URL** den Link einfügen und auf **Import** klicken.

Kopuz holt **alle** Tracks — kein 100-Track-Limit, keine Anmeldung, kein
API-Key. Das funktioniert, weil Kopuz dieselbe interne Schnittstelle nutzt wie
der Spotify-Web-Player selbst.

> Voraussetzung: Du musst in Kopuz bei **YouTube Music angemeldet** sein, denn
> die geklonte Playlist landet in deinem YT-Music-Konto.

---

## Weg 2 — Eigenes Konto verbinden (nur für *private* Playlists & Lieblingssongs)

Nur nötig, wenn du **deine eigenen privaten** Playlists oder deine
**Lieblingssongs** importieren willst — die sind über die URL nicht erreichbar.
Öffentliche Playlists brauchen das **nicht** (siehe Weg 1).

### Schritt 1 — App erstellen

1. Öffne **https://developer.spotify.com/dashboard** und melde dich an.
2. Klick auf **Create app**.
3. Felder ausfüllen:
   - **App name:** `Kopuz` (oder beliebig)
   - **App description:** irgendwas (Pflichtfeld)
   - **Redirect URIs:** **exakt** das hier eintragen und auf **Add** klicken:
     ```
     http://127.0.0.1:8898/callback
     ```
     ⚠️ Genau so — gleicher Port, `/callback` am Ende, kein `https`.
   - **Which API/SDKs are you planning to use?** → **Web API** anhaken.
   - Die **Terms of Service** anhaken.
4. **Save** klicken.

### Schritt 2 — Client-ID kopieren

1. In der App auf **Settings** gehen.
2. Die **Client ID** kopieren. Ein **Client Secret** brauchst du **nicht** —
   Kopuz nutzt PKCE.

### Schritt 3 — In Kopuz verbinden

1. Im Spotify-Import-Fenster den Tab **Account** öffnen.
2. **Client ID** einfügen → **Connect** → im Browser einmal autorisieren.
3. Fertig — jetzt kannst du deine eigenen Playlists und Lieblingssongs direkt
   auswählen.

---

## Probleme?

| Symptom | Lösung |
|---|---|
| Import schlägt sofort fehl | Bist du in Kopuz bei **YouTube Music** angemeldet? Das ist Pflicht (Ziel des Klons). |
| „No tracks found" bei URL | Playlist ist **privat** — entweder öffentlich schalten oder über Weg 2 (Konto verbinden) importieren. |
| „INVALID_CLIENT" / Redirect-Fehler (Weg 2) | Die Redirect URI muss **exakt** `http://127.0.0.1:8898/callback` sein. |
| „Cannot bind 127.0.0.1:8898" (Weg 2) | Ein anderes Programm belegt Port 8898 — schließ es und versuch es erneut. |

Deine Client-ID wird **nur lokal** in deiner Kopuz-Konfiguration gespeichert.
