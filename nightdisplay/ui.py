"""A compact frosted-glass style panel; one default Windows display policy."""
import contextlib
import queue
import re
import threading
import tkinter as tk
import webbrowser
from tkinter import messagebox

from PIL import Image, ImageDraw, ImageFilter, ImageTk

from . import APP, AUTHOR, SPONSOR, VERSION
from . import backends as B
from .core import (
    IS_MAC,
    IS_WIN,
    load,
    log,
    parse_hm,
    parse_ranges,
    read_log_tail,
    update,
)

FONT = "Microsoft YaHei UI" if IS_WIN else ("PingFang SC" if IS_MAC else "Noto Sans CJK SC")
TEXT, MUTED, BLUE = "#24324A", "#64748B", "#4169E1"


def glass_background(width=760, height=560):
    image = Image.new("RGB", (width, height), "#E8EDF8")
    glow = Image.new("RGBA", image.size)
    d = ImageDraw.Draw(glow)
    d.ellipse((-190, -210, 540, 370), fill=(171, 186, 255, 215))
    d.ellipse((420, 160, 1010, 760), fill=(172, 217, 234, 220))
    d.ellipse((350, -190, 1040, 200), fill=(227, 190, 235, 190))
    image = Image.alpha_composite(image.convert("RGBA"), glow.filter(ImageFilter.GaussianBlur(90)))
    shadow = Image.new("RGBA", image.size)
    sd = ImageDraw.Draw(shadow)
    for box in ((26, 106, 734, 282), (26, 302, 734, 416)):
        sd.rounded_rectangle(tuple(v + (6 if i % 2 else 0) for i, v in enumerate(box)), radius=24, fill=(46, 67, 113, 25))
    image = Image.alpha_composite(image, shadow.filter(ImageFilter.GaussianBlur(14)))
    panels = Image.new("RGBA", image.size)
    pd = ImageDraw.Draw(panels)
    for box in ((26, 106, 734, 282), (26, 302, 734, 416)):
        pd.rounded_rectangle(box, radius=24, fill=(255, 255, 255, 174), outline=(255, 255, 255, 235), width=1)
    return Image.alpha_composite(image, panels).convert("RGB")


class Button(tk.Canvas):
    def __init__(self, parent, text, command, width=142, primary=False, bg="#ECF0F8"):
        super().__init__(parent, width=width, height=42, bg=bg, highlightthickness=0, cursor="hand2", takefocus=True)
        self.command, self.primary, self.width = command, primary, width
        self.text = text
        self.bind("<Button-1>", lambda event: command())
        self.bind("<Return>", lambda event: command())
        self.bind("<space>", lambda event: command())
        self.bind("<Enter>", lambda event: self.draw(True))
        self.bind("<Leave>", lambda event: self.draw())
        self.draw()

    def draw(self, hover=False):
        self.delete("all")
        fill = ("#3558CA" if hover else BLUE) if self.primary else ("#DEE6F5" if hover else "#F7F9FD")
        self.create_polygon(13, 1, self.width-13, 1, self.width-1, 1, self.width-1, 13,
                            self.width-1, 29, self.width-1, 41, self.width-13, 41, 13, 41,
                            1, 41, 1, 29, 1, 13, 1, 1, smooth=True, fill=fill, outline="")
        self.create_text(self.width/2, 21, text=self.text, fill="white" if self.primary else TEXT, font=(FONT, 10, "bold"))

    def set_text(self, text):
        self.text = text; self.draw()


class MainWindow:
    def __init__(self, root, overlay, act):
        self.root, self.overlay, self.act = root, overlay, act
        self.has_tray = False
        root.title("NightDisplay")
        root.geometry("760x560"); root.resizable(False, False)
        root.protocol("WM_DELETE_WINDOW", self.on_close)
        self.canvas = c = tk.Canvas(root, width=760, height=560, highlightthickness=0)
        c.pack(fill="both", expand=True)
        self.background = ImageTk.PhotoImage(glass_background(), master=root)
        c.create_image(0, 0, image=self.background, anchor="nw")
        self.text(28, 29, "NightDisplay", 20, bold=True)
        self.text(30, 65, "本地安静黑屏，远程照常使用。", 11, MUTED)
        self.text(732, 35, "v" + VERSION, 9, MUTED, anchor="ne")
        self.text(50, 126, "每日显示计划", 10, MUTED)
        self.state_text = self.text(50, 156, "屏幕正常显示", 22, bold=True)
        self.detail_text = self.text(52, 204, "", 10, MUTED)
        self.test_text = self.text(52, 242, "", 10, MUTED)
        self.text(560, 131, "默认方案", 9, MUTED)
        self.mode_text = self.text(560, 157, "虚拟屏优先" if IS_WIN else "系统显示控制", 13, bold=True)
        self.text(560, 190, "自动保持唤醒", 10, MUTED)
        self.text(560, 215, "到点恢复屏幕", 10, MUTED)
        self.text(50, 320, "设置时间", 11, bold=True)
        cfg = load()
        self.start = tk.StringVar(value=cfg["start"])
        self.end = tk.StringVar(value=cfg["end"])
        self.text(51, 368, "开始黑屏", 10, MUTED)
        self.text(306, 368, "恢复显示", 10, MUTED)
        for x, var in ((146, self.start), (402, self.end)):
            frame = tk.Frame(c, bg="#F9FBFF", padx=10, pady=6, highlightthickness=1, highlightbackground="#E1E7F2")
            tk.Entry(frame, textvariable=var, width=7, font=(FONT, 13), bd=0, relief="flat", bg="#F9FBFF", fg=TEXT,
                     justify="center", insertbackground=TEXT).pack()
            c.create_window(x, 355, window=frame, anchor="nw")
        self.button("保存时间", self.save_times, 560, 355, width=148, bg="#ECF2F8")
        self.button("测试 15 秒", self.test, 26, 438, width=146)
        self.enable = self.button("启用每日计划", self.toggle, 184, 438, width=172, primary=True)
        self.button("立即黑屏", lambda: self.act({"action": "night", "mode": "auto"}), 368, 438, width=146)
        self.button("恢复并暂停", lambda: self.act("emergency"), 526, 438, width=208)
        if IS_WIN:
            self.driver = self.button("安装内置驱动", self.install_driver, 26, 498, width=146, bg="#E5EAF6")
        self.button("运行日志", self.logs, 184, 498, width=112, bg="#E5EAF6")
        self.button("设置", self.settings, 308, 498, width=112, bg="#E5EAF6")
        self.text(732, 501, "传康Kk  ·  赞助联系 1837620622", 9, MUTED, anchor="ne")
        self.unlock_text = self.text(732, 522, "解锁：C + K", 9, MUTED, anchor="ne")
        self.refresh()
        self._fade()
        self.root.after(3000, lambda: self.check_update(quiet=True))

    def _fade(self):
        try:
            self.root.attributes("-alpha", 0.4)
            def step(value=0.4):
                value = min(1, value + 0.15)
                self.root.attributes("-alpha", value)
                if value < 1:
                    self.root.after(30, lambda: step(value))
            step()
        except tk.TclError:
            pass

    def text(self, x, y, text, size=11, color=TEXT, bold=False, **kwargs):
        return self.canvas.create_text(x, y, text=text, fill=color, anchor=kwargs.pop("anchor", "nw"),
                                       font=(FONT, size, "bold" if bold else "normal"), **kwargs)

    def button(self, text, command, x, y, **kwargs):
        button = Button(self.canvas, text, lambda: self.safe(command), **kwargs)
        self.canvas.create_window(x, y, window=button, anchor="nw")
        return button

    def safe(self, fn):
        try:
            result = fn()
            if isinstance(result, str) and result.startswith("error:"):
                raise RuntimeError(result[6:].strip())
            if result == "locked":
                raise RuntimeError("请按住 C+K，或使用“恢复并暂停”")
            if self.root.winfo_exists():
                self.refresh(schedule=False)
            return result
        except Exception as e:
            log("界面操作失败：%s" % e)
            messagebox.showerror(APP, str(e), parent=self.root)

    def save_times(self):
        start, end = ["%02d:%02d" % parse_hm(v.get()) for v in (self.start, self.end)]
        if start == end:
            raise ValueError("开始和结束时间不能相同")
        if load().get("enabled"):
            raise RuntimeError("请先暂停计划，再修改时间")
        return update(start=start, end=end, mode="auto", keep_awake=True)

    def install_driver(self):
        from .app import install_vdd
        if install_vdd():
            raise RuntimeError("驱动安装未完成，请查看日志")
        if not B.has_virtual_display():
            raise RuntimeError("Windows 尚未提供虚拟屏，请查看安装日志，必要时重启后重试。")
        messagebox.showinfo(APP, "虚拟屏已就绪，可以测试。", parent=self.root)

    def test(self):
        self.save_times()
        return self.act({"action": "test", "mode": "auto", "seconds": 15})

    def toggle(self):
        from .app import activate, pause
        if load().get("enabled"):
            pause()
        else:
            cfg = self.save_times()
            activate("auto", cfg["start"], cfg["end"])
            return self.act("reconcile")

    def refresh(self, schedule=True):
        try:
            cfg = load()
            from .chord import fmt
            self.canvas.itemconfigure(self.unlock_text, text="解锁：" + fmt(cfg.get("unlock_chord") or ["C", "K"]))
            self.canvas.itemconfigure(self.state_text, text="夜间运行中" if cfg["state"] == "night" else "屏幕正常显示")
            self.canvas.itemconfigure(self.detail_text, text=("%s    %s — %s" % (
                "每日计划已开启" if cfg["enabled"] else "每日计划已暂停", cfg["start"], cfg["end"])) +
                (" · 另有 %d 段" % len(cfg["extra_ranges"]) if cfg.get("extra_ranges") else ""))
            message = "测试已恢复，可启用计划" if cfg.get("last_test_ok") else \
                      "测试进行中，稍后自动恢复" if cfg.get("test_id") else "首次使用请先测试，并确认远程画面正常"
            if cfg["state"] == "night":
                self.canvas.itemconfigure(self.mode_text, text={"virtual_only": "仅虚拟屏", "overlay": "防窥黑幕", "screen_off": "基础关屏"}.get(cfg.get("night_mode"), "系统显示控制"))
                if cfg.get("night_mode") == "screen_off":
                    message = "当前为基础关屏，键鼠输入会唤醒屏幕"
            self.canvas.itemconfigure(self.test_text, text=message)
            self.enable.set_text("暂停计划" if cfg["enabled"] else "启用每日计划")
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
        window = tk.Toplevel(self.root); window.title("运行日志")
        text = tk.Text(window, width=95, height=25, wrap="word", font=(FONT, 10))
        text.pack(fill="both", expand=True)
        text.insert("end", read_log_tail()); text.see("end"); text.configure(state="disabled")

    def settings(self):
        from . import chord
        window = tk.Toplevel(self.root); window.title("设置与关于")
        window.geometry("610x430"); window.transient(self.root)
        body = tk.Frame(window, padx=24, pady=18); body.pack(fill="both", expand=True)
        cfg = load()
        def label(text):
            tk.Label(body, text=text, font=(FONT, 10), anchor="w").pack(anchor="w", pady=(8, 4))
        label("额外时间段（多个用逗号分隔）")
        extra = tk.StringVar(value=", ".join("%s-%s" % tuple(r) for r in cfg.get("extra_ranges", [])))
        tk.Entry(body, textvariable=extra, font=(FONT, 11), width=49).pack(anchor="w")
        label("例如 12:00-13:30, 02:00-04:00；每段都可以跨午夜。")
        chord_var = tk.StringVar(value=chord.fmt(cfg.get("unlock_chord") or chord.DEFAULT_CHORD))
        label("本地解锁组合键（同时按住约 0.3 秒）")
        row = tk.Frame(body); row.pack(anchor="w")
        entry = tk.Entry(row, textvariable=chord_var, font=(FONT, 11), width=25)
        entry.pack(side="left", padx=(0, 12))
        held, recording = [], {"on": False}
        def record():
            held.clear(); recording["on"] = True
            chord_var.set("请同时按住 2～3 个键"); entry.focus_set()
        def down(event):
            if recording["on"]:
                try:
                    key = chord.normalize(event.keysym)
                    if key not in held:
                        held.append(key)
                    chord_var.set(chord.fmt(held))
                except ValueError:
                    pass
                return "break"
        def up(event):
            if recording["on"]:
                recording["on"] = False
                chord_var.set(chord.fmt(held))
                return "break"
        entry.bind("<KeyPress>", down); entry.bind("<KeyRelease>", up)
        tk.Button(row, text="录制", command=record).pack(side="left")
        locked = tk.BooleanVar(value=cfg.get("lock", True))
        autostart = tk.BooleanVar(value=cfg.get("autostart_preference", cfg.get("autostart", True)))
        tk.Checkbutton(body, text="黑屏时本地只能用组合键解锁", variable=locked, font=(FONT, 10)).pack(anchor="w", pady=(12, 0))
        tk.Checkbutton(body, text="登录系统后自动运行", variable=autostart, font=(FONT, 10)).pack(anchor="w")
        def save():
            if load().get("state") == "night" or load().get("enabled"):
                raise RuntimeError("请先恢复并暂停计划，再修改设置")
            keys = chord.validate([k.strip() for k in re.split(r"[+＋]", chord_var.get())])
            rs = [list(r) for r in parse_ranges(extra.get())]
            B.set_autostart(autostart.get())
            update(extra_ranges=rs, unlock_chord=keys, lock=locked.get(), autostart=autostart.get(),
                   autostart_preference=autostart.get())
            window.destroy()
        row = tk.Frame(body); row.pack(anchor="w", pady=10)
        tk.Button(row, text="保存设置", command=lambda: self.safe(save)).pack(side="left", padx=(0, 12))
        tk.Button(row, text="检查更新", command=self.check_update).pack(side="left", padx=(0, 12))
        tk.Button(row, text="卸载计划与自启", command=lambda: self.safe(self.remove)).pack(side="left")
        label(AUTHOR + "   |   " + SPONSOR)

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
                self.root.after(100, poll); return
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
            raise RuntimeError("黑屏已锁定，请先使用解锁组合键")
        uninstall(self.overlay); self.root.destroy()

    def on_close(self):
        cfg = load()
        if cfg.get("state") == "night" and cfg.get("lock", True) and not self.has_tray:
            messagebox.showwarning(APP, "黑屏已锁定，请先使用解锁组合键", parent=self.root)
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
    d = ImageDraw.Draw(img)
    d.ellipse((6, 6, 58, 58), fill=(46, 107, 255, 255))
    d.ellipse((22, 0, 66, 44), fill=(0, 0, 0, 0))
    menu = pystray.Menu(
        pystray.MenuItem("打开 NightDisplay", lambda: act("show"), default=True),
        pystray.MenuItem("立即进入夜间", lambda: act("night")),
        pystray.MenuItem("立即恢复显示", lambda: act("day")),
        pystray.MenuItem("应急恢复（暂停计划）", lambda: act("emergency")),
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
