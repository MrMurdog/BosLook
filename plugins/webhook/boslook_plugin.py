"""Bibliothek für BosLook-Plugins in Python (Schnittstelle 1).

Einfach neben das Plugin kopieren, es werden keine weiteren Pakete gebraucht.
Doku: docs/PLUGINS.md

    from boslook_plugin import Plugin, DROP

    plugin = Plugin()

    @plugin.on_process
    def process(telegram):
        if "Probealarm" in telegram["message"]:
            return DROP
        return {"message": telegram["message"].upper()}

    plugin.run()
"""

import json
import sys
import threading
import traceback

API = 1

#: Rückgabe von ``on_process``: Telegramm verwerfen
DROP = object()


class Plugin:
    def __init__(self):
        #: Einstellungen aus ``[plugin.<name>]`` (ohne ``enabled``)
        self.config = {}
        #: Name des Plugins
        self.name = None
        #: Ordner für eigene Daten (``plugins/<name>/data``)
        self.data_dir = None
        #: Version von BosLook
        self.boslook = None
        self._init = None
        self._telegram = None
        self._process = None
        self._notify = None
        self._out = threading.Lock()

    def on_init(self, handler):
        """``handler()`` läuft einmal nach dem Start; ``config`` usw. sind dann gesetzt."""
        self._init = handler
        return handler

    def on_telegram(self, handler):
        """Fähigkeit ``observe``: ``handler(telegram)`` für jedes gespeicherte Telegramm."""
        self._telegram = handler
        return handler

    def on_process(self, handler):
        """Fähigkeit ``process``: ``handler(telegram)`` gibt zurück:
        ``None`` (unverändert), ``DROP`` (verwerfen) oder ein dict mit geänderten Feldern."""
        self._process = handler
        return handler

    def on_notify(self, handler):
        """Fähigkeit ``notify``: ``handler(target, title, body, priority, telegram)``.
        ``target`` sind die Einstellungen des Benachrichtigungsziels (mit ``name``),
        ``telegram`` ist ``None`` bei ``boslook test-notify``. Eine Ausnahme meldet den
        Fehler an BosLook. Läuft in einem eigenen Thread."""
        self._notify = handler
        return handler

    def send_telegram(self, kind, address, message="", sub="", source=None):
        """Fähigkeit ``input``: liefert ein Telegramm an BosLook (aus jedem Thread erlaubt).
        ``kind``: pocsag, fms, zvei oder wetter; ``source``: Reiter, Standard = Name des Plugins."""
        telegram = {"kind": kind, "address": address, "sub": sub, "message": message}
        if source:
            telegram["source"] = source
        self._send({"type": "telegram", "telegram": telegram})

    def log(self, *args):
        """Zeile ins Log von BosLook (stderr). ``print`` nicht benutzen: stdout gehört dem Protokoll."""
        print(*args, file=sys.stderr, flush=True)

    def _send(self, message):
        line = json.dumps(message, ensure_ascii=False)
        with self._out:
            sys.stdout.write(line + "\n")
            sys.stdout.flush()

    def _notify_thread(self, msg):
        reply = {"type": "notified", "id": msg["id"]}
        try:
            self._notify(msg["target"], msg["title"], msg["body"], msg["priority"], msg.get("telegram"))
        except Exception as e:
            reply["error"] = str(e) or type(e).__name__
        self._send(reply)

    def _handle(self, msg):
        kind = msg.get("type")
        if kind == "init":
            if msg.get("api") != API:
                self.log(f"BosLook spricht Schnittstelle {msg.get('api')}, dieses Plugin {API}")
            self.config = msg.get("config") or {}
            self.name = msg.get("name")
            self.data_dir = msg.get("data_dir")
            self.boslook = msg.get("boslook")
            if self._init:
                self._init()
        elif kind == "telegram" and self._telegram:
            self._telegram(msg["telegram"])
        elif kind == "process":
            reply = {"type": "processed", "id": msg["id"]}
            try:
                result = self._process(msg["telegram"]) if self._process else None
            except Exception:
                self.log(traceback.format_exc())
                result = None
            if result is DROP:
                reply["drop"] = True
            elif result is not None:
                reply["telegram"] = result
            self._send(reply)
        elif kind == "notify":
            if self._notify:
                # Kein Daemon-Thread: Python wartet beim Beenden, bis die Benachrichtigung raus ist
                threading.Thread(target=self._notify_thread, args=(msg,)).start()
            else:
                self._send({"type": "notified", "id": msg["id"], "error": "Plugin hat kein on_notify"})

    def run(self):
        """Liest Nachrichten, bis BosLook stdin schließt (dann endet das Plugin)."""
        for line in sys.stdin:
            if not line.strip():
                continue
            try:
                self._handle(json.loads(line))
            except Exception:
                self.log(traceback.format_exc())
