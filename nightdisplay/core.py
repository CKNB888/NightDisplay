# -*- coding: utf-8 -*-
"""配置、日志、时间计划（与平台无关，可单元测试）。"""
import datetime as dt, json, os, sys, glob

from . import APP

IS_WIN = sys.platform.startswith("win")
IS_MAC = sys.platform == "darwin"
IS_LINUX = sys.platform.startswith("linux")


def app_home():
    if os.environ.get("NIGHTDISPLAY_HOME"):
        return os.environ["NIGHTDISPLAY_HOME"]
    if IS_WIN:
        return os.path.join(os.environ.get("LOCALAPPDATA") or os.path.expanduser("~"), APP)
    if IS_MAC:
        return os.path.expanduser("~/Library/Application Support/" + APP)
    return os.path.join(os.environ.get("XDG_STATE_HOME") or os.path.expanduser("~/.local/state"), "nightdisplay")


def home():
    h = app_home()
    os.makedirs(os.path.join(h, "backups"), exist_ok=True)
    os.makedirs(os.path.join(h, "logs"), exist_ok=True)
    return h


def log(msg):
    line = "%s  %s" % (dt.datetime.now().strftime("%Y-%m-%d %H:%M:%S"), msg)
    path = os.path.join(home(), "logs", dt.date.today().strftime("%Y-%m-%d") + ".log")
    with open(path, "a", encoding="utf-8") as f:
        f.write(line + "\n")
    try:
        if sys.stdout:
            print(line, flush=True)
    except Exception:
        pass
    logs = sorted(glob.glob(os.path.join(home(), "logs", "*.log")))
    for old in logs[:-30]:  # 只保留 30 天
        try:
            os.remove(old)
        except OSError:
            pass
    return line


def read_log_tail(n=6000):
    logs = sorted(glob.glob(os.path.join(home(), "logs", "*.log")))
    out = ""
    for p in logs[-2:]:
        with open(p, encoding="utf-8") as f:
            out += f.read()
    return out[-n:]


DEFAULT = {
    "version": 3,
    "mode": "auto",          # auto | virtual_only | overlay | screen_off
    "enabled": False,
    "start": "20:00",
    "end": "10:30",
    "keep_awake": True,
    "virtual_match": "",     # 虚拟屏名称关键字，空=自动识别
    "hotkey": "Ctrl+Alt+Shift+R",
    "last_test_mode": "", "last_test_at": "", "last_test_ok": False,
    "last_recovery_at": "", "last_recovery_mode": "",
    "state": "day",          # day | night  (程序认为当前所处状态)
}


def config_path():
    return os.path.join(home(), "config.json")


def load():
    cfg = dict(DEFAULT)
    try:
        with open(config_path(), encoding="utf-8") as f:
            cfg.update(json.load(f))
    except Exception:
        pass
    return cfg


def save(cfg):
    tmp = config_path() + ".tmp"
    with open(tmp, "w", encoding="utf-8") as f:
        json.dump(cfg, f, ensure_ascii=False, indent=2)
    os.replace(tmp, config_path())


def update(**kw):
    c = load(); c.update(kw); save(c); return c


def parse_hm(s):
    h, m = str(s).strip().split(":")
    h, m = int(h), int(m)
    if not (0 <= h <= 23 and 0 <= m <= 59):
        raise ValueError("时间格式应为 HH:MM，例如 20:00")
    return h, m


def is_night(start, end, now=None):
    now = now or dt.datetime.now()
    n = now.hour * 60 + now.minute
    sh, sm = parse_hm(start); eh, em = parse_hm(end)
    s, e = sh * 60 + sm, eh * 60 + em
    if s == e:
        return False
    if s < e:
        return s <= n < e
    return n >= s or n < e


def seconds_until(hm, now=None):
    now = now or dt.datetime.now()
    h, m = parse_hm(hm)
    t = now.replace(hour=h, minute=m, second=0, microsecond=0)
    if t <= now:
        t += dt.timedelta(days=1)
    return int((t - now).total_seconds())


def next_switch(cfg, now=None):
    """返回 (下一次动作 'night'/'day', 距离秒数)。"""
    now = now or dt.datetime.now()
    if is_night(cfg["start"], cfg["end"], now):
        return "day", seconds_until(cfg["end"], now)
    return "night", seconds_until(cfg["start"], now)


def save_backup(kind, data):
    name = "%s-%s.json" % (kind, dt.datetime.now().strftime("%Y%m%d-%H%M%S-%f"))
    p = os.path.join(home(), "backups", name)
    with open(p, "w", encoding="utf-8") as f:
        json.dump(data, f, ensure_ascii=False, indent=1)
    files = sorted(glob.glob(os.path.join(home(), "backups", kind + "-2*.json")))
    for old in files[:-10]:
        os.remove(old)
    # "最后已知正常的白天配置"单独保存，永不轮转
    with open(os.path.join(home(), "backups", "%s-lastgood.json" % kind), "w", encoding="utf-8") as f:
        json.dump(data, f, ensure_ascii=False, indent=1)
    return p


def load_backup(kind):
    p = os.path.join(home(), "backups", "%s-lastgood.json" % kind)
    if not os.path.exists(p):
        return None
    with open(p, encoding="utf-8") as f:
        return json.load(f)
