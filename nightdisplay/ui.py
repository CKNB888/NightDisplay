"""设置窗口用系统控件，不自绘。

Windows 上 Tk 的 vista 主题走 comctl6 / uxtheme，按钮和输入框由系统绘制。
WinUI 3 和 WPF 要另带一套运行时，而这扇窗口夜里会撤掉，黑屏本体仍是原来的无边框窗口。
两套界面叠在一起只增加启动和内存，不加快黑屏。这里不放装饰动画，也不另做配色。
"""
import contextlib
import queue
import re
import threading
import time
import tkinter as tk
import tkinter.font as tkfont
import webbrowser
from tkinter import filedialog, messagebox, ttk

from . import APP, AUTHOR, SPONSOR, VERSION
from . import backends as B
from .chord import DEFAULT_CHORD, fmt
from .core import (
    IS_MAC,
    IS_WIN,
    export_logs,
    format_hm,
    hm_parts,
    load,
    log,
    read_log_tail,
    update,
)

HOURS = ["%02d" % n for n in range(24)]
MINUTES = ["%02d" % n for n in range(60)]

SUBTITLE = "这台电脑到点会黑屏，远程的人还能用。"
GUIDE = "第一次用：先点「测试 15 秒」，等屏幕自己亮回来，再点「启用每天自动黑屏」。开机后会自己在后台运行，桌面上的图标也能打开它。"


def use_native_theme(root):
    """选系统主题，只改字体族以便中文落在系统界面字体上。不覆盖颜色。"""
    style = ttk.Style(root)
    names = set(style.theme_names())
    if IS_WIN and "vista" in names:
        with contextlib.suppress(tk.TclError):
            style.theme_use("vista")
    elif IS_MAC and "aqua" in names:
        with contextlib.suppress(tk.TclError):
            style.theme_use("aqua")
    if IS_WIN:
        family, size = "Microsoft YaHei UI", 9
    elif IS_MAC:
        family, size = "PingFang SC", 13
    else:
        family, size = "Noto Sans CJK SC", 10
    for name in ("TkDefaultFont", "TkTextFont", "TkHeadingFont", "TkMenuFont"):
        with contextlib.suppress(tk.TclError):
            tkfont.nametofont(name).configure(family=family, size=size)
    return style


def lock_hint(cfg=None):
    """锁定说明用当前组合键。这个窗口上的按钮解不开，开始菜单里的恢复项可以。"""
    cfg = load() if cfg is None else cfg
    keys = fmt(cfg.get("unlock_chord") or DEFAULT_CHORD)
    return "屏幕锁住了。同时按住 %s，大约半秒。这个窗口上的按钮解不开。请到开始菜单打开「恢复并暂停 NightDisplay」。" % keys


def settings_block_message(cfg):
    """设置保存被拒绝时的说明。空字符串表示现在可以保存。"""
    if cfg.get("state") == "night" and cfg.get("lock", True):
        return lock_hint(cfg)
    if cfg.get("state") == "night":
        return "屏幕还黑着。先点「恢复屏幕并暂停」，等屏幕亮了，再改设置。"
    if cfg.get("enabled"):
        return "先暂停每天自动黑屏，再改设置。"
    return ""


def prepare_manual_test(cfg):
    """计划还开着时，测试要说明的是测试本身，不去读时间。"""
    if cfg.get("enabled"):
        raise RuntimeError("请先暂停每天自动黑屏，再测试。")


def manual_test_request(cfg):
    """点一次就测 15 秒。不读取、不保存时间。"""
    prepare_manual_test(cfg)
    return {"action": "test", "mode": "auto", "seconds": 15}


def env_status():
    """环境一行。内置驱动和旁边的远程虚拟屏分开写，用能看懂的话，不把 UU 写成已经就绪的内置屏。"""
    try:
        if B.bundled_virtual_connected():
            return "已经准备好。你这边黑屏时，远程还能用。"
        if B.has_virtual_display():
            return "已经准备好。用的是另外安装的远程屏幕。"
        if IS_WIN and B.win_build() >= 19041:
            return "已经准备好。黑屏时会在你这边盖上黑幕。"
        return "已经准备好。到点会关掉屏幕，动一下鼠标就会亮。"
    except Exception:
        return "已经准备好。"


def pack_clock(parent, hour_var, minute_var, on_pick=None):
    """小时和分钟两个只读下拉框。不能打字，所以不会再出现时间格式错误。"""
    hour = ttk.Combobox(parent, textvariable=hour_var, values=HOURS, width=4, state="readonly")
    hour.pack(side="left")
    ttk.Label(parent, text="点").pack(side="left", padx=(2, 4))
    minute = ttk.Combobox(parent, textvariable=minute_var, values=MINUTES, width=4, state="readonly")
    minute.pack(side="left")
    ttk.Label(parent, text="分").pack(side="left")
    if on_pick:
        hour.bind("<<ComboboxSelected>>", on_pick)
        minute.bind("<<ComboboxSelected>>", on_pick)
    return hour, minute


def make_export_icon(master):
    """16 像素单色图标：一页纸加一个向下的箭头。调用方要留着 PhotoImage 引用。"""
    img = tk.PhotoImage(master=master, width=16, height=16)
    for x in range(16):
        for y in range(16):
            with contextlib.suppress(tk.TclError):
                img.transparency_set(x, y, True)

    def ink(x, y):
        img.put("#202020", (x, y))
        with contextlib.suppress(tk.TclError):
            img.transparency_set(x, y, False)

    for x in range(3, 11):
        ink(x, 1)
        ink(x, 10)
    for y in range(1, 11):
        ink(2, y)
        ink(10, y)
    for y in (4, 6, 8):
        for x in range(4, 9):
            ink(x, y)
    for y in range(12, 15):
        ink(6, y)
    ink(5, 14)
    ink(6, 15)
    ink(7, 14)
    return img


def _put(widget, text):
    if str(widget.cget("text")) != text:
        widget.configure(text=text)


def _lock_size(window):
    """控件排完再锁定大小。提前禁止拉伸会把窗口冻在 Tk 的默认小尺寸上，而且用户拉不开。"""
    window.update_idletasks()
    window.geometry("%dx%d" % (window.winfo_reqwidth(), window.winfo_reqheight()))
    window.resizable(False, False)


class MainWindow:
    def __init__(self, root, overlay, act):
        self.root, self.overlay, self.act = root, overlay, act
        self.has_tray = False
        use_native_theme(root)
        root.title("NightDisplay")
        root.protocol("WM_DELETE_WINDOW", self.on_close)
        outer = ttk.Frame(root, padding=11)
        outer.pack(fill="both", expand=True)

        head = ttk.Frame(outer)
        head.pack(fill="x")
        ttk.Label(head, text="NightDisplay").pack(side="left")
        ttk.Label(head, text="v" + VERSION).pack(side="right")
        ttk.Label(outer, text=SUBTITLE, wraplength=460).pack(anchor="w", pady=(2, 2))
        ttk.Label(outer, text=GUIDE, wraplength=460).pack(anchor="w", pady=(0, 8))

        status = ttk.LabelFrame(outer, text="现在的情况", padding=8)
        status.pack(fill="x")
        self.state_text = ttk.Label(status, text="屏幕正常显示")
        self.state_text.pack(anchor="w")
        self.detail_text = ttk.Label(status, text="", wraplength=460)
        self.detail_text.pack(anchor="w", pady=(2, 0))
        self.mode_text = ttk.Label(status, text="", wraplength=460)
        self.mode_text.pack(anchor="w")
        self.test_text = ttk.Label(status, text="", wraplength=460)
        self.test_text.pack(anchor="w", pady=(2, 0))
        self.env_text = ttk.Label(status, text="正在检查这台电脑…", wraplength=460)
        self.env_text.pack(anchor="w", pady=(2, 0))
        self.boot_text = ttk.Label(status, text="", wraplength=460)
        self.boot_text.pack(anchor="w", pady=(2, 0))

        times = ttk.LabelFrame(outer, text="每天什么时候黑屏", padding=8)
        times.pack(fill="x", pady=(8, 0))
        cfg = load()
        sh, sm = hm_parts(cfg.get("start"), (20, 0))
        eh, em = hm_parts(cfg.get("end"), (10, 30))
        self.start_h = tk.StringVar(value="%02d" % sh)
        self.start_m = tk.StringVar(value="%02d" % sm)
        self.end_h = tk.StringVar(value="%02d" % eh)
        self.end_m = tk.StringVar(value="%02d" % em)
        row = ttk.Frame(times)
        row.pack(anchor="w")
        ttk.Label(row, text="开始").pack(side="left", padx=(0, 4))
        start_boxes = pack_clock(row, self.start_h, self.start_m, self._picked_time)
        ttk.Label(row, text="结束").pack(side="left", padx=(12, 4))
        end_boxes = pack_clock(row, self.end_h, self.end_m, self._picked_time)
        self._clocks = start_boxes + end_boxes
        self.time_hint = ttk.Label(times, text="", wraplength=460)
        self.time_hint.pack(anchor="w", pady=(6, 0))

        actions = ttk.Frame(outer)
        actions.pack(anchor="w", pady=(8, 0))
        self._button(actions, "测试 15 秒", self.test).pack(side="left", padx=(0, 6))
        self.enable = self._button(actions, "启用每天自动黑屏", self.toggle)
        self.enable.pack(side="left", padx=(0, 6))
        self._button(actions, "现在就黑屏", self.night).pack(side="left", padx=(0, 6))
        self._button(actions, "恢复屏幕并暂停", self.emergency).pack(side="left")

        tools = ttk.Frame(outer)
        tools.pack(fill="x", pady=(8, 0))
        self._button(tools, "运行日志", self.logs).pack(side="left", padx=(0, 6))
        self._export_button(tools, self.export_saved_logs).pack(side="left", padx=(0, 6))
        self._button(tools, "设置", self.settings).pack(side="left")
        side = ttk.Frame(tools)
        side.pack(side="right")
        ttk.Label(side, text="传康Kk  ·  1837620622").pack(anchor="e")
        self.unlock_text = ttk.Label(side, text="亮屏：同时按住 C 和 K")
        self.unlock_text.pack(anchor="e")

        self._last_state = None
        self._notice = ""
        self._notice_until = 0.0
        self.refresh(schedule=False)
        _lock_size(root)
        self.root.after(1000, self.refresh)
        self.root.after(3000, lambda: self.check_update(quiet=True))

    def _button(self, parent, text, command):
        return ttk.Button(parent, text=text, command=lambda: self.safe(command))

    def _export_button(self, parent, command):
        icon = self._export_icon()
        options = {"text": "导出日志", "command": lambda: self.safe(command)}
        if icon is not None:
            options["image"] = icon
            options["compound"] = "left"
        return ttk.Button(parent, **options)

    def _export_icon(self):
        if getattr(self, "_export_photo", None) is None:
            try:
                self._export_photo = make_export_icon(self.root)
            except tk.TclError:
                self._export_photo = None
        return self._export_photo

    def safe(self, fn):
        try:
            result = fn()
            if isinstance(result, str) and result.startswith("error:"):
                raise RuntimeError(result[6:].strip())
            if result == "locked":
                raise RuntimeError(lock_hint())
            if self.root.winfo_exists():
                self.refresh(schedule=False)
            return result
        except Exception as e:
            log("界面操作失败：%s" % e)
            messagebox.showerror(APP, str(e), parent=self.root)

    def _show_saved_time(self, cfg):
        sh, sm = hm_parts(cfg.get("start"), (20, 0))
        eh, em = hm_parts(cfg.get("end"), (10, 30))
        self.start_h.set("%02d" % sh)
        self.start_m.set("%02d" % sm)
        self.end_h.set("%02d" % eh)
        self.end_m.set("%02d" % em)

    def save_times(self):
        start = format_hm(self.start_h.get(), self.start_m.get())
        end = format_hm(self.end_h.get(), self.end_m.get())
        if start == end:
            raise ValueError("开始和结束不能选同一个时间。")
        if load().get("enabled"):
            raise RuntimeError("每天自动黑屏还开着。先暂停，再改时间。")
        return update(start=start, end=end, mode="auto", keep_awake=True)

    def _picked_time(self, _event=None):
        self.safe(self._apply_picked_time)

    def _apply_picked_time(self):
        try:
            self.save_times()
        except Exception:
            self._show_saved_time(load())
            raise
        self._note("时间已经记下。到点会自动黑，到结束时间会自动亮。")

    def install_driver(self):
        # 只留给设置里的入口用，主界面不再引导用户手动装驱动。
        from .app import ensure_environment
        report = ensure_environment(force=True)
        if report.get("virtual_display"):
            messagebox.showinfo(APP, "已经修好了。先点「测试 15 秒」，看远程还能不能看到画面。", parent=self.root)
        else:
            messagebox.showinfo(APP, "远程屏幕这次没装上。程序会改用黑幕或关屏，这台电脑仍然能黑。", parent=self.root)

    def _note(self, text, seconds=4):
        """一句状态文字。不建进度条，也不改窗口透明度。"""
        self._notice = text
        self._notice_until = time.monotonic() + seconds

    def test(self):
        request = manual_test_request(load())
        result = self.act(request)
        if not (isinstance(result, str) and result.startswith("error:")):
            self._note("屏幕会黑 15 秒，然后自己亮回来。这段时间不用按键。")
        return result

    def night(self):
        keys = fmt(load().get("unlock_chord") or DEFAULT_CHORD)
        if not messagebox.askyesno(
                APP,
                "屏幕会马上变黑。\n要亮回来，同时按住 %s，大约半秒。\n\n确定现在就黑屏吗？" % keys,
                parent=self.root):
            return None
        return self.act({"action": "night", "mode": "auto"})

    def emergency(self):
        result = self.act("emergency")
        if isinstance(result, str) and result.startswith("error:"):
            raise RuntimeError(result[6:].strip())
        if result == "locked":
            raise RuntimeError(lock_hint())
        self._note("屏幕已经亮了，每天自动黑屏也暂停了。")
        self._last_state = "day"
        return result

    def toggle(self):
        from .app import activate, pause
        if load().get("enabled"):
            pause()
            self._note("每天自动黑屏已暂停。开机后程序仍会在后台运行。")
            return None
        cfg = self.save_times()
        activate("auto", cfg["start"], cfg["end"])
        self._note("已经启用。到了开始时间会自动黑，到了结束时间会自动亮。")
        return self.act("reconcile")

    def refresh(self, schedule=True):
        try:
            cfg = load()
            if cfg["state"] == "night":
                self._notice = ""
            elif self._last_state == "night":
                self._note("已恢复显示")
            self._last_state = cfg["state"]
            keys = fmt(cfg.get("unlock_chord") or DEFAULT_CHORD)
            _put(self.unlock_text, "亮屏：同时按住 " + keys)
            _put(self.state_text, "正在黑屏" if cfg["state"] == "night" else "屏幕正常显示")
            detail = "%s    %s 到 %s" % (
                "每天自动黑屏已开启" if cfg["enabled"] else "每天自动黑屏还没开",
                cfg["start"], cfg["end"])
            if cfg.get("extra_ranges"):
                detail += "，另外还有 %d 段" % len(cfg["extra_ranges"])
            _put(self.detail_text, detail)
            tested = cfg.get("last_test_mode")
            names = {
                "virtual_only": "你这边黑屏，远程还能用",
                "overlay": "你这边盖上黑幕，远程还能用",
                "screen_off": "关掉屏幕。动一下鼠标会亮",
            }
            if cfg["state"] == "night":
                mode = names.get(cfg.get("night_mode"), "按系统的方式处理屏幕")
            else:
                mode = "到点后：你这边黑，远程还能用" if IS_WIN else "到点后：按系统的方式处理屏幕"
            _put(self.mode_text, mode)
            if cfg.get("last_test_ok") and cfg.get("last_test_requested") == "auto" and tested in names and tested != "virtual_only":
                message = "测试好了。启用之后会是这样：" + names[tested]
            elif cfg.get("last_test_ok"):
                message = "测试好了。可以点「启用每天自动黑屏」。"
            elif cfg.get("test_id"):
                message = "正在测试。屏幕会自己亮回来，不用按键。"
            else:
                message = "还没测试过。点「测试 15 秒」，屏幕黑 15 秒，然后自己亮。"
            if cfg["state"] == "night" and cfg.get("test_id"):
                message = "正在测试。大约 15 秒后屏幕会自己亮回来，不用按键。"
            elif cfg["state"] == "night":
                if cfg.get("night_mode") == "screen_off":
                    message = "现在是关屏。碰键盘或鼠标会亮。要一直黑着，同时按住 %s，大约半秒。" % keys
                elif cfg.get("lock", True):
                    message = "你这边是黑的。同时按住 %s，大约半秒，屏幕会亮。暂停不会把屏幕点亮。" % keys
                else:
                    message = "你这边是黑的。暂停不会点亮屏幕，请点「恢复屏幕并暂停」。"
            elif self._notice and time.monotonic() < self._notice_until:
                message = self._notice
            _put(self.test_text, message)
            _put(self.enable, "暂停每天自动黑屏" if cfg["enabled"] else "启用每天自动黑屏")
            _put(self.env_text, env_status())
            pref = cfg.get("autostart_preference", cfg.get("autostart", True))
            _put(self.boot_text, "开机后会自动在后台运行，不用自己再打开。" if pref else "开机后不会自动运行。要改的话，打开「设置」。")
            _put(self.time_hint, "每天自动黑屏开着。要改时间，先点「暂停每天自动黑屏」。" if cfg.get("enabled")
                 else "点开选择，不用打字。选完会自己记下。测试 15 秒不会改这个时间。")
            clock_state = "disabled" if cfg.get("enabled") else "readonly"
            for box in self._clocks:
                if str(box.cget("state")) != clock_state:
                    box.configure(state=clock_state)
            if cfg.get("enabled"):
                self._show_saved_time(cfg)
        except Exception as e:
            log("界面状态刷新失败：%s" % e)
        finally:
            if schedule:
                try:
                    if self.root.winfo_exists():
                        self.root.after(1000, self.refresh)
                except tk.TclError:
                    pass

    def logs(self):
        window = tk.Toplevel(self.root)
        window.title("运行日志")
        window.transient(self.root)
        bar = ttk.Frame(window, padding=(8, 8, 8, 0))
        bar.pack(fill="x")
        self._export_button(bar, lambda: self.export_saved_logs(window)).pack(side="left")
        ttk.Label(bar, text="下面是最近的记录。导出会把保存下来的日志全部存成一个文件。").pack(side="left", padx=(8, 0))
        text = tk.Text(window, width=88, height=24, wrap="word")
        text.pack(fill="both", expand=True)
        text.insert("end", read_log_tail() or "还没有日志。用过一会儿再来看。")
        text.see("end")
        text.configure(state="disabled")

    def export_saved_logs(self, parent=None):
        parent = parent or self.root
        if load().get("state") == "night":
            raise RuntimeError("屏幕黑着的时候不弹出保存窗口。等屏幕亮了再导出日志。")
        dest = filedialog.asksaveasfilename(
            parent=parent,
            title="把日志存成文件",
            defaultextension=".txt",
            initialfile="NightDisplay-logs.txt",
            filetypes=[("文本", "*.txt")])
        if not dest:
            return None
        count = export_logs(dest)
        if count:
            messagebox.showinfo(APP, "已把 %d 天的日志存到：\n%s" % (count, dest), parent=parent)
        else:
            messagebox.showinfo(APP, "现在还没有日志。用过一段时间后再导出。", parent=parent)
        return dest

    def settings(self):
        from . import chord
        window = tk.Toplevel(self.root)
        window.title("设置")
        window.transient(self.root)
        body = ttk.Frame(window, padding=11)
        body.pack(fill="both", expand=True)
        cfg = load()

        extra_box = ttk.LabelFrame(body, text="其他时段", padding=8)
        extra_box.pack(fill="x")
        ttk.Label(extra_box, text="除了主窗口上的每天时段，还可以再加几段，比如中午。点开选，不用打字。不需要就不用加。",
                  wraplength=460).pack(anchor="w")
        rows_host = ttk.Frame(extra_box)
        rows_host.pack(fill="x", pady=(6, 0))
        extra_rows = []

        def fit():
            window.resizable(True, True)
            window.update_idletasks()
            window.geometry("%dx%d" % (window.winfo_reqwidth(), window.winfo_reqheight()))
            window.resizable(False, False)

        def add_row(start="12:00", end="13:30", from_user=False):
            if from_user and len(extra_rows) >= 4:
                raise RuntimeError("最多再加 4 段。不需要的段点「去掉」。")
            sh, sm = hm_parts(start, (12, 0))
            eh, em = hm_parts(end, (13, 30))
            line = ttk.Frame(rows_host)
            line.pack(anchor="w", pady=(0, 4))
            picked = {
                "sh": tk.StringVar(value="%02d" % sh),
                "sm": tk.StringVar(value="%02d" % sm),
                "eh": tk.StringVar(value="%02d" % eh),
                "em": tk.StringVar(value="%02d" % em),
            }
            pack_clock(line, picked["sh"], picked["sm"])
            ttk.Label(line, text="到").pack(side="left", padx=6)
            pack_clock(line, picked["eh"], picked["em"])
            item = {"frame": line, "picked": picked}

            def remove(current=item):
                current["frame"].destroy()
                extra_rows.remove(current)
                add_btn.state(["!disabled"])
                fit()

            ttk.Button(line, text="去掉", command=remove).pack(side="left", padx=(8, 0))
            extra_rows.append(item)
            if len(extra_rows) >= 4:
                add_btn.state(["disabled"])
            fit()

        add_btn = ttk.Button(extra_box, text="再加一段", command=lambda: self.safe(lambda: add_row(from_user=True)))
        add_btn.pack(anchor="w", pady=(2, 0))
        for pair in cfg.get("extra_ranges") or []:
            if isinstance(pair, (list, tuple)) and len(pair) == 2:
                add_row(pair[0], pair[1])

        chord_box = ttk.LabelFrame(body, text="怎么把屏幕弄亮", padding=8)
        chord_box.pack(fill="x", pady=(8, 0))
        ttk.Label(chord_box, text="屏幕黑了以后，在这台电脑上同时按住下面这两个键，大约半秒，屏幕会亮。默认是 C 和 K，一般不用改。",
                  wraplength=460).pack(anchor="w")
        row = ttk.Frame(chord_box)
        row.pack(anchor="w", pady=(4, 0))
        chord_var = tk.StringVar(value=chord.fmt(cfg.get("unlock_chord") or chord.DEFAULT_CHORD))
        ttk.Entry(row, textvariable=chord_var, width=24, state="readonly").pack(side="left", padx=(0, 6))
        held, recording = [], {"on": False}

        def record():
            held.clear()
            recording["on"] = True
            chord_var.set("请同时按下两个键")
            window.bind("<KeyPress>", down)
            window.bind("<KeyRelease>", up)
            window.focus_set()

        def stop_recording():
            recording["on"] = False
            window.unbind("<KeyPress>")
            window.unbind("<KeyRelease>")

        def down(event):
            if recording["on"]:
                try:
                    key = chord.normalize(event.keysym)
                    if key not in held and len(held) < 3:
                        held.append(key)
                    chord_var.set(chord.fmt(held))
                except ValueError:
                    pass
                return "break"

        def up(event):
            if recording["on"]:
                stop_recording()
                if len(held) < 2:
                    chord_var.set(chord.fmt(cfg.get("unlock_chord") or chord.DEFAULT_CHORD))
                    messagebox.showinfo(APP, "按得太少了。请同时按住两个键，比如 C 和 K。", parent=window)
                else:
                    chord_var.set(chord.fmt(held))
                return "break"

        ttk.Button(row, text="改按键", command=record).pack(side="left")

        opts = ttk.Frame(body)
        opts.pack(anchor="w", pady=(8, 0))
        locked = tk.BooleanVar(value=cfg.get("lock", True))
        autostart = tk.BooleanVar(value=cfg.get("autostart_preference", cfg.get("autostart", True)))
        ttk.Checkbutton(opts, text="黑屏时，这台电脑只能用上面的按键亮回来", variable=locked).pack(anchor="w")
        ttk.Checkbutton(opts, text="开机后自动在后台运行（建议开着。关掉的话，每次开机都要自己去桌面打开）",
                        variable=autostart).pack(anchor="w", pady=(4, 0))

        def save():
            blocked = settings_block_message(load())
            if blocked:
                raise RuntimeError(blocked)
            if recording["on"] or "请同时" in chord_var.get():
                stop_recording()
                raise RuntimeError("按键还没设好。请点「改按键」，同时按住两个键，比如 C 和 K。")
            keys = chord.validate([k.strip() for k in re.split(r"[+＋]", chord_var.get())])
            rs = []
            for item in extra_rows:
                picked = item["picked"]
                start = format_hm(picked["sh"].get(), picked["sm"].get())
                end = format_hm(picked["eh"].get(), picked["em"].get())
                if start == end:
                    raise ValueError("每一段的开始和结束不能选同一个时间。")
                rs.append([start, end])
            B.set_autostart(autostart.get())
            update(extra_ranges=rs, unlock_chord=keys, lock=locked.get(), autostart=autostart.get(),
                   autostart_preference=autostart.get())
            window.destroy()

        actions = ttk.Frame(body)
        actions.pack(anchor="w", pady=(8, 0))
        ttk.Button(actions, text="保存设置", command=lambda: self.safe(save)).pack(side="left", padx=(0, 6))
        ttk.Button(actions, text="检查更新", command=self.check_update).pack(side="left", padx=(0, 6))
        ttk.Button(actions, text="取消每天自动黑屏，并取消开机运行", command=lambda: self.safe(self.remove)).pack(side="left")
        if IS_WIN:
            ttk.Button(body, text="远程看不到画面时，再修一次（平时不用点）",
                       command=lambda: self.safe(self.install_driver)).pack(anchor="w", pady=(6, 0))
        ttk.Label(body, text=AUTHOR + "    " + SPONSOR).pack(anchor="w", pady=(8, 0))
        fit()

    def check_update(self, quiet=False):
        if getattr(self, "_checking_update", False):
            return
        self._checking_update = True
        results = queue.Queue()

        def work():
            from . import updates
            try:
                results.put((True, updates.check()))
            except Exception as e:
                results.put((False, str(e)))

        threading.Thread(target=work, daemon=True).start()

        def poll():
            try:
                ok, result = results.get_nowait()
            except queue.Empty:
                self.root.after(100, poll)
                return
            self._checking_update = False
            if not ok:
                log("检查更新失败：%s" % result)
                if not quiet:
                    messagebox.showerror(APP, result, parent=self.root)
            elif result["newer"]:
                if messagebox.askyesno(APP, "发现新版本 %s，打开下载页？" % result["version"], parent=self.root):
                    webbrowser.open(result["url"])
            elif not quiet:
                messagebox.showinfo(APP, "当前已是最新正式版本。", parent=self.root)

        self.root.after(100, poll)

    def remove(self):
        from .app import uninstall
        if load().get("state") == "night" and load().get("lock", True):
            raise RuntimeError(lock_hint())
        uninstall(self.overlay)
        self.root.destroy()

    def on_close(self):
        cfg = load()
        if cfg.get("state") == "night" and cfg.get("lock", True) and not self.has_tray:
            messagebox.showwarning(APP, lock_hint(cfg), parent=self.root)
            return
        if self.has_tray:
            self.root.withdraw()
        else:
            self.root.destroy()


def start_tray(root, act):
    try:
        import pystray
        from PIL import Image, ImageDraw
    except Exception:
        return None
    if not IS_WIN and not IS_MAC and pystray.Icon.__module__.endswith("._xorg"):
        try:
            from Xlib.display import Display
            display = Display()
            try:
                atom = display.intern_atom("_NET_SYSTEM_TRAY_S%d" % display.get_default_screen())
                if not display.get_selection_owner(atom):
                    return None
            finally:
                display.close()
        except Exception:
            return None
    img = Image.new("RGBA", (64, 64), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)
    draw.ellipse((6, 6, 58, 58), fill=(32, 32, 32, 255))
    menu = pystray.Menu(
        pystray.MenuItem("打开 NightDisplay", lambda: act("show"), default=True),
        pystray.MenuItem("现在就黑屏", lambda: act("night")),
        pystray.MenuItem("现在恢复屏幕", lambda: act("day")),
        pystray.MenuItem("恢复屏幕并暂停", lambda: act("emergency")),
        pystray.Menu.SEPARATOR,
        pystray.MenuItem("退出", lambda: act("quit")))
    extra = {}
    if IS_MAC:
        from AppKit import NSApplication
        extra["darwin_nsapplication"] = NSApplication.sharedApplication()
    icon = pystray.Icon("NightDisplay", img, "NightDisplay", menu, **extra)
    ready, cancelled = threading.Event(), threading.Event()

    def setup(icon):
        # A custom setup is required so start_tray can tell whether the icon
        # actually became visible instead of hiding the only window forever.
        if cancelled.is_set():
            with contextlib.suppress(Exception):
                icon.stop()
            return
        try:
            icon.visible = True
        finally:
            ready.set()

    try:
        if IS_MAC:
            icon.run_detached(setup=setup)
        else:
            threading.Thread(target=icon.run, kwargs={"setup": setup}, daemon=True).start()
        if not ready.wait(2):
            cancelled.set()
            with contextlib.suppress(Exception):
                icon.stop()
            log("托盘未在 2 秒内就绪，保留主窗口")
            return None
    except Exception as e:
        cancelled.set()
        log("托盘不可用，保留主窗口：%s" % e)
        return None
    return icon
