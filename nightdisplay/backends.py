"""各平台动作：夜间/白天、模式选择、防睡眠、系统计划任务。"""
import contextlib
import functools
import os
import platform
import re
import shlex
import subprocess
import sys
import threading
import time
import uuid

from .core import (
    IS_LINUX,
    IS_MAC,
    IS_WIN,
    boundaries,
    home,
    load,
    load_backup,
    log,
    next_switch,
    parse_hm,
    ranges,
    save_backup,
    update,
)
from .storage import file_lock, write_json

MODES = {
    "auto": "自动（最接近 UU 关闭显示屏）",
    "virtual_only": "仅虚拟屏（实体屏脱离桌面）",
    "overlay": "防窥黑幕（本地黑、远程可见）",
    "screen_off": "基础关屏（动鼠标会亮）",
}


def serialized(fn):
    @functools.wraps(fn)
    def call(*args, **kwargs):
        with file_lock(os.path.join(home(), "actions.lock")):
            return fn(*args, **kwargs)
    return call


def available_modes():
    if IS_WIN:
        # WDA_EXCLUDEFROMCAPTURE is only available on Windows 10 2004+.
        modes = ["auto", "virtual_only", "screen_off"]
        if win_build() >= 19041:
            modes.insert(2, "overlay")
        return modes
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
            return any(d["available"] and W.is_virtual(d, match) for d in W.describe())
        if IS_LINUX:
            import re

            from . import linux_display as L
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
        return "未检测到虚拟屏，" + ("使用防窥黑幕" if win_build() >= 19041 else "使用基础关屏（输入会唤醒）")
    return "macOS 暂无内置虚拟屏，使用基础关屏"


def capability_fingerprint():
    """硬件能力指纹。自动降级测过之后，只有指纹没变才允许按测到的方式启用。"""
    try:
        modes = ",".join(available_modes())
    except Exception:
        modes = ""
    try:
        virtual = 1 if has_virtual_display(load().get("virtual_match", "")) else 0
    except Exception:
        virtual = 0
    build = win_build() if IS_WIN else 0
    return "modes=%s;virtual=%s;build=%s" % (modes, virtual, build)


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


_native_lock = threading.RLock()


def native_env():
    env = dict(os.environ)
    if getattr(sys, "frozen", False):
        for key in ("LD_LIBRARY_PATH", "LIBPATH"):
            if key + "_ORIG" in env:
                env[key] = env[key + "_ORIG"]
            else:
                env.pop(key, None)
    return env


@contextlib.contextmanager
def native_launch():
    with _native_lock:
        if IS_WIN and getattr(sys, "frozen", False):
            import ctypes
            ctypes.windll.kernel32.SetDllDirectoryW.argtypes = [ctypes.c_wchar_p]
            ctypes.windll.kernel32.SetDllDirectoryW(None)
            try:
                yield
            finally:
                ctypes.windll.kernel32.SetDllDirectoryW(sys._MEIPASS)
        else:
            yield


def run(cmd, check=True, timeout=60, env=None):
    kw = {"creationflags": 0x08000000} if IS_WIN else {}
    merged = native_env()
    if env:
        merged.update(env)
    try:
        with native_launch():
            p = subprocess.run(cmd, capture_output=True, text=True, encoding=_sys_encoding(), errors="replace",
                               timeout=timeout, env=merged, **kw)
    except FileNotFoundError as e:
        raise RuntimeError("找不到命令 %s" % cmd[0]) from e
    except subprocess.TimeoutExpired as e:
        raise RuntimeError("%s 超过 %s 秒未响应" % (cmd[0], timeout)) from e
    if check and p.returncode:
        raise RuntimeError("%s 失败(%s): %s" % (cmd[0], p.returncode, (p.stderr or p.stdout).strip()))
    return p

# ---------- 防睡眠 ----------
_awake_proc = None


def keep_awake(on, seconds=12 * 3600):
    global _awake_proc
    # Revoke the previous helper's token. Never kill a PID read from disk:
    # executable names cannot distinguish a reused Python/caffeinate PID.
    token = uuid.uuid4().hex if on else ""
    update(awake_token=token)
    pidf = os.path.join(home(), "awake.pid")
    if os.path.exists(pidf):
        with contextlib.suppress(OSError):
            os.remove(pidf)
    if not on:
        log("已停止保持唤醒")
        return True
    cmd = self_cmd() + ["--action", "awake", "--seconds", str(seconds), "--awake-token", token]
    try:
        _awake_proc = spawn_detached(cmd)
        ready_path = os.path.join(home(), "awake-ready.json")
        deadline = time.monotonic() + 12
        while time.monotonic() < deadline:
            try:
                import json
                with open(ready_path, encoding="utf-8") as f:
                    ready = json.load(f)
                if ready.get("token") == token:
                    break
            except (OSError, ValueError):
                pass
            if _awake_proc.poll() is not None:
                update(awake_token="")
                raise RuntimeError("保持唤醒启动失败，请查看日志")
            time.sleep(0.1)
        else:
            update(awake_token="")
            raise RuntimeError("保持唤醒启动超时，未继续黑屏")
        log("保持唤醒：%s (pid %s)" % (cmd[0], _awake_proc.pid))
        return True
    except FileNotFoundError as e:
        update(awake_token="")
        raise RuntimeError("找不到 %s，无法阻止睡眠" % cmd[0]) from e

def spawn_detached(cmd):
    kw = dict(stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, stdin=subprocess.DEVNULL)
    if getattr(sys, "frozen", False):
        # Timed recovery must keep its own extracted files after the GUI exits.
        kw["env"] = dict(os.environ, PYINSTALLER_RESET_ENVIRONMENT="1")
    if IS_WIN:
        kw["creationflags"] = 0x00000008 | 0x00000200 | 0x08000000  # DETACHED|NEW_GROUP|NO_WINDOW
    else:
        kw["start_new_session"] = True
    return subprocess.Popen(cmd, **kw)


def windows_execution_flags(night_mode):
    """阻止系统睡眠。只有防窥黑幕额外禁止显示器休眠。

    winbase.h：ES_SYSTEM_REQUIRED 是 0x1，ES_DISPLAY_REQUIRED 是 0x2，
    ES_CONTINUOUS 是 0x80000000。0x2 会重置显示器空闲计时器，把已经关掉的
    屏幕重新点亮，所以仅虚拟屏和基础关屏只用 0x80000001。防窥黑幕画在真实
    屏上，显示器休眠后窗口会消失，才加 0x2。退出时用单独的 0x80000000 清掉。
    夜间必须一直醒着，远程才能操作；这个请求只活在唤醒进程里，进程结束就释放。
    """
    if night_mode == "overlay":
        return 0x80000003
    return 0x80000001


def awake_loop(seconds, token=""):
    """Own any child by its live Popen handle; exit when the token is revoked."""
    child = None
    if token and load().get("awake_token") != token:
        return
    try:
        if IS_WIN:
            import ctypes
            flags = windows_execution_flags(load().get("night_mode"))
            if not ctypes.windll.kernel32.SetThreadExecutionState(flags):
                raise RuntimeError("Windows 拒绝阻止系统睡眠")
        elif IS_MAC:
            child = subprocess.Popen(["/usr/bin/caffeinate", "-i", "-s", "-t", str(seconds)], env=native_env())
        else:
            child = subprocess.Popen(["systemd-inhibit", "--what=sleep:idle", "--who=NightDisplay",
                                      "--why=NightDisplay", "--mode=block", "sleep", str(seconds)], env=native_env())
        if child is not None:
            time.sleep(0.2)
            if child.poll() is not None and child.returncode:
                raise RuntimeError("保持唤醒进程启动失败")
        end = time.monotonic() + seconds
        heartbeat = 0
        while time.monotonic() < end:
            if token and load().get("awake_token") != token:
                break
            if token and time.monotonic() >= heartbeat:
                write_json(os.path.join(home(), "awake-ready.json"), {"token": token, "at": time.time()})
                heartbeat = time.monotonic() + 5
            if child is not None and child.poll() is not None:
                if child.returncode:
                    raise RuntimeError("保持唤醒进程退出，代码 %s" % child.returncode)
                break
            time.sleep(min(0.25, max(0, end - time.monotonic())))
    finally:
        if child is not None and child.poll() is None:
            child.terminate()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill(); child.wait()
        if IS_WIN:
            ctypes.windll.kernel32.SetThreadExecutionState(0x80000000)


def ensure_night():
    """Repair resume/display changes without replacing the daytime backup."""
    c = load()
    if c.get("state") != "night" or c.get("recovery_pending"):
        return
    if c.get("night_mode") == "virtual_only":
        if IS_WIN:
            from . import win_display as display
        else:
            from . import linux_display as display
        match = c.get("virtual_match", "")
        if IS_WIN:
            from . import win_power
            # 修复夜间拓扑时不要把显示器空闲又禁掉，否则一体机背光会一直亮。
            win_power.enter(keep_display_on=False)
        if not display.verify_virtual_only(match):
            display.apply_virtual_only(match, log)
            if not display.verify_virtual_only(match):
                raise RuntimeError("夜间输出已改变，无法重新确认只有虚拟屏")
    elif IS_WIN and c.get("night_mode") == "screen_off":
        # 基础关屏只发一次的话，键鼠把屏幕点亮后会一直亮到下一个时间边界。
        # 失败只记日志：这里抛出去会让定时检查关掉每日计划。
        try:
            screen_off(quiet=True)
        except Exception as e:
            log("再次关屏未成功，保持夜间：%s" % e)
    if c.get("keep_awake"):
        try:
            import json
            with open(os.path.join(home(), "awake-ready.json"), encoding="utf-8") as f:
                ready = json.load(f)
            healthy = ready.get("token") == c.get("awake_token") and 0 <= time.time() - ready["at"] < 15
        except (OSError, ValueError, KeyError, TypeError):
            healthy = False
        if not healthy:
            keep_awake(True, next_switch(c)[1] + 600 if c.get("enabled") else 6 * 3600)


# ---------- 显示动作 ----------

def _backup_display():
    match = load().get("virtual_match", "")
    if IS_WIN:
        from . import win_display as W
        b = W.backup_active()
        if b.get("displays") and all(W.is_virtual(d, match) for d in b["displays"]):
            raise RuntimeError("当前只剩虚拟屏，不能覆盖正常显示备份；请先应急恢复实体屏")
    elif IS_LINUX:
        from . import linux_display as L
        b = L.backup()
        active = [o for o in b.get("outputs") or [] if o.get("active")]
        if active and all(L.is_virtual_name(o.get("name", ""), match) for o in active):
            raise RuntimeError("当前只剩虚拟屏，不能覆盖正常显示备份；请先应急恢复实体屏")
    else:
        return None
    p = save_backup("display", b)
    log("已备份当前显示配置 -> %s" % os.path.basename(p))
    return b


def restore_display(log_=None):
    lg = log_ or log
    try:
        b = load_backup("display")
    except (ValueError, OSError) as e:
        lg("显示备份不可读，尝试系统恢复：%s" % e)
        b = None
    if b is not None and not isinstance(b, dict):
        lg("显示备份格式不正确，尝试系统恢复")
        b = None
    if IS_WIN:
        from . import win_display as W
        return W.restore(b, lg)
    if IS_LINUX:
        from . import linux_display as L
        return L.restore(b, lg)
    return 0


def screen_off(quiet=False):
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
    if not quiet:
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


@serialized
def go_night(mode, overlay_cb=None, dry_run=False):
    if mode == "auto" and not dry_run:
        chosen = resolve_mode(mode, load().get("virtual_match", ""))
        candidates = [chosen]
        if IS_WIN and chosen == "virtual_only" and win_build() >= 19041 and overlay_cb:
            candidates.append("overlay")
        if "screen_off" not in candidates:
            candidates.append("screen_off")
        for candidate in candidates:
            try:
                return _go_night(candidate, overlay_cb, dry_run)
            except Exception as e:
                if load().get("state") == "night" or load().get("recovery_pending"):
                    raise
                log("%s 失败：%s，尝试下一种兼容方式" % (candidate, e))
                if candidate == candidates[-1]:
                    raise
    return _go_night(mode, overlay_cb, dry_run)


def _go_night(mode, overlay_cb=None, dry_run=False):
    """进入夜间。overlay_cb: GUI 进程提供的黑幕回调。返回实际模式。"""
    cfg = load()
    real = resolve_mode(mode, cfg.get("virtual_match", ""))
    if not dry_run and cfg.get("state") == "night":
        if real == cfg.get("night_mode"):
            return real
        raise RuntimeError("请先恢复显示，再切换夜间模式")
    log("进入夜间：设置模式=%s，实际模式=%s" % (mode, real))
    if real == "virtual_only":
        if not dry_run:
            _backup_display()  # 只在白天状态备份，避免把夜间配置当成“正常”
            update(night_mode=real, recovery_pending=True)
        try:
            if IS_WIN:
                from . import win_display as W
                if not dry_run:
                    W.plan_virtual_only(cfg.get("virtual_match", ""))
                    from . import win_power
                    # 实体屏已经离开桌面。系统保持醒着，显示器空闲仍允许，背光才能灭。
                    win_power.enter(keep_display_on=False)
                W.apply_virtual_only(cfg.get("virtual_match", ""), log, dry_run=dry_run)
                if not dry_run:
                    time.sleep(3)
                    if not W.verify_virtual_only(cfg.get("virtual_match", "")):
                        raise RuntimeError("应用后检查失败：仍有实体屏处于活动状态")
            else:
                from . import linux_display as L
                L.apply_virtual_only(cfg.get("virtual_match", ""), log, dry_run=dry_run)
                if not dry_run and not L.verify_virtual_only(cfg.get("virtual_match", "")):
                    raise RuntimeError("应用后检查失败：仍有实体屏处于活动状态")
        except Exception as e:
            log("仅虚拟屏失败，立即回滚: %s" % e)
            if not dry_run:
                try:
                    restore_display()
                    if IS_WIN:
                        from . import win_power
                        win_power.restore()
                    update(state="day", night_mode="", recovery_pending=False)
                except Exception as recovery:
                    update(state="night", night_mode=real)
                    raise RuntimeError("切换失败且回滚失败：%s；%s" % (e, recovery)) from e
            raise
    elif real == "overlay":
        if overlay_cb is None:
            raise RuntimeError("防窥黑幕需要托盘程序在运行")
        if not dry_run and overlay_cb(True) is not True:
            with contextlib.suppress(Exception):
                overlay_cb(False)
            raise RuntimeError("防窥黑幕未能确认显示（窗口未创建或排除采集失败），未进入夜间")
    elif real == "screen_off":
        if not dry_run:
            screen_off()
    if not dry_run:
        update(state="night", night_mode=real, recovery_pending=False)
        if IS_WIN and real != "virtual_only":
            try:
                from . import win_power
                win_power.enter(keep_display_on=real != "screen_off")
            except Exception:
                go_day(overlay_cb, raise_errors=True)
                raise
        if cfg.get("keep_awake", True):
            try:
                keep_awake(True, next_switch(cfg)[1] + 600 if cfg.get("enabled") else 6 * 3600)
            except Exception:
                go_day(overlay_cb, raise_errors=True)
                raise
    return real


@serialized
def go_day(overlay_cb=None, mode=None, raise_errors=True):
    cfg = load()
    real = cfg.get("night_mode") if mode == "all" else mode or cfg.get("night_mode")
    real = real or "screen_off"
    log("恢复白天显示（上次夜间模式=%s）" % real)
    errors = []
    if overlay_cb:
        try:
            if overlay_cb(False) is not True:
                raise RuntimeError("黑幕未确认关闭")
        except Exception as e:
            errors.append(str(e))
    elif real == "overlay" and cfg.get("state") == "night":
        # No resident process means its windows have already been destroyed.
        log("黑幕常驻进程已退出，继续唤醒显示器")
    if real == "virtual_only":
        try:
            restore_display()
        except Exception as e:
            errors.append(str(e))
    try:
        screen_on()
    except Exception as e:
        errors.append(str(e))
    if IS_WIN:
        try:
            from . import win_power
            win_power.restore()
        except Exception as e:
            errors.append(str(e))
    if errors:
        update(recovery_pending=True)
        log("恢复警告（部分失败）: " + " | ".join(errors))
        if raise_errors:
            raise RuntimeError("恢复部分失败: " + " | ".join(errors))
        return real
    update(state="day", night_mode="", recovery_pending=False, hold_night_until="")
    keep_awake(False)
    return real

# ---------- 系统计划任务 ----------
TASKS = ("NightDisplay_Night", "NightDisplay_Day")
LA_LABEL = "com.nightdisplay.agent"
SD_UNIT = "nightdisplay"


def self_cmd():
    if getattr(sys, "frozen", False):
        return [sys.executable, "--home", os.path.abspath(home())]
    return [sys.executable, os.path.abspath(os.path.join(os.path.dirname(__file__), "..", "nightdisplay_main.py")),
            "--home", os.path.abspath(home())]


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


# schtasks /Create 写不出 StartWhenAvailable。任务名仍按边界顺序分配，
# 和已有的 NightDisplay_Night/Day/Txx 查询、删除保持一致，不按“夜间/白天”改名。
# -At 的类型是 DateTime，用 24 小时制解析，避免系统区域把 20:00 当成上午。
# LogonType Interactive 等于 schtasks /IT：只在当前已登录的交互会话里跑，不保存密码。
# 账户用 DOMAIN\user，不用单独的用户名。
_WIN_TASK_SCRIPT = r"""
$ErrorActionPreference = "Stop"
$payload = $env:NIGHTDISPLAY_TASKS | ConvertFrom-Json
$action = New-ScheduledTaskAction -Execute $payload.exe -Argument $payload.args
$settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -StartWhenAvailable -MultipleInstances IgnoreNew
$account = [System.Security.Principal.WindowsIdentity]::GetCurrent().Name
$principal = New-ScheduledTaskPrincipal -UserId $account -LogonType Interactive -RunLevel Limited
foreach ($task in $payload.tasks) {
  $when = [datetime]::ParseExact($task.time, "HH:mm", [System.Globalization.CultureInfo]::InvariantCulture)
  $trigger = New-ScheduledTaskTrigger -Daily -At $when
  Register-ScheduledTask -TaskName $task.name -Action $action -Trigger $trigger -Settings $settings -Principal $principal -Force | Out-Null
}
"""


def windows_task_plan(cfg):
    """当前用户交互式会话里、每个时间边界一个任务。醒来后补跑错过的边界。"""
    cmd = self_cmd() + ["--action", "reconcile"]
    tasks = []
    for i, stamp in enumerate(boundaries(cfg)):
        name = TASKS[i] if i < 2 else "NightDisplay_T%02d" % i
        tasks.append({"name": name, "time": stamp})
    return {"exe": cmd[0], "args": subprocess.list2cmdline(cmd[1:]), "tasks": tasks}


def _win_register_tasks(plan):
    import json
    run(["powershell", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", _WIN_TASK_SCRIPT],
        timeout=60, env={"NIGHTDISPLAY_TASKS": json.dumps(plan, ensure_ascii=True)})


def schedule_install(cfg):
    if IS_WIN:
        _win_delete_tasks()
        plan = windows_task_plan(cfg)
        try:
            _win_register_tasks(plan)
        except Exception as e:
            # 计划任务模块不可用时仍要有每日任务，只是睡眠期间错过的点要等下一次到点。
            # PowerShell 可能已经登记了一部分同名任务，先删掉再交给 schtasks /F。
            log("补跑计划注册失败，改用基础计划任务：%s" % e)
            try:
                _win_delete_tasks()
            except Exception as cleanup:
                log("清理未完成的计划任务失败：%s" % cleanup)
            tr = subprocess.list2cmdline(self_cmd() + ["--action", "reconcile"])
            for task in plan["tasks"]:
                run(["schtasks", "/Create", "/F", "/TN", task["name"], "/SC", "DAILY",
                     "/ST", task["time"], "/TR", tr, "/IT"])
        set_autostart(cfg.get("autostart", True))
    elif IS_MAC:
        import plistlib
        path = os.path.expanduser("~/Library/LaunchAgents/%s.plist" % LA_LABEL)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        run(["launchctl", "unload", path], check=False)
        with open(path, "wb") as f:
            plistlib.dump(launchd_plist(cfg), f)
        run(["launchctl", "load", path])
        set_autostart(cfg.get("autostart", True))
    else:
        d = _systemd_dir(); os.makedirs(d, exist_ok=True)
        svc, tim = systemd_units(cfg)
        with open(os.path.join(d, SD_UNIT + ".service"), "w") as f:
            f.write(svc)
        with open(os.path.join(d, SD_UNIT + ".timer"), "w") as f:
            f.write(tim)
        run(["systemctl", "--user", "daemon-reload"])
        run(["systemctl", "--user", "enable", "--now", SD_UNIT + ".timer"])
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
                winreg.SetValueEx(k, "NightDisplay", 0, winreg.REG_SZ, subprocess.list2cmdline(self_cmd() + ["--tray"]))
            else:
                with contextlib.suppress(OSError):
                    winreg.DeleteValue(k, "NightDisplay")
    elif IS_MAC:
        import plistlib
        path = os.path.expanduser("~/Library/LaunchAgents/com.nightdisplay.tray.plist")
        run(["launchctl", "unload", path], check=False)
        if on:
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "wb") as f:
                plistlib.dump({"Label": "com.nightdisplay.tray", "ProgramArguments": self_cmd() + ["--tray"],
                              "RunAtLoad": True}, f)
            run(["launchctl", "load", path])
        elif os.path.exists(path):
            os.remove(path)
    elif IS_LINUX:
        p = os.path.expanduser("~/.config/autostart/nightdisplay.desktop")
        if on:
            os.makedirs(os.path.dirname(p), exist_ok=True)
            with open(p, "w") as f:
                f.write("[Desktop Entry]\nType=Application\nName=NightDisplay\nExec=%s --tray\nX-GNOME-Autostart-enabled=true\n" % " ".join(shlex.quote(c) for c in self_cmd()))
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
    return os.path.exists(os.path.expanduser("~/Library/LaunchAgents/com.nightdisplay.tray.plist"))


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
        if autostart_status():
            set_autostart(False); removed.append("开机自启")
    else:
        tim = os.path.join(_systemd_dir(), SD_UNIT + ".timer")
        if os.path.exists(tim):
            with contextlib.suppress(Exception):
                run(["systemctl", "--user", "disable", "--now", SD_UNIT + ".timer"], check=False)
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
        p = run(["schtasks", "/Query", "/FO", "CSV", "/NH"], check=False)
        names = set(re.findall(r'"\\?(NightDisplay_(?:Night|Day|T\d+))"', p.stdout or ""))
        out = ["%s：已注册" % n for n in sorted(names)]
        out += ["%s：未注册" % n for n in TASKS if n not in names]
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
