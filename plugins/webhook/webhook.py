"""Beispiel-Plugin: Alarme per HTTP POST senden.

Der Server bekommt JSON:
    {"title": "...", "body": "...", "priority": "normal", "telegram": {...}}
"""

import json
import urllib.request

from boslook_plugin import Plugin

plugin = Plugin()


@plugin.on_notify
def notify(target, title, body, priority, telegram):
    data = json.dumps({"title": title, "body": body, "priority": priority, "telegram": telegram}).encode()
    request = urllib.request.Request(target["url"], data=data, method="POST", headers={"Content-Type": "application/json"})
    if target.get("token"):
        request.add_header("Authorization", "Bearer " + target["token"])
    # Fehler (z. B. HTTP 500) werden als Ausnahme an BosLook gemeldet
    with urllib.request.urlopen(request, timeout=plugin.config.get("timeout", 10)):
        pass


plugin.run()
