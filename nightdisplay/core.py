"""配置、日志、时间计划（与平台无关，可单元测试）。"""
import contextlib
import copy
import datetime as dt
import glob
import json
import os
import re
import sys

from . import APP
from .storage import file_lock, write_json

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
    # Logging must never be the reason a display/restore action fails.
    try:
        path = os.path.join(home(), "logs", dt.date.today().strftime("%Y-%m-%d") + ".log")
        with open(path, "a", encoding="utf-8") as f:
            f.write(line + "\n")
        logs = sorted(glob.glob(os.path.join(home(), "logs", "????-??-??.log")))
        for old in logs[:-30]:  # 只保留 30 天
            with contextlib.suppress(OSError):
                os.remove(old)
    except OSError:
        pass
    try:
        if sys.stdout:
            print(line, flush=True)
    except Exception:
        pass
    return line


def read_log_tail(n=6000):
    logs = sorted(glob.glob(os.path.join(home(), "logs", "????-??-??.log")))
    out = ""
    for p in logs[-2:]:
        try:
            with open(p, encoding="utf-8") as f:
                out += f.read()
        except OSError:
            continue
    return out[-n:]


DEFAULT = {
    "version": 3,
    "mode": "auto",          # auto | virtual_only | overlay | screen_off
    "enabled": False,
    "unlock_chord": ["C", "K"],
    "lock": True,
    "override_until": "",
    "autostart": True,
    "autostart_preference": True,
    "start": "20:00",
    "end": "10:30",
    "keep_awake": True,
    "virtual_match": "",     # 虚拟屏名称关键字，空=自动识别
    "hotkey": "Ctrl+Alt+Shift+R",
    "last_test_mode": "", "last_test_at": "", "last_test_ok": False,
    "last_test_requested": "", "last_test_night_ok": False,
    "last_recovery_at": "", "last_recovery_mode": "",
    "extra_ranges": [], "night_mode": "", "test_id": "", "test_aborted": False,
    "awake_token": "", "recovery_pending": False,
    "theme": "light",
    "state": "day",          # day | night  (程序认为当前所处状态)
}


def config_path():
    return os.path.join(home(), "config.json")


def load():
    cfg = copy.deepcopy(DEFAULT)
    try:
        with open(config_path(), encoding="utf-8") as f:
            data = json.load(f)
        if not isinstance(data, dict):
            raise ValueError("配置内容必须是 JSON 对象")
        cfg.update(data)
        validate_config(cfg)
    except FileNotFoundError:
        pass
    except (ValueError, TypeError, KeyError) as e:
        raise RuntimeError("配置文件损坏，请检查 %s：%s" % (config_path(), e)) from e
    return cfg


def save(cfg):
    validate_config(cfg)
    with file_lock(config_path() + ".lock"):
        write_json(config_path(), cfg)


def update(**kw):
    return mutate(lambda c: c.update(kw))


def mutate(fn):
    with file_lock(config_path() + ".lock"):
        c = load()
        fn(c)
        validate_config(c)
        write_json(config_path(), c)
        return c


def validate_config(cfg):
    if not isinstance(cfg, dict):
        raise ValueError("配置内容必须是 JSON 对象")
    parse_hm(cfg.get("start", DEFAULT["start"]))
    parse_hm(cfg.get("end", DEFAULT["end"]))
    extra = cfg.get("extra_ranges", [])
    if not isinstance(extra, (list, tuple)):
        raise ValueError("额外时间段必须是列表")
    for pair in extra:
        if not isinstance(pair, (list, tuple)) or len(pair) != 2:
            raise ValueError("额外时间段应包含开始和结束时间")
        a, b = pair
        if parse_hm(a) == parse_hm(b):
            raise ValueError("额外时间段开始和结束不能相同")
    if cfg.get("mode", "auto") not in ("auto", "virtual_only", "overlay", "screen_off"):
        raise ValueError("未知显示模式")
    if cfg.get("state", "day") not in ("day", "night"):
        raise ValueError("未知显示状态")
    if cfg.get("theme", "light") not in ("light", "dark"):
        raise ValueError("未知界面主题")
    if cfg.get("night_mode", "") not in ("", "virtual_only", "overlay", "screen_off"):
        raise ValueError("未知夜间实际模式")
    # Runtime state is written by several processes; reject wrong JSON types
    # early instead of letting a later display action fail halfway through.
    for key in ("virtual_match", "hotkey", "test_id", "awake_token", "override_until",
                "last_test_at", "last_test_mode", "last_test_requested",
                "last_recovery_at", "last_recovery_mode"):
        if key in cfg and not isinstance(cfg[key], str):
            raise ValueError("%s 必须为字符串" % key)
    for key in ("last_test_mode", "last_test_requested", "last_recovery_mode"):
        if cfg.get(key) and cfg[key] not in ("auto", "virtual_only", "overlay", "screen_off"):
            raise ValueError("未知测试模式")
    if cfg.get("virtual_match"):
        if len(cfg["virtual_match"]) > 120:
            raise ValueError("虚拟屏匹配规则过长（最多 120 字符）")
        try:
            re.compile(cfg["virtual_match"])
        except re.error as e:
            raise ValueError("虚拟屏匹配规则不是有效正则表达式：%s" % e) from e
    if "unlock_chord" in cfg:
        from .chord import validate
        if not isinstance(cfg["unlock_chord"], list):
            raise ValueError("解锁组合键应为按键列表")
        # Store the normalized form so a hand-edited config such as
        # ["ctrl", "k"] works in the global key reader as well.
        cfg["unlock_chord"] = validate(cfg["unlock_chord"])
    for key in ("enabled", "lock", "keep_awake", "autostart", "autostart_preference",
                "last_test_ok", "last_test_night_ok", "test_aborted", "recovery_pending"):
        if key in cfg and not isinstance(cfg[key], bool):
            raise ValueError("%s 必须为 true 或 false" % key)


def parse_hm(s):
    if not re.fullmatch(r"\d{1,2}:\d{2}", str(s).strip()):
        raise ValueError("时间格式应为 HH:MM，例如 20:00")
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


def parse_ranges(text):
    """解析额外时间段文本，如 "12:00-13:30, 02:00-04:00"，返回 [(start,end)]。"""
    out = []
    for part in re.split(r"[,，;；\s]+", (text or "").strip()):
        if not part:
            continue
        m = re.fullmatch(r"(\d{1,2}:\d{2})[-~–](\d{1,2}:\d{2})", part)
        if not m:
            raise ValueError("时间段格式应为 HH:MM-HH:MM：%s" % part)
        a, b = ["%02d:%02d" % parse_hm(x) for x in m.groups()]
        if a == b:
            raise ValueError("时间段开始和结束不能相同：%s" % part)
        out.append((a, b))
    return out


def ranges(cfg):
    """全部夜间时间段：主时间段 + 额外时间段（每段都可跨午夜）。"""
    r = [(cfg["start"], cfg["end"])]
    for x in cfg.get("extra_ranges") or []:
        r.append(tuple(x))
    return r


def in_night(cfg, now=None):
    return any(is_night(a, b, now) for a, b in ranges(cfg))


def boundaries(cfg):
    return sorted({"%02d:%02d" % parse_hm(t) for r in ranges(cfg) for t in r})


def next_switch(cfg, now=None):
    """返回 (下一次动作 'night'/'day', 距离秒数)，支持多个、重叠、跨午夜时间段。"""
    now = now or dt.datetime.now()
    cur = in_night(cfg, now)
    base = now.replace(second=0, microsecond=0)
    for i in range(1, 1441):
        t = base + dt.timedelta(minutes=i)
        if in_night(cfg, t) != cur:
            return ("day" if cur else "night"), int((t - now).total_seconds())
    return None, 86400  # 全天覆盖或没有有效时间段，不存在切换点


def save_backup(kind, data):
    with file_lock(os.path.join(home(), "backups", kind + ".lock")):
        return _save_backup(kind, data)


def _save_backup(kind, data):
    t = dt.datetime.now()
    while True:  # Windows 时钟精度低，同一时刻多次备份时顺延 1 微秒，保证文件名唯一且有序
        p = os.path.join(home(), "backups", "%s-%s.json" % (kind, t.strftime("%Y%m%d-%H%M%S-%f")))
        if not os.path.exists(p):
            break
        t += dt.timedelta(microseconds=1)
    write_json(p, data)
    files = sorted(glob.glob(os.path.join(home(), "backups", kind + "-2*.json")))
    for old in files[:-10]:
        with contextlib.suppress(OSError):
            os.remove(old)
    # "最后已知正常的白天配置"单独保存，永不轮转
    write_json(os.path.join(home(), "backups", "%s-lastgood.json" % kind), data)
    return p


def load_backup(kind):
    p = os.path.join(home(), "backups", "%s-lastgood.json" % kind)
    if not os.path.exists(p):
        return None
    with open(p, encoding="utf-8") as f:
        data = json.load(f)
    if not isinstance(data, dict):
        raise ValueError("备份内容必须是 JSON 对象")
    return data
