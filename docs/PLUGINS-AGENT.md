# Anleitung für KI-Agenten: BosLook-Plugin entwickeln

Diese Seite richtet sich an KI-Agenten (Claude, Copilot usw.), die für einen Nutzer ein
BosLook-Plugin schreiben. Sie ist vollständig: Mit ihr allein lässt sich ein funktionierendes
Plugin bauen. Ausführliche Erklärungen für Menschen stehen in [PLUGINS.md](PLUGINS.md).

## Regeln

1. Ein Plugin ist ein **Ordner `plugins/<name>/`** neben der `boslook.toml` mit einer
   **`plugin.toml`** und einem Programm in beliebiger Sprache.
2. `<name>` = Ordnername = `name` in `plugin.toml`; nur `a–z 0–9 - _`; nicht `telegram`,
   `pushover`, `email`.
3. Kommunikation: **stdin** liefert JSON-Nachrichten, **stdout** nimmt JSON-Nachrichten,
   jeweils **genau ein JSON-Objekt pro Zeile**, Zeile mit `\n` abschließen und **sofort flushen**.
4. **Nie** Text auf stdout schreiben, der kein Protokoll-JSON ist. Logausgaben nach **stderr**.
5. Beenden, sobald stdin endet (EOF).
6. Jede Anfrage mit `id` (`process`, `notify`) bekommt **genau eine** Antwort mit derselben `id`.
7. `process` innerhalb von 2 s beantworten, `notify` innerhalb von 30 s.
8. Unbekannte Nachrichten und Felder ignorieren.
9. Eigene Dateien nur in `data_dir` (aus `init`) schreiben.
10. Für Python `sdk/python/boslook_plugin.py` neben das Plugin kopieren und benutzen; für Rust
    das Crate `sdk/rust` (`boslook-plugin`). Andere Sprachen: Protokoll unten direkt umsetzen.

## plugin.toml

```toml
name = "<name>"
version = "1.0.0"
description = "<ein Satz>"
author = "<Autor>"
api = 1
command = ["python3", "<datei>.py"]              # relativer Pfad mit Ordner gilt ab Plugin-Ordner
command_windows = ["python", "<datei>.py"]       # optional
capabilities = ["observe", "process", "notify", "input"]   # nur die benutzten

[[field]]                 # 0..n, Einstellungen unter [plugin.<name>], kommen in init.config
key = "<key>"
label = "<Beschriftung>"
input = "text"            # text | password | number | bool | select | lines | multiline
options = []              # nur bei select
required = false
default = "<wert>"        # optional
help = "<Hilfetext>"

[[notifier_field]]        # 0..n, nur mit notify: Einstellungen je Ziel, kommen in notify.target
key = "url"
label = "Adresse"
required = true
```

JSON-Werte: `number` → Zahl, `bool` → true/false, `lines` → Liste von Texten, sonst Text.
Nicht ausgefüllte Felder ohne `default` fehlen.

## Nachrichten

BosLook → Plugin:

```json
{"type":"init","api":1,"boslook":"0.2.0","name":"<name>","config":{…},"data_dir":"<pfad>"}
{"type":"telegram","telegram":TELEGRAMM}
{"type":"process","id":7,"telegram":TELEGRAMM}
{"type":"notify","id":3,"target":{"name":"<Zielname>",…},"title":"…","body":"…","priority":"normal","telegram":TELEGRAMM|null}
```

Plugin → BosLook:

```json
{"type":"processed","id":7}
{"type":"processed","id":7,"telegram":{"message":"geändert"}}
{"type":"processed","id":7,"drop":true}
{"type":"notified","id":3}
{"type":"notified","id":3,"error":"Fehlertext"}
{"type":"telegram","telegram":{"kind":"pocsag","address":"1234567","sub":"a","message":"…","source":"<Reiter>"}}
```

TELEGRAMM:

```json
{"id":42,"time":"2026-10-22T21:02:51+02:00","source":"SDR","kind":"pocsag",
 "address":"1234567","sub":"a","message":"…","ort":"…","kurz":"…","lang":""}
```

- `kind`: `pocsag` | `fms` | `zvei` | `wetter`. POCSAG: `address` 7-stellig mit führenden
  Nullen, `sub` `a`–`d`. FMS/ZVEI: `sub` leer.
- Bei `process` sind `id` 0 und `ort`/`kurz`/`lang` leer (Beschreibungen kommen danach).
- `processed.telegram` darf nur die geänderten Felder enthalten.
- `telegram` vom Plugin (Fähigkeit `input`): `kind`, `address` Pflicht; `sub`, `message`
  optional; `source` Standard = Name des Plugins.

| Fähigkeit | Bekommt               | Muss senden          |
|-----------|-----------------------|----------------------|
| observe   | `telegram`            | –                    |
| process   | `process`             | `processed`          |
| notify    | `notify`              | `notified`           |
| input     | –                     | `telegram` (beliebig) |

Ablauf im Server: dekodieren → process-Plugins → Beschreibungen → Bearbeitungsregeln → Filter →
speichern → Alarme (notify-Plugins) → observe-Plugins.

## Konfiguration für den Nutzer

```toml
[plugin.<name>]               # Abschnitt vorhanden = eingeschaltet
<key> = <wert>                # aus [[field]]

[[notifier]]                  # nur bei notify
name = "<Zielname>"
type = "<name>"
<key> = <wert>                # aus [[notifier_field]]

[[alarm]]
name = "…"
address = "^1234567$"
notify = ["<Zielname>"]
```

Danach BosLook neu starten. Zustand und Log: Seite „Plugins" in der Weboberfläche.

## Vollständiges Minimal-Plugin (Python, ohne Bibliothek)

Zeigt alle vier Fähigkeiten; zum Abwandeln Teile weglassen.

`plugins/minimal/plugin.toml`:

```toml
name = "minimal"
version = "1.0.0"
description = "Minimal-Plugin"
api = 1
command = ["python3", "minimal.py"]
command_windows = ["python", "minimal.py"]
capabilities = ["observe", "process", "notify", "input"]

[[field]]
key = "prefix"
label = "Präfix"
default = "[M] "
```

`plugins/minimal/minimal.py`:

```python
import json, sys, threading

lock = threading.Lock()
config = {}

def send(msg):
    with lock:
        sys.stdout.write(json.dumps(msg) + "\n")
        sys.stdout.flush()

def log(*args):
    print(*args, file=sys.stderr, flush=True)

for line in sys.stdin:                      # endet, wenn BosLook stdin schließt
    if not line.strip():
        continue
    msg = json.loads(line)
    kind = msg.get("type")
    if kind == "init":
        config = msg["config"]
        log("gestartet mit", config)
        send({"type": "telegram", "telegram": {"kind": "fms", "address": "12345678", "message": "Status 2"}})
    elif kind == "process":
        t = msg["telegram"]
        if "Probealarm" in t["message"]:
            send({"type": "processed", "id": msg["id"], "drop": True})
        else:
            send({"type": "processed", "id": msg["id"], "telegram": {"message": config.get("prefix", "") + t["message"]}})
    elif kind == "notify":
        log("Benachrichtigung an", msg["target"]["name"], ":", msg["title"])
        send({"type": "notified", "id": msg["id"]})
    elif kind == "telegram":
        log("gespeichert:", msg["telegram"]["id"])
```

## Checkliste

- [ ] Ordnername = `name`, `api = 1`, `command` stimmt (unter Windows ggf. `command_windows`)
- [ ] `capabilities` enthält genau die benutzten Fähigkeiten
- [ ] Jede `process`/`notify`-Nachricht wird genau einmal mit derselben `id` beantwortet
- [ ] Keine Ausgaben außer Protokoll-JSON auf stdout; stdout nach jeder Zeile geflusht
- [ ] Plugin endet bei EOF auf stdin
- [ ] Ausnahmen in Handlern führen nicht zum Absturz (sonst Neustart nach 5 s)
- [ ] Test ohne BosLook:
      `printf '%s\n' '{"type":"init","api":1,"boslook":"0.2.0","name":"<name>","config":{},"data_dir":"data"}' '{"type":"process","id":1,"telegram":{"kind":"pocsag","address":"1234567","sub":"a","message":"Test"}}' | <command>`
      liefert genau eine Zeile `processed` mit `"id": 1`
- [ ] Dem Nutzer erklärt: Ordner nach `plugins/`, Abschnitt `[plugin.<name>]` bzw. „aktiv" im
      Formular, BosLook neu starten, Seite „Plugins" prüfen
