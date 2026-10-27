# Installation und Betrieb

## Download

Fertige Programme gibt es unter [Releases](https://github.com/MrMurdog/BosLook/releases):

| Plattform | Datei |
|---|---|
| Linux x64 (PC, vServer) | `boslook-<version>-x86_64-unknown-linux-gnu.tar.gz` |
| Raspberry Pi 3/4/5 mit 64-bit-OS | `boslook-<version>-aarch64-unknown-linux-gnu.tar.gz` |
| Raspberry Pi 3/4/Zero 2 mit 32-bit-OS | `boslook-<version>-armv7-unknown-linux-gnueabihf.tar.gz` |
| Windows x64 | `boslook-<version>-x86_64-pc-windows-gnu.zip` |

Jedes Archiv enthält `boslook` (Server und Client), `boslook-desktop` (Desktop-App), diese
Anleitung als README und die Beispielkonfigurationen. Die Linux-Programme brauchen glibc 2.36
oder neuer (Debian 12, Raspberry Pi OS Bookworm, Ubuntu 24.04) und ALSA (`libasound2`). Welche
Variante ein Pi braucht, zeigt `uname -m`: `aarch64` → 64 bit, `armv7l` → 32 bit.

```sh
tar -xzf boslook-*-aarch64-unknown-linux-gnu.tar.gz
sudo cp boslook-*/boslook /usr/local/bin/
```

Die Prüfsummen aller Dateien stehen in `SHA256SUMS` (`sha256sum -c SHA256SUMS --ignore-missing`).

## Server einrichten

```sh
cp boslook.example.toml boslook.toml
boslook hash-password          # Passwort eingeben, Hash in boslook.toml eintragen
boslook serve                  # oder: boslook serve -c /pfad/zur/config.toml
```

Ohne `[web]` läuft BosLook ohne Weboberfläche, z. B. als reiner Client. Die Konfiguration lässt
sich danach in der Weboberfläche unter ⚙ bearbeiten.

## Datenbank

Standard ist eine SQLite-Datei (`database = "boslook.db"`). Für MySQL/MariaDB oder
PostgreSQL wird stattdessen eine Adresse eingetragen:

```toml
database = "mysql://boslook:passwort@localhost/boslook"
database = "postgres://boslook:passwort@localhost/boslook"
```

Datenbank und Benutzer müssen vorher angelegt sein, die Tabellen legt BosLook beim Start an.

## TLS

`[web] tls` in der Konfiguration:

- `off`: HTTP
- `self-signed`: Zertifikat wird beim ersten Start erzeugt und unter `cert`/`key` gespeichert
- `files`: vorhandenes Zertifikat laden, z. B. von Let's Encrypt

## Als Dienst

Ein Client lässt sich mit `boslook setup <code> --service` einrichten (siehe unten). Für einen
Server von Hand unter Linux, z. B. als `/etc/systemd/system/boslook.service`:

```ini
[Unit]
Description=BosLook
Wants=network-online.target
After=network-online.target

[Service]
ExecStart=/usr/local/bin/boslook serve -c /opt/boslook/boslook.toml
WorkingDirectory=/opt/boslook
Restart=always

[Install]
WantedBy=multi-user.target
```

```sh
sudo systemctl enable --now boslook
journalctl -u boslook -f       # Log ansehen
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

## Updates

BosLook fragt einmal am Tag bei GitHub nach einer neuen Version (abschaltbar mit
`update_check = false`, Vorabversionen mit `update_channel = "beta"`). Gibt es eine, erscheint
in der Kopfzeile der Weboberfläche „Update *Version*" mit einem Link zu den Änderungen und
„Jetzt aktualisieren": BosLook lädt das passende Programm, prüft es gegen die `SHA256SUMS` des
Releases, ersetzt sich selbst und startet neu. Eingespielt wird nie automatisch, damit kein
Update mitten in eine Alarmierung fällt. Das Programm muss dafür in einem Ordner liegen, in den
BosLook schreiben darf (als Dienst mit root kein Problem).

Clients ab 0.3.0 aktualisiert der Server mit: Auf der Seite „Clients" erscheint dann „Clients auf
*Version* aktualisieren"; die Clients holen sich dieselbe Version und starten neu. Ältere Clients
werden einmal von Hand aktualisiert. Die Desktop-App zeigt neue Versionen ebenso in ihrer
Kopfzeile an und spielt sie auf Knopfdruck ein.

## Selbst bauen

```sh
cargo build --release                                   # Linux
cargo build --release --target x86_64-pc-windows-gnu    # Windows (benötigt mingw-w64)
cargo build --release --features desktop --bin boslook-desktop                                  # Desktop-App
cargo build --release --features desktop --bin boslook-desktop --target x86_64-pc-windows-gnu
cargo test
```

Unter Linux wird `libasound2-dev` gebraucht. Releases baut der Workflow
`.github/workflows/release.yml` (Actions → Release → Run workflow) mit
`.github/build.sh` für alle Plattformen: Runner (von GitHub oder ein eigener Linux-Runner mit
Docker), Branch, Versionsnummer (muss mit `Cargo.toml` übereinstimmen) und Vorabversion lassen
sich beim Start auswählen; das Release mit Tag `v<version>`, allen Dateien und `SHA256SUMS`
wird automatisch veröffentlicht.

Diese Dokumentation baut sich mit MkDocs (z. B. auf Read the Docs):

```sh
pip install -r docs/requirements.txt
mkdocs serve
```
