# BosLook

Dekodiert ZVEI-, FMS- und POCSAG-Telegramme (512/1200 Baud) aus dem Audiosignal eines
Funkempfängers, speichert sie in SQLite und zeigt sie live in einer Weboberfläche an.
Läuft unter Linux und Windows.

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

## Server mit Weboberfläche

```sh
cp boslook.example.toml boslook.toml
boslook hash-password          # Passwort eingeben, Hash in boslook.toml eintragen
boslook serve                  # oder: boslook serve -c /pfad/zur/config.toml
```

Live-Audio wird über stdin eingespeist (signed 16 Bit, mono, Abtastrate wie in der Konfiguration):

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

## Tests

```sh
cargo test
```
