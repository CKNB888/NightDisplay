# -*- coding: utf-8 -*-
"""各平台动作：夜间/白天、模式选择、防睡眠、系统计划任务。"""
import os, platform, shlex, subprocess, sys, datetime as dt

from .core import IS_WIN, IS_MAC, IS_LINUX, log, load, save, update, save_backup, load_backup, home, parse_hm, seconds_until, ranges, boundaries, next_switch

MODES = {
    "auto": "自动（最接近 UU 关闭显示屏）",
    "virtual_only": "仅虚拟屏（实体屏脱离桌面）",
    "overlay": "防窥黑幕（本地黑、远程可见）",
    "screen_off": "基础关屏（动鼠标会亮）",
}


def available_modes():
    if IS_WIN:
        return ["auto", "virtual_only", "overlay", "screen_off"]
    if IS_LINUX:
        return ["auto", "virtual_only", "screen_off"]
    return ["auto", "screen_off"]


def win_build():
    try:
        return sys.getwindowsversion().build
    except Exception:
        return 0


def has_virtual_display(match=""):
    try:
        if IS_WIN:
            from . import win_display as W
            return any(W.is_virtual(d, match) for d in W.describe())
        if IS_LINUX:
            from . import linux_display as L
            import re
            return any(o["connected"] and re.search(match or L.VIRTUAL_RE, o["name"]) for o in L.query())
    except Exception as e:
        log("检测虚拟屏失败: %s" % e)
    return False


def validate_mode(mode):
    if mode not in available_modes():
        raise RuntimeError("不支持的模式“%s”。本系统可用：%s" % (mode, "、".join(available_modes())))
    return mode


def resolve_reason(match=""):
    """说明 auto 为什么选择/降级到某个模式。"""
    if (IS_WIN or IS_LINUX) and has_virtual_display(match):
        return "检测到已连接的虚拟屏"
    if IS_LINUX:
        import shutil
        if not shutil.which("xrandr"):
            return "未找到 xrandr（请安装 x11-xserver-utils），降级为基础关屏"
        return "未检测到虚拟屏（VIRTUAL*/DUMMY*/evdi），降级为基础关屏"
    if IS_WIN:
        return "未检测到虚拟屏，" + ("降级为防窥黑幕" if win_build() >= 19041 else "系统低于 Win10 2004，降级为基础关屏")
    return "macOS 暂无内置虚拟屏，使用基础关屏"


def resolve_mode(mode, match=""):
    """auto：有虚拟屏→仅虚拟屏；Windows 10 2004+→防窥黑幕；否则基础关屏。"""
    validate_mode(mode)
    if mode != "auto":
        return mode
    if (IS_WIN or IS_LINUX) and has_virtual_display(match):
        return "virtual_only"
    if IS_WIN and win_build() >= 19041:
        return "overlay"
    return "screen_off"


def _sys_encoding():
    if IS_WIN:
        try:
            import ctypes
            return "cp%d" % ctypes.windll.kernel32.GetOEMCP()
        except Exception:
            return "mbcs"
    return "utf-8"


def run(cmd, check=True):
    kw = {"creationflags": 0x08000000} if IS_WIN else {}
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, encoding=_sys_encoding(), errors="replace", **kw)
    except FileNotFoundError:
        raise RuntimeError("找不到命令 %s" % cmd[0])
    if check and p.returncode:
        raise RuntimeError("%s 失败(%s): %s" % (cmd[0], p.returncode, (p.stderr or p.stdout).strip()))
    return p

# ---------- 防睡眠 ----------
_awake_proc = None


def keep_awake(on, seconds=12 * 3600):
    global _awake_proc
    if _awake_proc and _awake_proc.poll() is None:
        _awake_proc.terminate(); _awake_proc = None
    pidf = os.path.join(home(), "awake.pid")
    if os.path.exists(pidf):
        try:
            os.kill(int(open(pidf).read()), 15)
        except Exception:
            pass
        os.remove(pidf)
    if not on:
        log("已停止保持唤醒")
        return True
    if IS_WIN:
        cmd = self_cmd() + ["--action", "awake", "--seconds", str(seconds)]
    elif IS_MAC:
        cmd = ["/usr/bin/caffeinate", "-i", "-s", "-t", str(seconds)]
    else:
        cmd = ["systemd-inhibit", "--what=sleep:idle", "--who=NightDisplay", "--why=夜间黑屏期间保持开机",
               "--mode=block", "sleep", str(seconds)]
    try:
        _awake_proc = spawn_detached(cmd)
        open(pidf, "w").write(str(_awake_proc.pid))
        log("保持唤醒：%s (pid %s)" % (cmd[0], _awake_proc.pid))
        return True
    except FileNotFoundError:
        log("警告：找不到 %s，无法阻止睡眠" % cmd[0])
        return False

def spawn_detached(cmd):
    kw = dict(stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL)
    if IS_WIN:
        kw["creationflags"] = 0x00000008 | 0x00000200 | 0x08000000  # DETACHED|NEW_GROUP|NO_WINDOW
    else:
        kw["start_new_session"] = True
    return subprocess.Popen(cmd, **kw)


def win_awake_loop(seconds):
    """Windows：本进程持有 ES_SYSTEM_REQUIRED 直到超时或被结束。"""
    import ctypes, time
    r = ctypes.windll.kernel32.SetThreadExecutionState(0x80000000 | 0x1)
    log("SetThreadExecutionState(ES_CONTINUOUS|ES_SYSTEM_REQUIRED) -> 0x%x，最多 %s 秒" % (r & 0xFFFFFFFF, seconds))
    end = time.time() + seconds
    while time.time() < end:
        time.sleep(min(60, seconds))
        if load().get("state") != "night":  # 白天了就退出（onefile 下 kill 不一定命中子进程）
            break
    return r


# ---------- 显示动作 ----------

def _backup_display():
    if IS_WIN:
        from . import win_display as W
        b = W.backup_active()
    elif IS_LINUX:
        from . import linux_display as L
        b = L.backup()
    else:
        return None
    p = save_backup("display", b)
    log("已备份当前显示配置 -> %s" % os.path.basename(p))
    return b


def restore_display(log_=None):
    lg = log_ or log
    b = load_backup("display")
    if IS_WIN:
        from . import win_display as W
        return W.restore(b, lg)
    if IS_LINUX:
        from . import linux_display as L
        return L.restore(b, lg)
    return 0


def screen_off():
    if IS_WIN:
        import ctypes
        from ctypes import wintypes
        res = ctypes.c_size_t()
        ok = ctypes.windll.user32.SendMessageTimeoutW(wintypes.HWND(0xFFFF), 0x0112, 0xF170, 2, 0x2, 2000, ctypes.byref(res))
        if not ok:
            raise RuntimeError("Windows 拒绝关闭显示器")
    elif IS_MAC:
        run(["/usr/bin/pmset", "displaysleepnow"])
    else:
        from . import linux_display as L
        L.screen_off()
    log("已关闭显示器（基础关屏）")


def screen_on():
    if IS_WIN:
        import ctypes
        from ctypes import wintypes
        res = ctypes.c_size_t()
        ctypes.windll.user32.SendMessageTimeoutW(wintypes.HWND(0xFFFF), 0x0112, 0xF170, -1, 0x2, 2000, ctypes.byref(res))
        # 模拟一次零位移鼠标事件以可靠唤醒（不拦截任何输入）
        ctypes.windll.user32.mouse_event(0x0001, 0, 0, 0, 0)
    elif IS_MAC:
        run(["/usr/bin/caffeinate", "-u", "-t", "2"], check=False)
    else:
        from . import linux_display as L
        L.screen_on()


def go_night(mode, overlay_cb=None, dry_run=False):
    """进入夜间。overlay_cb: GUI 进程提供的黑幕回调。返回实际模式。"""
    cfg = load()
    real = resolve_mode(mode, cfg.get("virtual_match", ""))
    log("进入夜间：设置模式=%s，实际模式=%s" % (mode, real))
    if real == "virtual_only":
        if cfg.get("state") != "night" and not dry_run:
            _backup_display()  # 只在白天状态备份，避免把夜间配置当成“正常”
        try:
            if IS_WIN:
                from . import win_display as W
                W.apply_virtual_only(cfg.get("virtual_match", ""), log, dry_run=dry_run)
                if not dry_run:
                    import time; time.sleep(3)
                    if not W.verify_virtual_only(cfg.get("virtual_match", "")):
                        raise RuntimeError("应用后检查失败：仍有实体屏处于活动状态")
            else:
                from . import linux_display as L
                L.apply_virtual_only(cfg.get("virtual_match", ""), log, dry_run=dry_run)
        except Exception as e:
            log("仅虚拟屏失败，立即回滚: %s" % e)
            if not dry_run:
                restore_display()
            raise
    elif real == "overlay":
        if overlay_cb is None:
            raise RuntimeError("防窥黑幕需要托盘程序在运行")
        overlay_cb(True)
    elif real == "screen_off":
        if not dry_run:
            screen_off()
    if cfg.get("keep_awake", True) and not dry_run:
        keep_awake(True, next_switch(cfg)[1] + 600 if cfg.get("enabled") else 6 * 3600)
    if not dry_run:
        update(state="night", night_mode=real)
    return real


def go_day(overlay_cb=None, mode=None, raise_errors=False):
    cfg = load()
    real = mode or cfg.get("night_mode") or "screen_off"
    log("恢复白天显示（上次夜间模式=%s）" % real)
    errors = []
    if overlay_cb:
        try:
            overlay_cb(False)
        except Exception as e:
            errors.append(str(e))
    if real == "virtual_only" or mode == "all":
        try:
            restore_display()
        except Exception as e:
            errors.append(str(e))
    try:
        screen_on()
    except Exception as e:
        errors.append(str(e))
    keep_awake(False)
    update(state="day")
    if errors:
        log("恢复警告（部分失败）: " + " | ".join(errors))
        if raise_errors:
            raise RuntimeError("恢复部分失败: " + " | ".join(errors))
    return real

# ---------- 系统计划任务 ----------
TASKS = ("NightDisplay_Night", "NightDisplay_Day")
LA_LABEL = "com.nightdisplay.agent"
SD_UNIT = "nightdisplay"


def self_cmd():
    if getattr(sys, "frozen", False):
        return [sys.executable]
    return [sys.executable, "-m", "nightdisplay"]


def _systemd_dir():
    return os.path.join(os.environ.get("XDG_CONFIG_HOME") or os.path.expanduser("~/.config"), "systemd", "user")


def systemd_units(cfg):
    exe = " ".join(shlex.quote(c) for c in self_cmd())
    service = ("[Unit]\nDescription=NightDisplay reconcile\n\n[Service]\nType=oneshot\n"
               "ExecStart=%s --action reconcile\n" % exe)
    cal = "".join("OnCalendar=*-*-* %02d:%02d:00\n" % parse_hm(t) for t in boundaries(cfg))
    timer = ("[Unit]\nDescription=NightDisplay daily switch\n\n[Timer]\n" + cal +
             "OnStartupSec=60\nPersistent=true\n\n[Install]\nWantedBy=timers.target\n")
    return service, timer


def launchd_plist(cfg):
    times = []
    for t in boundaries(cfg):
        h, m = parse_hm(t)
        times.append({"Hour": h, "Minute": m})
    return {"Label": LA_LABEL, "ProgramArguments": self_cmd() + ["--action", "reconcile"],
            "StartCalendarInterval": times, "RunAtLoad": True,
            "StandardOutPath": os.path.join(home(), "launchd.log"),
            "StandardErrorPath": os.path.join(home(), "launchd.log")}


def schedule_install(cfg):
    if IS_WIN:
        tr = " ".join('"%s"' % c for c in self_cmd()) + " --action reconcile"
        _win_delete_tasks()
        for i, t in enumerate(boundaries(cfg)):
            name = TASKS[i] if i < 2 else "NightDisplay_T%02d" % i
            run(["schtasks", "/Create", "/F", "/TN", name, "/SC", "DAILY", "/ST", t, "/TR", tr])
        set_autostart(cfg.get("autostart", True))
    elif IS_MAC:
        import plistlib
        path = os.path.expanduser("~/Library/LaunchAgents/%s.plist" % LA_LABEL)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        run(["launchctl", "unload", path], check=False)
        with open(path, "wb") as f:
            plistlib.dump(launchd_plist(cfg), f)
        run(["launchctl", "load", path], check=False)
    else:
        d = _systemd_dir(); os.makedirs(d, exist_ok=True)
        svc, tim = systemd_units(cfg)
        open(os.path.join(d, SD_UNIT + ".service"), "w").write(svc)
        open(os.path.join(d, SD_UNIT + ".timer"), "w").write(tim)
        p = run(["systemctl", "--user", "daemon-reload"], check=False)
        p2 = run(["systemctl", "--user", "enable", "--now", SD_UNIT + ".timer"], check=False)
        if p2.returncode:
            log("警告：systemctl --user 不可用（%s），单元文件已写入 %s" % ((p2.stderr or "").strip()[:120], d))
    log("已注册系统计划：夜间时间段 %s" % "，".join("%s→%s" % r for r in ranges(cfg)))


def _win_delete_tasks():
    """删除本程序创建的全部任务（NightDisplay_Night/Day/Txx，不限数量）。返回删除数。"""
    p = run(["schtasks", "/Query", "/FO", "CSV", "/NH"], check=False)
    import re
    names = set(re.findall(r'"\\?(NightDisplay_(?:Night|Day|T\d+))"', p.stdout or ""))
    n = 0
    for name in sorted(names):
        if run(["schtasks", "/Delete", "/F", "/TN", name], check=False).returncode == 0:
            n += 1
    return n


RUN_KEY = r"Software\Microsoft\Windows\CurrentVersion\Run"


def set_autostart(on):
    """开机自启（托盘常驻，负责黑幕和组合键）。Windows: HKCU Run；macOS: LaunchAgent RunAtLoad 已包含；Linux: XDG autostart。"""
    if IS_WIN:
        import winreg
        with winreg.CreateKeyEx(winreg.HKEY_CURRENT_USER, RUN_KEY, 0, winreg.KEY_SET_VALUE) as k:
            if on:
                winreg.SetValueEx(k, "NightDisplay", 0, winreg.REG_SZ, " ".join('"%s"' % c for c in self_cmd()) + " --tray")
            else:
                try:
                    winreg.DeleteValue(k, "NightDisplay")
                except OSError:
                    pass
    elif IS_LINUX:
        p = os.path.expanduser("~/.config/autostart/nightdisplay.desktop")
        if on:
            os.makedirs(os.path.dirname(p), exist_ok=True)
            open(p, "w").write("[Desktop Entry]\nType=Application\nName=NightDisplay\nExec=%s --tray\nX-GNOME-Autostart-enabled=true\n" % " ".join(shlex.quote(c) for c in self_cmd()))
        elif os.path.exists(p):
            os.remove(p)


def autostart_status():
    if IS_WIN:
        import winreg
        try:
            with winreg.OpenKey(winreg.HKEY_CURRENT_USER, RUN_KEY) as k:
                return bool(winreg.QueryValueEx(k, "NightDisplay")[0])
        except OSError:
            return False
    if IS_LINUX:
        return os.path.exists(os.path.expanduser("~/.config/autostart/nightdisplay.desktop"))
    return os.path.exists(os.path.expanduser("~/Library/LaunchAgents/%s.plist" % LA_LABEL))


def schedule_remove():
    """删除计划任务和自启项，并把配置 autostart 置 False。返回实际删除的项目列表。"""
    removed = []
    if IS_WIN:
        n = _win_delete_tasks()
        if n:
            removed.append("计划任务 %d 个" % n)
        if autostart_status():
            set_autostart(False); removed.append("开机自启(注册表 Run)")
    elif IS_MAC:
        path = os.path.expanduser("~/Library/LaunchAgents/%s.plist" % LA_LABEL)
        if os.path.exists(path):
            run(["launchctl", "unload", path], check=False)
            os.remove(path); removed.append("LaunchAgent")
    else:
        tim = os.path.join(_systemd_dir(), SD_UNIT + ".timer")
        if os.path.exists(tim):
            try:
                run(["systemctl", "--user", "disable", "--now", SD_UNIT + ".timer"], check=False)
            except Exception:
                pass
        for ext in (".service", ".timer"):
            p = os.path.join(_systemd_dir(), SD_UNIT + ext)
            if os.path.exists(p):
                os.remove(p); removed.append("systemd " + ext[1:])
        if autostart_status():
            set_autostart(False); removed.append("XDG 自启")
    update(autostart=False)
    log("已删除：%s" % "、".join(removed) if removed else "无已注册项（计划任务/自启项本来就不存在）")
    return removed


def schedule_status():
    if IS_WIN:
        out = []
        for n in TASKS:
            p = run(["schtasks", "/Query", "/TN", n, "/FO", "LIST"], check=False)
            out.append("%s：%s" % (n, "已注册" if p.returncode == 0 else "未注册"))
        return "；".join(out)
    if IS_MAC:
        p = run(["launchctl", "list"], check=False)
        return "LaunchAgent：" + ("已加载" if LA_LABEL in (p.stdout or "") else "未加载")
    ok = os.path.exists(os.path.join(_systemd_dir(), SD_UNIT + ".timer"))
    return "systemd 用户定时器：" + ("已写入" if ok else "未注册")


def status_text():
    from . import VERSION
    cfg = load()
    return "\n".join([
        "版本 %s ｜ %s %s" % (VERSION, platform.system(), platform.release()),
        "模式：%s ｜ 自动计划：%s ｜ 夜间 %s ｜ 当前状态：%s" % (
            MODES.get(cfg["mode"], cfg["mode"]), "已启用" if cfg["enabled"] else "未启用",
            "，".join("%s→%s" % r for r in ranges(cfg)), "夜间" if cfg["state"] == "night" else "白天"),
        "上次测试：%s %s 成功=%s 自动恢复=%s" % (cfg["last_test_mode"], cfg["last_test_at"][:19], cfg["last_test_ok"], cfg["last_recovery_at"][:19]),
        schedule_status(),
        "开机自启：配置=%s，实际=%s" % ("开" if cfg.get("autostart", True) else "关", "已注册" if autostart_status() else "未注册"),
        "数据目录：%s" % home()])


def selfcheck():
    """Non-destructive environment report used by QA and support."""
    from . import VERSION
    r = {"version": VERSION, "os": platform.platform(), "python": sys.version.split()[0]}
    for k, f in (("modes", available_modes), ("resolved_mode", lambda: resolve_mode("auto")),
                 ("resolve_reason", resolve_reason),
                 ("virtual_display", has_virtual_display), ("autostart_actual", autostart_status),
                 ("autostart_config", lambda: load().get("autostart", True))):
        try:
            r[k] = f()
        except Exception as e:
            r[k] = "error: %s" % e
    if IS_WIN:
        r["win_build"] = win_build()
    try:
        cfg = load()
        r["ranges"] = [list(x) for x in ranges(cfg)]
        r["unlock_chord"] = cfg.get("unlock_chord")
    except Exception as e:
        r["config"] = "error: %s" % e
    return r
