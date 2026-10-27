# -*- coding: utf-8 -*-
"""界面：左侧导航 + 圆角卡片 + 大状态卡开关 + 动画，浅色/深色主题。自有品牌（月亮图标），不使用任何第三方商标素材。"""
import math, sys, threading, datetime as dt
import tkinter as tk
from tkinter import messagebox

from . import VERSION, APP, CREDIT, AUTHOR, SPONSOR
from .core import IS_WIN, IS_MAC, load, update, parse_hm, next_switch, read_log_tail, is_night, log
from . import backends as B

THEMES = {
    "light": dict(bg="#F3F5F9", side="#FFFFFF", card="#FFFFFF", text="#1D2129", sub="#86909C", line="#E5E8EF",
                  accent="#2E6BFF", accent2="#E8EFFF", night="#3B3F8F", ok="#00B42A", warn="#FF7D00", danger="#F53F3F"),
    "dark": dict(bg="#17181C", side="#1E2026", card="#24262D", text="#F2F3F5", sub="#8A8F99", line="#30333B",
                 accent="#4C82FF", accent2="#25314F", night="#7C83FF", ok="#27C346", warn="#FF9A2E", danger="#F76560"),
}
FONT = "Microsoft YaHei UI" if IS_WIN else ("PingFang SC" if IS_MAC else "Noto Sans CJK SC")


def rrect(c, x1, y1, x2, y2, r, **kw):
    pts = [x1 + r, y1, x2 - r, y1, x2, y1, x2, y1 + r, x2, y2 - r, x2, y2, x2 - r, y2, x1 + r, y2, x1, y2, x1, y2 - r, x1, y1 + r, x1, y1]
    return c.create_polygon(pts, smooth=True, **kw)


class Card(tk.Canvas):
    """圆角卡片：内部放一个 Frame。"""
    def __init__(self, parent, app, height, **kw):
        super().__init__(parent, height=height, highlightthickness=0, bd=0, bg=app.t["bg"])
        self.app = app
        self.inner = tk.Frame(self, bg=app.t["card"])
        self.win = self.create_window(16, 14, window=self.inner, anchor="nw")
        self.bind("<Configure>", self._draw)

    def _draw(self, e=None):
        self.delete("bgshape")
        w, h = self.winfo_width(), self.winfo_height()
        rrect(self, 1, 1, w - 1, h - 1, 18, fill=self.app.t["card"], outline=self.app.t["line"], tags="bgshape")
        self.tag_lower("bgshape")
        self.itemconfigure(self.win, width=w - 32, height=h - 28)


class Hero(tk.Canvas):
    """远控客户端风格的渐变主卡片（类似“本机设备”大卡），自绘渐变 + 圆角。"""
    def __init__(self, parent, app, height, night=False):
        super().__init__(parent, height=height, highlightthickness=0, bd=0, bg=app.t["bg"])
        self.app = app
        self.c1, self.c2, self.c3 = (("#2B2F77", "#353A8C", "#3E449C") if night else ("#1F6BFF", "#2C76FF", "#3A81FF"))
        self.inner = tk.Frame(self, bg=self.c1)
        self.win = self.create_window(26, 22, window=self.inner, anchor="nw")
        self.bind("<Configure>", self._draw)

    def _draw(self, e=None):
        self.delete("g")
        w, h = self.winfo_width(), self.winfo_height()
        rrect(self, 0, 0, w, h, 22, fill=self.c1, outline=self.c1, tags="g")
        # 装饰性半透明圆（自绘，非任何品牌素材）
        self.create_oval(w - 330, -120, w + 60, 260, fill=self.c2, outline="", tags="g")
        self.create_oval(w - 120, h - 70, w + 40, h + 90, fill=self.c3, outline="", tags="g")
        self.tag_lower("g")
        self.itemconfigure(self.win, width=max(200, w - 260))


class Toggle(tk.Canvas):
    """iOS / 远控客户端风格的滑动开关，带动画。"""
    def __init__(self, parent, app, value, command):
        super().__init__(parent, width=58, height=32, highlightthickness=0, bd=0, bg=parent["bg"])
        self.app, self.value, self.command, self.pos = app, value, command, 1.0 if value else 0.0
        self.bind("<Button-1>", lambda e: self.command(not self.value))
        self.draw()

    def set(self, v):
        self.value = v
        self._anim()

    def _anim(self):
        target = 1.0 if self.value else 0.0
        self.pos += (target - self.pos) * 0.35
        if abs(target - self.pos) < 0.02:
            self.pos = target
        self.draw()
        if self.pos != target:
            self.after(16, self._anim)

    def draw(self):
        t = self.app.t
        self.delete("all")
        col = t["accent"] if self.pos > 0.5 else t["line"]
        rrect(self, 2, 2, 56, 30, 14, fill=col, outline=col)
        x = 17 + self.pos * 24
        self.create_oval(x - 12, 4, x + 12, 28, fill="#FFFFFF", outline="")


class Pill(tk.Label):
    def __init__(self, parent, app, text, command, primary=False, danger=False):
        t = app.t
        bg = t["danger"] if danger else (t["accent"] if primary else t["accent2"])
        fg = "#FFFFFF" if (primary or danger) else t["accent"]
        super().__init__(parent, text=text, bg=bg, fg=fg, font=(FONT, 10, "bold" if primary else "normal"),
                         padx=16, pady=8, cursor="hand2")
        self.bind("<Button-1>", lambda e: command())
        self.bind("<Enter>", lambda e: self.configure(bg=self._shade(bg)))
        self.bind("<Leave>", lambda e: self.configure(bg=bg))

    @staticmethod
    def _shade(c):
        r, g, b = int(c[1:3], 16), int(c[3:5], 16), int(c[5:7], 16)
        return "#%02x%02x%02x" % (int(r * .88), int(g * .88), int(b * .88))


class MainWindow:
    NAV = [("home", "◐  首页"), ("schedule", "◷  定时计划"), ("mode", "▣  显示模式"), ("log", "≡  运行日志"),
           ("settings", "⚙  设置"), ("about", "ⓘ  关于")]

    def __init__(self, root, overlay, act):
        self.root, self.overlay, self.act = root, overlay, act
        self.theme = load().get("theme", "light")
        self.t = THEMES[self.theme]
        root.title("NightDisplay")
        root.geometry("920x600")
        root.minsize(820, 540)
        root.protocol("WM_DELETE_WINDOW", self.on_close)
        self.page = "home"
        self.build()
        self.fade_in()

    # ---- 框架 ----
    def build(self):
        for w in self.root.winfo_children():
            if not isinstance(w, tk.Toplevel):
                w.destroy()
        t = self.t
        self.root.configure(bg=t["bg"])
        side = tk.Frame(self.root, bg=t["side"], width=200)
        side.pack(side="left", fill="y"); side.pack_propagate(False)
        logo = tk.Canvas(side, width=200, height=78, bg=t["side"], highlightthickness=0)
        logo.pack()
        logo.create_oval(22, 22, 54, 54, fill=t["accent"], outline="")
        logo.create_oval(32, 18, 62, 48, fill=t["side"], outline="")
        logo.create_text(66, 38, text="NightDisplay", anchor="w", fill=t["text"], font=(FONT, 13, "bold"))
        self.navbtns = {}
        for key, label in self.NAV:
            b = tk.Label(side, text=label, anchor="w", padx=22, pady=10, font=(FONT, 11), cursor="hand2")
            b.pack(fill="x", padx=10, pady=2)
            b.bind("<Button-1>", lambda e, k=key: self.show(k))
            self.navbtns[key] = b
        tk.Label(side, text="v" + VERSION, bg=t["side"], fg=t["sub"], font=(FONT, 9)).pack(side="bottom", pady=12)
        self.body = tk.Frame(self.root, bg=t["bg"])
        self.body.pack(side="left", fill="both", expand=True, padx=22, pady=18)
        self.show(self.page)

    def show(self, key):
        self.page = key
        t = self.t
        for k, b in self.navbtns.items():
            sel = k == key
            b.configure(bg=t["accent2"] if sel else t["side"], fg=t["accent"] if sel else t["text"])
        for w in self.body.winfo_children():
            w.destroy()
        getattr(self, "page_" + key)()
        self.slide_in()

    def fade_in(self):
        try:
            self.root.attributes("-alpha", 0.0)
            def step(a=0.0):
                a = min(1.0, a + 0.08)
                self.root.attributes("-alpha", a)
                if a < 1.0:
                    self.root.after(15, lambda: step(a))
            step()
        except tk.TclError:
            pass

    def slide_in(self):
        """页面切换的轻量位移动画。"""
        def step(i=0):
            if i <= 6:
                self.body.pack_configure(padx=(22 + (6 - i) * 4, 22))
                self.root.after(14, lambda: step(i + 1))
        step()

    def title(self, text, sub=""):
        tk.Label(self.body, text=text, bg=self.t["bg"], fg=self.t["text"], font=(FONT, 18, "bold")).pack(anchor="w")
        if sub:
            tk.Label(self.body, text=sub, bg=self.t["bg"], fg=self.t["sub"], font=(FONT, 10)).pack(anchor="w", pady=(2, 10))

    def label(self, parent, text, size=10, color="text", bold=False, **kw):
        kw.setdefault("bg", parent["bg"])
        return tk.Label(parent, text=text, fg=self.t[color], font=(FONT, size, "bold" if bold else "normal"),
                        justify="left", **kw)

    def safe(self, fn):
        try:
            r = fn()
            self.refresh_soon()
            return r
        except Exception as e:
            log("错误: %s" % e)
            messagebox.showerror(APP, str(e))

    def refresh_soon(self):
        self.root.after(400, lambda: self.show(self.page))

    # ---- 首页：大状态卡 ----
    def page_home(self):
        cfg = load()
        night = cfg.get("state") == "night"
        top = tk.Frame(self.body, bg=self.t["bg"]); top.pack(fill="x", pady=(0, 12))
        self.label(top, "首页", 18, bold=True, bg=self.t["bg"]).pack(side="left")
        self.label(top, "● 夜间进行中" if night else "● 运行正常", 10, "night" if night else "ok", bg=self.t["bg"]).pack(side="right")
        hero = Hero(self.body, self, 220, night); hero.pack(fill="x")
        f = hero.inner
        W = lambda txt, size=10, bold=False, fg="#FFFFFF": tk.Label(f, text=txt, bg=hero.c1, fg=fg, font=(FONT, size, "bold" if bold else "normal"), anchor="w")
        W("本机状态", 10, fg="#D6E4FF").pack(anchor="w")
        W("夜间模式 · 本地已黑屏" if night else "白天 · 屏幕正常显示", 20, True).pack(anchor="w", pady=(2, 6))
        nxt, secs = next_switch(cfg)
        W(("%s 后%s" % (self._fmt(secs), "恢复显示" if nxt == "day" else "进入夜间")) if cfg["enabled"] else "自动计划未开启 · 先在“显示模式”里测试一次", 10, fg="#E8F0FF").pack(anchor="w")
        row = tk.Frame(f, bg=hero.c1); row.pack(anchor="w", pady=(16, 0))
        tk.Label(row, text="每日自动计划", bg=hero.c1, fg="#FFFFFF", font=(FONT, 11)).pack(side="left", padx=(0, 10))
        tg = Toggle(row, self, cfg["enabled"], self.toggle_enabled); tg.configure(bg=hero.c1); tg.pack(side="left")
        ring = tk.Canvas(hero, width=170, height=170, bg=hero.c2, highlightthickness=0)
        hero.create_window(0, 0, window=ring, anchor="ne", tags="ring")
        hero.bind("<Configure>", lambda e: (hero._draw(), hero.coords("ring", e.width - 28, 25)), add=False)
        self.anim_ring(ring, cfg, hero=True, hero_bg=hero.c2)
        # 快捷操作卡片（三宫格）
        grid = tk.Frame(self.body, bg=self.t["bg"]); grid.pack(fill="x", pady=14)
        items = [("◑", "立即进入夜间", "按当前模式黑屏", lambda: self.act("night")),
                 ("☀", "立即恢复显示", "恢复实体屏与备份配置", lambda: self.act("day")),
                 ("⛑", "应急恢复", "恢复显示并暂停计划", lambda: self.act("emergency"))]
        for i, (ic, t1, t2, fn) in enumerate(items):
            c = Card(grid, self, 92); c.grid(row=0, column=i, sticky="ew", padx=(0 if i == 0 else 7, 0 if i == 2 else 7))
            grid.columnconfigure(i, weight=1)
            fr = c.inner
            lab = tk.Label(fr, text=ic, bg=self.t["accent2"], fg=self.t["danger"] if i == 2 else self.t["accent"], font=(FONT, 16), width=2)
            lab.pack(side="left", padx=(0, 10))
            tx = tk.Frame(fr, bg=fr["bg"]); tx.pack(side="left", fill="x")
            self.label(tx, t1, 11, bold=True).pack(anchor="w"); self.label(tx, t2, 9, "sub").pack(anchor="w")
            for w in (c, fr, lab, tx) + tuple(tx.winfo_children()):
                w.bind("<Button-1>", lambda e, fn=fn: self.safe(fn)); w.configure(cursor="hand2")
        tip = Card(self.body, self, 86); tip.pack(fill="x")
        self.label(tip.inner, "模式：%s ｜ 时间：%s → %s" % (B.MODES.get(cfg["mode"]), cfg["start"], cfg["end"]), 10, bold=True).pack(anchor="w")
        self.label(tip.inner, "不拦截键盘鼠标 · 切换前完整备份，失败自动回滚 · 应急快捷键 Ctrl+Alt+Shift+R（Windows）", 9, "sub").pack(anchor="w", pady=4)

    def _fmt(self, s):
        h, m = s // 3600, (s % 3600) // 60
        return ("%d 小时 %d 分" % (h, m)) if h else ("%d 分钟" % m)

    def anim_ring(self, c, cfg, hero=False, hero_bg="#2C76FF"):
        t = dict(self.t)
        if hero:
            t.update(line="#6FA0FF", night="#FFFFFF", text="#FFFFFF", sub="#E0EAFF", accent="#FFD24D", card=hero_bg)
        sh, sm = parse_hm(cfg["start"]); eh, em = parse_hm(cfg["end"])
        s, e = sh * 60 + sm, eh * 60 + em
        span = (e - s) % 1440
        now = dt.datetime.now(); n = now.hour * 60 + now.minute

        def draw(p):
            c.delete("all")
            c.create_oval(15, 15, 155, 155, outline=t["line"], width=14)
            c.create_arc(15, 15, 155, 155, start=90 - s / 4, extent=-(span / 4) * p, style="arc", outline=t["night"], width=14)
            a = math.radians(90 - n / 4)
            c.create_oval(85 + 70 * math.cos(a) - 7, 85 - 70 * math.sin(a) - 7, 85 + 70 * math.cos(a) + 7, 85 - 70 * math.sin(a) + 7,
                          fill=t["accent"], outline=t["card"], width=3)
            c.create_text(85, 76, text=now.strftime("%H:%M"), fill=t["text"], font=(FONT, 18, "bold"))
            c.create_text(85, 102, text="夜间 %s–%s" % (cfg["start"], cfg["end"]), fill=t["sub"], font=(FONT, 8))
        def step(p=0.0):
            p = min(1.0, p + 0.06)
            draw(1 - (1 - p) ** 3)
            if p < 1.0:
                c.after(16, lambda: step(p))
        step()

    def toggle_enabled(self, v):
        cfg = load()
        if v:
            self.safe(lambda: __import__("nightdisplay.app", fromlist=["x"]).activate(cfg["mode"], cfg["start"], cfg["end"]))
        else:
            self.safe(lambda: __import__("nightdisplay.app", fromlist=["x"]).pause())

    # ---- 定时计划 ----
    def page_schedule(self):
        cfg = load()
        self.title("定时计划", "每天几点进入夜间、几点恢复（可跨午夜）")
        card = Card(self.body, self, 190); card.pack(fill="x")
        f = card.inner
        hours = ["%02d" % i for i in range(24)]; mins = ["%02d" % i for i in range(0, 60, 5)]
        self.sv = [tk.StringVar(value=x) for x in cfg["start"].split(":") + cfg["end"].split(":")]
        for i, lab in enumerate(("进入夜间（黑屏）", "恢复显示")):
            row = tk.Frame(f, bg=f["bg"]); row.pack(anchor="w", pady=8)
            self.label(row, lab, 11, width=16, anchor="w").pack(side="left")
            for j, vals in ((0, hours), (1, mins)):
                om = tk.OptionMenu(row, self.sv[i * 2 + j], *vals)
                om.configure(bg=self.t["accent2"], fg=self.t["text"], relief="flat", highlightthickness=0, font=(FONT, 11), width=3)
                om.pack(side="left", padx=4)
                if j == 0:
                    self.label(row, ":", 12).pack(side="left")
        kv = tk.BooleanVar(value=cfg.get("keep_awake", True))
        tk.Checkbutton(f, text="夜间阻止系统睡眠（保持远程在线）", variable=kv, bg=f["bg"], fg=self.t["text"],
                       selectcolor=self.t["card"], font=(FONT, 10), activebackground=f["bg"]).pack(anchor="w", pady=6)

        def save_():
            s = "%s:%s" % (self.sv[0].get(), self.sv[1].get()); e = "%s:%s" % (self.sv[2].get(), self.sv[3].get())
            parse_hm(s); parse_hm(e)
            c = update(start=s, end=e, keep_awake=bool(kv.get()))
            if c["enabled"]:
                B.schedule_install(c)
            messagebox.showinfo(APP, "已保存：%s → %s" % (s, e))
        Pill(self.body, self, "保存计划", lambda: self.safe(save_), primary=True).pack(anchor="w", pady=14)

    # ---- 显示模式 ----
    def page_mode(self):
        cfg = load()
        self.title("显示模式", "推荐“自动”：有虚拟屏就只保留虚拟屏，否则用防窥黑幕")
        mv = tk.StringVar(value=cfg["mode"])
        desc = {"auto": "自动选择最接近 UU“关闭显示屏”效果的方式",
                "virtual_only": "实体屏脱离桌面，桌面只输出到虚拟屏（需虚拟显示驱动）",
                "overlay": "本地盖黑幕，远程画面看不到黑幕；鼠标键盘穿透（Win10 2004+）",
                "screen_off": "发送关屏命令；本地动鼠标/键盘会亮"}
        card = Card(self.body, self, 46 * len(B.available_modes()) + 30); card.pack(fill="x")
        for m in B.available_modes():
            r = tk.Radiobutton(card.inner, text="%s — %s" % (B.MODES[m], desc[m]), value=m, variable=mv,
                               command=lambda: update(mode=mv.get()), bg=card.inner["bg"], fg=self.t["text"],
                               selectcolor=self.t["card"], activebackground=card.inner["bg"], font=(FONT, 10), anchor="w")
            r.pack(anchor="w", pady=6)
        real = B.resolve_mode(cfg["mode"], cfg.get("virtual_match", ""))
        self.label(self.body, "当前将实际使用：%s ｜ 检测到虚拟屏：%s" % (B.MODES[real], "是" if B.has_virtual_display(cfg.get("virtual_match", "")) else "否"), 10, "sub", bg=self.t["bg"]).pack(anchor="w", pady=8)
        row = tk.Frame(self.body, bg=self.t["bg"]); row.pack(anchor="w", pady=6)

        def test():
            if messagebox.askyesno(APP, "将立即进入夜间（%s），120 秒后自动恢复。\n请先确认远程软件已能连接，并有人在电脑旁。继续？" % B.MODES[B.resolve_mode(mv.get())]):
                from .app import start_test
                start_test(mv.get(), 120, self.overlay)
                self.overlay and None
                self.root.after(120000, lambda: self.act("day"))
        Pill(row, self, "测试（120 秒自动恢复）", lambda: self.safe(test), primary=True).pack(side="left", padx=(0, 10))
        if IS_WIN:
            Pill(row, self, "安装虚拟显示驱动（需管理员）", lambda: self.safe(lambda: __import__("nightdisplay.app", fromlist=["x"]).install_vdd())).pack(side="left")

    # ---- 日志 ----
    def page_log(self):
        self.title("运行日志", "每次切换、备份、回滚都会记录")
        txt = tk.Text(self.body, bg=self.t["card"], fg=self.t["text"], relief="flat", font=("Consolas" if IS_WIN else "Menlo", 9), padx=12, pady=10)
        txt.pack(fill="both", expand=True)
        txt.insert("end", read_log_tail() or "（还没有日志）"); txt.see("end")
        self.label(self.body, B.status_text(), 9, "sub", bg=self.t["bg"]).pack(anchor="w", pady=8)

    # ---- 设置 ----
    def page_settings(self):
        self.title("设置")
        card = Card(self.body, self, 150); card.pack(fill="x")
        row = tk.Frame(card.inner, bg=card.inner["bg"]); row.pack(anchor="w", pady=6)
        self.label(row, "深色模式", 11, width=16, anchor="w").pack(side="left")
        Toggle(row, self, self.theme == "dark", self.set_dark).pack(side="left")
        self.label(card.inner, "应急快捷键：Ctrl+Alt+Shift+R（仅 Windows 全局有效）", 10, "sub").pack(anchor="w", pady=6)
        row2 = tk.Frame(self.body, bg=self.t["bg"]); row2.pack(anchor="w", pady=16)
        def uninst():
            if messagebox.askyesno(APP, "卸载：恢复显示、删除计划任务和开机自启，然后退出。继续？"):
                from .app import uninstall
                uninstall(self.overlay); self.root.destroy()
        Pill(row2, self, "卸载计划与自启", uninst, danger=True).pack(side="left")

    def set_dark(self, v):
        self.theme = "dark" if v else "light"
        update(theme=self.theme)
        self.t = THEMES[self.theme]
        self.build()

    # ---- 关于 ----
    def page_about(self):
        self.title("关于 NightDisplay")
        card = Card(self.body, self, 250); card.pack(fill="x")
        f = card.inner
        self.label(f, "NightDisplay  v" + VERSION, 14, bold=True).pack(anchor="w")
        self.label(f, "作者：" + AUTHOR, 11).pack(anchor="w", pady=(8, 2))
        self.label(f, SPONSOR, 12, "accent", bold=True).pack(anchor="w", pady=(2, 8))
        self.label(f, "独立开源工具，与网易 UU 远程、ToDesk 等无隶属关系。\n"
                      "参考：RustDesk 隐私模式思路（AGPL，未复制代码）、Virtual-Display-Driver（MIT）、UUyc-companion（MIT）。\n"
                      "仅在你本人拥有或已获授权的电脑上使用。", 9, "sub", wraplength=600).pack(anchor="w")

    def on_close(self):
        if getattr(self, "has_tray", False):
            self.root.withdraw()
        else:
            self.root.destroy()


def start_tray(root, act):
    try:
        import pystray
        from PIL import Image, ImageDraw
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
    icon = pystray.Icon("NightDisplay", img, "NightDisplay", menu)
    try:
        if IS_MAC:
            return None  # macOS 托盘需主线程，后续版本处理；窗口关闭即退出
        threading.Thread(target=icon.run, daemon=True).start()
    except Exception:
        return None
    return icon
