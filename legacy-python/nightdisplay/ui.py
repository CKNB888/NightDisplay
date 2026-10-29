"""设置窗口用系统控件，不自绘。

Windows 上 Tk 的 vista 主题走 comctl6 / uxtheme，按钮和输入框由系统绘制。
Mac 上用 aqua：系统字体、系统次级文字色、主按钮靠右并交给系统画成蓝色。
不自绘红绿灯、毛玻璃或圆角卡片。那些东西盖住系统控件，也不是 Mac 的界面。
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

SUBTITLE = "到点这台电脑黑屏，远程还能用。"


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
        family, size = "Microsoft YaHei UI", 12
    elif IS_MAC:
        family, size = "PingFang SC", 13
    else:
        family, size = "Noto Sans CJK SC", 11
    for name in ("TkDefaultFont", "TkTextFont", "TkHeadingFont", "TkMenuFont"):
        with contextlib.suppress(tk.TclError):
            tkfont.nametofont(name).configure(family=family, size=size)
    # aqua 的按钮高度和圆角由系统定。再写 padding 会把系统按钮撑变形。
    # Windows 的下拉箭头很小，只在那边加大可点区域。
    if not IS_MAC:
        with contextlib.suppress(tk.TclError):
            style.configure("TButton", padding=(10, 5))
            style.configure("TMenubutton", padding=(10, 4))
    # 次级说明用独立样式。Mac 才改成系统灰字；Windows 保持 vista 默认字色。
    with contextlib.suppress(tk.TclError):
        style.configure("Hint.TLabel")
    if IS_MAC:
        with contextlib.suppress(tk.TclError):
            style.configure("Hint.TLabel", foreground="systemSecondaryLabelColor")
        _mac_window(root)
    return style


def _mac_window(window):
    """Mac 窗口底色用系统窗口色。深色模式会跟着变。其他系统不改。"""
    if not IS_MAC:
        return
    with contextlib.suppress(tk.TclError):
        window.configure(background="systemWindowBackgroundColor")


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


def _time_menu(parent, variable, values, on_pick):
    """整颗按钮都能点开。窄下拉框的箭头点不中，计划开着时还会被禁用。"""
    button = ttk.Menubutton(parent, text=variable.get() or values[0], width=4)

    def choose(value):
        variable.set(value)
        button.configure(text=value)
        if on_pick:
            on_pick()

    menu = tk.Menu(button, tearoff=0)
    for value in values:
        menu.add_command(label=value, command=lambda picked=value: choose(picked))
    button["menu"] = menu

    def show(*_args):
        button.configure(text=variable.get())

    variable.trace_add("write", show)
    return button


def pack_clock(parent, hour_var, minute_var, on_pick=None):
    """小时和分钟点按钮选出。不能打字，所以不会再出现时间格式错误。"""
    hour = _time_menu(parent, hour_var, HOURS, on_pick)
    hour.pack(side="left")
    ttk.Label(parent, text="点").pack(side="left", padx=(8, 10))
    minute = _time_menu(parent, minute_var, MINUTES, on_pick)
    minute.pack(side="left")
    ttk.Label(parent, text="分").pack(side="left", padx=(8, 0))
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
    def __init__(self, root, overlay, act, on_user_hide=None):
        self.root, self.overlay, self.act = root, overlay, act
        self._on_user_hide = on_user_hide
        self._hide_noted = False
        self.has_tray = False
        use_native_theme(root)
        root.title("NightDisplay")
        root.protocol("WM_DELETE_WINDOW", self.on_close)
        if IS_WIN:
            family, body = "Microsoft YaHei UI", 12
        elif IS_MAC:
            family, body = "PingFang SC", 13
        else:
            family, body = "Noto Sans CJK SC", 11
        # 分区标题用系统字体加粗。状态只比正文大一号，不再做空标题。
        self._section_font = tkfont.Font(root, family=family, size=body, weight="bold")
        status_font = tkfont.Font(root, family=family, size=body + 3)

        outer = ttk.Frame(root, padding=(20, 18, 20, 16))
        outer.pack(fill="both", expand=True)

        head = ttk.Frame(outer)
        head.pack(fill="x")
        ttk.Label(head, text="NightDisplay", font=self._section_font).pack(side="left")
        ttk.Label(head, text="v" + VERSION, style="Hint.TLabel").pack(side="right")
        ttk.Label(outer, text=SUBTITLE, style="Hint.TLabel").pack(anchor="w", pady=(4, 0))

        self.state_text = ttk.Label(outer, text="屏幕正常显示", font=status_font)
        self.state_text.pack(anchor="w", pady=(16, 0))
        self.detail_text = ttk.Label(outer, text="", style="Hint.TLabel")
        self.detail_text.pack(anchor="w", pady=(4, 0))
        self.hint_text = ttk.Label(outer, text="", style="Hint.TLabel", wraplength=420)
        self.hint_text.pack(anchor="w", pady=(2, 0))

        # 开始、结束各占一行。挤在一行里，时间按钮会贴在一起，点不中。
        section = ttk.Frame(outer)
        section.pack(fill="x", pady=(18, 0))
        ttk.Label(section, text="每天", font=self._section_font).pack(anchor="w", pady=(0, 8))
        cfg = load()
        sh, sm = hm_parts(cfg.get("start"), (20, 0))
        eh, em = hm_parts(cfg.get("end"), (10, 30))
        self.start_h = tk.StringVar(value="%02d" % sh)
        self.start_m = tk.StringVar(value="%02d" % sm)
        self.end_h = tk.StringVar(value="%02d" % eh)
        self.end_m = tk.StringVar(value="%02d" % em)
        start_row = ttk.Frame(section)
        start_row.pack(anchor="w", pady=(0, 8))
        ttk.Label(start_row, text="开始", width=6, anchor="w").pack(side="left")
        pack_clock(start_row, self.start_h, self.start_m, self._picked_time)
        end_row = ttk.Frame(section)
        end_row.pack(anchor="w")
        ttk.Label(end_row, text="结束", width=6, anchor="w").pack(side="left")
        pack_clock(end_row, self.end_h, self.end_m, self._picked_time)
        self.unlock_text = ttk.Label(section, text="亮回来：同时按住 C 和 K", style="Hint.TLabel")
        self.unlock_text.pack(anchor="w", pady=(10, 0))

        # 主操作靠右，和 Mac 系统设置一样。测试留在左边，避免和启用贴在一起误点。
        plan = ttk.Frame(outer)
        plan.pack(fill="x", pady=(18, 0))
        self._button(plan, "测试 15 秒", self.test).pack(side="left")
        self.enable = self._button(plan, "启用每天自动黑屏", self.toggle, default="active")
        self.enable.pack(side="right")

        now = ttk.Frame(outer)
        now.pack(anchor="w", pady=(8, 0))
        self._button(now, "现在就黑屏", self.night).pack(side="left", padx=(0, 8))
        self._button(now, "恢复屏幕并暂停", self.emergency).pack(side="left")

        tools = ttk.Frame(outer)
        tools.pack(fill="x", pady=(16, 0))
        self._button(tools, "运行日志", self.logs).pack(side="left", padx=(0, 8))
        self._export_button(tools, self.export_saved_logs).pack(side="left", padx=(0, 8))
        self._button(tools, "设置", self.settings).pack(side="left")
        ttk.Label(tools, text="传康Kk  ·  1837620622", style="Hint.TLabel").pack(side="right")

        self._last_state = None
        self._notice = ""
        self._notice_until = 0.0
        self.refresh(schedule=False)
        _lock_size(root)
        self.root.after(1000, self.refresh)
        self.root.after(3000, lambda: self.check_update(quiet=True))

    def _button(self, parent, text, command, default=None):
        options = {"text": text, "command": lambda: self.safe(command)}
        # default=active 在 aqua 上是系统蓝色主按钮，在 vista 上是默认按钮。不另涂颜色。
        if default:
            options["default"] = default
        return ttk.Button(parent, **options)

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
        from .app import retimed
        start = format_hm(self.start_h.get(), self.start_m.get())
        end = format_hm(self.end_h.get(), self.end_m.get())
        if start == end:
            raise ValueError("开始和结束不能选同一个时间。")
        return retimed(start, end)

    def _picked_time(self, _event=None):
        self.safe(self._apply_picked_time)

    def _apply_picked_time(self):
        try:
            self.save_times()
        except Exception:
            self._show_saved_time(load())
            raise
        if load().get("enabled"):
            self._note("时间已改。每天自动黑屏会按这个时间走。")
        else:
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
            _put(self.unlock_text, "亮回来：同时按住 " + keys)
            _put(self.state_text, "正在黑屏" if cfg["state"] == "night" else "屏幕正常显示")
            span = "%s–%s" % (cfg["start"], cfg["end"])
            if cfg.get("extra_ranges"):
                span += "，另有 %d 段" % len(cfg["extra_ranges"])
            plan = "每天自动黑屏已开" if cfg["enabled"] else "每天自动黑屏还没开"
            _put(self.detail_text, "%s  ·  %s" % (span, plan))
            names = {
                "virtual_only": "你这边黑屏，远程还能用",
                "overlay": "你这边盖上黑幕，远程还能用",
                "screen_off": "关掉屏幕。动一下鼠标会亮",
            }
            tested = cfg.get("last_test_mode")
            if cfg["state"] == "night" and cfg.get("test_id"):
                message = "正在测试。大约 15 秒后自己亮回来。"
            elif cfg["state"] == "night":
                if cfg.get("night_mode") == "screen_off":
                    message = "现在是关屏，碰一下会亮。要亮着不动，同时按住 %s。" % keys
                elif cfg.get("lock", True):
                    message = "你这边是黑的。同时按住 %s 才会亮。" % keys
                else:
                    message = "你这边是黑的。要亮回来，点「恢复屏幕并暂停」。"
            elif self._notice and time.monotonic() < self._notice_until:
                message = self._notice
            elif cfg.get("last_test_ok") and cfg.get("last_test_requested") == "auto" and tested in names and tested != "virtual_only":
                message = "测试好了。启用之后：" + names[tested]
            elif cfg.get("last_test_ok"):
                message = "测试好了。可以启用每天自动黑屏。"
            elif cfg.get("test_id"):
                message = "正在测试。屏幕会自己亮回来。"
            else:
                message = "先点「测试 15 秒」。黑 15 秒后自己亮回来。"
            _put(self.hint_text, message)
            _put(self.enable, "暂停每天自动黑屏" if cfg["enabled"] else "启用每天自动黑屏")
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
        _mac_window(window)
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
        _mac_window(window)
        body = ttk.Frame(window, padding=(20, 16, 20, 16))
        body.pack(fill="both", expand=True)
        cfg = load()

        extra_box = ttk.Frame(body)
        extra_box.pack(fill="x")
        ttk.Label(extra_box, text="其他时段", font=self._section_font).pack(anchor="w")
        ttk.Label(
            extra_box,
            text="主窗口那段之外，还可以再加，比如中午。点时间选出。",
            style="Hint.TLabel",
        ).pack(anchor="w", pady=(4, 8))
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

        chord_box = ttk.Frame(body)
        chord_box.pack(fill="x", pady=(18, 0))
        ttk.Label(chord_box, text="怎么把屏幕弄亮", font=self._section_font).pack(anchor="w")
        ttk.Label(
            chord_box,
            text="黑屏后同时按住这两个键，大约半秒。默认 C 和 K。",
            style="Hint.TLabel",
        ).pack(anchor="w", pady=(4, 8))
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
        opts.pack(anchor="w", pady=(16, 0))
        locked = tk.BooleanVar(value=cfg.get("lock", True))
        autostart = tk.BooleanVar(value=cfg.get("autostart_preference", cfg.get("autostart", True)))
        ttk.Checkbutton(opts, text="黑屏时，只有上面的按键能亮回来", variable=locked).pack(anchor="w")
        ttk.Checkbutton(opts, text="开机登录后自动运行", variable=autostart).pack(anchor="w", pady=(6, 0))

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
        actions.pack(fill="x", pady=(18, 0))
        ttk.Button(actions, text="检查更新", command=self.check_update).pack(side="left")
        ttk.Button(actions, text="保存设置", default="active",
                   command=lambda: self.safe(save)).pack(side="right")
        ttk.Button(body, text="取消每天自动黑屏，并取消开机运行",
                   command=lambda: self.safe(self.remove)).pack(anchor="w", pady=(12, 0))
        if IS_WIN:
            ttk.Button(body, text="远程看不到画面时，再修一次",
                       command=lambda: self.safe(self.install_driver)).pack(anchor="w", pady=(8, 0))
        ttk.Label(body, text=AUTHOR, style="Hint.TLabel").pack(anchor="w", pady=(16, 0))
        ttk.Label(body, text=SPONSOR, style="Hint.TLabel").pack(anchor="w")
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
        # 右上角关闭只藏窗口。进程继续按设定时间黑屏，开机登录项也不动。
        # 真正退出在右下角图标的「退出 NightDisplay」。退出才会把屏幕亮回来。
        try:
            self.root.withdraw()
        except tk.TclError:
            return
        if not self._hide_noted:
            self._hide_noted = True
            log("窗口已隐藏到右下角。每天的时间和开机登录都还在。")
            if self._on_user_hide:
                self._on_user_hide()


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
    # 浅色圆底，深色任务栏上也看得见。中间一块黑，表示屏幕是黑的。
    draw.ellipse((4, 4, 60, 60), fill=(236, 236, 236, 255))
    draw.rectangle((18, 16, 46, 40), fill=(20, 20, 20, 255))
    draw.rectangle((30, 40, 34, 48), fill=(20, 20, 20, 255))
    menu = pystray.Menu(
        pystray.MenuItem("打开 NightDisplay", lambda: act("show"), default=True),
        pystray.MenuItem("现在就黑屏", lambda: act("night")),
        pystray.MenuItem("现在恢复屏幕", lambda: act("day")),
        pystray.MenuItem("恢复屏幕并暂停", lambda: act("emergency")),
        pystray.Menu.SEPARATOR,
        pystray.MenuItem("退出 NightDisplay", lambda: act("quit")))
    extra = {}
    if IS_MAC:
        from AppKit import NSApplication
        extra["darwin_nsapplication"] = NSApplication.sharedApplication()
    icon = pystray.Icon("NightDisplay", img, "NightDisplay 正在后台运行，到点会黑屏", menu, **extra)
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
