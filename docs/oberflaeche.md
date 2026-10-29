# Weboberfläche und Desktop-App

## Weboberfläche

Die Kopfzeile zeigt die Version von BosLook. Jede Quelle hat einen eigenen Reiter, sortiert
nach dem Alphabet. Die Reiter lassen sich mit der Maus verschieben, „Ansicht" → „Reiter A–Z
sortieren" stellt die alphabetische Reihenfolge wieder her. Neue Telegramme in anderen Reitern
werden als Zahl am Reiter angezeigt. Unter „Ansicht" lassen sich Spalten ein- und ausblenden und
mit ▲/▼ verschieben; Spalten lassen sich auch an der Überschrift verschieben und am rechten
Rand breiter ziehen. „Spalten zurücksetzen" stellt den Standard wieder her. Ebenfalls unter „Ansicht" lassen sich
Reiter ausblenden (nur in diesem Browser, ihre Telegramme werden weiter empfangen und
gemeldet) und mit „Löschen" samt allen Telegrammen aus der Datenbank entfernen. Löschen geht nur
bei Reitern ohne Eingang, z. B. nach dem Umbenennen eines Eingangs; sonst wäre der Reiter beim
nächsten Telegramm wieder da. „Clients" zeigt die
weiterleitenden Clients und erzeugt Einrichtungscodes (siehe
[Weiterleitung](installation.md#weiterleitung-an-einen-server)). Ein Hinweis „Update …“ erscheint, wenn
eine neue Version bereitsteht ([Updates](installation.md#updates)). Reihenfolge, Breiten
und der zuletzt gewählte Reiter werden im Browser gespeichert. Seiten von Plugins stehen im Menü
„Plugins", die Einstellungen hinter ⚙. Auf schmalen Bildschirmen liegt das alles hinter ☰.

Unter „Einstellungen" lässt sich die Konfiguration in einem Formular bearbeiten. Die Abschnitte
sind zugeklappt; enthält einer beim Speichern einen Fehler, geht er auf. Kommentare
in der Datei bleiben dabei erhalten. Passwörter und Tokens für Benutzer und Clients werden im
Klartext eingegeben und nur als Hash gespeichert; ein leeres Feld lässt sie unverändert. Über
„Als Text bearbeiten" lässt sich die Datei auch direkt bearbeiten. Beim Speichern wird die
Konfiguration geprüft; ist etwas falsch, wird nichts gespeichert und der Fehler angezeigt. „Speichern und neu starten" startet BosLook mit der neuen Konfiguration neu
(danach ist eine neue Anmeldung nötig). Jeder angemeldete Benutzer kann die Konfiguration
einschließlich Passwort-Hashes und Tokens sehen und ändern.

## Desktop-App

`boslook-desktop` verbindet sich wie die Weboberfläche mit einem BosLook-Server und zeigt die
Telegramme in Reitern. Zusätzlich führt es lokale Ereignisse aus (`[[event]]`, Felder wie bei
den Filtern):

- `priority = "normal"`: Systembenachrichtigung
- `priority = "high"` / `"emergency"`: Fenster im Vordergrund, bis es quittiert wird
- `sound`: Tondatei abspielen
- `program` und `args`: Programm starten, in `args` werden die Platzhalter wie bei den
  Alarmregeln ersetzt (`{kind}` ist dabei großgeschrieben, z. B. `POCSAG`)
- `title` und `text`: Vorlagen für Benachrichtigung bzw. Alarmfenster

Gleiche Telegramme lösen innerhalb von 60 Sekunden nur einmal aus (`repeat`). Beim ersten
Start wird `boslook-desktop.toml` angelegt und unter „Einstellungen" geöffnet. Unter
„Server-Konfiguration" lässt sich die Konfiguration des Servers bearbeiten, beides als Formular
wie in der Weboberfläche oder als Text. „Beschreibungen" öffnet den Editor für das
Adressbuch in einem eigenen Fenster, die Einträge im Menü „Plugins" (Übersicht
und Plugins mit eigener Seite) öffnen den Browser. Die Reiter sind alphabetisch sortiert. Spalten
lassen sich unter „Ansicht" ein- und ausblenden und mit ▲/▼ verschieben und (wie in der
Weboberfläche) mit der Maus breiter ziehen; alles bleibt gespeichert. Reiter lassen sich dort
ebenso ausblenden und löschen. Die Abschnitte der Formulare sind beim Start zugeklappt.

Beim Schließen läuft die App im Infobereich weiter („Beenden" im Menü des Symbols). Unter
Linux braucht das eine Arbeitsumgebung mit Infobereich (KDE, XFCE, GNOME mit
AppIndicator-Erweiterung), sonst wird die App beim Schließen beendet. Unter Wayland läuft die
App über XWayland, weil sich Fenster dort sonst weder verstecken noch im Vordergrund halten lassen.
