# Verarbeitung und Alarme

## Filter

Filter arbeiten mit regulären Ausdrücken auf `address`, `sub` und `message`, optional
eingeschränkt auf `kind` und mit `source` auf den Kanal (Name des Eingangs bzw. Reiter, z. B.
`source = "^SDR$"`). Das Feld `source` gibt es genauso bei Bearbeitungsregeln, Alarmen und
den Ereignissen der Desktop-App. Ein Filter mit `source` gilt nur für die passenden Kanäle,
einer ohne `source` für alle. Ein Telegramm wird verworfen, wenn ein Filter mit
`exclude = true` passt. Gelten für seinen Kanal Filter ohne `exclude`, muss mindestens einer
davon passen; Kanäle ohne eigene Filter zeigen also alles.
Beispiele stehen in `boslook.example.toml`.

## Bearbeitung

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
