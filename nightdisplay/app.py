"""NightDisplay 主程序：命令行 / 托盘常驻 / 图形界面 / 全局快捷键 / 防窥黑幕。"""
import argparse
import contextlib
import datetime as dt
import json
import os
import queue
import sys
import threading
import time
import uuid

from . import APP, ipc
from . import backends as B
from .core import (
    IS_LINUX,
    IS_MAC,
    IS_WIN,
    home,
    in_night,
    load,
    log,
    manual_night_until,
    next_switch,
    parse_hm,
    save,
    update,
)

# ---------------- 单实例 IPC ----------------

def send_ipc(cmd, timeout=60):
    return ipc.send(cmd, timeout)


def resident_action(command):
    result = send_ipc(command)
    if result is not None:
        return result
    B.spawn_detached(B.self_cmd() + ["--tray"])
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        time.sleep(0.2)
        result = send_ipc(command)
        if result is not None:
            return result
    raise RuntimeError("常驻程序未启动，尚未切换显示；请打开图形界面并查看日志")

# ---------------- 业务动作 ----------------

@B.serialized
def activate(mode, start, end):
    start, end = ["%02d:%02d" % parse_hm(t) for t in (start, end)]
    if parse_hm(start) == parse_hm(end):
        raise RuntimeError("开始和结束时间不能相同")
    cfg = load()
    B.validate_mode(mode)
    real = B.resolve_mode(mode, cfg.get("virtual_match", ""))
    tested = cfg.get("last_test_mode")
    # The effective mode must match the one that was actually tested.  A
    # previously tested "auto" request is not enough: auto may now resolve to
    # a different (untested) mode after a driver or display change.
    mode_matches = tested == real
    if not (cfg.get("last_test_ok") and mode_matches and tested in B.available_modes()
            and cfg.get("last_recovery_mode") == tested and cfg.get("last_recovery_at")):
        raise RuntimeError("请先用“%s”完成一次测试（自动恢复），确认屏幕和远程都正常后再启用。" % B.MODES.get(real, real))
    try:
        age = (dt.datetime.now() - dt.datetime.fromisoformat(cfg["last_test_at"])).total_seconds()
    except (ValueError, TypeError):
        age = -1
    if not 0 <= age <= 86400:
        raise RuntimeError("上次测试已超过 24 小时，请重新测试。")
    autostart = cfg.get("autostart_preference", cfg.get("autostart", True))
    candidate = dict(cfg, mode=mode, start=start, end=end, autostart=autostart)
    try:
        B.schedule_install(candidate)
    except Exception:
        update(enabled=False)
        try:
            B.schedule_remove()
        except Exception as e:
            log("清理未完成的计划失败：%s" % e)
        raise
    update(mode=mode, start=start, end=end, enabled=True, autostart=autostart)
    log("已启用每日计划：%s → %s，模式 %s" % (start, end, mode))


@B.serialized
def pause():
    update(enabled=False, hold_night_until="")
    B.schedule_remove()
    log("已暂停每日计划（不改变当前显示）")


@B.serialized
def start_test(mode, seconds, overlay_cb=None):
    cfg = load()
    if cfg.get("enabled"):
        raise RuntimeError("请先暂停每日计划再测试")
    if cfg.get("state") == "night" or cfg.get("test_id"):
        raise RuntimeError("请先恢复显示，等待当前测试完成")
    if not 10 <= seconds <= 3600:
        raise ValueError("测试时长应为 10 到 3600 秒")
    B.validate_mode(mode)
    real = B.resolve_mode(mode, cfg.get("virtual_match", ""))
    test_id = uuid.uuid4().hex
    update(test_id=test_id, last_test_requested=mode, last_test_mode=real, last_test_at=dt.datetime.now().isoformat(), last_test_ok=False, last_test_night_ok=False,
           last_recovery_at="", last_recovery_mode="", test_aborted=False)
    # 先启动独立的恢复进程（窗口被关也会执行），再切换
    try:
        B.spawn_detached(B.self_cmd() + ["--action", "day", "--delay", str(seconds), "--test-recovery", "--test-id", test_id])
        log("测试开始（%s），%d 秒后自动恢复" % (real, seconds))
        real = B.go_night(mode, overlay_cb)
        update(last_test_night_ok=True, last_test_mode=real)
    except Exception:
        update(test_id="", last_test_ok=False, test_aborted=False)
        raise
    return real


@B.serialized
def finish_test(test_id, overlay_cb=None):
    c = load()
    if not test_id or c.get("test_id") != test_id:
        return "stale"
    aborted = bool(c.get("test_aborted"))
    try:
        B.go_day(overlay_cb, raise_errors=True)
        if not c.get("last_test_night_ok") or load().get("state") != "day":
            raise RuntimeError("测试未成功进入夜间或未恢复")
    except Exception:
        update(test_id="", last_test_ok=False, last_recovery_at="",
               last_test_night_ok=False, test_aborted=False)
        raise
    update(test_id="", last_recovery_at=dt.datetime.now().isoformat(),
           last_recovery_mode=c.get("last_test_mode"), last_test_ok=not aborted,
           last_test_night_ok=False, test_aborted=False)
    if aborted:
        log("测试已中止，显示已恢复；本次不计为成功测试")
    else:
        log("测试自动恢复完成，请确认实体屏与远程画面")
    return "ok"


@B.serialized
def abort_test(overlay_cb=None):
    """Stop a test when the GUI exits, without marking the shortened test successful.

    The delayed helper keeps its test id if immediate recovery fails, so it can
    still try again later.  In that case ``test_aborted`` prevents the helper
    from turning an interrupted test into a successful one.
    """
    cfg = load()
    if not cfg.get("test_id"):
        return "none"
    update(last_test_ok=False, test_aborted=True)
    B.go_day(overlay_cb, raise_errors=True)
    update(test_id="", last_test_night_ok=False, test_aborted=False)
    log("测试已中止，显示已恢复；本次不计为成功测试")
    return "aborted"


def is_locked():
    c = load()
    return c.get("state") == "night" and c.get("lock", True)


def remember_explicit_day(cfg=None):
    """用户明确要回白天。计划夜间还没结束时，先记下剩余时间，避免恢复失败后又被拉回黑屏。"""
    cfg = cfg or load()
    patch = {"hold_night_until": ""}
    if cfg.get("enabled") and in_night(cfg):
        secs = max(next_switch(cfg)[1], 1)
        patch["override_until"] = (dt.datetime.now() + dt.timedelta(seconds=secs)).isoformat()
    update(**patch)


def forget_unlock_override():
    """再次主动进入夜间时，上一次解锁的截止时间不再有效。"""
    if load().get("override_until"):
        update(override_until="")


@B.serialized
def chord_unlock(overlay_cb=None):
    """本地唯一解锁方式：按住组合键。恢复显示；若在计划时段内，则本时段剩余时间不再自动黑屏。"""
    cfg = load()
    remember_explicit_day(cfg)
    B.go_day(overlay_cb)
    update(test_id="", last_test_ok=False, test_aborted=False, hold_night_until="")
    log("已通过组合键 %s 解除黑屏" % " + ".join(cfg.get("unlock_chord") or ["C", "K"]))


@B.serialized
def emergency(overlay_cb=None, quiet=False):
    errors = []
    try:
        load()
    except RuntimeError:
        from . import core
        damaged = core.config_path() + ".damaged-" + uuid.uuid4().hex
        os.replace(core.config_path(), damaged)
        save(dict(core.DEFAULT, night_mode="virtual_only", state="night"))
        log("损坏的配置已保留在 %s，使用显示备份和系统恢复" % damaged)
    if IS_WIN and not load().get("night_mode"):
        from . import win_display as W
        try:
            if W.verify_virtual_only(load().get("virtual_match", "")):
                update(state="night", night_mode="virtual_only")
        except Exception as e:
            log("应急显示检测失败，仍尝试唤醒：%s" % e)
    try:
        pause()
    except Exception as e:
        errors.append(str(e))
    update(test_id="", last_test_ok=False, test_aborted=False)
    try:
        B.go_day(overlay_cb, raise_errors=True)
    except Exception as e:
        errors.append(str(e))
    if errors:
        raise RuntimeError("应急恢复部分失败：" + " | ".join(errors))
    if not quiet:
        log("应急恢复完成。如仍黑屏：Windows 按 Win+P 选“仅电脑屏幕”；Mac/Linux 按任意键。")


@B.serialized
def uninstall(overlay_cb=None):
    emergency(overlay_cb, quiet=True)
    log("已卸载计划任务和自启项；日志与备份保留在 %s" % home())


def note_manual_night():
    """计划开启且当前不在夜间时段时，记下手动夜间应保持到的早晨边界。"""
    cfg = load()
    if not (cfg.get("enabled") and cfg.get("state") == "night") or in_night(cfg):
        if cfg.get("hold_night_until"):
            update(hold_night_until="")
        return
    until = manual_night_until(cfg)
    if until is None:
        return
    update(hold_night_until=until.replace(microsecond=0).isoformat())


@B.serialized
def reconcile(overlay_cb=None):
    cfg = load()
    if cfg.get("recovery_pending"):
        B.go_day(overlay_cb)
        cfg = load()
    if not cfg.get("enabled"):
        return "disabled"
    want = "night" if in_night(cfg) else "day"
    # 白天手动进入夜间后，在计划早晨结束前不要被定时检查拉回白天。
    # 组合键解锁的 override_until 写在后面，仍然优先恢复白天。
    hold = cfg.get("hold_night_until") or ""
    if hold:
        try:
            until = dt.datetime.fromisoformat(hold)
            holding = dt.datetime.now(until.tzinfo) < until
        except (TypeError, ValueError):
            holding = False
        if holding:
            want = "night"
        else:
            update(hold_night_until="")
            cfg = load()
    ov = cfg.get("override_until") or ""
    if ov:
        try:
            until = dt.datetime.fromisoformat(ov)
            active_override = dt.datetime.now(until.tzinfo) < until
        except (TypeError, ValueError):
            active_override = False
        if active_override:
            want = "day"  # 本次夜间已用组合键解锁，到本时段结束前不再自动黑屏
        else:
            update(override_until="")
    if want == cfg.get("state"):
        if want == "night":
            try:
                B.ensure_night()
            except Exception:
                update(enabled=False, last_test_ok=False)
                B.go_day(overlay_cb)
                raise
        return "unchanged"
    if want == "night":
        B.go_night(cfg["mode"], overlay_cb)
    else:
        B.go_day(overlay_cb)
    return want

# ---------------- Windows：黑幕、快捷键 ----------------

# 系统光标资源号。SetSystemCursor 会吃掉句柄，每个都要单独复制一份空白光标。
CURSOR_IDS = (32512, 32513, 32514, 32515, 32516, 32640, 32641, 32642, 32643,
              32644, 32645, 32646, 32648, 32649, 32650)
_cursor = {"hidden": False}


def overlay_geometry(x, y, width, height):
    """Tk 几何串。原点允许为负，才能盖住摆在主屏左边或上边的副屏。"""
    if width <= 0 or height <= 0:
        raise ValueError("黑幕尺寸无效")
    return "%dx%d%+d%+d" % (width, height, x, y)


def overlay_dwm_attributes(build):
    """按系统版本返回要写给 DwmSetWindowAttribute 的 (属性, 值)。

    2 是 DWMWA_NCRENDERING_POLICY，值 1 为 DWMNCRP_DISABLED。
    3 是 DWMWA_TRANSITIONS_FORCEDISABLED。这两项从 Vista 起就有。
    33 是 DWMWA_WINDOW_CORNER_PREFERENCE，值 1 为 DWMWCP_DONOTROUND。
    34 是 DWMWA_BORDER_COLOR，值 0xFFFFFFFE 为 DWMWA_COLOR_NONE。
    33 和 34 只在 Windows 11 版本 22000 及以后有效，更早的系统不发送。
    """
    attrs = [(2, 1), (3, 1)]
    if build >= 22000:
        attrs.extend([(33, 1), (34, 0xFFFFFFFE)])
    return tuple(attrs)


def borderless_style(style):
    """去掉标题栏、厚边框和重叠窗口样式，保留其余位，再标成弹出窗口。"""
    return (style & ~0x00CF0000) | 0x80000000


def settings_visibility(viewable, night, remembered):
    """夜间收起设置窗口。只有进入夜间前窗口本来就开着，白天才再显示。

    返回 (withdraw, deiconify, remembered)。托盘自启时窗口本来就是藏的，
    remembered 保持为假，白天不会被这次黑屏带出来。
    """
    if night:
        if viewable:
            return True, False, True
        return False, False, remembered
    if remembered:
        return False, True, False
    return False, False, False


def _ctype(fn, argtypes, restype):
    """给 ctypes 函数补原型。测试里的普通替身没有这些属性，忽略即可。"""
    target = getattr(fn, "__func__", fn)
    try:
        target.argtypes = argtypes
        target.restype = restype
    except (AttributeError, TypeError):
        return


def hide_system_cursor(user32):
    """把本机系统光标换成 1×1 空白。远程采集若自带指针形状，黑幕期间会看到空白指针。

    仅虚拟屏不走这里，远程端的指针保持原样。重复调用是幂等的，不会累加 ShowCursor 计数。
    """
    import ctypes
    handle = ctypes.c_void_p
    _ctype(user32.GetSystemMetrics, [ctypes.c_int], ctypes.c_int)
    _ctype(user32.CreateCursor, [handle, ctypes.c_int, ctypes.c_int, ctypes.c_int,
                                 ctypes.c_int, ctypes.c_void_p, ctypes.c_void_p], handle)
    _ctype(user32.CopyIcon, [handle], handle)
    _ctype(user32.SetSystemCursor, [handle, ctypes.c_uint], ctypes.c_int)
    _ctype(user32.DestroyCursor, [handle], ctypes.c_int)
    width = user32.GetSystemMetrics(13) or 32
    height = user32.GetSystemMetrics(14) or 32
    row = ((width + 15) // 16) * 2
    and_mask = (ctypes.c_ubyte * (row * height))(*([0xFF] * (row * height)))
    xor_mask = (ctypes.c_ubyte * (row * height))(*([0x00] * (row * height)))
    blank = user32.CreateCursor(None, 0, 0, width, height, and_mask, xor_mask)
    if not blank:
        raise RuntimeError("无法创建空白光标")
    try:
        for cid in CURSOR_IDS:
            copy = user32.CopyIcon(blank)
            if not copy or not user32.SetSystemCursor(copy, cid):
                raise RuntimeError("无法隐藏系统光标")
    finally:
        user32.DestroyCursor(blank)


def restore_system_cursor(user32):
    import ctypes
    _ctype(user32.SystemParametersInfoW, [ctypes.c_uint, ctypes.c_uint, ctypes.c_void_p, ctypes.c_uint], ctypes.c_int)
    if not user32.SystemParametersInfoW(0x0057, 0, None, 0):  # SPI_SETCURSORS
        raise RuntimeError("无法恢复系统光标")


def apply_cursor(hidden):
    """黑幕期间反复换成空白光标。已经是显示状态时不再调用恢复，避免每次检查都重置光标。"""
    if not IS_WIN:
        return
    if _cursor["hidden"] == hidden and not hidden:
        return
    try:
        import ctypes
        user32 = ctypes.windll.user32
        if hidden:
            hide_system_cursor(user32)
        elif _cursor["hidden"]:
            restore_system_cursor(user32)
        _cursor["hidden"] = hidden
    except Exception as e:
        log("光标显示切换失败：%s" % e)


def apply_overlay_chrome(user32, hwnd, build):
    """去掉标题栏边框。Windows 11 22000 起再去掉圆角和彩色边。失败不影响黑幕本身。"""
    import ctypes
    style = user32.GetWindowLongW(hwnd, -16) & 0xFFFFFFFF
    user32.SetWindowLongW(hwnd, -16, borderless_style(style))
    try:
        dwm = ctypes.WinDLL("dwmapi")
    except OSError:
        return
    dwm.DwmSetWindowAttribute.argtypes = [ctypes.c_void_p, ctypes.c_uint, ctypes.c_void_p, ctypes.c_uint]
    dwm.DwmSetWindowAttribute.restype = ctypes.c_long
    for attr, value in overlay_dwm_attributes(build):
        blob = ctypes.c_uint(value)
        dwm.DwmSetWindowAttribute(hwnd, attr, ctypes.byref(blob), ctypes.sizeof(blob))


def make_overlay(root, on_cover=None, on_fail=None):
    """返回 overlay_cb(on)。Windows 用 WDA_EXCLUDEFROMCAPTURE：本地黑，屏幕采集看不到它；鼠标键盘穿透，不拦截输入。"""
    state = {"win": None}

    def fail(window):
        if window is not None:
            with contextlib.suppress(Exception):
                window.destroy()
        state["win"] = None
        apply_cursor(False)
        if on_fail:
            on_fail()

    def cb(on):
        box = {"ok": False}
        def do():
            import tkinter as tk
            if on and state["win"] is None:
                w = None
                try:
                    if on_cover:
                        on_cover()
                    w = tk.Toplevel(root)
                    w.overrideredirect(True)
                    w.configure(bg="#000000")
                    x = y = 0
                    width, height = w.winfo_screenwidth(), w.winfo_screenheight()
                    if IS_WIN:
                        import ctypes
                        gm = ctypes.windll.user32.GetSystemMetrics
                        vx, vy, vw, vh = gm(76), gm(77), gm(78), gm(79)  # 虚拟屏幕原点与尺寸
                        if vw > 0 and vh > 0:
                            x, y, width, height = vx, vy, vw, vh
                    w.geometry(overlay_geometry(x, y, width, height))
                    w.attributes("-topmost", True)
                    # 黑幕上不放文字、图标或动画，避免本机看出电脑还开着。
                    w.update_idletasks()
                    if IS_WIN:
                        import ctypes
                        from ctypes import wintypes as T
                        u = ctypes.windll.user32
                        u.GetParent.argtypes = [T.HWND]
                        u.GetParent.restype = T.HWND
                        u.GetWindowLongW.argtypes = [T.HWND, ctypes.c_int]
                        u.GetWindowLongW.restype = T.LONG
                        # 样式高位含 WS_POPUP，用无符号 32 位，避免 LONG 溢出。
                        u.SetWindowLongW.argtypes = [T.HWND, ctypes.c_int, ctypes.c_uint]
                        u.SetLayeredWindowAttributes.argtypes = [T.HWND, T.DWORD, ctypes.c_ubyte, T.DWORD]
                        u.SetWindowDisplayAffinity.argtypes = [T.HWND, T.DWORD]
                        u.SetWindowPos.argtypes = [T.HWND, T.HWND, ctypes.c_int, ctypes.c_int,
                                                  ctypes.c_int, ctypes.c_int, T.UINT]
                        hwnd = u.GetParent(w.winfo_id()) or w.winfo_id()
                        apply_overlay_chrome(u, hwnd, B.win_build())
                        GWL_EXSTYLE = -20
                        ex = u.GetWindowLongW(hwnd, GWL_EXSTYLE)
                        # LAYERED|TRANSPARENT|TOOLWINDOW|NOACTIVATE|TOPMOST。穿透才能让远程点击落到真实窗口。
                        u.SetWindowLongW(hwnd, GWL_EXSTYLE, ex | 0x80000 | 0x20 | 0x80 | 0x08000000 | 0x8)
                        u.SetLayeredWindowAttributes(hwnd, 0, 255, 2)
                        # SWP_NOACTIVATE | SWP_FRAMECHANGED | SWP_SHOWWINDOW
                        if not u.SetWindowPos(hwnd, T.HWND(-1), x, y, width, height, 0x10 | 0x20 | 0x40):
                            raise RuntimeError("无法覆盖全部显示器")
                        ok = u.SetWindowDisplayAffinity(hwnd, 0x11)  # WDA_EXCLUDEFROMCAPTURE
                        log("防窥黑幕已显示（排除采集=%s，输入穿透）" % bool(ok))
                        if not ok:
                            raise RuntimeError("排除采集失败")
                    state["win"] = w
                    apply_cursor(True)
                except Exception as e:
                    log("防窥黑幕未能确认显示：%s" % e)
                    fail(w)
                    return
            elif not on and state["win"] is not None:
                state["win"].destroy()
                state["win"] = None
                apply_cursor(False)
                log("防窥黑幕已关闭")
            box["ok"] = (state["win"] is not None) if on else True
        if threading.current_thread() is threading.main_thread():
            do()
        else:
            raise RuntimeError("黑幕操作必须由界面主线程执行")
        return box["ok"]
    cb.is_on = lambda: state["win"] is not None
    return cb


def start_hotkey(root, on_hotkey):
    if not IS_WIN:
        return False
    import ctypes
    from ctypes import wintypes

    def loop():
        u = ctypes.windll.user32
        if not u.RegisterHotKey(None, 1, 0x2 | 0x1 | 0x4 | 0x4000, 0x52):  # Ctrl+Alt+Shift+R
            log("警告：全局快捷键 Ctrl+Alt+Shift+R 注册失败（可能被占用）")
            return
        log("全局快捷键 Ctrl+Alt+Shift+R 已注册（应急恢复）")
        msg = wintypes.MSG()
        while u.GetMessageW(ctypes.byref(msg), None, 0, 0) > 0:
            if msg.message == 0x0312:
                on_hotkey()
    threading.Thread(target=loop, daemon=True).start()
    return True

# ---------------- 托盘 + 界面 ----------------

def run_app(show_window=True, selftest=0):
    import tkinter as tk

    from . import chord, ui
    if IS_WIN:
        try:
            import ctypes
            ctypes.windll.user32.SetProcessDPIAware()
        except (AttributeError, OSError):
            pass
    root = tk.Tk()
    # Do not flash a window when the app is started by the login autostart.
    if not show_window:
        root.withdraw()
    # 首次打开就把环境准备好：GUI 打开时同步做一次（只弹一次 UAC，失败也不挡打开）；
    # 托盘自启时放后台线程做，避免 UAC 弹窗卡住登录；自测时跳过，不在 CI 里弹 UAC。
    if not selftest:
        if show_window:
            try:
                ensure_environment()
            except Exception as e:
                log("环境准备失败，软件仍可正常打开：%s" % e)
        else:
            def _prepare_environment():
                # 和夜间切换共用动作锁，避免装驱动的同时改显示拓扑、写坏白天备份。
                from .storage import file_lock
                try:
                    with file_lock(os.path.join(home(), "actions.lock"), timeout=180):
                        ensure_environment()
                except Exception as e:
                    log("环境准备失败，软件仍可正常打开：%s" % e)
            threading.Thread(target=_prepare_environment, daemon=True).start()
    appearance = {"restore": False}

    def _viewable():
        try:
            return bool(root.winfo_viewable())
        except tk.TclError:
            return False

    def conceal_settings():
        withdraw, _show, remembered = settings_visibility(_viewable(), True, appearance["restore"])
        appearance["restore"] = remembered
        if withdraw:
            with contextlib.suppress(tk.TclError):
                root.withdraw()

    def reveal_settings():
        _hide, deiconify, remembered = settings_visibility(_viewable(), False, appearance["restore"])
        appearance["restore"] = remembered
        if deiconify:
            with contextlib.suppress(tk.TclError):
                root.deiconify()

    def sync_night_window(refresh_cursor=False):
        if not IS_WIN:
            return
        night = load().get("state") == "night"
        withdraw, deiconify, remembered = settings_visibility(_viewable(), night, appearance["restore"])
        appearance["restore"] = remembered
        try:
            if withdraw:
                root.withdraw()
            elif deiconify:
                root.deiconify()
        except tk.TclError:
            pass
        if refresh_cursor or not night:
            apply_cursor(night and load().get("night_mode") == "overlay")

    overlay = make_overlay(root, on_cover=conceal_settings, on_fail=reveal_settings)
    pending = queue.Queue()
    closing = threading.Event()

    def act(command):
        data = command if isinstance(command, dict) else {"action": command}
        name = data.get("action")
        try:
            try:
                return _act(name, data)
            except Exception as e:
                log("错误(%s): %s" % (name, e))
                return "error: %s" % e
        finally:
            sync_night_window()

    def _act(name, data):
        if name in ("day", "emergency", "quit") and is_locked():
            return "locked"
        if name in ("night", "test") and load().get("lock", True):
            reader = chord.Reader(load().get("unlock_chord") or chord.DEFAULT_CHORD)
            reader.close()
        if name == "unlock":
            chord_unlock(overlay)
        elif name == "test_day":
            return finish_test(data.get("test_id"), overlay)
        elif name in ("remote_emergency", "emergency"):
            emergency(overlay)
        elif name == "night":
            forget_unlock_override()
            B.go_night(data.get("mode") or load()["mode"], overlay)
            note_manual_night()
        elif name == "test":
            start_test(data.get("mode") or load()["mode"], data.get("seconds", 120), overlay)
        elif name == "day":
            remember_explicit_day()
            B.go_day(overlay)
            update(test_id="", last_test_ok=False, test_aborted=False)
        elif name == "reconcile":
            return reconcile(overlay)
        elif name == "ping":
            return "ok"
        elif name == "show":
            # 夜间再弹出设置窗口会在本机屏幕上露出界面。用户明确打开过的窗口，白天由 sync 放回来。
            if IS_WIN and load().get("state") == "night":
                log("夜间不打开设置窗口")
                return "ok"
            appearance["restore"] = False
            root.deiconify()
            root.lift()
        elif name == "quit":
            root.after(50, root.destroy)
        else:
            raise ValueError("未知操作：%s" % name)
        return "ok"

    def dispatch(command):
        if threading.current_thread() is threading.main_thread():
            return act(command)
        done, box = threading.Event(), {}
        pending.put((command, done, box))
        if not done.wait(55):
            # 解锁和应急已经进队列后不能丢掉。客户端可以先超时返回，主线程空出来仍要执行。
            name = command.get("action") if isinstance(command, dict) else command
            if name not in ("unlock", "emergency", "remote_emergency"):
                box["cancelled"] = True
            return "error: 操作超时，请查看状态"
        return box.get("result", "error: 程序正在退出")

    def drain():
        while not pending.empty():
            command, done, box = pending.get_nowait()
            try:
                if not box.get("cancelled"):
                    box["result"] = act(command)
            finally:
                done.set()
        if not closing.is_set():
            root.after(30, drain)

    server = ipc.Server(dispatch)
    tray = None
    watcher = None
    started = False
    try:
        server.start()
        started = True
        root.after(0, drain)
        start_hotkey(root, lambda: dispatch("emergency"))
        watcher = chord.Watcher(lambda: load().get("unlock_chord") or chord.DEFAULT_CHORD,
                                lambda: dispatch("unlock"))

        def sync_watch():
            try:
                (watcher.active.set if is_locked() else watcher.active.clear)()
            except Exception as e:
                log("组合键状态检查失败：%s" % e)
            root.after(1000, sync_watch)
        root.after(500, sync_watch)
        if IS_MAC:
            chord.request_permission()

        def tick():
            try:
                reconcile(overlay)
                if not load().get("enabled") and load().get("state") == "night":
                    B.ensure_night()
            except Exception as e:
                log("定时检查出错: %s" % e)
            finally:
                # 黑幕期间每轮都重新盖住光标。别的程序可能把系统光标改回去。
                sync_night_window(refresh_cursor=True)
            root.after(5000, tick)
        root.after(2000, tick)
        # A destroyed overlay cannot still be treated as active after a restart.
        if load().get("state") == "night" and load().get("night_mode") == "overlay":
            cfg = load()
            changes = {"state": "day", "night_mode": ""}
            if cfg.get("test_id"):
                # The app exited before the overlay test could be completed.
                changes.update(test_id="", last_test_ok=False,
                               last_test_night_ok=False, test_aborted=False)
            update(**changes)
        win = ui.MainWindow(root, overlay, dispatch)
        tray = ui.start_tray(root, dispatch)
        win.has_tray = bool(tray)
        if not show_window and not tray:
            root.deiconify()
        if selftest:
            root.after(selftest * 1000, root.destroy)
        root.mainloop()
    finally:
        closing.set()
        if watcher:
            watcher.stop()
        if tray:
            with contextlib.suppress(Exception):
                tray.stop()
        # Quit restores the display so the user is not left locked out; the
        # registered daily schedule remains and will run again at its boundary.
        if started:
            try:
                if load().get("test_id"):
                    abort_test()
            except Exception as e:
                log("退出时中止测试失败：%s" % e)
        try:
            if started and load().get("state") == "night":
                B.go_day(overlay)
        except Exception as e:
            log("退出时恢复显示失败：%s" % e)
        finally:
            apply_cursor(False)
            if server.server is not None:
                server.close()
            with contextlib.suppress(tk.TclError):
                root.destroy()

# ---------------- 入口 ----------------

def _fix_stdout():
    for st in (sys.stdout, sys.stderr):
        with contextlib.suppress(Exception):
            st.reconfigure(encoding="utf-8", errors="replace")


def main(argv=None):
    _fix_stdout()
    ap = argparse.ArgumentParser(prog=APP)
    ap.add_argument("--action", default="gui", choices=[
        "gui", "tray", "night", "day", "reconcile", "test", "activate", "pause", "emergency", "uninstall",
        "status", "awake", "backup", "restore", "displays", "plan-virtual", "install-vdd", "uninstall-vdd", "selfcheck",
        "env", "setup"])
    ap.add_argument("--tray", action="store_true", help="开机自启：只显示托盘")
    ap.add_argument("--home", help="指定数据目录（计划与辅助进程继承此路径）")
    ap.add_argument("--mode")
    ap.add_argument("--start"); ap.add_argument("--end")
    ap.add_argument("--delay", type=int, default=0)
    ap.add_argument("--seconds", type=int, default=120)
    ap.add_argument("--test-recovery", action="store_true")
    ap.add_argument("--test-id", default="")
    ap.add_argument("--awake-token", default="")
    ap.add_argument("--selftest", type=int, default=0, help="界面自检：N 秒后自动退出")
    ap.add_argument("--debug", action="store_true", help="出错时在终端打印完整 traceback")
    a = ap.parse_args(argv)
    try:
        if a.home:
            os.environ["NIGHTDISPLAY_HOME"] = os.path.abspath(a.home)
        if a.delay < 0 or a.seconds <= 0 or a.selftest < 0:
            raise ValueError("时长必须为正数，延迟和界面自检时长不能为负数")
        if a.mode:
            B.validate_mode(a.mode)
        if a.action in ("gui", "tray") or a.tray:
            tray_only = a.tray or a.action == "tray"
            # An existing resident is enough for login autostart: do not pop
            # the window just because the user signed in.
            r = send_ipc("ping" if tray_only else "show")
            if r is not None:
                if r != "ok":
                    raise RuntimeError(r)
                return 0
            return run_app(show_window=not tray_only, selftest=a.selftest)
        if a.delay:
            time.sleep(a.delay)
        if a.action == "status":
            update()
            status = B.status_text()
            with open(os.path.join(home(), "status.txt"), "w", encoding="utf-8") as f:
                f.write(status + "\n")
            print(status)
        elif a.action == "awake":
            B.awake_loop(a.seconds, a.awake_token)
        elif a.action == "night":
            r = resident_action({"action": "night", "mode": a.mode})
            if r != "ok":
                raise RuntimeError("托盘进程返回：%s" % r)
        elif a.action == "day":
            if is_locked() and not a.test_recovery:
                print("黑屏已锁定：本地请按住解锁组合键；远程可用 --action emergency"); return 2
            if a.test_recovery:
                try:
                    r = send_ipc({"action": "test_day", "test_id": a.test_id})
                except RuntimeError as e:
                    log("常驻进程未回复测试恢复，使用独立恢复进程：%s" % e)
                    r = None
                if r is None:
                    r = finish_test(a.test_id)
                if r not in ("ok", "stale"):
                    raise RuntimeError(r)
            else:
                r = send_ipc("day")
                if r is None:
                    B.go_day(raise_errors=True)
                    update(test_id="", last_test_ok=False)
                elif r != "ok":
                    raise RuntimeError(r)
        elif a.action == "reconcile":
            r = send_ipc("reconcile")
            if r is not None and r.startswith(("error", "timeout")):
                raise RuntimeError("托盘进程返回：%s" % r)
            if r is None:
                cfg = load()
                if cfg.get("enabled"):
                    B.spawn_detached(B.self_cmd() + ["--tray"])  # 黑幕需要常驻进程
                else:
                    print(reconcile())
        elif a.action == "test":
            r = resident_action({"action": "test", "mode": a.mode, "seconds": a.seconds})
            if r != "ok":
                raise RuntimeError(r)
        elif a.action == "activate":
            c = load(); activate(a.mode or c["mode"], a.start or c["start"], a.end or c["end"])
        elif a.action == "pause":
            pause()
        elif a.action == "emergency":
            # 隐藏的远程恢复：在远程会话里运行，不受黑屏锁限制
            r = send_ipc("remote_emergency")
            if r is None:
                emergency()
            elif r != "ok":
                raise RuntimeError("托盘进程返回：%s" % r)
        elif a.action == "uninstall":
            r = send_ipc("remote_emergency")
            if r is not None and r != "ok":
                raise RuntimeError("托盘进程返回：%s" % r)
            if r is None:
                uninstall()
            else:
                send_ipc("quit")
        elif a.action == "backup":
            B._backup_display()
        elif a.action == "restore":
            print("level", B.restore_display())
        elif a.action == "displays":
            if IS_WIN:
                from . import win_display as W
                for d in W.describe():
                    print(d["tgt_id"], "active" if d["active"] else "inactive", "virtual" if W.is_virtual(d) else "physical", d["name"], d["tech"], d["device_path"])
            elif IS_LINUX:
                from . import linux_display as L
                for o in L.query():
                    print(o)
        elif a.action == "plan-virtual":
            B.go_night("virtual_only", dry_run=True)
        elif a.action == "env":
            print(json.dumps(environment_report(), ensure_ascii=False, indent=1))
        elif a.action == "setup":
            print(json.dumps(ensure_environment(force=True), ensure_ascii=False, indent=1))
        elif a.action == "install-vdd":
            return install_vdd("install")
        elif a.action == "uninstall-vdd":
            r = send_ipc("remote_emergency")
            if r is None:
                emergency()
            elif r != "ok":
                raise RuntimeError(r)
            return install_vdd("uninstall")
        elif a.action == "selfcheck":
            from .storage import write_json
            report = B.selfcheck()
            write_json(os.path.join(home(), "selfcheck.json"), report)
            print(json.dumps(report, ensure_ascii=False, indent=1))
    except Exception as e:
        import traceback
        tb = traceback.format_exc()
        if a.debug:
            sys.stderr.write(tb)
        log("错误(%s): %s" % (a.action, e))
        try:
            with open(os.path.join(home(), "logs", "traceback.log"), "a", encoding="utf-8") as f:
                f.write("%s %s\n%s\n" % (dt.datetime.now().isoformat(), a.action, tb))
        except Exception:
            pass
        print("错误：%s（详细信息见日志；加 --debug 显示完整堆栈）" % e, file=sys.stderr)
        return 1
    return 0


def resource(name):
    return os.path.join(os.path.dirname(os.path.abspath(__file__)), "resources", name)


def vdd_dir():
    """内置的 VDD 驱动目录（打包时放入 vdd/；源码运行时为 vendor/vdd）。"""
    if hasattr(sys, "_MEIPASS"):
        return os.path.join(sys._MEIPASS, "vdd")
    return os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "vendor", "vdd")


def install_vdd(op="install"):
    """以管理员身份（UAC 弹窗由用户确认）安装/卸载内置虚拟显示驱动。op: install/uninstall/status。"""
    if not IS_WIN:
        print("仅 Windows 需要；macOS 使用内置 CGVirtualDisplay，Linux 请用 evdi/dummy 驱动")
        return 1
    import ctypes
    import subprocess
    script = resource("install_vdd.ps1")
    extra = {"install": [], "uninstall": ["-Uninstall"], "status": ["-Status"]}[op]
    argv = ["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-File", script,
            "-Source", vdd_dir(), "-LogDirectory", os.path.join(home(), "logs")] + extra
    def _print_vdd_log_tail():
        try:
            with open(os.path.join(home(), "logs", "install_vdd.log"), encoding="utf-8", errors="replace") as f:
                tail = f.read()[-3000:]
            if tail:
                print("install_vdd 日志尾部：\n" + tail, file=sys.stderr)
        except OSError:
            pass
    if op == "status" or ctypes.windll.shell32.IsUserAnAdmin():
        rc = subprocess.call(["powershell"] + argv, timeout=300)
        if rc:
            _print_vdd_log_tail()
        return rc
    args = subprocess.list2cmdline(argv)
    from ctypes import wintypes as T
    class ExecuteInfo(ctypes.Structure):
        _fields_ = [("cbSize", T.DWORD), ("fMask", T.ULONG), ("hwnd", T.HWND),
                    ("lpVerb", T.LPCWSTR), ("lpFile", T.LPCWSTR), ("lpParameters", T.LPCWSTR),
                    ("lpDirectory", T.LPCWSTR), ("nShow", ctypes.c_int), ("hInstApp", T.HINSTANCE),
                    ("lpIDList", ctypes.c_void_p), ("lpClass", T.LPCWSTR), ("hkeyClass", T.HKEY),
                    ("dwHotKey", T.DWORD), ("hIcon", T.HANDLE), ("hProcess", T.HANDLE)]
    shell, kernel = ctypes.windll.shell32, ctypes.windll.kernel32
    shell.ShellExecuteExW.argtypes = [ctypes.POINTER(ExecuteInfo)]
    shell.ShellExecuteExW.restype = T.BOOL
    kernel.WaitForSingleObject.argtypes = [T.HANDLE, T.DWORD]
    kernel.GetExitCodeProcess.argtypes = [T.HANDLE, ctypes.POINTER(T.DWORD)]
    kernel.CloseHandle.argtypes = [T.HANDLE]
    info = ExecuteInfo(cbSize=ctypes.sizeof(ExecuteInfo), fMask=0x40 | 0x100,
                       lpVerb="runas", lpFile="powershell.exe", lpParameters=args, nShow=1)
    if not shell.ShellExecuteExW(ctypes.byref(info)):
        raise RuntimeError("未获得驱动安装权限，或管理员确认被取消")
    try:
        if kernel.WaitForSingleObject(info.hProcess, 300000) != 0:
            raise RuntimeError("驱动安装超过 5 分钟未返回，请查看安装日志")
        code = T.DWORD()
        if not kernel.GetExitCodeProcess(info.hProcess, ctypes.byref(code)):
            raise RuntimeError("无法读取驱动安装结果")
        log("驱动%s结束，退出码 %s" % (op, code.value))
        if code.value:
            _print_vdd_log_tail()
        return code.value
    finally:
        kernel.CloseHandle(info.hProcess)


def bundled_driver_present():
    """打包里是否带了全部 VDD 驱动文件（inf/cat/dll，任一层级都算）。"""
    import glob
    pattern = os.path.join(vdd_dir(), "**", "MttVDD.inf")
    return bool(glob.glob(pattern, recursive=True))


def environment_report():
    """不装驱动、不改系统，只检查：返回当前环境是否已就绪、实际可用模式。"""
    from .storage import write_json
    report = {
        "platform": "windows" if IS_WIN else ("macos" if IS_MAC else "linux"),
        "bundled_driver": bundled_driver_present() if IS_WIN else False,
        "virtual_display": False,
        "registry_writable": False,
        "autostart_actual": False,
        "available_modes": [],
        "ready_mode": "",
        "pure_black": True,
    }
    with contextlib.suppress(Exception):
        report["virtual_display"] = B.has_virtual_display()
    try:
        report["available_modes"] = B.available_modes()
        report["ready_mode"] = B.resolve_mode("auto", load().get("virtual_match", ""))
    except Exception:
        pass
    with contextlib.suppress(Exception):
        report["autostart_actual"] = B.autostart_status()
    if IS_WIN:
        # 注册表写入只测 HKCU Run（普通用户就有权限），不碰 HKLM；
        # 驱动进驱动库（pnputil）才要管理员，那一步只在首次自动装驱动时弹一次 UAC。
        import winreg
        try:
            with winreg.CreateKeyEx(winreg.HKEY_CURRENT_USER, B.RUN_KEY, 0, winreg.KEY_SET_VALUE) as k:
                winreg.SetValueEx(k, "NightDisplayEnvTest", 0, winreg.REG_SZ, "ok")
                winreg.DeleteValue(k, "NightDisplayEnvTest")
            report["registry_writable"] = True
        except OSError:
            report["registry_writable"] = False
    else:
        report["registry_writable"] = True
    # 黑幕窗口只有纯黑底色，设置窗口里的进度动画在进入夜间后会停掉。
    report["pure_black"] = True
    with contextlib.suppress(Exception):
        write_json(os.path.join(home(), "env.json"), report)
    return report


def ensure_environment(force=False):
    """首次打开就把环境准备好，用户不用手动装这个驱动、那个驱动。

    Windows 且无虚拟屏、且包里带了驱动时，自动装一次内置 VDD（只装一次，
    失败或用户点了 UAC 取消后 7 天内不再自动重试，软件照常打开、自动落到
    黑幕模式，绝不因为驱动没装成就打不开）。装完再写一次开机自启并回读
    校验，注册表写不进也只记日志、不挡打开。非 Windows 不装驱动。
    返回 environment_report()，永不因驱动失败而抛错挡住主界面。
    """
    import time as _time

    from .storage import write_json
    report = environment_report()
    if not IS_WIN:
        return report
    if report["virtual_display"]:
        return report
    if not report["bundled_driver"]:
        log("环境准备：包里没有内置 VDD 驱动文件，自动用黑幕/关屏模式，功能正常")
        return report
    state_path = os.path.join(home(), "env.json")
    try:
        with open(state_path, encoding="utf-8") as f:
            prev = json.load(f)
    except Exception:
        prev = {}
    last = prev.get("driver_auto_setup_at", 0)
    if not force and last and _time.time() - last < 7 * 24 * 3600:
        return report
    try:
        rc = install_vdd("install")
        import time as _sleep
        if rc == 0:
            _sleep.sleep(3)
            report = environment_report()
            log("环境准备：内置 VDD 已自动安装，虚拟屏可用=%s" % report["virtual_display"])
        else:
            log("环境准备：内置 VDD 自动安装未成功（退出码 %s），自动用黑幕/关屏模式，功能正常" % rc)
    except Exception as e:
        log("环境准备：内置 VDD 自动安装失败：%s，自动用黑幕/关屏模式，功能正常" % e)
    report["driver_auto_setup_at"] = _time.time()
    with contextlib.suppress(Exception):
        write_json(state_path, report)
    # 开机自启：按用户偏好写 HKCU Run，写完回读校验。
    try:
        if load().get("autostart_preference", True):
            B.set_autostart(True)
            report["autostart_actual"] = B.autostart_status()
            if not report["autostart_actual"]:
                log("环境准备：开机自启注册表写回校验未通过，软件仍可正常打开")
    except Exception as e:
        log("环境准备：开机自启写入失败：%s，软件仍可正常打开" % e)
    return report
