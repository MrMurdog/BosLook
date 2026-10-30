<p align="center"><img src="images/Logo-Banner-dunkel.svg" alt="BosLook" width="480"></p>

# BosLook

Dekodiert ZVEI-, FMS- und POCSAG-Telegramme (512/1200 Baud) aus dem Audiosignal eines
Funkempfängers oder direkt von einem RTL-SDR-Stick, holt Wetterwarnungen des DWD ab, speichert
alles und zeigt es live in einer Weboberfläche und einer Desktop-App an. Läuft unter Linux
(auch Raspberry Pi) und Windows.

> Das Abhören und Auswerten von BOS-Funk ist nur Berechtigten erlaubt.

- **Eingänge:** Soundkarte, RTL-SDR-Stick über `rtl_fm` oder mehrere Frequenzen gleichzeitig
  über `rtl_sdr`, stdin, WAV-Dateien
- **Verarbeitung:** Beschreibungen (Adressbuch, BosMon-Import), Bearbeitungsregeln, Filter pro
  Kanal, Alarme per Telegram, Pushover, E-Mail
- **Server und Clients:** Empfangsrechner leiten an einen zentralen Server weiter, werden per
  Einrichtungscode verbunden und vom Server aus eingestellt
- **Weboberfläche und Desktop-App** mit Reitern pro Kanal, Alarmfenster und Tönen
- **Plugins** in beliebiger Sprache, **Updates** auf Knopfdruck

## Download

Fertige Programme für Linux x64, Raspberry Pi (64 und 32 bit) und Windows gibt es unter
[Releases](https://git.boskrypt.ru/MrMurdog/BosLook/releases). Welche Datei wofür, steht unter
[Installation](docs/installation.md#download).

## Schnellstart

### 1. Server

```sh
tar -xzf boslook-*-x86_64-unknown-linux-gnu.tar.gz && cd boslook-*/
cp boslook.example.toml boslook.toml
./boslook hash-password        # Passwort eingeben, Hash bei [[user]] in boslook.toml eintragen
./boslook serve
```

Die Weboberfläche läuft dann unter `https://<server>:8443` (selbst signiertes Zertifikat). Alles
Weitere lässt sich dort unter ⚙ einstellen, als Erstes die Eingänge: Die Beispiele (WAV-Datei,
stdin) durch die eigene Soundkarte oder den eigenen Stick ersetzen. Als Dienst einrichten: siehe
[Installation](docs/installation.md#als-dienst).

### 2. Raspberry Pi mit RTL-SDR-Stick als Client

1. Auf dem Pi: `sudo apt install rtl-sdr` und das passende Archiv entpacken
   (`uname -m`: `aarch64` → aarch64, `armv7l` → armv7).
2. Auf dem Server: ⚙ → „Clients" → Client anlegen, „Fernkonfiguration" einschalten und einen
   Eingang `rtl_fm` (eine Frequenz) oder `rtl_sdr` (mehrere Frequenzen innerhalb von 1 MHz)
   hinzufügen, speichern.
3. Auf dem Server: Seite „Clients" → „Einrichtungscode".
4. Auf dem Pi: `sudo ./boslook setup <code> --service`

Der Pi verbindet sich, bekommt seine Eingänge vom Server und erscheint auf der Seite „Clients"
mit dem Zustand seiner Eingänge.

### 3. Desktop-App

`boslook-desktop` starten, beim ersten Start Server-Adresse, Benutzer und Passwort eintragen. Die
App zeigt die Telegramme, spielt Töne, öffnet Alarmfenster und läuft im Infobereich weiter.

## Dokumentation

- [Installation und Betrieb](docs/installation.md) – Download, Server, TLS, Datenbank, Dienst,
  Clients, Updates, selbst bauen
- [Eingänge](docs/eingaenge.md) – Soundkarte, `rtl_fm`, `rtl_sdr`, stdin, WAV-Datei dekodieren
- [Verarbeitung und Alarme](docs/verarbeitung.md) – Filter, Bearbeitung, Beschreibungen,
  Benachrichtigungen, Wetterwarnungen
- [Weboberfläche und Desktop-App](docs/oberflaeche.md)
- [Plugins benutzen](docs/plugins.md), [Plugins entwickeln](docs/PLUGINS.md),
  [Anleitung für KI-Agenten](docs/PLUGINS-AGENT.md)

Alle Einstellungen mit Erklärungen stehen außerdem in `boslook.example.toml` und
`boslook-client.example.toml`. Die Dokumentation lässt sich mit MkDocs auch als Website bauen
(z. B. auf Read the Docs), siehe [Selbst bauen](docs/installation.md#selbst-bauen).
