"""一套深色设计系统：Mac / SwiftUI 的观感，全部用 Tk 自带的 Canvas 和 Frame 画。

不引入新框架。动画只用 after()，每段 120–200 毫秒、8 帧左右，结束就停；
窗口藏起来时不重画。颜色、字体、圆角集中在这里，ui.py 只组合控件。
三个系统同一套外观；字体按系统选。
"""
import contextlib
import tkinter as tk
import tkinter.font as tkfont

from .core import IS_MAC, IS_WIN

# macOS 深色模式系统色（近似值）
BG = "#1c1c1e"
SIDEBAR = "#242427"
CARD = "#2c2c2e"
CARD_HI = "#363639"
BORDER = "#3a3a3d"
TEXT = "#f5f5f7"
SUB = "#9a9aa0"
FAINT = "#5a5a60"
ACCENT = "#0a84ff"
ACCENT_HI = "#409cff"
ACCENT_LO = "#0062cc"
GREEN = "#30d158"
ORANGE = "#ff9f0a"
RED = "#ff453a"
TRACK = "#3a3a3c"

if IS_WIN:
    FAMILY = "Microsoft YaHei UI"
elif IS_MAC:
    FAMILY = "PingFang SC"
else:
    FAMILY = "Noto Sans CJK SC"

_scale = [1.0]
_fonts = {}  # 命名字体的 Python 对象被回收时 Tk 会删掉字体，必须留着引用


def setup(root):
    """按 DPI 算缩放倍数，建好命名字体。返回缩放倍数。"""
    try:
        _scale[0] = max(1.0, float(root.winfo_fpixels("1i")) / 96.0)
    except tk.TclError:
        _scale[0] = 1.0
    base = 13 if IS_MAC else 10
    sizes = {"ND.Body": (base, "normal"), "ND.Small": (base - 1, "normal"),
             "ND.Bold": (base, "bold"), "ND.Title": (base + 7, "bold"),
             "ND.Head": (base + 2, "bold"), "ND.Big": (base + 15, "bold"),
             "ND.Mono": (base + 4, "bold")}
    for name, (size, weight) in sizes.items():
        try:
            tkfont.nametofont(name).configure(family=FAMILY, size=size, weight=weight)
        except tk.TclError:
            _fonts[name] = tkfont.Font(root, name=name, family=FAMILY, size=size, weight=weight)
    for name in ("TkDefaultFont", "TkTextFont", "TkMenuFont"):
        with contextlib.suppress(tk.TclError):
            tkfont.nametofont(name).configure(family=FAMILY, size=base)
    with contextlib.suppress(tk.TclError):
        root.configure(background=BG)
        root.option_add("*Menu.background", CARD)
        root.option_add("*Menu.foreground", TEXT)
        root.option_add("*Menu.activeBackground", ACCENT)
        root.option_add("*Menu.activeForeground", "#ffffff")
        root.option_add("*Menu.relief", "flat")
    if IS_WIN:
        dark_titlebar(root)
    return _scale[0]


def S(v):
    return int(round(v * _scale[0]))


def dark_titlebar(window):
    """Win10 20H1+/Win11 深色标题栏（DWMWA_USE_IMMERSIVE_DARK_MODE=20，旧版 19）。失败无影响。"""
    if not IS_WIN:
        return
    try:
        import ctypes
        window.update_idletasks()
        hwnd = ctypes.windll.user32.GetParent(window.winfo_id()) or window.winfo_id()
        on = ctypes.c_int(1)
        for attr in (20, 19):
            if ctypes.windll.dwmapi.DwmSetWindowAttribute(hwnd, attr, ctypes.byref(on), ctypes.sizeof(on)) == 0:
                break
    except Exception:  # noqa: BLE001
        pass


def mix(a, b, t):
    """两种 #rrggbb 颜色按 t 混合。"""
    a, b = a.lstrip("#"), b.lstrip("#")
    ca = [int(a[i:i + 2], 16) for i in (0, 2, 4)]
    cb = [int(b[i:i + 2], 16) for i in (0, 2, 4)]
    return "#%02x%02x%02x" % tuple(int(x + (y - x) * t) for x, y in zip(ca, cb))


def round_rect(canvas, x1, y1, x2, y2, r, **kw):
    r = max(0, min(r, (x2 - x1) / 2, (y2 - y1) / 2))
    pts = [x1 + r, y1, x2 - r, y1, x2, y1, x2, y1 + r, x2, y2 - r, x2, y2,
           x2 - r, y2, x1 + r, y2, x1, y2, x1, y2 - r, x1, y1 + r, x1, y1]
    return canvas.create_polygon(pts, smooth=True, **kw)


def animate(widget, duration, fn, done=None, frames=8):
    """在 duration 毫秒内用 ease-out 调 fn(t)。窗口没显示时直接跳到结尾。"""
    try:
        visible = bool(widget.winfo_viewable())
    except tk.TclError:
        return
    if not visible:
        fn(1.0)
        if done:
            done()
        return
    step_ms = max(10, duration // frames)

    def step(i=1):
        t = min(1.0, i / frames)
        try:
            fn(1 - (1 - t) ** 3)
        except tk.TclError:
            return
        if t < 1.0:
            widget.after(step_ms, step, i + 1)
        elif done:
            done()
    widget.after(0, step)


def label(parent, text="", font="ND.Body", fg=TEXT, bg=CARD, **kw):
    return tk.Label(parent, text=text, font=font, fg=fg, bg=bg, anchor="w", justify="left", **kw)


class Card(tk.Canvas):
    """圆角卡片。内容放进 .body。"""

    def __init__(self, parent, pad=16, radius=12, bg=CARD, outer=BG):
        super().__init__(parent, bg=outer, highlightthickness=0, bd=0, height=S(40))
        self.pad, self.radius, self.fill = S(pad), S(radius), bg
        self.body = tk.Frame(self, bg=bg)
        self._win = self.create_window(self.pad, self.pad, anchor="nw", window=self.body)
        self.body.bind("<Configure>", self._fit)
        self.bind("<Configure>", self._draw)

    def _fit(self, _e=None):
        h = self.body.winfo_reqheight() + 2 * self.pad
        if int(self.cget("height")) != h:
            self.configure(height=h)

    def _draw(self, e):
        self.delete("bg")
        round_rect(self, 1, 1, e.width - 1, e.height - 1, self.radius, fill=self.fill, outline=BORDER, tags="bg")
        self.tag_lower("bg")
        self.itemconfigure(self._win, width=max(1, e.width - 2 * self.pad))


class Button(tk.Canvas):
    """胶囊按钮：悬停变亮、按下变暗。kind=primary/secondary/danger/ghost。"""

    COLORS = {"primary": (ACCENT, ACCENT_HI, ACCENT_LO, "#ffffff"),
              "secondary": (CARD_HI, "#434346", "#2a2a2c", TEXT),
              "danger": ("#3a2424", "#4a2a2a", "#2e1c1c", RED),
              "ghost": (CARD, CARD_HI, BORDER, ACCENT)}

    def __init__(self, parent, text, command, kind="secondary", width=None, bg=CARD):
        self.kind, self.command, self.text = kind, command, text
        self._font = tkfont.nametofont("ND.Bold")
        self._fixed_w = width
        w, h = self._size()
        super().__init__(parent, width=w, height=h, bg=bg, highlightthickness=0, bd=0, cursor="hand2")
        self._state = "normal"
        self._enabled = True
        self._draw()
        self.bind("<Enter>", lambda e: self._set("hover"))
        self.bind("<Leave>", lambda e: self._set("normal"))
        self.bind("<ButtonPress-1>", lambda e: self._set("press"))
        self.bind("<ButtonRelease-1>", self._release)

    def _size(self):
        w = self._fixed_w or self._font.measure(self.text) + S(30)
        return w, S(32)

    def _draw(self):
        self.delete("all")
        base, hi, lo, fg = self.COLORS[self.kind]
        fill = {"normal": base, "hover": hi, "press": lo}[self._state]
        if not self._enabled:
            fill, fg = mix(base, CARD, 0.6), FAINT
        w, h = int(self.cget("width")), int(self.cget("height"))
        round_rect(self, 0, 0, w - 1, h - 1, h // 2, fill=fill, outline="")
        self.create_text(w // 2, h // 2, text=self.text, fill=fg, font=self._font)

    def _set(self, state):
        if self._state != state:
            self._state = state
            self._draw()

    def _release(self, e):
        inside = 0 <= e.x <= self.winfo_width() and 0 <= e.y <= self.winfo_height()
        self._set("hover" if inside else "normal")
        if inside and self._enabled and self.command:
            self.command()

    def set_text(self, text):
        if text != self.text:
            self.text = text
            w, h = self._size()
            self.configure(width=w, height=h)
            self._draw()

    def set_kind(self, kind):
        if kind != self.kind:
            self.kind = kind
            self._draw()

    def set_enabled(self, on):
        if bool(on) != self._enabled:
            self._enabled = bool(on)
            self.configure(cursor="hand2" if on else "arrow")
            self._draw()


class Switch(tk.Canvas):
    """iOS 风格开关，旋钮滑动约 140 毫秒。"""

    def __init__(self, parent, variable, command=None, bg=CARD):
        self.W, self.H = S(44), S(26)
        super().__init__(parent, width=self.W, height=self.H, bg=bg, highlightthickness=0, bd=0, cursor="hand2")
        self.var, self.command = variable, command
        self._pos = 1.0 if variable.get() else 0.0
        self._hover = False
        self._draw()
        self.bind("<ButtonRelease-1>", self._toggle)
        self.bind("<Enter>", lambda e: self._hov(True))
        self.bind("<Leave>", lambda e: self._hov(False))
        variable.trace_add("write", lambda *a: self._sync())

    def _hov(self, on):
        self._hover = on
        self._draw()

    def _draw(self):
        self.delete("all")
        p = self._pos
        track = mix(TRACK, GREEN, p)
        if self._hover:
            track = mix(track, "#ffffff", 0.08)
        round_rect(self, 0, 0, self.W - 1, self.H - 1, self.H // 2, fill=track, outline="")
        r = self.H // 2 - S(3)
        cx = S(3) + r + p * (self.W - 2 * r - S(6))
        cy = self.H // 2
        self.create_oval(cx - r, cy - r + 1, cx + r, cy + r + 1, fill=mix(track, "#000000", 0.35), outline="")
        self.create_oval(cx - r, cy - r, cx + r, cy + r, fill="#ffffff", outline="")

    def _sync(self):
        start, end = self._pos, 1.0 if self.var.get() else 0.0
        if start == end:
            return

        def fn(t):
            self._pos = start + (end - start) * t
            self._draw()
        animate(self, 140, fn, frames=7)

    def _toggle(self, _e=None):
        self.var.set(not self.var.get())
        if self.command:
            self.command()


class Stepper(tk.Frame):
    """数值步进器：－ 值 ＋。按住连续变，滚轮也能调，点数字弹出列表直接选。"""

    def __init__(self, parent, variable, maximum, on_change=None, bg=CARD_HI):
        super().__init__(parent, bg=bg, highlightthickness=1, highlightbackground=BORDER, highlightcolor=BORDER)
        self.var, self.max, self.on_change = variable, maximum, on_change
        self._job = None
        self._commit_job = None
        self.minus = self._key("−", -1, bg)
        self.value = tk.Label(self, textvariable=variable, font="ND.Mono", fg=TEXT, bg=bg, width=3, cursor="hand2")
        self.plus = self._key("+", 1, bg)
        self.minus.pack(side="left")
        self.value.pack(side="left")
        self.plus.pack(side="left")
        self.value.bind("<Button-1>", self._menu)
        for w in (self, self.value, self.minus, self.plus):
            w.bind("<MouseWheel>", self._wheel)
            w.bind("<Button-4>", lambda e: self.bump(1))
            w.bind("<Button-5>", lambda e: self.bump(-1))

    def _key(self, text, delta, bg):
        k = tk.Label(self, text=text, font="ND.Head", fg=SUB, bg=bg, width=2, cursor="hand2")
        k.bind("<Enter>", lambda e: k.configure(fg=TEXT, bg=mix(bg, "#ffffff", 0.08)))
        k.bind("<Leave>", lambda e: k.configure(fg=SUB, bg=bg))
        k.bind("<ButtonPress-1>", lambda e: self._press(delta, k))
        k.bind("<ButtonRelease-1>", lambda e: self._release(k, bg))
        return k

    def bump(self, delta):
        try:
            v = int(self.var.get())
        except ValueError:
            v = 0
        self.var.set("%02d" % ((v + delta) % (self.max + 1)))
        self._schedule_commit()

    def _press(self, delta, key):
        key.configure(fg=ACCENT)
        self.bump(delta)
        self._job = self.after(420, self._repeat, delta)

    def _repeat(self, delta):
        self.bump(delta)
        self._job = self.after(70, self._repeat, delta)

    def _release(self, key, bg):
        key.configure(fg=TEXT)
        if self._job:
            self.after_cancel(self._job)
            self._job = None

    def _wheel(self, e):
        self.bump(1 if e.delta > 0 else -1)

    def _schedule_commit(self):
        if self._commit_job:
            self.after_cancel(self._commit_job)
        if self.on_change:
            self._commit_job = self.after(700, self._commit)

    def _commit(self):
        self._commit_job = None
        if self.on_change:
            self.on_change()

    def _menu(self, _e=None):
        menu = tk.Menu(self, tearoff=0)
        for n in range(0, self.max + 1):
            v = "%02d" % n
            menu.add_command(label=v, command=lambda v=v: (self.var.set(v), self._commit()))
        with contextlib.suppress(tk.TclError):
            menu.tk_popup(self.value.winfo_rootx(), self.value.winfo_rooty() + self.value.winfo_height())


class Dropdown(tk.Frame):
    """只读小时/分钟下拉：点开从列表选，不能手打，避免「19.30」「七点半」这类乱输入。

    maximum=23 → 00–23（24 小时制），maximum=59 → 00–59。
    """

    def __init__(self, parent, variable, maximum, on_change=None, bg=CARD_HI, width=4):
        super().__init__(parent, bg=bg, highlightthickness=1,
                         highlightbackground=BORDER, highlightcolor=BORDER)
        self.var, self.max, self.on_change = variable, maximum, on_change
        self._popup = None
        self.btn = tk.Label(self, textvariable=variable, font="ND.Mono", fg=TEXT, bg=bg,
                            width=width, cursor="hand2", padx=S(6), pady=S(4))
        self.chev = tk.Label(self, text="▾", font="ND.Small", fg=SUB, bg=bg, cursor="hand2",
                             padx=S(4), pady=S(4))
        self.btn.pack(side="left")
        self.chev.pack(side="left")
        for w in (self, self.btn, self.chev):
            w.bind("<Button-1>", self._open)
            w.bind("<Enter>", lambda e: self.chev.configure(fg=TEXT))
            w.bind("<Leave>", lambda e: self.chev.configure(fg=SUB))

    def _open(self, _e=None):
        if self._popup is not None:
            with contextlib.suppress(tk.TclError):
                self._popup.destroy()
            self._popup = None
        pop = tk.Toplevel(self)
        pop.withdraw()
        pop.overrideredirect(True)
        pop.attributes("-topmost", True)
        pop.configure(bg=BORDER)
        frame = tk.Frame(pop, bg=CARD_HI, highlightthickness=0)
        frame.pack(padx=1, pady=1)
        canvas = tk.Canvas(frame, bg=CARD_HI, highlightthickness=0,
                           width=S(64), height=S(220))
        scroll = tk.Scrollbar(frame, orient="vertical", command=canvas.yview)
        inner = tk.Frame(canvas, bg=CARD_HI)
        win = canvas.create_window((0, 0), window=inner, anchor="nw")
        canvas.configure(yscrollcommand=scroll.set)

        def _size(_event=None):
            canvas.configure(scrollregion=canvas.bbox("all"))
            canvas.itemconfigure(win, width=canvas.winfo_width())

        inner.bind("<Configure>", _size)
        canvas.bind("<Configure>", _size)
        canvas.pack(side="left", fill="both", expand=True)
        scroll.pack(side="right", fill="y")

        current = self.var.get()
        target = None
        for n in range(0, self.max + 1):
            v = "%02d" % n
            row = tk.Label(inner, text=v, font="ND.Mono", fg=ACCENT if v == current else TEXT,
                           bg=CARD_HI, width=6, cursor="hand2", pady=S(3))
            row.pack(fill="x")
            if v == current:
                target = row

            def pick(val=v, lab=row):
                self.var.set(val)
                if self.on_change:
                    self.on_change()
                with contextlib.suppress(tk.TclError):
                    pop.destroy()
                self._popup = None

            def enter(e, lab=row):
                lab.configure(bg=mix(CARD_HI, "#ffffff", 0.10))

            def leave(e, lab=row, val=v):
                lab.configure(bg=CARD_HI, fg=ACCENT if val == self.var.get() else TEXT)

            row.bind("<Button-1>", lambda e, p=pick: p())
            row.bind("<Enter>", enter)
            row.bind("<Leave>", leave)

        pop.update_idletasks()
        x = self.winfo_rootx()
        y = self.winfo_rooty() + self.winfo_height() + 2
        pop.geometry("+%d+%d" % (x, y))
        pop.deiconify()
        pop.focus_force()
        self._popup = pop
        if target is not None:
            with contextlib.suppress(tk.TclError):
                canvas.update_idletasks()
                # 滚到当前值附近
                frac = max(0.0, (int(current or "0") / max(1, self.max)) - 0.15)
                canvas.yview_moveto(frac)

        def dismiss(event=None):
            if self._popup is None:
                return
            w = event.widget if event else None
            # 点在弹层里不关
            if w is not None:
                try:
                    if str(w).startswith(str(pop)):
                        return
                except Exception:
                    pass
            with contextlib.suppress(tk.TclError):
                pop.destroy()
            self._popup = None

        pop.bind("<Escape>", lambda e: dismiss())
        pop.bind("<FocusOut>", lambda e: self.after(120, dismiss))
        # 点别处关闭
        self.winfo_toplevel().bind("<Button-1>", dismiss, add="+")



class Ring(tk.Canvas):
    """首页大倒计时圆环。只在数值变化时重画。"""

    def __init__(self, parent, size=200, bg=CARD):
        self.size = S(size)
        super().__init__(parent, width=self.size, height=self.size, bg=bg, highlightthickness=0, bd=0)
        self._last = None
        self._shown = 0.0

    def show(self, frac, big, small, color, animate_to=False):
        frac = max(0.0, min(1.0, frac))
        key = (round(frac, 3), big, small, color)
        if key == self._last:
            return
        self._last = key
        if animate_to and abs(frac - self._shown) > 0.01:
            start = self._shown
            animate(self, 220, lambda t: self._paint(start + (frac - start) * t, big, small, color), frames=9)
        else:
            self._paint(frac, big, small, color)

    def _paint(self, frac, big, small, color):
        self._shown = frac
        self.delete("all")
        s, w = self.size, S(12)
        m = w // 2 + S(4)
        self.create_oval(m, m, s - m, s - m, outline=TRACK, width=w)
        if frac > 0.002:
            extent = -359.9 * frac
            self.create_arc(m, m, s - m, s - m, start=90, extent=extent, style="arc", outline=color, width=w)
            # 圆头端点
            import math
            for ang in (90, 90 + extent):
                a = math.radians(ang)
                r = (s - 2 * m) / 2
                cx, cy = s / 2 + r * math.cos(a), s / 2 - r * math.sin(a)
                self.create_oval(cx - w / 2, cy - w / 2, cx + w / 2, cy + w / 2, fill=color, outline="")
        self.create_text(s / 2, s / 2 - S(8), text=big, fill=TEXT, font="ND.Big")
        self.create_text(s / 2, s / 2 + S(26), text=small, fill=SUB, font="ND.Small")


class SideItem(tk.Canvas):
    """侧边栏一项：选中是蓝底圆角，悬停是浅灰。"""

    def __init__(self, parent, glyph, text, command, width=164):
        self.W, self.H = S(width), S(34)
        super().__init__(parent, width=self.W, height=self.H, bg=SIDEBAR, highlightthickness=0, bd=0, cursor="hand2")
        self.glyph, self.text, self.command = glyph, text, command
        self.selected, self.hover = False, False
        self._draw()
        self.bind("<Enter>", lambda e: self._h(True))
        self.bind("<Leave>", lambda e: self._h(False))
        self.bind("<ButtonRelease-1>", lambda e: command())

    def _h(self, on):
        self.hover = on
        self._draw()

    def set_selected(self, on):
        self.selected = on
        self._draw()

    def _draw(self):
        self.delete("all")
        fill = ACCENT if self.selected else (CARD_HI if self.hover else "")
        if fill:
            round_rect(self, S(4), 0, self.W - S(4), self.H - 1, S(8), fill=fill, outline="")
        fg = "#ffffff" if self.selected else TEXT
        self.create_text(S(22), self.H // 2, text=self.glyph, fill="#ffffff" if self.selected else ACCENT,
                         font="ND.Head")
        self.create_text(S(42), self.H // 2, text=self.text, fill=fg, font="ND.Body", anchor="w")
