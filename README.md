# BosLook

Dekodiert ZVEI-, FMS- und POCSAG-Telegramme (512/1200 Baud) aus dem Audiosignal eines
Funkempfängers, speichert sie in SQLite und zeigt sie live in einer Weboberfläche an,
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

## Benachrichtigungen

`[[notifier]]` legt ein Ziel fest (Telegram, E-Mail oder Pushover), `[[alarm]]` eine Regel mit
denselben Feldern wie ein Filter und der Liste `notify` mit den Namen der Ziele. Passen mehrere
Regeln auf ein Telegramm, bekommt jedes Ziel trotzdem nur eine Nachricht. Beispiele stehen in
`boslook.example.toml`.

```sh
boslook test-notify            # Testnachricht an alle Ziele senden
```

## Weiterleitung an einen Server

Ein BosLook mit angeschlossenem Empfänger (Client) kann seine Telegramme an einen anderen
BosLook (Server, z. B. auf einem vServer) weiterleiten. Die Verbindung läuft als WebSocket
über den Webport des Servers, bei TLS also verschlüsselt.

1. Token ausdenken (lang und zufällig) und mit `boslook hash-password` den Hash erzeugen.
2. Server: `[[client]]` mit Name und `token_hash` eintragen (siehe `boslook.example.toml`).
3. Client: `[forward]` mit Server-Adresse, Name und Token eintragen
   (siehe `boslook-client.example.toml`). Bei `tls = "self-signed"` die Zertifikatsdatei
   des Servers zum Client kopieren und als `cert` angeben.

Ist der Server nicht erreichbar, puffert der Client bis zu 10 000 Telegramme und sendet sie
nach dem Verbindungsaufbau mit ihrer ursprünglichen Empfangszeit nach. Die Filter des
Servers gelten auch für weitergeleitete Telegramme.

## Tests

```sh
cargo test
```
