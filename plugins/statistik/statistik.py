"""Beispiel-Plugin: zählt Telegramme und zeigt sie auf einer eigenen Seite.

Zeigt die Fähigkeiten observe (mitlesen) und web (eigene Seite), dazu Datenabfragen:
beim Start wird der Verlauf aus der Datenbank geladen, die Suche nutzt die Beschreibungen.
"""

import threading
import urllib.parse
from collections import Counter
from datetime import datetime, timedelta

from boslook_plugin import Plugin

plugin = Plugin()
lock = threading.Lock()
#: (Zeit, Telegramm) der letzten Stunden
seen = []


def hours():
    return int(plugin.config.get("stunden", 24))


def local_time(telegram):
    # "2026-10-22T21:02:51.123+02:00" -> Ortszeit ohne Bruchteile
    return datetime.strptime(telegram["time"][:19], "%Y-%m-%dT%H:%M:%S")


def forget_old():
    limit = datetime.now() - timedelta(hours=hours())
    seen[:] = [(t, x) for t, x in seen if t >= limit]


@plugin.on_init
def load_history():
    # Verlauf aus der Datenbank, damit die Zahlen nach einem Neustart stimmen
    limit = datetime.now() - timedelta(hours=hours())
    history = []
    for source in plugin.query("sources"):
        for t in plugin.query("telegrams", source=source, limit=1000):
            if local_time(t) >= limit:
                history.append((local_time(t), t))
    with lock:
        seen.extend(history)
    plugin.log(f"{len(history)} Telegramme der letzten {hours()} Stunden geladen")


@plugin.on_telegram
def count(telegram):
    with lock:
        seen.append((local_time(telegram), telegram))
        forget_old()


def stats():
    with lock:
        forget_old()
        items = list(seen)
    now = datetime.now().replace(minute=0, second=0, microsecond=0)
    per_hour = Counter(t.replace(minute=0, second=0, microsecond=0) for t, _ in items)
    timeline = [now - timedelta(hours=h) for h in reversed(range(hours()))]
    top = Counter((x["kind"], x["address"], x["sub"]) for _, x in items).most_common(10)
    names = {(x["kind"], x["address"], x["sub"]): x for _, x in items}
    return {
        "total": len(items),
        "hours": [{"hour": h.strftime("%H:%M"), "count": per_hour.get(h, 0)} for h in timeline],
        "top": [
            {"kind": k, "address": a, "sub": s, "count": n, "ort": names[(k, a, s)].get("ort", ""), "kurz": names[(k, a, s)].get("kurz", "")}
            for (k, a, s), n in top
        ],
    }


@plugin.on_http
def http(method, path, query, body, user):
    if path == "/stats":
        return stats()
    if path == "/suche":
        q = urllib.parse.parse_qs(query).get("q", [""])[0]
        return plugin.query("descriptions", q=q, limit=10)
    return (404, "text/plain", "unbekannt: " + path)


plugin.run()
