"""Check releases without downloading or executing an update automatically."""
import json
import re
import urllib.request

from . import VERSION

RELEASES = "https://github.com/CKNB888/NightDisplay/releases"


def version_tuple(tag):
    m = re.fullmatch(r"v?(\d+)\.(\d+)\.(\d+)", tag)
    return tuple(map(int, m.groups())) if m else None


def check():
    req = urllib.request.Request("https://api.github.com/repos/CKNB888/NightDisplay/releases/latest",
                                 headers={"User-Agent": "NightDisplay/" + VERSION, "Accept": "application/vnd.github+json"})
    with urllib.request.urlopen(req, timeout=8) as response:
        raw = response.read(1024 * 1024 + 1)
    if len(raw) > 1024 * 1024:
        raise RuntimeError("更新信息过大，已停止读取")
    data = json.loads(raw)
    tag = data.get("tag_name", "")
    remote = version_tuple(tag)
    if not remote or data.get("prerelease") or data.get("draft"):
        raise RuntimeError("没有有效的正式版本信息")
    return {"version": tag, "newer": remote > version_tuple(VERSION), "url": RELEASES + "/tag/" + tag}
