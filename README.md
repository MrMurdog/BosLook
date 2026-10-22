<p align="center"><img src="images/Logo-Banner-dunkel.svg" alt="BosLook" width="480"></p>

# BosLook

Dekodiert ZVEI-, FMS- und POCSAG-Telegramme (512/1200 Baud) aus dem Audiosignal eines
Funkempfängers, holt Wetterwarnungen des DWD ab, speichert sie in SQLite und zeigt sie live in einer Weboberfläche an,
mit einem eigenen Reiter pro Quelle. Läuft unter Linux und Windows.

> Das Abhören und Auswerten von BOS-Funk ist nur Berechtigten erlaubt.

## Bauen

```sh
cargo build --release                                   # Linux
cargo build --release --target x86_64-pc-windows-gnu    # Windows (benötigt mingw-w64)
```

## WAV-Datei dekodieren

```sh
boslook decode aufnahme.wav
boslook decode aufnahme.wav -d pocsag1200,fms
```

Ausgabe (tab-getrennt): Position in der Datei, Art, Adresse, Sub, Meldung.

POCSAG-Meldungen werden als Text dargestellt. Mit `-p numeric` immer als Ziffern, mit
`-p auto` bei Sub a als Ziffern und sonst als Text. In der Konfiguration geht das pro
Eingang mit `pocsag = "..."`.

## Server mit Weboberfläche

```sh
cp boslook.example.toml boslook.toml
boslook hash-password          # Passwort eingeben, Hash in boslook.toml eintragen
boslook serve                  # oder: boslook serve -c /pfad/zur/config.toml
```

### Soundkarte

```sh
boslook devices        # Aufnahmegeräte anzeigen
```

Die angezeigte ID als `device` beim Eingang mit `type = "soundcard"` eintragen (ohne `device`
wird das Standardgerät verwendet). Wird die Soundkarte abgezogen, versucht BosLook alle
5 Sekunden, sie wieder zu öffnen.

### stdin

Live-Audio kann auch über stdin eingespeist werden (signed 16 Bit, mono, Abtastrate wie in der Konfiguration):

```sh
rtl_fm -f 173.255M -M fm -s 22050 - | boslook serve        # SDR-Stick
arecord -f S16_LE -r 22050 -c 1 | boslook serve             # Soundkarte (Linux)
```

Die Polarität des POCSAG-Signals wird automatisch erkannt.

### TLS

`[web] tls` in der Konfiguration:

- `off`: HTTP
- `self-signed`: Zertifikat wird beim ersten Start erzeugt und unter `cert`/`key` gespeichert
- `files`: vorhandenes Zertifikat laden, z. B. von Let's Encrypt

### Filter

Filter arbeiten mit regulären Ausdrücken auf `address`, `sub` und `message`, optional
eingeschränkt auf `kind`. Ein Telegramm wird verworfen, wenn ein Filter mit
`exclude = true` passt. Gibt es Filter ohne `exclude`, muss mindestens einer davon passen.
Beispiele stehen in `boslook.example.toml`.

### Weboberfläche

Jede Quelle hat einen eigenen Reiter. Die Reiter lassen sich mit der Maus verschieben; die
Reihenfolge und der zuletzt gewählte Reiter werden im Browser gespeichert. Neue Telegramme
in anderen Reitern werden als Zahl am Reiter angezeigt.

Unter „Einstellungen" lässt sich die Konfiguration in einem Formular bearbeiten. Kommentare
in der Datei bleiben dabei erhalten. Passwörter und Tokens für Benutzer und Clients werden im
Klartext eingegeben und nur als Hash gespeichert; ein leeres Feld lässt sie unverändert. Über
„Als Text bearbeiten" lässt sich die Datei auch direkt bearbeiten. Beim Speichern wird die
Konfiguration geprüft; ist etwas falsch, wird nichts gespeichert und der Fehler angezeigt. „Speichern und neu starten" startet BosLook mit der neuen Konfiguration neu
(danach ist eine neue Anmeldung nötig). Jeder angemeldete Benutzer kann die Konfiguration
einschließlich Passwort-Hashes und Tokens sehen und ändern.

## Benachrichtigungen

`[[notifier]]` legt ein Ziel fest (Telegram, E-Mail oder Pushover), `[[alarm]]` eine Regel mit
denselben Feldern wie ein Filter und der Liste `notify` mit den Namen der Ziele. Passen mehrere
Regeln auf ein Telegramm, bekommt jedes Ziel trotzdem nur eine Nachricht. Beispiele stehen in
`boslook.example.toml`.

```sh
boslook test-notify            # Testnachricht an alle Ziele senden
```

Gleiche Telegramme werden innerhalb von 60 Sekunden nur einmal gemeldet, weil Alarme oft
wiederholt gesendet werden (`notify_repeat`). Mit `priority = "high"` oder `"emergency"` in der
Alarmregel kommen Pushover-Nachrichten auch bei „Nicht stören" bzw. so lange, bis sie
bestätigt werden.

## Wetterwarnungen

Warnungen des Deutschen Wetterdienstes für die Gebiete unter `[weather]` erscheinen im Reiter
„Wetter". Sie lassen sich mit `kind = "wetter"` filtern und für Alarmregeln verwenden; `sub` ist
die Warnstufe (1 Vorabinformation, 2 Wetterwarnung, 3 markantes Wetter, 4 Unwetter,
5 extremes Unwetter).

```sh
boslook warncell Leipzig       # Warncell-ID für die Konfiguration suchen
```

## Weiterleitung an einen Server

Ein BosLook mit angeschlossenem Empfänger (Client) kann seine Telegramme an einen anderen
BosLook (Server, z. B. auf einem vServer) weiterleiten. Die Verbindung läuft als WebSocket
über den Webport des Servers, bei TLS also verschlüsselt.

1. Token ausdenken (lang und zufällig).
2. Server: unter „Einstellungen" → „Clients" Name und Token eintragen (oder in der Datei
   `[[client]]` mit Name und `token_hash` von `boslook hash-password`, siehe `boslook.example.toml`).
3. Client: `[forward]` mit Server-Adresse, Name und Token eintragen
   (siehe `boslook-client.example.toml`). Bei `tls = "self-signed"` die Zertifikatsdatei
   des Servers zum Client kopieren und als `cert` angeben.

Ist der Server nicht erreichbar, puffert der Client bis zu 10 000 Telegramme und sendet sie
nach dem Verbindungsaufbau mit ihrer ursprünglichen Empfangszeit nach. Die Filter des
Servers gelten auch für weitergeleitete Telegramme.

## Desktop-App

`boslook-desktop` verbindet sich wie die Weboberfläche mit einem BosLook-Server und zeigt die
Telegramme in Reitern. Zusätzlich führt es lokale Ereignisse aus (`[[event]]`, Felder wie bei
den Filtern):

- `priority = "normal"`: Systembenachrichtigung
- `priority = "high"` / `"emergency"`: Fenster im Vordergrund, bis es quittiert wird
- `sound`: Tondatei abspielen
- `program` und `args`: Programm starten, in `args` werden `{kind}`, `{address}`, `{sub}`,
  `{message}` und `{source}` ersetzt

Gleiche Telegramme lösen innerhalb von 60 Sekunden nur einmal aus (`repeat`). Beim ersten
Start wird `boslook-desktop.toml` angelegt und unter „Einstellungen" geöffnet. Unter
„Server-Konfiguration" lässt sich die Konfiguration des Servers bearbeiten, beides als Formular
wie in der Weboberfläche oder als Text. Die Spaltenbreiten
der Tabelle lassen sich (wie in der Weboberfläche) mit der Maus ziehen und bleiben gespeichert.

Beim Schließen läuft die App im Infobereich weiter („Beenden" im Menü des Symbols). Unter
Linux braucht das eine Arbeitsumgebung mit Infobereich (KDE, XFCE, GNOME mit
AppIndicator-Erweiterung), sonst wird die App beim Schließen beendet. Unter Wayland läuft die
App über XWayland, weil sich Fenster dort sonst weder verstecken noch im Vordergrund halten lassen.

```sh
cargo build --release --features desktop --bin boslook-desktop
cargo build --release --features desktop --bin boslook-desktop --target x86_64-pc-windows-gnu
```

## Tests

```sh
cargo test
```
