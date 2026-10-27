# BosLook

BosLook dekodiert POCSAG-, FMS- und ZVEI-Telegramme von Soundkarte, RTL-SDR-Stick oder
WAV-Datei, holt Wetterwarnungen des DWD ab, speichert alles und zeigt es live in einer
Weboberfläche und einer Desktop-App an. Alarme benachrichtigen per Telegram, Pushover, E-Mail
oder über Plugins. Empfangsrechner (z. B. ein Raspberry Pi am Funkscanner) leiten ihre
Telegramme an einen zentralen Server weiter und werden von dort eingestellt.

> Das Abhören und Auswerten von BOS-Funk ist nur Berechtigten erlaubt.

- [Installation und Betrieb](installation.md) – Download, Server, Clients, Updates, selbst bauen
- [Eingänge](eingaenge.md) – Soundkarte, `rtl_fm`, mehrere Frequenzen mit `rtl_sdr`, stdin, WAV
- [Verarbeitung und Alarme](verarbeitung.md) – Filter, Bearbeitung, Beschreibungen,
  Benachrichtigungen, Wetterwarnungen
- [Weboberfläche und Desktop-App](oberflaeche.md)
- [Plugins benutzen](plugins.md), [Plugins entwickeln](PLUGINS.md),
  [Anleitung für KI-Agenten](PLUGINS-AGENT.md)

Alle Einstellungen mit Erklärungen stehen außerdem in `boslook.example.toml` und
`boslook-client.example.toml`.
