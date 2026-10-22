# BosLook

BosLook dekodiert POCSAG-, FMS- und ZVEI-Telegramme von Soundkarte, SDR oder WAV-Datei,
speichert sie und zeigt sie in einer Weboberfläche und einer Desktop-App an. Alarme
benachrichtigen per Telegram, Pushover, E-Mail oder über Plugins.

Diese Seiten beschreiben das **Plugin-System**. Installation und Bedienung von BosLook stehen
in der `README.md` des Projekts.

- [Plugins benutzen](#plugins-benutzen) – installieren, einschalten, überwachen
- [Plugins entwickeln](PLUGINS.md) – Protokoll, `plugin.toml`, Bibliotheken für Python und Rust
- [Anleitung für KI-Agenten](PLUGINS-AGENT.md) – kompakte Referenz mit Regeln, Checkliste und
  vollständigem Beispiel, zum Weitergeben an einen KI-Assistenten

## Plugins benutzen

Plugins erweitern den Server um

- **Mitlesen**: jedes gespeicherte Telegramm bekommen, z. B. für Statistiken oder andere Systeme
- **Bearbeiten**: Telegramme vor der Verarbeitung ändern oder verwerfen
- **Benachrichtigungsarten**: neue Dienste neben Telegram, Pushover und E-Mail
- **Eingänge**: Telegramme aus anderen Quellen liefern
- **Eigene Seiten**: z. B. Statistiken oder Einstellungen, erreichbar über einen Knopf mit dem
  Namen des Plugins in der Weboberfläche und der Desktop-App (öffnet den Browser)

### Installieren

Ein Plugin ist ein Ordner. Kopiere ihn nach `plugins/` neben deine `boslook.toml`:

```text
boslook.toml
plugins/
  webhook/
    plugin.toml
    webhook.py
```

Manche Plugins brauchen noch etwas, z. B. Python oder einen Build-Schritt; das steht beim
Plugin. Installieren über die Weboberfläche ist bewusst nicht möglich: Plugins sind Programme
mit den Rechten von BosLook. **Installiere nur Plugins, denen du vertraust.**

### Einschalten

Unter „Einstellungen" hat jedes installierte Plugin einen Abschnitt „Plugin: *Name*". Hake
„aktiv" an, fülle die Felder aus und wähle „Speichern und neu starten". In der Datei sieht das
so aus:

```toml
[plugin.webhook]
timeout = 10
```

Mit `enabled = false` im Abschnitt bleibt ein Plugin aus, ohne dass seine Einstellungen
verloren gehen. Bearbeiten-Plugins laufen in der Reihenfolge ihrer Abschnitte.

Ein Plugin mit Benachrichtigungsart erscheint unter „Benachrichtigungsziele" als weiterer
Dienst:

```toml
[[notifier]]
name = "Leitstelle"
type = "webhook"
url = "https://example.org/alarm"
```

`boslook test-notify` schickt auch an diese Ziele eine Testnachricht.

### Überwachen

Die Seite **„Plugins"** (Knopf in der Weboberfläche und in der Desktop-App) zeigt alle
installierten Plugins mit Version, Fähigkeiten, Zustand und den letzten Logzeilen. Dort stehen
auch Fehler in einer `plugin.toml`. Das Log erscheint außerdem in der Ausgabe von BosLook als
`Plugin '<name>': …`.

- Ein abgestürztes Plugin startet BosLook nach 5 Sekunden neu.
- Antwortet ein Bearbeiten-Plugin nicht innerhalb von 2 Sekunden, läuft das Telegramm
  unverändert weiter, damit kein Alarm verloren geht.

### Mitgelieferte Beispiele

| Plugin                 | Was es macht                                                   |
|------------------------|----------------------------------------------------------------|
| `webhook`              | Benachrichtigungsart: Alarme per HTTP POST (JSON) senden; braucht Python 3 |
| `beispiel-bearbeitung` | Ersetzt Abkürzungen in Meldungen (z. B. `FF=Freiwillige Feuerwehr`); vorher `cargo build --release` im Ordner |
| `statistik`            | Eigene Seite mit Telegrammen pro Stunde, den häufigsten Adressen und einer Suche in den Beschreibungen; braucht Python 3 |
