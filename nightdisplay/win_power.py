"""Use a disposable night power scheme and restore the user's original scheme."""
import json
import os
import re
import uuid

from .core import home, log
from .storage import write_json

GUID = r"[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}"


def _run(*args):
    from .backends import run
    return run(["powercfg", *args], timeout=15).stdout


def _path():
    return os.path.join(home(), "night-power.json")


def _active():
    match = re.search(GUID, _run("/getactivescheme"))
    if not match:
        raise RuntimeError("无法读取当前 Windows 电源计划")
    return match.group().lower()


def _read():
    try:
        with open(_path(), encoding="utf-8") as f:
            state = json.load(f)
        if not isinstance(state, dict):
            raise ValueError("invalid power plan state")
        if not all(isinstance(state.get(k), str) and re.fullmatch(GUID, state[k]) for k in ("original", "temporary")):
            raise ValueError("invalid power plan IDs")
        return state
    except FileNotFoundError:
        return None
    except (OSError, ValueError, TypeError, KeyError) as e:
        log("夜间电源记录不可读，忽略：%s" % e)
        return None


def enter(keep_display_on=True):
    state = _read()
    if state:
        if _active() == state["temporary"]:
            return
        restore()
    original, temporary = _active(), str(uuid.uuid4())
    _run("/duplicatescheme", original, temporary)
    state = {"original": original, "temporary": temporary}
    write_json(_path(), state)
    try:
        _run("/changename", temporary, "NightDisplay night")
        # Do not disable hibernation globally. Only change this temporary plan.
        settings = [("SUB_SLEEP", "STANDBYIDLE"), ("SUB_SLEEP", "HIBERNATEIDLE"),
                    ("SUB_SLEEP", "HYBRIDSLEEP"), ("SUB_BUTTONS", "LIDACTION")]
        if keep_display_on:
            settings.append(("SUB_VIDEO", "VIDEOIDLE"))
        for subgroup, setting in settings:
            for kind in ("/setacvalueindex", "/setdcvalueindex"):
                _run(kind, temporary, subgroup, setting, "0")
        _run("/setactive", temporary)
        if _active() != temporary:
            raise RuntimeError("Windows 未启用夜间电源配置")
        what = "自动睡眠、休眠和合盖睡眠" + ("和显示空闲超时" if keep_display_on else "")
        log("夜间电源配置已启用：%s已关闭" % what)
    except Exception:
        restore()
        raise


def restore():
    state = _read()
    if not state:
        return
    # Respect a power plan that the user explicitly selected during the night.
    if _active() == state["temporary"]:
        _run("/setactive", state["original"])
        if _active() != state["original"]:
            raise RuntimeError("Windows 未恢复原电源计划")
    _run("/delete", state["temporary"])
    os.unlink(_path())
    log("原电源计划已恢复，临时夜间计划已删除")
