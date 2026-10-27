# -*- coding: utf-8 -*-
"""NightDisplay v1.0 - 可见、可恢复的每日定时黑屏工具 (Windows / macOS)。

安全原则：不拦截键鼠、不安装隐藏服务、不联网、不提权；所有定时任务在系统
"任务计划程序"(Windows) 或 ~/Library/LaunchAgents (macOS) 中可见，可随时删除。
同一个程序既是 GUI，也接受命令行：
  NightDisplay --action night|day|reconcile|emergency|status [--mode ScreenOff|VirtualOnly]
"""
import argparse, datetime as dt, json, os, platform, subprocess, sys, time

APP = "NightDisplay"
VERSION = "1.0.0"
IS_WIN = sys.platform.startswith("win")
IS_MAC = sys.platform == "darwin"
MODES = ["ScreenOff", "VirtualOnly"] if IS_WIN else ["ScreenOff"]
TASK_NIGHT, TASK_DAY = "NightDisplay_Night", "NightDisplay_Day"
LA_LABEL = "com.nightdisplay.reconcile"
CAFF_PID = "caffeinate.pid"


def app_home():
    if IS_WIN:
        base = os.environ.get("LOCALAPPDATA") or os.path.expanduser("~")
        return os.path.join(base, APP)
    if IS_MAC:
        return os.path.expanduser("~/Library/Application Support/" + APP)
    return os.path.expanduser("~/.nightdisplay")


HOME = app_home()
CONFIG = os.path.join(HOME, "config.json")
LOG = os.path.join(HOME, "nightdisplay.log")
DEFAULT = {"version": 2, "mode": "ScreenOff", "enabled": False, "start": "20:00", "end": "10:30",
           "keep_awake": True, "last_test_mode": "", "last_test_at": "", "last_recovery_at": "",
           "last_recovery_mode": "", "last_test_ok": False}


def ensure_home():
    os.makedirs(HOME, exist_ok=True)


def log(msg):
    ensure_home()
    line = "%s  %s" % (dt.datetime.now().strftime("%Y-%m-%d %H:%M:%S"), msg)
    with open(LOG, "a", encoding="utf-8") as f:
        f.write(line + "\n")
    try:
        print(line)
    except Exception:
        pass
    return line


def load():
    cfg = dict(DEFAULT)
    try:
        with open(CONFIG, encoding="utf-8") as f:
            cfg.update(json.load(f))
    except Exception:
        pass
    return cfg


def save(cfg):
    ensure_home()
    with open(CONFIG, "w", encoding="utf-8") as f:
        json.dump(cfg, f, ensure_ascii=False, indent=2)


def parse_hm(s):
    h, m = s.strip().split(":")
    h, m = int(h), int(m)
    if not (0 <= h <= 23 and 0 <= m <= 59):
        raise ValueError("时间格式应为 HH:MM，例如 20:00")
    return h, m


def is_night(start, end, now=None):
    now = now or dt.datetime.now()
    n = now.hour * 60 + now.minute
    s = parse_hm(start); e = parse_hm(end)
    s, e = s[0] * 60 + s[1], e[0] * 60 + e[1]
    if s == e:
        return False
    if s < e:
        return s <= n < e
    return n >= s or n < e  # 跨午夜


def minutes_until(hm):
    h, m = parse_hm(hm)
    now = dt.datetime.now()
    t = now.replace(hour=h, minute=m, second=0, microsecond=0)
    if t <= now:
        t += dt.timedelta(days=1)
    return int((t - now).total_seconds())


def self_cmd():
    """命令行前缀：打包后为 exe 本身，开发时为 python 脚本。"""
    if getattr(sys, "frozen", False):
        return [sys.executable]
    return [sys.executable, os.path.abspath(__file__)]


def run(cmd, check=True):
    kw = {}
    if IS_WIN:
        kw["creationflags"] = 0x08000000  # CREATE_NO_WINDOW
    p = subprocess.run(cmd, capture_output=True, text=True, **kw)
    if check and p.returncode != 0:
        raise RuntimeError("%s 失败(%s): %s" % (cmd[0], p.returncode, (p.stderr or p.stdout).strip()))
    return p

# ---------------- Windows 显示控制 ----------------

def win_monitor(state):
    import ctypes
    from ctypes import wintypes
    res = ctypes.c_size_t()
    r = ctypes.windll.user32.SendMessageTimeoutW(wintypes.HWND(0xFFFF), 0x0112, 0xF170, state,
                                                 0x0002, 2000, ctypes.byref(res))
    return r != 0


def win_screen_count():
    import ctypes
    return ctypes.windll.user32.GetSystemMetrics(80)  # SM_CMONITORS


def win_displayswitch(arg):
    exe = os.path.join(os.environ.get("SystemRoot", r"C:\Windows"), "System32", "DisplaySwitch.exe")
    if not os.path.exists(exe):
        raise RuntimeError("找不到 DisplaySwitch.exe")
    run([exe, arg], check=False)


def win_has_vdd():
    p = run(["powershell", "-NoProfile", "-Command",
             "(Get-CimInstance Win32_VideoController).Name; (Get-PnpDevice -Class Display -EA SilentlyContinue).FriendlyName"],
            check=False)
    import re
    return bool(re.search(r"Virtual|VDD|IddSample|Indirect|Parsec|MttVDD|UU", p.stdout or "", re.I))

# ---------------- macOS ----------------

def mac_keep_awake(seconds):
    mac_stop_awake()
    p = subprocess.Popen(["/usr/bin/caffeinate", "-i", "-s", "-t", str(max(60, seconds))],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
    with open(os.path.join(HOME, CAFF_PID), "w") as f:
        f.write(str(p.pid))
    log("caffeinate 已启动 (pid %s)，阻止系统睡眠 %s 秒（仅显示器熄灭）。" % (p.pid, seconds))


def mac_stop_awake():
    path = os.path.join(HOME, CAFF_PID)
    if os.path.exists(path):
        try:
            pid = int(open(path).read().strip())
            os.kill(pid, 15)
        except Exception:
            pass
        os.remove(path)

# ---------------- 动作 ----------------

def do_night(mode, cfg=None, check_virtual=True):
    cfg = cfg or load()
    if IS_WIN:
        if mode == "ScreenOff":
            if not win_monitor(2):
                raise RuntimeError("Windows 拒绝了关闭显示器的请求")
            log("已发送关屏命令（ScreenOff）。本地动鼠标/键盘可能立即唤醒屏幕。")
        elif mode == "VirtualOnly":
            if check_virtual and not win_has_vdd():
                raise RuntimeError("未识别到虚拟显示驱动(VDD)。请先安装并测试，不要用实体副屏代替。")
            win_displayswitch("/extend")
            time.sleep(3)
            if win_screen_count() < 2:
                raise RuntimeError("执行 /extend 后不足两个显示器，已取消 /external 以免黑屏无法恢复。")
            win_displayswitch("/external")
            log("已请求“仅第二屏（虚拟屏）”模式。实体屏背光是否熄灭取决于硬件。")
        else:
            raise RuntimeError("不支持的模式: %s" % mode)
    elif IS_MAC:
        if cfg.get("keep_awake", True):
            secs = minutes_until(cfg.get("end", "10:30")) if cfg.get("enabled") else 6 * 3600
            mac_keep_awake(secs)
        run(["/usr/bin/pmset", "displaysleepnow"])
        log("已执行 pmset displaysleepnow（显示器睡眠）。本地动鼠标/键盘会唤醒。")
    else:
        raise RuntimeError("仅支持 Windows 和 macOS")


def do_day(mode):
    if IS_WIN:
        if mode == "VirtualOnly":
            win_displayswitch("/internal")
            log("已请求“仅电脑屏幕”(/internal)。请确认实体屏已显示。")
        if not win_monitor(-1):
            log("警告：唤醒显示器命令未被确认，请按任意键。")
        else:
            log("已请求唤醒显示器。")
    elif IS_MAC:
        mac_stop_awake()
        run(["/usr/bin/caffeinate", "-u", "-t", "2"], check=False)
        log("已唤醒显示器（caffeinate -u），并停止保持唤醒。")


def do_reconcile():
    cfg = load()
    if not cfg.get("enabled"):
        log("自动计划未启用，跳过。")
        return
    if is_night(cfg["start"], cfg["end"]):
        do_night(cfg["mode"], cfg)
    else:
        do_day(cfg["mode"])


def do_test(mode, seconds=120):
    cfg = load()
    if cfg.get("enabled"):
        raise RuntimeError("请先暂停每日自动计划，再进行测试。")
    if IS_WIN and mode == "VirtualOnly" and not win_has_vdd():
        raise RuntimeError("未识别到虚拟显示驱动(VDD)。")
    cfg.update(last_test_mode=mode, last_test_at=dt.datetime.now().isoformat(), last_recovery_at="",
               last_recovery_mode="", last_test_ok=False)
    save(cfg)
    # 先启动独立的恢复进程（即使本窗口被关闭也会执行），再切换显示。
    cmd = self_cmd() + ["--action", "day", "--mode", mode, "--delay", str(seconds), "--test-recovery"]
    kw = {"stdout": subprocess.DEVNULL, "stderr": subprocess.DEVNULL, "stdin": subprocess.DEVNULL}
    if IS_WIN:
        kw["creationflags"] = 0x00000008 | 0x00000200 | 0x08000000
    else:
        kw["start_new_session"] = True
    subprocess.Popen(cmd, **kw)
    log("测试开始(%s)，%s 秒后自动恢复。" % (mode, seconds))
    try:
        do_night(mode, cfg, check_virtual=False)
        cfg = load(); cfg["last_test_ok"] = True; save(cfg)
    except Exception as e:
        log("测试失败: %s" % e)
        try:
            do_day(mode)
        except Exception as e2:
            log("恢复失败: %s" % e2)
        raise

# ---------------- 计划任务 ----------------

def schedule_install(cfg):
    exe = self_cmd()
    if IS_WIN:
        tr = " ".join('"%s"' % c for c in exe) + " --action reconcile"
        for name, t in ((TASK_NIGHT, cfg["start"]), (TASK_DAY, cfg["end"])):
            run(["schtasks", "/Create", "/F", "/TN", name, "/SC", "DAILY", "/ST", t, "/TR", tr])
        # 登录校准：当前用户的 Run 注册表项（可见，可在“任务管理器-启动”中关闭）
        import winreg
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, r"Software\Microsoft\Windows\CurrentVersion\Run", 0,
                            winreg.KEY_SET_VALUE) as k:
            winreg.SetValueEx(k, APP, 0, winreg.REG_SZ, tr)
    elif IS_MAC:
        import plistlib
        times = []
        for t in (cfg["start"], cfg["end"]):
            h, m = parse_hm(t)
            times.append({"Hour": h, "Minute": m})
        plist = {"Label": LA_LABEL, "ProgramArguments": exe + ["--action", "reconcile"],
                 "StartCalendarInterval": times, "RunAtLoad": True,
                 "StandardOutPath": os.path.join(HOME, "launchd.log"),
                 "StandardErrorPath": os.path.join(HOME, "launchd.log")}
        path = os.path.expanduser("~/Library/LaunchAgents/%s.plist" % LA_LABEL)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        run(["launchctl", "unload", path], check=False)
        with open(path, "wb") as f:
            plistlib.dump(plist, f)
        run(["launchctl", "load", path], check=False)
    log("已注册每日计划：%s 黑屏，%s 恢复。" % (cfg["start"], cfg["end"]))


def schedule_remove():
    if IS_WIN:
        for name in (TASK_NIGHT, TASK_DAY):
            run(["schtasks", "/Delete", "/F", "/TN", name], check=False)
        try:
            import winreg
            with winreg.OpenKey(winreg.HKEY_CURRENT_USER, r"Software\Microsoft\Windows\CurrentVersion\Run", 0,
                                winreg.KEY_SET_VALUE) as k:
                winreg.DeleteValue(k, APP)
        except OSError:
            pass
    elif IS_MAC:
        path = os.path.expanduser("~/Library/LaunchAgents/%s.plist" % LA_LABEL)
        run(["launchctl", "unload", path], check=False)
        if os.path.exists(path):
            os.remove(path)
    log("已删除所有每日计划任务。")


def schedule_status():
    out = []
    if IS_WIN:
        for name in (TASK_NIGHT, TASK_DAY):
            p = run(["schtasks", "/Query", "/TN", name, "/FO", "LIST"], check=False)
            if p.returncode != 0:
                out.append("%s：未注册" % name)
            else:
                keep = [l.strip() for l in p.stdout.splitlines() if ":" in l][:6]
                out.append("%s：\n  %s" % (name, "\n  ".join(keep)))
    elif IS_MAC:
        p = run(["launchctl", "list"], check=False)
        out.append("LaunchAgent %s：%s" % (LA_LABEL, "已加载" if LA_LABEL in (p.stdout or "") else "未加载"))
    return "\n".join(out)


def activate(mode, start, end):
    cfg = load()
    parse_hm(start); parse_hm(end)
    if start == end:
        raise RuntimeError("开始和结束时间不能相同")
    if not (cfg.get("last_test_ok") and cfg.get("last_test_mode") == mode and
            cfg.get("last_recovery_mode") == mode and cfg.get("last_recovery_at")):
        raise RuntimeError("请先对模式 %s 完成一次“测试（自动恢复）”，并确认恢复正常。" % mode)
    t = dt.datetime.fromisoformat(cfg["last_test_at"])
    if (dt.datetime.now() - t).total_seconds() > 24 * 3600:
        raise RuntimeError("上次测试已超过 24 小时，请重新测试。")
    cfg.update(mode=mode, start=start, end=end, enabled=True)
    save(cfg)
    schedule_install(cfg)
    log("已启用每日计划（模式 %s）。" % mode)


def pause():
    cfg = load(); cfg["enabled"] = False; save(cfg)
    schedule_remove()
    if IS_MAC:
        mac_stop_awake()
    log("已暂停每日计划（当前显示状态未改变）。")


def emergency():
    pause()
    cfg = load(); cfg["last_test_ok"] = False; save(cfg)
    for m in MODES[::-1]:
        try:
            do_day(m)
        except Exception as e:
            log("恢复警告: %s" % e)
    log("已尝试应急恢复。若仍黑屏：Windows 按 Win+P 选择“仅电脑屏幕”；Mac 按任意键或合盖再打开。")


def status_text():
    cfg = load()
    lines = ["版本: %s   系统: %s %s" % (VERSION, platform.system(), platform.release()),
             "程序数据: %s" % HOME,
             "模式: %s   自动计划: %s   时间: %s → %s" % (cfg["mode"], "已启用" if cfg["enabled"] else "未启用",
                                                       cfg["start"], cfg["end"]),
             "上次测试: %s %s  成功:%s  自动恢复于:%s" % (cfg["last_test_mode"], cfg["last_test_at"][:19],
                                                     cfg["last_test_ok"], cfg["last_recovery_at"][:19]),
             schedule_status(), "日志: %s" % LOG]
    return "\n".join(lines)

# ---------------- GUI ----------------

def gui():
    import tkinter as tk
    from tkinter import ttk, messagebox, scrolledtext
    root = tk.Tk()
    root.title("NightDisplay 定时黑屏 v%s" % VERSION)
    root.geometry("640x560")
    cfg = load()
    frm = ttk.Frame(root, padding=10); frm.pack(fill="both", expand=True)

    ttk.Label(frm, text="每天黑屏时间段（可跨午夜，如 20:00 → 10:30）", font=("", 11, "bold")).grid(row=0, column=0, columnspan=4, sticky="w")
    hours = ["%02d" % i for i in range(24)]; mins = ["%02d" % i for i in range(0, 60, 5)]
    sv = [tk.StringVar(value=x) for x in cfg["start"].split(":") + cfg["end"].split(":")]
    ttk.Label(frm, text="开始黑屏").grid(row=1, column=0, sticky="w")
    ttk.Combobox(frm, values=hours, textvariable=sv[0], width=4).grid(row=1, column=1)
    ttk.Combobox(frm, values=mins, textvariable=sv[1], width=4).grid(row=1, column=2, sticky="w")
    ttk.Label(frm, text="恢复显示").grid(row=2, column=0, sticky="w")
    ttk.Combobox(frm, values=hours, textvariable=sv[2], width=4).grid(row=2, column=1)
    ttk.Combobox(frm, values=mins, textvariable=sv[3], width=4).grid(row=2, column=2, sticky="w")

    mode = tk.StringVar(value=cfg["mode"] if cfg["mode"] in MODES else "ScreenOff")
    ttk.Label(frm, text="模式").grid(row=3, column=0, sticky="w")
    ttk.Radiobutton(frm, text="基础关屏（动鼠标会亮）", value="ScreenOff", variable=mode).grid(row=3, column=1, columnspan=2, sticky="w")
    if IS_WIN:
        ttk.Radiobutton(frm, text="仅虚拟屏（需VDD，供UU远程）", value="VirtualOnly", variable=mode).grid(row=4, column=1, columnspan=2, sticky="w")
    awake = tk.BooleanVar(value=cfg.get("keep_awake", True))
    if IS_MAC:
        ttk.Checkbutton(frm, text="黑屏期间阻止 Mac 睡眠 (caffeinate)", variable=awake).grid(row=4, column=1, columnspan=3, sticky="w")

    out = scrolledtext.ScrolledText(frm, height=14, wrap="word")
    out.grid(row=7, column=0, columnspan=4, sticky="nsew", pady=6)
    frm.rowconfigure(7, weight=1); frm.columnconfigure(3, weight=1)

    def show(t):
        out.insert("end", t + "\n"); out.see("end")

    def times():
        s = "%s:%s" % (sv[0].get(), sv[1].get()); e = "%s:%s" % (sv[2].get(), sv[3].get())
        parse_hm(s); parse_hm(e)
        return s, e

    def safe(fn):
        def w():
            try:
                fn()
            except Exception as ex:
                show("错误: %s" % ex); log("错误: %s" % ex)
                messagebox.showerror(APP, str(ex))
        return w

    def save_settings():
        c = load(); c["keep_awake"] = bool(awake.get()); save(c)

    def t_test():
        save_settings()
        m = mode.get()
        msg = "将立即黑屏（%s），120 秒后自动恢复。\n请先确认 UU 远程可连接，并有人在电脑旁。继续？" % m
        if m == "VirtualOnly":
            msg = "【高风险】需已安装虚拟显示驱动并在 UU 中验证可控。\n" + msg
        if messagebox.askyesno(APP, msg):
            root.after(1500, safe(lambda: (do_test(m, 120), show("测试已开始，120 秒后自动恢复。完成后点“刷新状态”。"))))

    def t_enable():
        s, e = times(); save_settings()
        if messagebox.askyesno(APP, "确认：已完成测试且屏幕/UU远程恢复正常？\n每天 %s 黑屏，%s 恢复（模式 %s）。" % (s, e, mode.get())):
            activate(mode.get(), s, e); show("已启用每日计划。"); show(status_text())

    def t_now():
        if messagebox.askyesno(APP, "立即黑屏（不自动恢复）？按键/鼠标或“应急恢复”可恢复。"):
            save_settings(); root.after(1500, safe(lambda: do_night(mode.get())))

    btns = [("测试（120秒自动恢复）", t_test), ("启用每日计划", t_enable),
            ("暂停计划", lambda: (pause(), show("已暂停。"))),
            ("立即黑屏", t_now), ("立即恢复", lambda: (do_day(mode.get()), show("已恢复。"))),
            ("应急恢复（暂停+恢复）", lambda: (emergency(), show("已应急恢复。"))),
            ("刷新状态", lambda: show(status_text())),
            ("查看日志", lambda: show(open(LOG, encoding="utf-8").read()[-4000:] if os.path.exists(LOG) else "暂无日志"))]
    bf = ttk.Frame(frm); bf.grid(row=6, column=0, columnspan=4, sticky="we", pady=6)
    for i, (t, f) in enumerate(btns):
        ttk.Button(bf, text=t, command=safe(f)).grid(row=i // 4, column=i % 4, sticky="we", padx=2, pady=2)
    for i in range(4):
        bf.columnconfigure(i, weight=1)
    show("说明：本工具不拦截键鼠、不安装隐藏服务。基础关屏时本地动鼠标会亮屏。\n"
         "步骤：设置时间 → 测试 → 确认恢复正常 → 启用每日计划。\n")
    show(status_text())
    root.mainloop()


def main():
    ap = argparse.ArgumentParser(prog=APP)
    ap.add_argument("--action", choices=["gui", "night", "day", "reconcile", "emergency", "pause", "status"], default="gui")
    ap.add_argument("--mode", choices=["ScreenOff", "VirtualOnly"])
    ap.add_argument("--delay", type=int, default=0)
    ap.add_argument("--test-recovery", action="store_true")
    a, _ = ap.parse_known_args()  # macOS 可能追加 -psn_ 参数
    try:
        if a.action == "gui":
            return gui()
        mode = a.mode or load()["mode"]
        if a.delay:
            time.sleep(a.delay)
        if a.action == "night":
            do_night(mode)
        elif a.action == "day":
            do_day(mode)
            if a.test_recovery:
                c = load(); c.update(last_recovery_mode=mode, last_recovery_at=dt.datetime.now().isoformat()); save(c)
                log("测试自动恢复完成(%s)，请人工确认实体屏已亮。" % mode)
        elif a.action == "reconcile":
            do_reconcile()
        elif a.action == "emergency":
            emergency()
        elif a.action == "pause":
            pause()
        elif a.action == "status":
            print(status_text())
    except Exception as e:
        log("错误(%s): %s" % (a.action, e))
        return 1


if __name__ == "__main__":
    sys.exit(main() or 0)
