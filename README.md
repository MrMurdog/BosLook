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
Eingang mit `pocsag = "..."`. Ist ein Codewort mitten in einer Meldung so gestört, dass es sich
nicht mehr korrigieren lässt, stehen dort `�` (etwa drei Zeichen) und die Meldung geht danach
weiter; so bleiben z. B. auch verschlüsselte Meldungen bis auf diese Stelle lesbar.

## Server mit Weboberfläche

```sh
cp boslook.example.toml boslook.toml
boslook hash-password          # Passwort eingeben, Hash in boslook.toml eintragen
boslook serve                  # oder: boslook serve -c /pfad/zur/config.toml
```

### Datenbank

Standard ist eine SQLite-Datei (`database = "boslook.db"`). Für MySQL/MariaDB oder
PostgreSQL wird stattdessen eine Adresse eingetragen:

```toml
database = "mysql://boslook:passwort@localhost/boslook"
database = "postgres://boslook:passwort@localhost/boslook"
```

Datenbank und Benutzer müssen vorher angelegt sein, die Tabellen legt BosLook beim Start an.

### Soundkarte

```sh
boslook devices        # Aufnahmegeräte anzeigen
```

Die angezeigte ID als `device` beim Eingang mit `type = "soundcard"` eintragen (ohne `device`
wird das Standardgerät verwendet). Wird die Soundkarte abgezogen, versucht BosLook alle
5 Sekunden, sie wieder zu öffnen.

### RTL-SDR-Stick (rtl_fm)

Mit `type = "rtl_fm"` startet BosLook `rtl_fm` selbst und startet es neu, falls es sich beendet
(z. B. Stick abgezogen):

```toml
[[input]]
name = "Funk"
type = "rtl_fm"
frequency = "173.255M"
ppm = 42                 # Frequenzkorrektur des Sticks
gain = "auto"            # oder z. B. "38.6" (dB)
# device = "0"           # Index oder Seriennummer des Sticks
# squelch = 0            # Rauschsperre, 0 = aus (für Dekoder empfohlen)
# program = "/usr/local/bin/rtl_fm"   # falls nicht im PATH
```

`rtl_fm` wird nicht mitgeliefert: unter Linux `apt install rtl-sdr`, unter Windows die
Osmocom-Dateien (`rtl_fm.exe` mit DLLs) neben `boslook.exe` legen. Meldungen von `rtl_fm`
erscheinen im Log von BosLook.

### Mehrere Frequenzen mit einem Stick (rtl_sdr)

Mit `type = "rtl_sdr"` liest BosLook die rohen Daten des Sticks über `rtl_sdr` (aus demselben
Paket wie `rtl_fm`) und demoduliert jede Frequenz selbst. Alle Kanäle werden **gleichzeitig**
empfangen, anders als beim Scan-Modus von `rtl_fm`. Jeder Kanal erscheint als eigener Reiter und
lässt sich mit seinem Namen in Filtern, Bearbeitungsregeln und Alarmen verwenden:

```toml
[[input]]
name = "Stick"
type = "rtl_sdr"
ppm = 42
gain = "auto"
[[input.channel]]
name = "POCSAG Kreis"
frequency = "173.255M"
[[input.channel]]
name = "FMS"
frequency = "172.890M"
```

Die Frequenzen müssen innerhalb von 1 MHz liegen. Mittenfrequenz und Abtastrate (240 kS/s für
eng beieinander liegende Kanäle, sonst 1,2 bzw. 2,4 MS/s) wählt BosLook selbst, dabei bleibt die
Mitte mindestens 25 kHz von jedem Kanal entfernt. Ein Kanal braucht auf einem Raspberry Pi geschätzt
etwa 10–20 % eines Kerns.

### stdin

Als Reserve kann Live-Audio auch über stdin eingespeist werden (signed 16 Bit, mono, Abtastrate wie in der Konfiguration):

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
eingeschränkt auf `kind` und mit `source` auf den Kanal (Name des Eingangs bzw. Reiter, z. B.
`source = "^SDR$"`). Das Feld `source` gibt es genauso bei Bearbeitungsregeln, Alarmen und
den Ereignissen der Desktop-App. Ein Filter mit `source` gilt nur für die passenden Kanäle,
einer ohne `source` für alle. Ein Telegramm wird verworfen, wenn ein Filter mit
`exclude = true` passt. Gelten für seinen Kanal Filter ohne `exclude`, muss mindestens einer
davon passen; Kanäle ohne eigene Filter zeigen also alles.
Beispiele stehen in `boslook.example.toml`.

### Bearbeitung

Bearbeitungsregeln (`[[rewrite]]`) ändern Telegramme, bevor sie gefiltert, gespeichert und
gemeldet werden. Eine Regel hat dieselben Bedingungen wie ein Filter und darunter beliebig viele
Ersetzungen in der Meldung (regulärer Ausdruck, `$1`, `$2` … übernehmen Klammern) sowie
`address_add`, das die RIC verschiebt (nur POCSAG; ergäbe sich keine gültige RIC, bleibt sie
unverändert). Passen mehrere Regeln, werden alle der Reihe nach angewendet, jede sieht das
Ergebnis der vorherigen; `stop = true` beendet nach dieser Regel.

```toml
[[rewrite]]
name = "Stichwort ausschreiben"
address = "^18599"
address_add = 1000

[[rewrite.replace]]
pattern = '^F(\d)\b'
with = "Feuer $1"
```

Ablauf: Beschreibung nachschlagen → Bearbeitungsregeln (hat sich die RIC geändert, wird die
Beschreibung neu nachgeschlagen) → Filter → speichern, Alarme, Anzeige, Weiterleitung. Das gilt
auch für Telegramme von Clients und für Wetterwarnungen. Im Formular gibt es unter den Regeln
ein Testfeld, das die noch nicht gespeicherten Regeln auf ein eingetipptes Telegramm anwendet.

### Weboberfläche

Die Kopfzeile zeigt die Version von BosLook. Jede Quelle hat einen eigenen Reiter, sortiert
nach dem Alphabet. Die Reiter lassen sich mit der Maus verschieben, „Ansicht" → „Reiter A–Z
sortieren" stellt die alphabetische Reihenfolge wieder her. Neue Telegramme in anderen Reitern
werden als Zahl am Reiter angezeigt. Unter „Ansicht" lassen sich Spalten ein- und ausblenden und
mit ▲/▼ verschieben; Spalten lassen sich auch an der Überschrift verschieben und am rechten
Rand breiter ziehen. „Spalten zurücksetzen" stellt den Standard wieder her. „Clients" zeigt die
weiterleitenden Clients und erzeugt Einrichtungscodes (siehe unten). Reihenfolge, Breiten
und der zuletzt gewählte Reiter werden im Browser gespeichert. Seiten von Plugins stehen im Menü
„Plugins", die Einstellungen hinter ⚙. Auf schmalen Bildschirmen liegt das alles hinter ☰.

Unter „Einstellungen" lässt sich die Konfiguration in einem Formular bearbeiten. Kommentare
in der Datei bleiben dabei erhalten. Passwörter und Tokens für Benutzer und Clients werden im
Klartext eingegeben und nur als Hash gespeichert; ein leeres Feld lässt sie unverändert. Über
„Als Text bearbeiten" lässt sich die Datei auch direkt bearbeiten. Beim Speichern wird die
Konfiguration geprüft; ist etwas falsch, wird nichts gespeichert und der Fehler angezeigt. „Speichern und neu starten" startet BosLook mit der neuen Konfiguration neu
(danach ist eine neue Anmeldung nötig). Jeder angemeldete Benutzer kann die Konfiguration
einschließlich Passwort-Hashes und Tokens sehen und ändern.

## Beschreibungen

Zu POCSAG-Adressen lassen sich Ort, Kurz- und Langbeschreibung hinterlegen. Sie werden beim
Empfang nachgeschlagen, mit dem Telegramm gespeichert und in eigenen Spalten angezeigt. Eine
Adresse ist RIC und Sub (`1234567a`, Sub `a`–`d`) oder RIC mit `*` für alle Subs; ein Eintrag
mit passender Sub hat Vorrang vor `*`. Filter und Alarmregeln können mit `ort`, `kurz` und
`lang` auch auf diese Felder prüfen.

Bearbeitet werden die Beschreibungen über „Beschreibungen" in der Weboberfläche (eigener Tab)
oder in der Desktop-App (eigenes Fenster). Die Suche arbeitet mit regulären Ausdrücken über
alle Spalten, z. B. `^19388` oder `FW|RD`. Änderungen werden beim Verlassen eines Felds
gespeichert.

Import als CSV:

- eigenes Format mit Kopfzeile `adresse;ort;kurzbeschreibung;langbeschreibung` (wie beim Export)
- BosMon-Format ohne Kopfzeile: `"1234567*","Ort","Kurz","Lang"`

Trennzeichen (`;` oder `,`) und Zeichensatz (UTF-8 oder Windows-1252) werden erkannt, doppelt
kodierte Umlaute („FÃ¼hrung") repariert. Vorhandene Adressen werden überschrieben, mit
„Vorher alles löschen" wird das Adressbuch ersetzt.

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

Titel und Text der Nachricht lassen sich pro Alarmregel mit `title` und `text` festlegen.
Platzhalter: `{kind}` `{address}` `{sub}` `{message}` `{ort}` `{kurz}` `{lang}` `{source}`
`{time}` `{alarm}` (Namen der passenden Regeln). Ohne Vorlage ist der Titel der Name der
Regel und der Text enthält Art, Adresse, Meldung, Ort und Kurzbeschreibung.

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

### Einrichten per Code

1. Server: unter „Einstellungen" → „Clients" einen Client mit Namen anlegen und speichern.
   Mit „Fernkonfiguration" werden seine Eingänge gleich dort eingestellt (siehe unten).
2. Server: Seite „Clients" (Knopf in der Kopfzeile) → „Einrichtungscode". Die Adresse, unter
   der der Client den Server erreicht, ist mit der Adresse im Browser vorbelegt.
3. Client: `boslook setup <code> --service` – schreibt `boslook.toml` und richtet BosLook als
   Dienst ein (Linux: systemd, mit `sudo`; Windows: Aufgabe beim Systemstart, als
   Administrator). Ohne `--service` wird nur die Konfiguration geschrieben, dann
   `boslook serve` starten.

Im Code stecken Server-Adresse, Name, ein neues Token und bei `tls = "self-signed"` der
Fingerabdruck des Server-Zertifikats; genau dieses Zertifikat wird dann akzeptiert. Der Code
wird nur einmal angezeigt; ein neuer Code macht das alte Token ungültig.

### Fernkonfiguration

Ist beim Client „Fernkonfiguration" eingeschaltet, stellt man seine Eingänge (Soundkarte,
`rtl_fm`, …) auf dem Server ein. Der Server schickt sie beim Verbinden und nach jedem
Speichern; der Client hebt sie in `boslook.remote.toml` auf (läuft also auch, wenn der Server
beim Start nicht erreichbar ist) und startet neu, sobald alle gepufferten Telegramme
übertragen sind. Eigene `[[input]]`-Abschnitte des Clients gelten nur ohne Fernkonfiguration.
Die Seite „Clients" zeigt Verbindung, Version und den Zustand der Eingänge jedes Clients.
Das braucht auf beiden Seiten mindestens Version 0.2.2; ältere Clients leiten weiter wie bisher.

### Von Hand einrichten

1. Token ausdenken (lang und zufällig).
2. Server: unter „Einstellungen" → „Clients" Name und Token eintragen (oder in der Datei
   `[[client]]` mit Name und `token_hash` von `boslook hash-password`, siehe `boslook.example.toml`).
3. Client: `[forward]` mit Server-Adresse, Name und Token eintragen
   (siehe `boslook-client.example.toml`). Bei `tls = "self-signed"` die Zertifikatsdatei
   des Servers zum Client kopieren und als `cert` angeben.

Ist der Server nicht erreichbar, puffert der Client bis zu 10 000 Telegramme und sendet sie
nach dem Verbindungsaufbau mit ihrer ursprünglichen Empfangszeit nach. Die Filter des
Servers gelten auch für weitergeleitete Telegramme.

## Plugins

Plugins sind eigenständige Programme in beliebiger Sprache, die den Server erweitern: Telegramme
mitlesen oder vor der Verarbeitung ändern (z. B. entschlüsseln), neue Benachrichtigungsarten
bereitstellen, selbst Telegramme liefern oder eigene Seiten anzeigen (Eintrag mit dem Namen des
Plugins im Menü „Plugins"). Ein Plugin ist ein Ordner unter `plugins/` neben
der `boslook.toml`; eingeschaltet wird es unter „Einstellungen" im Abschnitt „Plugin: *Name*"
(in der Datei `[plugin.<name>]`). Die Seite „Plugins" zeigt Zustand und Log aller Plugins.

```toml
[plugin.webhook]

[[notifier]]
name = "Leitstelle"
type = "webhook"
url = "https://example.org/alarm"
```

Mitgeliefert sind die Beispiele `plugins/webhook` (Python, Benachrichtigung per HTTP POST),
`plugins/statistik` (Python, eigene Seite mit Statistiken) und `plugins/beispiel-bearbeitung`
(Rust, ersetzt Abkürzungen; vorher dort `cargo build --release`).
Bibliotheken für eigene Plugins liegen unter `sdk/python` und `sdk/rust`.

Dokumentation in `docs/`: [Plugins benutzen](docs/index.md),
[Plugins entwickeln](docs/PLUGINS.md) und eine [Anleitung für KI-Agenten](docs/PLUGINS-AGENT.md).
Als Website (z. B. auf Read the Docs) baut sie sich mit MkDocs:

```sh
pip install -r docs/requirements.txt
mkdocs serve
```

## Desktop-App

`boslook-desktop` verbindet sich wie die Weboberfläche mit einem BosLook-Server und zeigt die
Telegramme in Reitern. Zusätzlich führt es lokale Ereignisse aus (`[[event]]`, Felder wie bei
den Filtern):

- `priority = "normal"`: Systembenachrichtigung
- `priority = "high"` / `"emergency"`: Fenster im Vordergrund, bis es quittiert wird
- `sound`: Tondatei abspielen
- `program` und `args`: Programm starten, in `args` werden die Platzhalter wie bei den
  Alarmregeln ersetzt (`{kind}` ist dabei großgeschrieben, z. B. `POCSAG`)
- `title` und `text`: Vorlagen für Benachrichtigung bzw. Alarmfenster

Gleiche Telegramme lösen innerhalb von 60 Sekunden nur einmal aus (`repeat`). Beim ersten
Start wird `boslook-desktop.toml` angelegt und unter „Einstellungen" geöffnet. Unter
„Server-Konfiguration" lässt sich die Konfiguration des Servers bearbeiten, beides als Formular
wie in der Weboberfläche oder als Text. „Beschreibungen" öffnet den Editor für das
Adressbuch in einem eigenen Fenster, die Einträge im Menü „Plugins" (Übersicht
und Plugins mit eigener Seite) öffnen den Browser. Die Reiter sind alphabetisch sortiert. Spalten
lassen sich unter „Ansicht" ein- und ausblenden und mit ▲/▼ verschieben und (wie in der
Weboberfläche) mit der Maus breiter ziehen; alles bleibt gespeichert.

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
