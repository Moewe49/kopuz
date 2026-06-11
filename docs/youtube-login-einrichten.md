# YouTube Music Login einrichten (eigener Google-Client)

> ⛔ **FUNKTIONIERT DERZEIT NICHT — diese Anleitung NICHT befolgen.** Google hat
> OAuth gegen die YouTube-Music-API 2025/2026 abgeschaltet: jede Anfrage gibt
> HTTP 400 `INVALID_ARGUMENT`, sogar mit korrekt eingerichtetem eigenem Client
> (verifiziert; dieselbe Änderung hat auch ytmusicapi und yt-dlp OAuth zerlegt).
> **Nutze stattdessen die Inkognito-Cookie-Methode** — siehe die App
> (Einstellungen → YouTube Music → „Cookies einfügen"). Diese Anleitung bleibt
> nur als Referenz, falls Google OAuth wieder öffnet.

Damit Kopuz dich **dauerhaft** bei YouTube Music angemeldet halten kann — ohne
offenen Browser und ohne ständiges Cookie-Kopieren — brauchst du einen eigenen
Google-OAuth-Client. Klingt technisch, ist aber in **~5 Minuten** erledigt und
du machst es **nur ein einziges Mal**.

> **Warum nötig?** Google hat den früher genutzten gemeinsamen Login-Zugang
> abgeschaltet. Mit deinem eigenen (kostenlosen) Zugang funktioniert es wieder —
> und zwar zuverlässig und dauerhaft.

Du brauchst nur ein normales Google-Konto. Es entstehen **keine Kosten**.

---

## Schritt 1 — Projekt anlegen

1. Öffne **https://console.cloud.google.com/projectcreate**
2. Bei **Projektname** etwas Beliebiges eingeben, z. B. `Kopuz`
3. Auf **Erstellen** klicken
4. Oben links im blauen Balken sicherstellen, dass dein neues Projekt
   ausgewählt ist (steht im Projekt-Auswahlmenü).

---

## Schritt 2 — YouTube-API aktivieren

1. Öffne **https://console.cloud.google.com/apis/library/youtube.googleapis.com**
2. Prüfe oben, dass dein Projekt (`Kopuz`) ausgewählt ist
3. Auf **Aktivieren** klicken und kurz warten

---

## Schritt 3 — Zustimmungsbildschirm einrichten

1. Öffne **https://console.cloud.google.com/auth/overview**
   (heißt „Google Auth Platform" / früher „OAuth-Zustimmungsbildschirm")
2. Falls du gefragt wirst „Jetzt starten" / **Get started**: anklicken
3. **App-Informationen:**
   - **App-Name:** `Kopuz` (oder beliebig)
   - **Nutzersupport-E-Mail:** deine eigene Mail auswählen
4. **Zielgruppe / Audience:** **Extern** auswählen
5. **Kontaktdaten:** deine eigene Mail eintragen
6. Bedingungen akzeptieren → **Erstellen**

---

## Schritt 4 — App veröffentlichen (WICHTIG!)

> Ohne diesen Schritt läuft dein Login **alle 7 Tage** ab und du müsstest dich
> jede Woche neu anmelden. Mit „Veröffentlichen" bleibst du **dauerhaft**
> eingeloggt.

1. Öffne **https://console.cloud.google.com/auth/audience**
2. Unter **Veröffentlichungsstatus** steht wahrscheinlich „Testing".
3. Auf **App veröffentlichen** / **Publish app** klicken und bestätigen.
   - Der Status wechselt zu **In Produktion**.

> Du musst **keine** Google-Verifizierung beantragen. Es ist deine private App
> für dich selbst — die spätere Warnung „Google hat diese App nicht überprüft"
> ist normal und sicher (siehe Schritt 6).

---

## Schritt 5 — Client-ID erstellen

1. Öffne **https://console.cloud.google.com/auth/clients**
2. Auf **Client erstellen** / **Create client** klicken
3. **Anwendungstyp:** **Fernseher und Geräte mit begrenzter Eingabe**
   (englisch: *TVs and Limited Input devices*)
4. **Name:** `Kopuz` (oder beliebig)
5. **Erstellen** klicken
6. Es erscheint ein Fenster mit **Client-ID** und **Clientschlüssel**
   (*Client secret*). Beide brauchst du gleich — lass das Fenster offen oder
   kopiere beide Werte irgendwohin.
   - **Client-ID** sieht so aus: `123456789-abcdef….apps.googleusercontent.com`
   - **Clientschlüssel** sieht so aus: `GOCSPX-xxxxxxxxxxxxxxxx`

---

## Schritt 6 — In Kopuz eintragen & anmelden

1. In Kopuz: **Einstellungen → Medienserver → YouTube Music**
2. Methode **„Mit Google anmelden"** auswählen
3. Den aufklappbaren Bereich **„Einmalige Einrichtung"** öffnen
4. **Client-ID** und **Clientschlüssel** aus Schritt 5 einfügen
5. Auf **„Mit Google anmelden"** klicken
6. Es öffnet sich der Browser mit einem **Code**. Den Code eingeben und mit
   **deinem YouTube-Konto** anmelden.
   - Kommt die Warnung **„Google hat diese App nicht überprüft"**?
     → **Erweitert** → **Zu Kopuz wechseln (unsicher)** klicken. Das ist hier
     völlig in Ordnung, weil **du** der Entwickler dieser App bist.
7. Zugriff **erlauben** → zurück zu Kopuz → **Speichern**.

Fertig! 🎉 Kopuz holt sich ab jetzt selbst frische Zugänge — kein Browser, kein
F12, kein erneutes Anmelden mehr.

---

## Probleme?

| Symptom | Lösung |
|---|---|
| „Mit Google anmelden" ist ausgegraut | Client-ID **und** Clientschlüssel müssen beide eingetragen sein. |
| Fehler `400 INVALID_ARGUMENT` / Library bleibt leer | YouTube-API nicht aktiviert (**Schritt 2**) oder falsches Projekt ausgewählt. |
| Login funktioniert, aber nach ~1 Woche ausgeloggt | App nicht veröffentlicht — **Schritt 4** nachholen (Status „In Produktion"). |
| „Access denied" / `access_denied` | Dein Konto ist nicht freigegeben. Entweder App veröffentlichen (Schritt 4) **oder** dich unter *Auth Platform → Zielgruppe → Testnutzer* hinzufügen. |
| „This app is blocked" | Anwendungstyp war falsch — in **Schritt 5** muss es *Fernseher und Geräte mit begrenzter Eingabe* sein, nicht „Webanwendung". |

Deine Client-ID und der Schlüssel sind **nur lokal** in deiner Kopuz-Konfiguration
gespeichert und werden nirgendwo geteilt.
