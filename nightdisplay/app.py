# -*- coding: utf-8 -*-
"""NightDisplay 主程序：命令行 / 托盘常驻 / 图形界面 / 全局快捷键 / 防窥黑幕。"""
import argparse, datetime as dt, os, socket, sys, threading, time

from . import VERSION, APP, CREDIT
from .core import in_night, IS_WIN, IS_MAC, IS_LINUX, log, load, save, update, is_night, parse_hm, next_switch, read_log_tail, home
from . import backends as B

PORT = 47831

# ---------------- 单实例 IPC ----------------

def send_ipc(cmd, timeout=2.0):
    try:
        with socket.create_connection(("127.0.0.1", PORT), timeout=timeout) as s:
            s.sendall(cmd.encode() + b"\n")
            return s.recv(4096).decode(errors="ignore").strip() or "ok"
    except OSError:
        return None

# ---------------- 业务动作 ----------------

def activate(mode, start, end):
    parse_hm(start); parse_hm(end)
    if start == end:
        raise RuntimeError("开始和结束时间不能相同")
    cfg = load()
    real = B.resolve_mode(mode, cfg.get("virtual_match", ""))
    if not (cfg.get("last_test_ok") and cfg.get("last_test_mode") == real and cfg.get("last_recovery_at")):
        raise RuntimeError("请先用“%s”完成一次测试（自动恢复），确认屏幕和远程都正常后再启用。" % B.MODES.get(real, real))
    if (dt.datetime.now() - dt.datetime.fromisoformat(cfg["last_test_at"])).total_seconds() > 86400:
        raise RuntimeError("上次测试已超过 24 小时，请重新测试。")
    update(mode=mode, start=start, end=end, enabled=True)
    B.schedule_install(load())
    log("已启用每日计划：%s → %s，模式 %s" % (start, end, mode))


def pause():
    update(enabled=False)
    B.schedule_remove()
    log("已暂停每日计划（不改变当前显示）")


def start_test(mode, seconds, overlay_cb=None):
    cfg = load()
    if cfg.get("enabled"):
        raise RuntimeError("请先暂停每日计划再测试")
    real = B.resolve_mode(mode, cfg.get("virtual_match", ""))
    update(last_test_mode=real, last_test_at=dt.datetime.now().isoformat(), last_test_ok=False,
           last_recovery_at="", last_recovery_mode="")
    # 先启动独立的恢复进程（窗口被关也会执行），再切换
    B.spawn_detached(B.self_cmd() + ["--action", "day", "--delay", str(seconds), "--test-recovery"])
    log("测试开始（%s），%d 秒后自动恢复" % (real, seconds))
    B.go_night(real, overlay_cb)
    update(last_test_ok=True)
    return real


def is_locked():
    c = load()
    return c.get("state") == "night" and c.get("lock", True)


def chord_unlock(overlay_cb=None):
    """本地唯一解锁方式：按住组合键。恢复显示；若在计划时段内，则本时段剩余时间不再自动黑屏。"""
    cfg = load()
    B.go_day(overlay_cb, mode="all")
    if cfg.get("enabled") and in_night(cfg):
        secs = next_switch(cfg)[1]
        update(override_until=(dt.datetime.now() + dt.timedelta(seconds=secs)).isoformat())
    log("已通过组合键 %s 解除黑屏" % " + ".join(cfg.get("unlock_chord") or ["C", "K"]))


def emergency(overlay_cb=None):
    pause()
    B.go_day(overlay_cb, mode="all")
    update(last_test_ok=False)
    log("应急恢复完成。如仍黑屏：Windows 按 Win+P 选“仅电脑屏幕”；Mac/Linux 按任意键。")


def uninstall(overlay_cb=None):
    emergency(overlay_cb)
    log("已卸载计划任务和自启项；日志与备份保留在 %s" % home())


def reconcile(overlay_cb=None):
    cfg = load()
    if not cfg.get("enabled"):
        return "disabled"
    want = "night" if in_night(cfg) else "day"
    ov = cfg.get("override_until") or ""
    if ov:
        if dt.datetime.now() < dt.datetime.fromisoformat(ov):
            want = "day"  # 本次夜间已用组合键解锁，到本时段结束前不再自动黑屏
        else:
            update(override_until="")
    if want == cfg.get("state"):
        return "unchanged"
    if want == "night":
        B.go_night(cfg["mode"], overlay_cb)
    else:
        B.go_day(overlay_cb)
    return want

# ---------------- Windows：黑幕、快捷键 ----------------

def make_overlay(root):
    """返回 overlay_cb(on)。Windows 用 WDA_EXCLUDEFROMCAPTURE：本地黑，屏幕采集看不到它；鼠标键盘穿透，不拦截输入。"""
    state = {"win": None}

    def cb(on):
        def do():
            import tkinter as tk
            if on and state["win"] is None:
                w = tk.Toplevel(root)
                w.overrideredirect(True)
                w.configure(bg="black")
                x = y = 0; W, H = w.winfo_screenwidth(), w.winfo_screenheight()
                if IS_WIN:
                    import ctypes
                    gm = ctypes.windll.user32.GetSystemMetrics
                    x, y, W, H = gm(76), gm(77), gm(78), gm(79)
                w.geometry("%dx%d+%d+%d" % (W, H, x, y))
                w.attributes("-topmost", True)
                tk.Label(w, text="", fg="#333333", bg="black",
                         font=("", 11)).place(relx=0.5, rely=0.97, anchor="s")
                w.update_idletasks()
                if IS_WIN:
                    import ctypes
                    u = ctypes.windll.user32
                    hwnd = u.GetParent(w.winfo_id()) or w.winfo_id()
                    GWL_EXSTYLE = -20
                    ex = u.GetWindowLongW(hwnd, GWL_EXSTYLE)
                    u.SetWindowLongW(hwnd, GWL_EXSTYLE, ex | 0x80000 | 0x20 | 0x80 | 0x08000000 | 0x8)  # LAYERED|TRANSPARENT|TOOLWINDOW|NOACTIVATE|TOPMOST
                    u.SetLayeredWindowAttributes(hwnd, 0, 255, 2)
                    ok = u.SetWindowDisplayAffinity(hwnd, 0x11)  # WDA_EXCLUDEFROMCAPTURE
                    log("防窥黑幕已显示（排除采集=%s，输入穿透）" % bool(ok))
                state["win"] = w
            elif not on and state["win"] is not None:
                state["win"].destroy(); state["win"] = None
                log("防窥黑幕已关闭")
        root.after(0, do)
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
        while u.GetMessageW(ctypes.byref(msg), None, 0, 0) != 0:
            if msg.message == 0x0312:
                root.after(0, on_hotkey)
    threading.Thread(target=loop, daemon=True).start()
    return True

# ---------------- 托盘 + 界面 ----------------

def run_app(show_window=True, selftest=0):
    import tkinter as tk
    from . import ui
    root = tk.Tk()
    overlay = make_overlay(root)

    def act(name):
        try:
            if name in ("day", "emergency", "quit") and is_locked():
                log("黑屏已锁定，已忽略本地操作“%s”（按住解锁组合键 %s 解除）" % (name, " + ".join(load().get("unlock_chord") or ["C", "K"])))
                return "locked"
            if name == "unlock":
                chord_unlock(overlay); watcher.active.clear()
            elif name == "test_day":
                B.go_day(overlay, mode="all"); watcher.active.clear()
            elif name == "remote_emergency":
                emergency(overlay); watcher.active.clear()
            elif name == "night":
                B.go_night(load()["mode"], overlay)
            elif name == "day":
                B.go_day(overlay, mode="all")
            elif name == "reconcile":
                return reconcile(overlay)
            elif name == "emergency":
                emergency(overlay)
            elif name == "show":
                root.after(0, lambda: (root.deiconify(), root.lift()))
            elif name == "quit":
                root.after(0, root.destroy)
            return "ok"
        except Exception as e:
            log("错误(%s): %s" % (name, e))
            return "error: %s" % e

    # IPC 服务
    srv = socket.socket(); srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1) if not IS_WIN else None
    srv.bind(("127.0.0.1", PORT)); srv.listen(4)

    def serve():
        while True:
            try:
                c, _ = srv.accept()
                cmd = c.makefile().readline().strip()
                box = {}
                ev = threading.Event()
                root.after(0, lambda: (box.__setitem__("r", act(cmd)), ev.set()))
                ev.wait(30)
                c.sendall((box.get("r") or "timeout").encode()); c.close()
            except Exception:
                pass
    threading.Thread(target=serve, daemon=True).start()

    start_hotkey(root, lambda: act("emergency"))
    from . import chord
    watcher = chord.Watcher(lambda: load().get("unlock_chord") or chord.DEFAULT_CHORD, lambda: root.after(0, lambda: act("unlock")))

    def sync_watch():
        (watcher.active.set if is_locked() else watcher.active.clear)()
        root.after(1000, sync_watch)
    root.after(500, sync_watch)
    if IS_MAC:
        chord.request_permission()

    def tick():
        try:
            reconcile(overlay)
        except Exception as e:
            log("定时检查出错: %s" % e)
        root.after(30000, tick)
    root.after(2000, tick)

    win = ui.MainWindow(root, overlay, act)
    tray = ui.start_tray(root, act)
    win.has_tray = bool(tray)
    if not show_window and tray:
        root.withdraw()
    if selftest:
        root.after(selftest * 1000, root.destroy)
    root.mainloop()
    if tray:
        try:
            tray.stop()
        except Exception:
            pass

# ---------------- 入口 ----------------

def _fix_stdout():
    for st in (sys.stdout, sys.stderr):
        try:
            st.reconfigure(encoding="utf-8", errors="replace")
        except Exception:
            pass


def main(argv=None):
    _fix_stdout()
    ap = argparse.ArgumentParser(prog=APP)
    ap.add_argument("--action", default="gui", choices=[
        "gui", "tray", "night", "day", "reconcile", "test", "activate", "pause", "emergency", "uninstall",
        "status", "awake", "backup", "restore", "displays", "plan-virtual", "install-vdd", "uninstall-vdd", "selfcheck"])
    ap.add_argument("--tray", action="store_true", help="开机自启：只显示托盘")
    ap.add_argument("--mode")
    ap.add_argument("--start"); ap.add_argument("--end")
    ap.add_argument("--delay", type=int, default=0)
    ap.add_argument("--seconds", type=int, default=120)
    ap.add_argument("--test-recovery", action="store_true")
    ap.add_argument("--selftest", type=int, default=0, help="界面自检：N 秒后自动退出")
    a, _ = ap.parse_known_args(argv)
    try:
        if a.action in ("gui", "tray") or a.tray:
            if send_ipc("show") is not None:
                return 0
            return run_app(show_window=not (a.tray or a.action == "tray"), selftest=a.selftest)
        if a.delay:
            time.sleep(a.delay)
        if a.action == "status":
            print(B.status_text())
        elif a.action == "awake":
            B.win_awake_loop(a.seconds) if IS_WIN else time.sleep(a.seconds)
        elif a.action == "night":
            if send_ipc("night") is None:
                B.go_night(a.mode or load()["mode"])
        elif a.action == "day":
            if is_locked() and not a.test_recovery:
                print("黑屏已锁定：本地请按住解锁组合键；远程可用 --action emergency"); return 2
            r = send_ipc("test_day" if a.test_recovery else "day")
            if r is None:
                B.go_day(mode="all" if a.test_recovery else None)
            if a.test_recovery:
                update(last_recovery_at=dt.datetime.now().isoformat(), last_recovery_mode=load().get("last_test_mode"))
                log("测试自动恢复完成，请人工确认实体屏已亮、远程正常")
        elif a.action == "reconcile":
            r = send_ipc("reconcile")
            if r is None:
                cfg = load()
                if cfg.get("enabled") and B.resolve_mode(cfg["mode"], cfg.get("virtual_match", "")) == "overlay":
                    B.spawn_detached(B.self_cmd() + ["--tray"])  # 黑幕需要常驻进程
                else:
                    print(reconcile())
        elif a.action == "test":
            start_test(a.mode or load()["mode"], a.seconds)
        elif a.action == "activate":
            c = load(); activate(a.mode or c["mode"], a.start or c["start"], a.end or c["end"])
        elif a.action == "pause":
            pause()
        elif a.action == "emergency":
            # 隐藏的远程恢复：在远程会话里运行，不受黑屏锁限制
            if send_ipc("remote_emergency") is None:
                emergency()
        elif a.action == "uninstall":
            send_ipc("remote_emergency"); uninstall(); send_ipc("quit")
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
        elif a.action == "install-vdd":
            return install_vdd("install")
        elif a.action == "uninstall-vdd":
            return install_vdd("uninstall")
        elif a.action == "selfcheck":
            import json
            print(json.dumps(B.selfcheck(), ensure_ascii=False, indent=1))
    except Exception as e:
        import traceback; traceback.print_exc()
        log("错误(%s): %s" % (a.action, e))
        return 1
    return 0


def resource(name):
    if hasattr(sys, "_MEIPASS"):
        base = os.path.join(sys._MEIPASS, "tools")
    else:
        base = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "tools")
    return os.path.join(base, name)


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
    import ctypes, subprocess
    script = resource("install_vdd.ps1")
    extra = {"install": [], "uninstall": ["-Uninstall"], "status": ["-Status"]}[op]
    argv = ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", script, "-Source", vdd_dir()] + extra
    if op == "status" or ctypes.windll.shell32.IsUserAnAdmin():
        return subprocess.call(["powershell"] + argv)
    args = subprocess.list2cmdline(argv)
    r = ctypes.windll.shell32.ShellExecuteW(None, "runas", "powershell.exe", args, None, 1)
    log("已请求管理员权限%s虚拟显示驱动（ShellExecute=%s）" % ("安装" if op == "install" else "卸载", r))
    return 0 if r > 32 else 1
