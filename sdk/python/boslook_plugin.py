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

import itertools
import json
import queue
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
        self._http = None
        self._out = threading.Lock()
        self._ids = itertools.count(1)
        #: offene Abfragen: id -> [threading.Event, Antwort]
        self._queries = {}

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

    def on_http(self, handler):
        """Fähigkeit ``web``: ``handler(method, path, query, body, user)`` beantwortet Anfragen
        der eigenen Seite an ``api/…`` (``path`` ohne ``/api``, z. B. ``/stats``). Rückgabe:
        dict oder list (wird JSON), str (Text) oder ``(status, content_type, body)``."""
        self._http = handler
        return handler

    def query(self, query, timeout=10, **params):
        """Daten von BosLook lesen (aus jedem Handler und Thread erlaubt):
        ``query("sources")``, ``query("telegrams", source="SDR", limit=100, before=None)``,
        ``query("descriptions", q="^19388", sort="ort", desc=False, offset=0, limit=100)``.
        Wirft ``RuntimeError`` bei einem Fehler oder ohne Antwort."""
        qid = next(self._ids)
        waiting = [threading.Event(), None]
        self._queries[qid] = waiting
        self._send({"type": "query", "id": qid, "query": query, **params})
        if not waiting[0].wait(timeout):
            self._queries.pop(qid, None)
            raise RuntimeError(f"keine Antwort auf Abfrage {query}")
        reply = waiting[1]
        if "error" in reply:
            raise RuntimeError(reply["error"])
        return reply["result"]

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

    def _http_reply(self, msg):
        reply = {"type": "response", "id": msg["id"]}
        try:
            result = self._http(msg["method"], msg["path"], msg.get("query", ""), msg.get("body", ""), msg.get("user"))
        except Exception:
            self.log(traceback.format_exc())
            result = (500, "text/plain; charset=utf-8", "Fehler im Plugin, siehe Log")
        if isinstance(result, tuple):
            reply["status"], reply["content_type"], reply["body"] = result
        elif isinstance(result, str):
            reply["body"] = result
        else:
            reply["json"] = result
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
        elif kind == "http":
            if self._http:
                self._http_reply(msg)
            else:
                self._send({"type": "response", "id": msg["id"], "status": 404, "body": "Plugin hat kein on_http"})

    def _read(self, messages):
        # Eigener Thread, damit Antworten auf query() ankommen, während ein Handler wartet
        for line in sys.stdin:
            if not line.strip():
                continue
            try:
                msg = json.loads(line)
            except ValueError:
                self.log("keine JSON-Nachricht:", line.strip())
                continue
            waiting = self._queries.pop(msg.get("id"), None) if msg.get("type") == "result" else None
            if waiting:
                waiting[1] = msg
                waiting[0].set()
            else:
                messages.put(msg)
        messages.put(None)

    def run(self):
        """Verarbeitet Nachrichten, bis BosLook stdin schließt (dann endet das Plugin)."""
        messages = queue.Queue()
        threading.Thread(target=self._read, args=(messages,), daemon=True).start()
        while (msg := messages.get()) is not None:
            try:
                self._handle(msg)
            except Exception:
                self.log(traceback.format_exc())
