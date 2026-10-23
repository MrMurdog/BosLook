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
7. `process` innerhalb von 2 s beantworten, `notify` innerhalb von 30 s, `http` innerhalb von 10 s.
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
capabilities = ["observe", "process", "notify", "input", "web"]   # nur die benutzten

[[field]]                 # 0..n, Einstellungen unter [plugin.<name>], kommen in init.config
key = "<key>"
label = "<Beschriftung>"
input = "text"            # text | password | number | bool | select | lines | multiline | group
options = []              # nur bei select
required = false
default = "<wert>"        # optional
help = "<Hilfetext>"
# bei input = "group": Felder je Eintrag als [[field.field]] (ohne group), Wert in init.config
# ist eine Liste von Objekten, in der Konfiguration [[plugin.<name>.<key>]]

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
{"type":"http","id":9,"method":"GET","path":"/stats","query":"a=1","body":"","user":"admin"}
{"type":"result","id":1,"result":…}
{"type":"result","id":1,"error":"Fehlertext"}
```

Plugin → BosLook:

```json
{"type":"processed","id":7}
{"type":"processed","id":7,"telegram":{"message":"geändert"}}
{"type":"processed","id":7,"drop":true}
{"type":"notified","id":3}
{"type":"notified","id":3,"error":"Fehlertext"}
{"type":"telegram","telegram":{"kind":"pocsag","address":"1234567","sub":"a","message":"…","source":"<Reiter>"}}
{"type":"response","id":9,"json":{…}}
{"type":"response","id":9,"status":404,"content_type":"text/plain","body":"…"}
{"type":"query","id":1,"query":"sources"}
{"type":"query","id":2,"query":"telegrams","source":"SDR","limit":100,"before":null}
{"type":"query","id":3,"query":"descriptions","q":"^19388","sort":"ort","desc":false,"offset":0,"limit":100}
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
- `query` darf jedes Plugin senden; die `id` vergibt das Plugin (eigene Zählung), `result`
  kommt mit derselben `id` irgendwann zwischen anderen Nachrichten. Wer in einem Handler auf
  `result` wartet, muss stdin in einem eigenen Thread lesen (die Bibliotheken tun das).
  Ergebnisse: `sources` → Liste von Texten; `telegrams` → Liste von TELEGRAMM, neueste
  zuerst (`limit` höchstens 1000, `before` = nur kleinere `id`); `descriptions` →
  `{"total":…,"rows":[{"address","ort","kurz","lang"}],"notice":…}`.
- `http`: `path` ohne `/plugins/<name>/api`. Antwort mit `json` (wird JSON) oder `body`
  (+ `content_type`, Standard Text); `status` Standard 200.

| Fähigkeit | Bekommt               | Muss senden          |
|-----------|-----------------------|----------------------|
| observe   | `telegram`            | –                    |
| process   | `process`             | `processed`          |
| notify    | `notify`              | `notified`           |
| input     | –                     | `telegram` (beliebig) |
| web       | `http`                | `response`           |

Ablauf im Server: dekodieren → process-Plugins → Beschreibungen → Bearbeitungsregeln → Filter →
speichern → Alarme (notify-Plugins) → observe-Plugins.

## Eigene Seite (Fähigkeit `web`)

- Dateien in `plugins/<name>/web/`, Startseite `index.html`; erreichbar unter
  `/plugins/<name>/` nur für angemeldete Benutzer. BosLook zeigt einen Knopf mit dem Namen.
- In der Seite relative Adressen benutzen: `fetch("api/stats")` → Nachricht `http` mit
  `path` `/stats`.
- `<link rel="stylesheet" href="/plugin.css">` für das Aussehen von BosLook (hell/dunkel);
  Klassen `.box`, `.row`, `.muted`, `.error`; `header`, `button`, `input`, `table` sind
  schon gestaltet. Für schmale Bildschirme Tabellen in `.box` legen (scrollt waagerecht).
- Python: `@plugin.on_http def http(method, path, query, body, user)` gibt dict/list (JSON),
  str oder `(status, content_type, body)` zurück. Rust: `fn http(&mut self, r: &HttpRequest)
  -> HttpResponse` mit `HttpResponse::json/text/not_found`.

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
- [ ] `capabilities` enthält genau die benutzten Fähigkeiten (`web` nur mit Ordner `web/`)
- [ ] Jede `process`/`notify`/`http`-Nachricht wird genau einmal mit derselben `id` beantwortet
- [ ] Keine Ausgaben außer Protokoll-JSON auf stdout; stdout nach jeder Zeile geflusht
- [ ] Plugin endet bei EOF auf stdin
- [ ] Ausnahmen in Handlern führen nicht zum Absturz (sonst Neustart nach 5 s)
- [ ] Test ohne BosLook:
      `printf '%s\n' '{"type":"init","api":1,"boslook":"0.2.0","name":"<name>","config":{},"data_dir":"data"}' '{"type":"process","id":1,"telegram":{"kind":"pocsag","address":"1234567","sub":"a","message":"Test"}}' | <command>`
      liefert genau eine Zeile `processed` mit `"id": 1`
- [ ] Dem Nutzer erklärt: Ordner nach `plugins/`, Abschnitt `[plugin.<name>]` bzw. „aktiv" im
      Formular, BosLook neu starten, Seite „Plugins" prüfen
