# Eingänge

Ein Eingang liefert Audio an die Dekoder (POCSAG 512/1200, FMS, ZVEI). Jeder Eingang
erscheint als eigener Reiter; bei `rtl_sdr` jeder Kanal. Eingänge stehen in der Konfiguration
unter `[[input]]` oder werden bei einem Client auf dem Server eingestellt
([Fernkonfiguration](installation.md#fernkonfiguration)).

## Soundkarte

```sh
boslook devices        # Aufnahmegeräte anzeigen
```

Die angezeigte ID als `device` beim Eingang mit `type = "soundcard"` eintragen (ohne `device`
wird das Standardgerät verwendet). Wird die Soundkarte abgezogen, versucht BosLook alle
5 Sekunden, sie wieder zu öffnen.

## RTL-SDR-Stick (rtl_fm)

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

## Mehrere Frequenzen mit einem Stick (rtl_sdr)

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

## stdin

Als Reserve kann Live-Audio auch über stdin eingespeist werden (signed 16 Bit, mono, Abtastrate wie in der Konfiguration):

```sh
rtl_fm -f 173.255M -M fm -s 22050 - | boslook serve        # SDR-Stick
arecord -f S16_LE -r 22050 -c 1 | boslook serve             # Soundkarte (Linux)
```

Die Polarität des POCSAG-Signals wird automatisch erkannt.

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
