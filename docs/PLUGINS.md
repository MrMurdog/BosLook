# Plugins entwickeln

Ein BosLook-Plugin ist ein **eigenständiges Programm in beliebiger Sprache**. BosLook startet
es und tauscht mit ihm JSON-Nachrichten aus, **eine Nachricht pro Zeile**:

| Kanal  | Richtung           | Inhalt                                    |
|--------|--------------------|-------------------------------------------|
| stdin  | BosLook → Plugin   | Nachrichten (JSON, eine pro Zeile)        |
| stdout | Plugin → BosLook   | Antworten und eigene Nachrichten (JSON)   |
| stderr | Plugin → Log       | beliebiger Text, erscheint im Log         |

Plugins laufen auf dem Server (`boslook serve`) und bei `boslook test-notify`. Für Python und
Rust gibt es fertige Bibliotheken ([Python](#python-bibliothek), [Rust](#rust-bibliothek)),
alle anderen Sprachen sprechen das [Protokoll](#protokoll) direkt.

## Aufbau

Jedes Plugin ist ein Ordner unter `plugins/` neben der `boslook.toml`. Der Ordner heißt wie
das Plugin:

```text
plugins/
  webhook/
    plugin.toml        Beschreibung (Pflicht)
    webhook.py         das Programm
    boslook_plugin.py  Python-Bibliothek, mitkopiert
    data/              legt BosLook an: Ordner für eigene Daten
    web/               nur mit Fähigkeit "web": die eigene Seite
      index.html
```

Eingeschaltet wird ein Plugin in der Konfiguration mit einem Abschnitt `[plugin.<name>]`, im
Formular über „aktiv" im Abschnitt „Plugin: *Name*". Wirksam wird das nach einem Neustart.

```toml
[plugin.webhook]
timeout = 10          # Einstellungen aus [[field]] der plugin.toml
# enabled = false     # abschalten, ohne die Einstellungen zu löschen
```

## plugin.toml

```toml
name = "webhook"                  # Pflicht, wie der Ordner: a–z, 0–9, - und _
version = "1.0.0"                 # Pflicht
description = "Alarme per HTTP POST senden"
author = "Name"
api = 1                           # Pflicht: Version der Schnittstelle
command = ["python3", "webhook.py"]          # Pflicht: Programm und Argumente
command_windows = ["python", "webhook.py"]   # optional, abweichend unter Windows
capabilities = ["notify"]         # was das Plugin kann, siehe unten

# Einstellungen unter [plugin.webhook]; erscheinen im Formular
[[field]]
key = "timeout"
label = "Zeitlimit (s)"
input = "number"
default = 10
help = "So lange auf die Antwort warten"

# Nur mit "notify": Einstellungen je Benachrichtigungsziel
[[notifier_field]]
key = "url"
label = "Adresse"
required = true
```

- **command**: Ein relativer Programmpfad mit Ordner (z. B. `target/release/plugin`) gilt ab
  dem Plugin-Ordner. Ein Name ohne Ordner (z. B. `python3`) wird im Suchpfad gesucht. Das
  Arbeitsverzeichnis ist immer der Plugin-Ordner.
- **api**: Passt die Zahl nicht zur Schnittstelle von BosLook, startet das Plugin nicht.
- Fehler in der `plugin.toml` zeigt die Seite „Plugins" an.

### Felder

Für `[[field]]` und `[[notifier_field]]`:

| Schlüssel  | Bedeutung                                                         |
|------------|-------------------------------------------------------------------|
| `key`      | Name in der Konfiguration (Pflicht)                               |
| `label`    | Beschriftung im Formular (Pflicht)                                |
| `help`     | Hilfetext unter dem Feld                                          |
| `input`    | Art der Eingabe, Standard `text` (siehe unten)                    |
| `options`  | Auswahlwerte bei `select`                                         |
| `required` | muss ausgefüllt sein; wird beim Speichern und beim Start geprüft  |
| `default`  | Wert, wenn nichts eingetragen ist                                 |

| `input`     | Wert im JSON          | Formular                       |
|-------------|-----------------------|--------------------------------|
| `text`      | Text                  | einzeilig                      |
| `password`  | Text                  | einzeilig, verdeckt            |
| `number`    | Zahl                  | einzeilig                      |
| `bool`      | `true` / `false`      | Kästchen                       |
| `select`    | Text aus `options`    | Auswahlliste                   |
| `lines`     | Liste von Texten      | mehrzeilig, ein Eintrag je Zeile |
| `multiline` | Text mit Zeilenumbrüchen | mehrzeilig                  |
| `group`     | Liste von Objekten    | Einträge mit eigenen Feldern, „+ Hinzufügen“ |

Bei `group` stehen die Felder eines Eintrags als `[[field.field]]` darunter (alle Eingaben
außer `group`). In der Konfiguration wird daraus `[[plugin.<name>.<key>]]`, z. B. Kanäle:

```toml
[[field]]
key = "kanal"
label = "Kanäle"
input = "group"
[[field.field]]
key = "name"
label = "Name"
required = true
[[field.field]]
key = "rics"
label = "RICs"
input = "lines"
```

```toml
[[plugin.beispiel.kanal]]
name = "SDR"
rics = ["1864080"]
```

Felder, die nicht ausgefüllt sind und keinen `default` haben, fehlen im JSON.

## Fähigkeiten

| Fähigkeit | BosLook schickt          | Plugin antwortet / schickt       |
|-----------|--------------------------|----------------------------------|
| `observe` | `telegram` (gespeichert) | –                                |
| `process` | `process`                | `processed` (innerhalb von 2 s)  |
| `notify`  | `notify`                 | `notified` (innerhalb von 30 s)  |
| `input`   | –                        | `telegram` (jederzeit)           |
| `web`     | `http`                   | `response` (innerhalb von 10 s)  |

Unabhängig von den Fähigkeiten darf jedes Plugin jederzeit Daten [abfragen](#query-result-plugin-boslook).

Ablauf eines Telegramms im Server:

1. dekodieren (oder von einem `input`-Plugin, einem Client oder dem DWD)
2. **`process`-Plugins**, der Reihe nach wie die Abschnitte `[plugin.…]` in der Konfiguration
3. Beschreibung nachschlagen (Ort, Kurz- und Langbeschreibung)
4. Bearbeitungsregeln, Filter
5. speichern, Alarme (darunter **`notify`-Plugins**), Anzeige
6. **`observe`-Plugins**

## Protokoll

Jede Nachricht ist ein JSON-Objekt in einer Zeile mit dem Feld `type`. Unbekannte Felder
sollten ignoriert werden, damit spätere BosLook-Versionen Felder ergänzen können.

### init (BosLook → Plugin)

Immer die erste Nachricht nach dem Start.

```json
{"type": "init", "api": 1, "boslook": "0.2.0", "name": "webhook",
 "config": {"timeout": 10}, "data_dir": "/pfad/plugins/webhook/data"}
```

`config` sind die Einstellungen aus `[plugin.<name>]` ohne `enabled`.

### telegram (BosLook → Plugin, Fähigkeit `observe`)

```json
{"type": "telegram", "telegram": {"id": 42, "time": "2026-10-22T21:02:51.123+02:00",
 "source": "SDR", "kind": "pocsag", "address": "1234567", "sub": "a",
 "message": "F2 Musterstraße 1", "ort": "Musterstadt", "kurz": "FF Musterstadt", "lang": ""}}
```

Keine Antwort.

### process / processed (Fähigkeit `process`)

```json
{"type": "process", "id": 7, "telegram": {"id": 0, "time": "…", "source": "SDR", "kind": "pocsag",
 "address": "1234567", "sub": "a", "message": "F2 Musterstraße 1", "ort": "", "kurz": "", "lang": ""}}
```

Antwort mit derselben `id`, eine von drei Formen:

```json
{"type": "processed", "id": 7}
{"type": "processed", "id": 7, "telegram": {"message": "Feuer 2 Musterstraße 1"}}
{"type": "processed", "id": 7, "drop": true}
```

- ohne `telegram`: unverändert
- mit `telegram`: die angegebenen Felder ersetzen die alten (es reicht, nur die geänderten zu
  schicken); `kind` muss `pocsag`, `fms`, `zvei` oder `wetter` sein
- `drop`: Telegramm verwerfen

Beim Bearbeiten sind `ort`, `kurz`, `lang` noch leer und `id` ist 0. Kommt nach **2 Sekunden**
keine Antwort, läuft das Telegramm unverändert weiter und im Log steht eine Warnung. Eine
spätere Antwort wird ignoriert. BosLook wartet auf jede Antwort, bevor das nächste Telegramm
kommt – lange Arbeiten gehören darum nicht hierher.

### notify / notified (Fähigkeit `notify`)

Ein Plugin mit `notify` stellt unter „Benachrichtigungsziele" einen Dienst mit seinem Namen
bereit. Alarme benutzen ihn wie die eingebauten Dienste:

```toml
[[notifier]]
name = "Leitstelle"
type = "webhook"            # Name des Plugins
url = "https://example.org/alarm"   # Felder aus [[notifier_field]]
```

```json
{"type": "notify", "id": 3, "target": {"name": "Leitstelle", "url": "https://example.org/alarm"},
 "title": "Alarm FF", "body": "POCSAG 1234567 a\nF2 …", "priority": "normal", "telegram": {…}}
```

- `target`: Einstellungen des Ziels und sein `name`
- `title`, `body`: fertiger Text (Vorlagen des Alarms sind schon angewendet)
- `priority`: `normal`, `high` oder `emergency`
- `telegram`: das Telegramm; `null` bei `boslook test-notify`

Antwort mit derselben `id`, bei Erfolg ohne, bei einem Fehler mit `error`:

```json
{"type": "notified", "id": 3}
{"type": "notified", "id": 3, "error": "Server antwortet mit HTTP 500"}
```

Ohne Antwort nach **30 Sekunden** gilt die Benachrichtigung als fehlgeschlagen. Benachrichtigungen
verschiedener Ziele können gleichzeitig unterwegs sein; die Antworten dürfen in beliebiger
Reihenfolge kommen.

### telegram (Plugin → BosLook, Fähigkeit `input`)

```json
{"type": "telegram", "telegram": {"kind": "fms", "address": "12345678", "sub": "",
 "message": "Status 3", "source": "Leitstelle"}}
```

`kind` und `address` sind Pflicht. `source` ist der Reiter in der Weboberfläche, Standard ist
der Name des Plugins. Zeit setzt BosLook. Das Telegramm durchläuft danach den ganzen Ablauf,
auch die `process`-Plugins.

### http / response (Fähigkeit `web`)

Ein Plugin mit `web` und einem Ordner `web/` hat eine eigene Seite unter `/plugins/<name>/`.
BosLook liefert die Dateien aus `web/` selbst aus (`index.html` als Startseite), nur an
angemeldete Benutzer. In der Weboberfläche erscheint ein Knopf mit dem Namen des Plugins; die
Desktop-App öffnet die Seite im Browser.

Anfragen der Seite an `/plugins/<name>/api/…` (jede Methode) schickt BosLook an das Plugin:

```json
{"type": "http", "id": 9, "method": "GET", "path": "/stats", "query": "tag=heute",
 "body": "", "user": "admin"}
```

- `path`: Pfad ohne `/plugins/<name>/api`
- `query`: Query-String ohne `?`, `body`: Inhalt als Text
- `user`: angemeldeter Benutzer

Antwort mit derselben `id`, entweder als JSON oder als Text:

```json
{"type": "response", "id": 9, "json": {"total": 42}}
{"type": "response", "id": 9, "status": 404, "content_type": "text/plain", "body": "unbekannt"}
```

`status` ist ohne Angabe 200, `content_type` ohne Angabe `text/plain; charset=utf-8`. Ohne
Antwort nach **10 Sekunden** bekommt die Seite den Fehler 502.

In der Seite gelten Adressen relativ zu `/plugins/<name>/`, also z. B. `fetch("api/stats")`.
Mit `<link rel="stylesheet" href="/plugin.css">` sieht die Seite aus wie BosLook, hell und
dunkel nach Systemeinstellung. Die Seite läuft unter derselben Adresse wie BosLook und darf
dessen Schnittstellen nutzen.

### query / result (Plugin → BosLook)

Jedes Plugin kann Daten lesen. Die `id` vergibt das Plugin selbst; BosLook antwortet mit
`result` und derselben `id`. Schreiben ist nicht möglich.

```json
{"type": "query", "id": 1, "query": "sources"}
{"type": "query", "id": 2, "query": "telegrams", "source": "SDR", "limit": 100, "before": 5120}
{"type": "query", "id": 3, "query": "descriptions", "q": "^19388", "sort": "ort", "desc": false, "offset": 0, "limit": 100}
```

| Abfrage        | Parameter                                              | Ergebnis |
|----------------|--------------------------------------------------------|----------|
| `sources`      | –                                                      | Liste der Quellen (Reiter) |
| `telegrams`    | `source` (Pflicht), `limit` (100, höchstens 1000), `before` (nur Telegramme mit kleinerer `id`) | Telegramme, neueste zuerst |
| `descriptions` | `q` (regulärer Ausdruck über alle Spalten), `sort` (`address`, `ort`, `kurz`, `lang`), `desc`, `offset`, `limit` (100, höchstens 1000) | `{"total": …, "rows": [{"address", "ort", "kurz", "lang"}], "notice": …}` |

```json
{"type": "result", "id": 2, "result": [TELEGRAMM, …]}
{"type": "result", "id": 9, "error": "unbekannte Abfrage 'x'"}
```

Antworten kommen in beliebiger Reihenfolge, auch zwischen anderen Nachrichten. Ein Plugin,
das in einem Handler auf ein Ergebnis wartet, muss stdin darum in einem eigenen Thread lesen –
die Bibliotheken tun das.

## Betrieb

- **Start:** beim Start von BosLook, in der Reihenfolge der Abschnitte. Nachrichten, die vor
  dem Start ankommen, warten.
- **Ende:** Wenn BosLook stdin schließt (Beenden, Neustart), muss sich das Plugin beenden.
  Die Bibliotheken tun das automatisch.
- **Absturz:** Endet ein Plugin, startet BosLook es nach **5 Sekunden** neu. Offene Anfragen
  gelten als fehlgeschlagen.
- **Log:** Alles auf stderr erscheint im Log von BosLook als `Plugin '<name>': …` und auf der
  Seite „Plugins" (letzte 100 Zeilen). Text auf stdout, der kein JSON ist, landet mit einem
  Hinweis ebenfalls im Log – `print` für Ausgaben ist der häufigste Fehler.
- **Daten:** `data_dir` gehört dem Plugin. Andere Ordner können sich bei Updates ändern.
- **Sicherheit:** Plugins laufen mit den Rechten von BosLook. Installiere nur Plugins, denen
  du vertraust.

## Python-Bibliothek

`sdk/python/boslook_plugin.py` ist eine einzelne Datei ohne weitere Abhängigkeiten. Kopiere
sie neben dein Plugin.

```python
from boslook_plugin import Plugin, DROP

plugin = Plugin()

@plugin.on_init
def init():
    plugin.log("Einstellungen:", plugin.config, "Daten in", plugin.data_dir)

@plugin.on_process                      # Fähigkeit process
def process(telegram):
    if "Probealarm" in telegram["message"]:
        return DROP                      # verwerfen
    return {"message": telegram["message"].replace("FF", "Feuerwehr")}  # oder None = unverändert

@plugin.on_telegram                     # Fähigkeit observe
def seen(telegram):
    plugin.log("gespeichert:", telegram["id"])

@plugin.on_notify                       # Fähigkeit notify, läuft in eigenem Thread
def notify(target, title, body, priority, telegram):
    if not target.get("url"):
        raise ValueError("keine Adresse")  # Ausnahme = Fehler an BosLook

@plugin.on_http                         # Fähigkeit web
def http(method, path, query, body, user):
    if path == "/zahl":
        return {"telegramme": 42}         # dict/list = JSON, str = Text
    return (404, "text/plain", "unbekannt")  # oder (Status, Typ, Inhalt)

# Fähigkeit input, aus jedem Thread:
# plugin.send_telegram("fms", "12345678", "Status 3", source="Leitstelle")

# Daten lesen, auch innerhalb der Handler:
# plugin.query("telegrams", source="SDR", limit=10)

plugin.run()
```

## Rust-Bibliothek

Das Crate `boslook-plugin` liegt unter `sdk/rust`:

```toml
[dependencies]
boslook-plugin = { path = "../../sdk/rust" }
```

```rust
use boslook_plugin::{HttpRequest, HttpResponse, Init, Notification, Output, Plugin, Process, Telegram};
use serde_json::json;

struct Mein {
    out: Option<Output>,
}

impl Plugin for Mein {
    fn init(&mut self, init: &Init, out: &Output) {
        boslook_plugin::log(format!("Einstellungen: {:?}", init.config));
        // Für Abfragen und Telegramme aufheben; lässt sich auch in Threads geben:
        // out.send_telegram("fms", "12345678", "", "Status 3", Some("Leitstelle"));
        self.out = Some(out.clone());
    }

    fn http(&mut self, r: &HttpRequest) -> HttpResponse {
        match r.path.as_str() {
            "/quellen" => match self.out.as_ref().map(|o| o.query("sources", json!({}))) {
                Some(Ok(list)) => HttpResponse::json(&list),
                _ => HttpResponse::text("Fehler"),
            },
            _ => HttpResponse::not_found(),
        }
    }

    fn process(&mut self, t: &Telegram) -> Process {
        if t.message.contains("Probealarm") {
            return Process::Drop;
        }
        Process::Changed(Telegram { message: t.message.replace("FF", "Feuerwehr"), ..t.clone() })
    }

    fn notify(&mut self, n: &Notification) -> Result<(), String> {
        boslook_plugin::log(format!("{}: {}", n.title, n.body));
        Ok(())
    }
}

fn main() {
    boslook_plugin::run(Mein { out: None });
}
```

Alle Methoden laufen nacheinander im Haupt-Thread; `Output::query` funktioniert trotzdem auch
darin. Methoden, die du nicht brauchst, lässt du weg. Für `json!` braucht das Plugin
`serde_json` als Abhängigkeit.

## Schritt für Schritt

1. Ordner `plugins/mein-plugin/` anlegen.
2. `plugin.toml` schreiben: `name = "mein-plugin"`, `version`, `api = 1`, `command`,
   `capabilities` und die Felder.
3. Programm schreiben (Bibliothek mitkopieren bzw. als Abhängigkeit eintragen, Rust: bauen).
4. Ohne BosLook testen, indem du Nachrichten von Hand eingibst:
   ```sh
   cd plugins/mein-plugin
   printf '%s\n' '{"type":"init","api":1,"boslook":"0.2.0","name":"mein-plugin","config":{},"data_dir":"data"}' \
     '{"type":"process","id":1,"telegram":{"kind":"pocsag","address":"1234567","sub":"a","message":"FF Probe"}}' \
     | python3 mein.py
   ```
   Auf stdout muss genau eine Zeile `{"type": "processed", "id": 1, …}` erscheinen.
5. BosLook neu starten. Die Seite „Plugins" zeigt das Plugin; unter „Einstellungen" erscheint
   der Abschnitt „Plugin: mein-plugin". „aktiv" anhaken, Felder ausfüllen, „Speichern und neu
   starten".
6. Auf der Seite „Plugins" prüfen, ob es „läuft", und das Log ansehen.

## Beispiele

| Plugin                         | Sprache | Fähigkeit | Zeigt                                   |
|--------------------------------|---------|-----------|-----------------------------------------|
| `plugins/webhook`              | Python  | notify    | Benachrichtigungsart mit eigenen Feldern |
| `plugins/beispiel-bearbeitung` | Rust    | process   | Meldungen ändern, Einstellungen lesen    |
| `plugins/statistik`            | Python  | observe, web | eigene Seite, Datenabfragen (Verlauf beim Start, Suche in den Beschreibungen) |

Das Rust-Beispiel muss vor dem Einschalten gebaut werden:

```sh
cd plugins/beispiel-bearbeitung
cargo build --release
```
