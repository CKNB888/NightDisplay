"""NightDisplay 主窗口：深色、圆角卡片、左侧栏、首页倒计时圆环（Mac / SwiftUI 观感）。

全部是 Tk Canvas/Frame 自绘（见 theme.py），Windows、macOS、Linux 同一套外观。
动画只在切页和开关时跑 120–220 毫秒；窗口藏起来后状态刷新也停下，托盘常驻不重画。
关闭按钮只藏到托盘；退出在托盘菜单里。
"""
import contextlib
import datetime as dt
import queue
import re
import threading
import time
import tkinter as tk
import webbrowser
from tkinter import filedialog, messagebox

from . import APP, AUTHOR, SPONSOR, VERSION
from . import backends as B
from . import theme as T
from .chord import DEFAULT_CHORD, fmt
from .core import (
    IS_MAC,
    IS_WIN,
    describe_ranges,
    export_logs,
    format_hm,
    hm_parts,
    load,
    log,
    next_switch,
    normalize_range_item,
    parse_hm,
    read_log_tail,
    schedule_mode,
    update,
)

SUBTITLE = "到点这台电脑黑屏，远程还能用。"
PAGES = (("home", "◐", "概览"), ("schedule", "◷", "时间"), ("settings", "⚙", "设置"))


def fade_in(window, steps=8, interval=18):
    """窗口淡入约 150 毫秒，只用 Tk 自带的 -alpha。系统不支持透明时直接显示。"""
    try:
        window.attributes("-alpha", 0.0)
    except tk.TclError:
        return

    def step(i=1):
        with contextlib.suppress(tk.TclError):
            window.attributes("-alpha", min(1.0, i / steps))
            if i < steps:
                window.after(interval, step, i + 1)
    window.after(0, step)


def fmt_span(seconds):
    seconds = max(0, int(seconds))
    h, rem = divmod(seconds, 3600)
    m, s = divmod(rem, 60)
    if h:
        return "%d:%02d" % (h, m) if h < 100 else "%d时" % h
    return "%d:%02d" % (m, s)


def human_span(seconds):
    seconds = max(0, int(seconds))
    h, rem = divmod(seconds, 3600)
    m, s = divmod(rem, 60)
    if h:
        return "%d 小时 %d 分" % (h, m)
    return "%d 分 %d 秒" % (m, s)


def phase_length(cfg, night):
    """主时段夜间/白天各多长（秒），给圆环算比例。"""
    try:
        s = parse_hm(cfg["start"])
        e = parse_hm(cfg["end"])
    except (KeyError, ValueError, TypeError):
        return 86400
    n = ((e[0] * 60 + e[1]) - (s[0] * 60 + s[1])) % 1440 or 1440
    return (n if night else 1440 - n or 1440) * 60


def ring_state(cfg, now=None):
    """首页圆环要显示什么：(比例, 大字, 小字, 颜色键)。纯函数，方便测试。"""
    now = now or dt.datetime.now()
    night = cfg.get("state") == "night"
    if night and cfg.get("test_id"):
        return 1.0, "测试中", "很快自己亮回来", "orange"
    if not cfg.get("enabled"):
        if night:
            return 1.0, "黑屏中", "手动黑屏 · 按键亮回来", "accent"
        return 0.0, "未启用", "到「时间」里打开", "faint"
    action, secs = next_switch(cfg, now)
    if action is None:
        return 1.0, "全天", "全天都在黑屏时段", "accent"
    total = phase_length(cfg, action == "day")
    frac = 1.0 - min(1.0, secs / float(total))
    if night or action == "day":
        return frac, fmt_span(secs), human_span(secs) + "后亮回来", "accent"
    return frac, fmt_span(secs), human_span(secs) + "后黑屏", "green"


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


def _put(widget, text):
    if str(widget.cget("text")) != text:
        widget.configure(text=text)



class MainWindow:
    def __init__(self, root, overlay, act, on_user_hide=None):
        self.root, self.overlay, self.act = root, overlay, act
        self._on_user_hide = on_user_hide
        self._hide_noted = False
        self.has_tray = False
        self._refresh_job = None
        T.setup(root)
        root.title("NightDisplay")
        root.protocol("WM_DELETE_WINDOW", self.on_close)
        root.configure(background=T.BG)
        self._last_state = None
        self._notice = ""
        self._notice_until = 0.0
        self.page = None
        self.pages = {}
        self.items = {}

        width, height = T.S(780), T.S(540)
        root.geometry("%dx%d" % (width, height))
        root.minsize(width, height)
        root.resizable(False, False)

        side = tk.Frame(root, bg=T.SIDEBAR, width=T.S(184))
        side.pack(side="left", fill="y")
        side.pack_propagate(False)
        tk.Frame(root, bg=T.BORDER, width=1).pack(side="left", fill="y")
        brand = tk.Frame(side, bg=T.SIDEBAR)
        brand.pack(fill="x", padx=T.S(16), pady=(T.S(22), T.S(18)))
        T.label(brand, "NightDisplay", font="ND.Head", bg=T.SIDEBAR).pack(anchor="w")
        T.label(brand, "v" + VERSION, font="ND.Small", fg=T.SUB, bg=T.SIDEBAR).pack(anchor="w")
        for key, glyph, text in PAGES:
            item = T.SideItem(side, glyph, text, lambda k=key: self.show_page(k))
            item.pack(padx=T.S(6), pady=T.S(1))
            self.items[key] = item
        T.label(side, "传康Kk\n1837620622", font="ND.Small", fg=T.FAINT, bg=T.SIDEBAR).pack(
            side="bottom", anchor="w", padx=T.S(18), pady=T.S(16))

        self.content = tk.Frame(root, bg=T.BG)
        self.content.pack(side="left", fill="both", expand=True)
        cfg = load()
        self._build_home()
        self._build_schedule(cfg)
        self._build_settings(cfg)
        self.show_page("home", animate=False)
        self.refresh(schedule=False)
        root.bind("<Map>", self._on_map, add="+")
        self._schedule_refresh()
        self.root.after(3000, lambda: self.check_update(quiet=True))

    # ---------- 页面骨架 ----------

    def _page(self, key, title, subtitle):
        page = tk.Frame(self.content, bg=T.BG)
        head = tk.Frame(page, bg=T.BG)
        head.pack(fill="x", padx=T.S(28), pady=(T.S(24), T.S(14)))
        T.label(head, title, font="ND.Title", bg=T.BG).pack(anchor="w")
        T.label(head, subtitle, font="ND.Small", fg=T.SUB, bg=T.BG).pack(anchor="w", pady=(T.S(2), 0))
        self.pages[key] = page
        return page

    def _card(self, parent, **pack):
        card = T.Card(parent)
        card.pack(fill="x", padx=T.S(28), pady=(0, T.S(12)), **pack)
        return card.body

    def show_page(self, key, animate=True):
        if key == self.page:
            return
        old = self.pages.get(self.page)
        self.page = key
        for k, item in self.items.items():
            item.set_selected(k == key)
        new = self.pages[key]
        if old is not None:
            old.place_forget()
        if not animate:
            new.place(x=0, y=0, relwidth=1, relheight=1)
            return
        offset = T.S(28)
        new.place(x=offset, y=0, relwidth=1, relheight=1)
        T.animate(new, 180, lambda t: new.place_configure(x=int(offset * (1 - t))), frames=8)
        if key == "home":
            self.refresh(schedule=False, animate_ring=True)

    # ---------- 概览 ----------

    def _build_home(self):
        page = self._page("home", "概览", SUBTITLE)
        top = T.Card(page)
        top.pack(fill="x", padx=T.S(28), pady=(0, T.S(12)))
        body = top.body
        self.ring = T.Ring(body, size=196)
        self.ring.pack(side="left", padx=(0, T.S(22)))
        info = tk.Frame(body, bg=T.CARD)
        info.pack(side="left", fill="both", expand=True)
        dot_row = tk.Frame(info, bg=T.CARD)
        dot_row.pack(anchor="w", pady=(T.S(14), 0))
        self.dot = tk.Canvas(dot_row, width=T.S(10), height=T.S(10), bg=T.CARD, highlightthickness=0)
        self.dot.pack(side="left", padx=(0, T.S(8)))
        self.state_text = T.label(dot_row, "屏幕正常显示", font="ND.Head")
        self.state_text.pack(side="left")
        self.detail_text = T.label(info, "", font="ND.Body", fg=T.SUB)
        self.detail_text.pack(anchor="w", pady=(T.S(8), 0))
        self.mode_text = T.label(info, "", font="ND.Small", fg=T.SUB, wraplength=T.S(300))
        self.mode_text.pack(anchor="w", pady=(T.S(4), 0))
        self.hint_text = T.label(info, "", font="ND.Body", wraplength=T.S(300))
        self.hint_text.pack(anchor="w", pady=(T.S(14), 0))

        actions = self._card(page)
        row = tk.Frame(actions, bg=T.CARD)
        row.pack(fill="x")
        self.home_enable = T.Button(row, "启用每天自动黑屏", lambda: self.safe(self.toggle), kind="primary")
        self.home_enable.pack(side="right")
        T.Button(row, "测试 15 秒", lambda: self.safe(self.test)).pack(side="left")
        T.Button(row, "现在就黑屏", lambda: self.safe(self.night)).pack(side="left", padx=(T.S(8), 0))
        T.Button(row, "恢复并暂停", lambda: self.safe(self.emergency), kind="ghost").pack(side="left", padx=(T.S(8), 0))
        self.unlock_text = T.label(page, "亮回来：同时按住 C 和 K", font="ND.Small", fg=T.SUB, bg=T.BG)
        self.unlock_text.pack(anchor="w", padx=T.S(32))

    # ---------- 时间 ----------

    def _build_schedule(self, cfg):
        page = self._page("schedule", "时间", "选好每天几点黑、几点亮。装完默认已经开着。")
        # 总开关
        plan = self._card(page)
        row = tk.Frame(plan, bg=T.CARD)
        row.pack(fill="x")
        text = tk.Frame(row, bg=T.CARD)
        text.pack(side="left", fill="x", expand=True)
        T.label(text, "每天自动黑屏", font="ND.Bold").pack(anchor="w")
        self.plan_text = T.label(text, "", font="ND.Small", fg=T.SUB)
        self.plan_text.pack(anchor="w", pady=(T.S(2), 0))
        self.enabled_var = tk.BooleanVar(value=bool(cfg.get("enabled")))
        self.enable_switch = T.Switch(row, self.enabled_var, lambda: self.safe(self._switch_plan))
        self.enable_switch.pack(side="right")

        # 单段 / 多段
        mode_card = self._card(page)
        head = tk.Frame(mode_card, bg=T.CARD)
        head.pack(fill="x")
        T.label(head, "怎么排时间", font="ND.Bold").pack(side="left")
        mode = schedule_mode(cfg)
        self.schedule_mode_var = tk.StringVar(value=mode)
        T.Segmented(head, self.schedule_mode_var,
                    (("single", "单段"), ("multi", "多段")),
                    on_change=lambda: self.safe(self._switch_schedule_mode)).pack(side="right")
        self.schedule_help = T.label(
            mode_card,
            "",
            font="ND.Small", fg=T.SUB, wraplength=T.S(480))
        self.schedule_help.pack(anchor="w", pady=(T.S(8), 0))

        # 单段：主时段
        self.single_box = tk.Frame(page, bg=T.BG)
        body = self._card(self.single_box)
        sh, sm = hm_parts(cfg.get("start"), (19, 30))
        eh, em = hm_parts(cfg.get("end"), (10, 30))
        self.start_h = tk.StringVar(value="%02d" % sh)
        self.start_m = tk.StringVar(value="%02d" % sm)
        self.end_h = tk.StringVar(value="%02d" % eh)
        self.end_m = tk.StringVar(value="%02d" % em)
        for i, (name, hv, mv, glyph) in enumerate((("开始黑屏", self.start_h, self.start_m, "☾"),
                                                   ("结束亮屏", self.end_h, self.end_m, "☀"))):
            if i:
                tk.Frame(body, bg=T.BORDER, height=1).pack(fill="x", pady=T.S(12))
            row = tk.Frame(body, bg=T.CARD)
            row.pack(fill="x")
            T.label(row, glyph + "  " + name, font="ND.Bold").pack(side="left")
            box = tk.Frame(row, bg=T.CARD)
            box.pack(side="right")
            T.Dropdown(box, hv, 23, self._picked_time).pack(side="left")
            T.label(box, "时", font="ND.Small", fg=T.SUB).pack(side="left", padx=(T.S(4), T.S(8)))
            T.Dropdown(box, mv, 59, self._picked_time).pack(side="left")
            T.label(box, "分", font="ND.Small", fg=T.SUB).pack(side="left", padx=(T.S(4), 0))
        T.label(self.single_box,
                "点开下拉选时间（24 小时制）。可以跨午夜，比如 19:30 黑 → 次日 10:30 亮。",
                font="ND.Small", fg=T.SUB, bg=T.BG, wraplength=T.S(520)).pack(
                    anchor="w", padx=T.S(32), pady=(0, T.S(8)))

        # 多段
        self.multi_box = tk.Frame(page, bg=T.BG)
        multi_card = self._card(self.multi_box)
        mhead = tk.Frame(multi_card, bg=T.CARD)
        mhead.pack(fill="x")
        T.label(mhead, "黑屏时段", font="ND.Bold").pack(side="left")
        self.multi_add_btn = T.Button(
            mhead, "＋ 再加一段",
            lambda: self.safe(lambda: self._add_multi_row(from_user=True)), kind="ghost")
        self.multi_add_btn.pack(side="right")
        T.label(multi_card,
                "每一段单独开关。关掉某一段，只是这一段不黑，其它段照旧。"
                "两段中间的时间是亮屏（空隙）。",
                font="ND.Small", fg=T.SUB, wraplength=T.S(480)).pack(anchor="w", pady=(T.S(6), 0))
        self.multi_rows_host = tk.Frame(multi_card, bg=T.CARD)
        self.multi_rows_host.pack(fill="x", pady=(T.S(8), 0))
        self.multi_rows = []
        # 主时段行
        self._add_multi_row(cfg.get("start", "19:30"), cfg.get("end", "10:30"),
                            enabled=bool(cfg.get("primary_enabled", True)), primary=True)
        for pair in cfg.get("extra_ranges") or []:
            try:
                item = normalize_range_item(pair)
            except ValueError:
                continue
            self._add_multi_row(item["start"], item["end"], enabled=item["enabled"], primary=False)
        T.label(self.multi_box,
                "时间都用下拉选（00–23 时 / 00–59 分），不能手打，免得写错。",
                font="ND.Small", fg=T.SUB, bg=T.BG, wraplength=T.S(520)).pack(
                    anchor="w", padx=T.S(32), pady=(0, T.S(8)))

        self._show_schedule_mode(mode)

    def _schedule_help_text(self, mode):
        if mode == "multi":
            return "多段：比如晚上黑一截、中午再黑一截。段与段之间自动亮屏。"
        return "单段：一天只有一段黑屏。默认 19:30 黑，次日 10:30 亮。"

    def _show_schedule_mode(self, mode):
        _put(self.schedule_help, self._schedule_help_text(mode))
        # pack_forget then pack to toggle
        self.single_box.pack_forget()
        self.multi_box.pack_forget()
        if mode == "multi":
            self.multi_box.pack(fill="x")
        else:
            self.single_box.pack(fill="x")

    def _switch_schedule_mode(self):
        mode = self.schedule_mode_var.get()
        if mode not in ("single", "multi"):
            mode = "single"
        prev = schedule_mode(load())
        update(schedule_mode=mode)
        self._show_schedule_mode(mode)
        if mode == "multi" and prev != "multi":
            # 切到多段时，把当前单段时间同步进第一行
            if self.multi_rows:
                p = self.multi_rows[0]["picked"]
                p["sh"].set(self.start_h.get())
                p["sm"].set(self.start_m.get())
                p["eh"].set(self.end_h.get())
                p["em"].set(self.end_m.get())
            self._persist_multi_ranges(note=True)
        elif mode == "single" and prev != "single":
            self._note("已改回单段。多出来的时段先留着但不生效；再切回「多段」还能用。")
            if load().get("enabled"):
                from .app import ensure_default_schedule
                ensure_default_schedule()
        self.refresh(schedule=False)

    def _add_multi_row(self, start="12:00", end="13:30", enabled=True, primary=False, from_user=False):
        # 主时段 1 行 + 额外最多 4 行
        extras = [r for r in self.multi_rows if not r["primary"]]
        if from_user and len(extras) >= 4:
            raise RuntimeError("最多再加 4 段。不需要的段可以关掉，或点「去掉」。")
        a, b = hm_parts(start, (12, 0)), hm_parts(end, (13, 30))
        line = tk.Frame(self.multi_rows_host, bg=T.CARD)
        line.pack(fill="x", pady=(T.S(6), 0))
        title = "第 1 段（主）" if primary else ("第 %d 段" % (len(self.multi_rows) + 1))
        T.label(line, title, font="ND.Small", fg=T.SUB, width=10).pack(side="left")
        picked = {k: tk.StringVar(value="%02d" % v) for k, v in
                  (("sh", a[0]), ("sm", a[1]), ("eh", b[0]), ("em", b[1]))}
        on_var = tk.BooleanVar(value=bool(enabled))
        T.Dropdown(line, picked["sh"], 23, lambda: self.safe(self._persist_multi_ranges)).pack(side="left")
        T.label(line, "时", font="ND.Small", fg=T.SUB).pack(side="left", padx=(T.S(2), T.S(4)))
        T.Dropdown(line, picked["sm"], 59, lambda: self.safe(self._persist_multi_ranges)).pack(side="left")
        T.label(line, "分", font="ND.Small", fg=T.SUB).pack(side="left", padx=(T.S(2), T.S(6)))
        T.label(line, "到", fg=T.SUB).pack(side="left", padx=(0, T.S(6)))
        T.Dropdown(line, picked["eh"], 23, lambda: self.safe(self._persist_multi_ranges)).pack(side="left")
        T.label(line, "时", font="ND.Small", fg=T.SUB).pack(side="left", padx=(T.S(2), T.S(4)))
        T.Dropdown(line, picked["em"], 59, lambda: self.safe(self._persist_multi_ranges)).pack(side="left")
        T.label(line, "分", font="ND.Small", fg=T.SUB).pack(side="left", padx=(T.S(2), T.S(8)))
        sw = T.Switch(line, on_var, lambda: self.safe(self._persist_multi_ranges), bg=T.CARD)
        sw.pack(side="left", padx=(T.S(4), 0))
        item = {"frame": line, "picked": picked, "on": on_var, "primary": primary, "switch": sw}

        def remove(current=item):
            if current["primary"]:
                raise RuntimeError("主时段不能去掉。不想用就关掉右边的开关，或改回「单段」。")
            current["frame"].destroy()
            self.multi_rows.remove(current)
            self._renumber_multi_rows()
            self.multi_add_btn.set_enabled(len([r for r in self.multi_rows if not r["primary"]]) < 4)
            self.safe(self._persist_multi_ranges)

        if not primary:
            T.Button(line, "去掉", remove, kind="danger").pack(side="left", padx=(T.S(8), 0))
        self.multi_rows.append(item)
        self.multi_add_btn.set_enabled(len([r for r in self.multi_rows if not r["primary"]]) < 4)
        if from_user:
            self._persist_multi_ranges(note=True)

    def _renumber_multi_rows(self):
        for i, item in enumerate(self.multi_rows):
            # first label child is the title
            kids = item["frame"].winfo_children()
            if kids:
                title = "第 1 段（主）" if item["primary"] else ("第 %d 段" % (i + 1))
                with contextlib.suppress(tk.TclError):
                    kids[0].configure(text=title)

    def _persist_multi_ranges(self, note=False):
        if not getattr(self, "multi_rows", None):
            return
        primary = next((r for r in self.multi_rows if r["primary"]), None)
        if primary is None:
            return
        p = primary["picked"]
        start = format_hm(p["sh"].get(), p["sm"].get())
        end = format_hm(p["eh"].get(), p["em"].get())
        if start == end:
            raise ValueError("每一段的开始和结束不能选同一个时间。")
        extras = []
        for item in self.multi_rows:
            if item["primary"]:
                continue
            q = item["picked"]
            a = format_hm(q["sh"].get(), q["sm"].get())
            b = format_hm(q["eh"].get(), q["em"].get())
            if a == b:
                raise ValueError("每一段的开始和结束不能选同一个时间。")
            extras.append({"start": a, "end": b, "enabled": bool(item["on"].get())})
        # 同步单段下拉显示
        self.start_h.set("%02d" % parse_hm(start)[0])
        self.start_m.set("%02d" % parse_hm(start)[1])
        self.end_h.set("%02d" % parse_hm(end)[0])
        self.end_m.set("%02d" % parse_hm(end)[1])
        update(start=start, end=end, primary_enabled=bool(primary["on"].get()),
               extra_ranges=extras, schedule_mode="multi")
        if load().get("enabled"):
            from .app import ensure_default_schedule
            ensure_default_schedule()
        if note:
            self._note("多段时间已保存。" + describe_ranges(load()))
        self.refresh(schedule=False)

    def _switch_plan(self):
        want = self.enabled_var.get()
        if want == bool(load().get("enabled")):
            return None
        try:
            return self.toggle()
        except Exception:
            self.enabled_var.set(bool(load().get("enabled")))
            raise

    # ---------- 设置 ----------

    def _build_settings(self, cfg):
        from . import chord
        page = self._page("settings", "设置", AUTHOR + "  ·  " + SPONSOR)
        scroll = tk.Frame(page, bg=T.BG)
        scroll.pack(fill="both", expand=True)

        keys = self._card(scroll)
        row = tk.Frame(keys, bg=T.CARD)
        row.pack(fill="x")
        left = tk.Frame(row, bg=T.CARD)
        left.pack(side="left")
        T.label(left, "亮回来的按键", font="ND.Bold").pack(anchor="w")
        T.label(left, "黑屏后同时按住，大约半秒。", font="ND.Small", fg=T.SUB).pack(anchor="w")
        self.chord_var = tk.StringVar(value=chord.fmt(cfg.get("unlock_chord") or chord.DEFAULT_CHORD))
        held, self._recording = [], {"on": False}
        T.Button(row, "改按键", lambda: record(), kind="secondary").pack(side="right")
        tk.Label(row, textvariable=self.chord_var, font="ND.Mono", fg=T.ACCENT, bg=T.CARD).pack(
            side="right", padx=T.S(12))

        def record():
            held.clear()
            self._recording["on"] = True
            self.chord_var.set("请同时按下两个键")
            self.root.bind("<KeyPress>", down)
            self.root.bind("<KeyRelease>", up)
            self.root.focus_set()

        def stop():
            self._recording["on"] = False
            self.root.unbind("<KeyPress>")
            self.root.unbind("<KeyRelease>")
        self._stop_recording = stop

        def down(event):
            if self._recording["on"]:
                with contextlib.suppress(ValueError):
                    key = chord.normalize(event.keysym)
                    if key not in held and len(held) < 3:
                        held.append(key)
                    self.chord_var.set(chord.fmt(held))
                return "break"

        def up(_event):
            if self._recording["on"]:
                stop()
                if len(held) < 2:
                    self.chord_var.set(chord.fmt(load().get("unlock_chord") or chord.DEFAULT_CHORD))
                    messagebox.showinfo(APP, "按得太少了。请同时按住两个键，比如 C 和 K。", parent=self.root)
                else:
                    self.chord_var.set(chord.fmt(held))
                return "break"

        if IS_WIN:
            # 黑屏快捷键（只在 Windows 上有虚拟屏方案，所以只在 Windows 上显示）。
            hk = self._card(scroll)
            hrow = tk.Frame(hk, bg=T.CARD)
            hrow.pack(fill="x")
            hl = tk.Frame(hrow, bg=T.CARD)
            hl.pack(side="left")
            T.label(hl, "黑屏快捷键", font="ND.Bold").pack(anchor="w")
            T.label(hl, "按一次黑屏，再按一次亮屏。", font="ND.Small",
                    fg=T.SUB).pack(anchor="w")
            cur = cfg.get("night_hotkey", ["C", "K"])
            self.hotkey_var = tk.StringVar(value=chord.fmt(cur) if cur else "关闭")
            hheld = []
            T.Button(hrow, "关闭", lambda: self.hotkey_var.set("关闭"), kind="secondary").pack(side="right")
            T.Button(hrow, "改快捷键", lambda: hrecord(), kind="secondary").pack(side="right", padx=(0, T.S(8)))
            tk.Label(hrow, textvariable=self.hotkey_var, font="ND.Mono", fg=T.ACCENT, bg=T.CARD).pack(
                side="right", padx=T.S(12))

            def hrecord():
                hheld.clear()
                self._recording["on"] = True
                self.hotkey_var.set("请同时按下两个键")
                self.root.bind("<KeyPress>", hdown)
                self.root.bind("<KeyRelease>", hup)
                self.root.focus_set()

            def hdown(event):
                if self._recording["on"]:
                    with contextlib.suppress(ValueError):
                        key = chord.normalize(event.keysym)
                        if key not in hheld and len(hheld) < 3:
                            hheld.append(key)
                        self.hotkey_var.set(chord.fmt(hheld))
                    return "break"

            def hup(_event):
                if self._recording["on"]:
                    stop()
                    if len(hheld) < 2:
                        old = load().get("night_hotkey", ["C", "K"])
                        self.hotkey_var.set(chord.fmt(old) if old else "关闭")
                        messagebox.showinfo(APP, "按得太少了。请同时按住两个键，比如 C 和 K。", parent=self.root)
                    else:
                        self.hotkey_var.set(chord.fmt(hheld))
                    return "break"

        opts = self._card(scroll)
        self.lock_var = tk.BooleanVar(value=cfg.get("lock", True))
        self.autostart_var = tk.BooleanVar(value=cfg.get("autostart_preference", cfg.get("autostart", True)))
        for i, (text, var) in enumerate((("黑屏时，只有上面的按键能亮回来", self.lock_var),
                                         ("开机登录后自动运行", self.autostart_var))):
            if i:
                tk.Frame(opts, bg=T.BORDER, height=1).pack(fill="x", pady=T.S(10))
            r = tk.Frame(opts, bg=T.CARD)
            r.pack(fill="x")
            T.label(r, text).pack(side="left")
            T.Switch(r, var).pack(side="right")

        bar = tk.Frame(scroll, bg=T.BG)
        bar.pack(fill="x", padx=T.S(28), pady=(T.S(2), 0))
        T.Button(bar, "保存设置", lambda: self.safe(self.save_settings), kind="primary", bg=T.BG).pack(side="right")
        T.Button(bar, "运行日志", lambda: self.safe(self.logs), bg=T.BG).pack(side="left")
        T.Button(bar, "导出日志", lambda: self.safe(self.export_saved_logs), bg=T.BG).pack(side="left", padx=(T.S(8), 0))
        T.Button(bar, "检查更新", lambda: self.check_update(), bg=T.BG).pack(side="left", padx=(T.S(8), 0))
        bar2 = tk.Frame(scroll, bg=T.BG)
        bar2.pack(fill="x", padx=T.S(28), pady=(T.S(10), 0))
        if IS_WIN:
            T.Button(bar2, "远程看不到画面时，再修一次", lambda: self.safe(self.install_driver), bg=T.BG).pack(side="left")
        T.Button(bar2, "取消自动黑屏并取消开机运行", lambda: self.safe(self.remove), kind="danger", bg=T.BG).pack(side="right")

    def save_settings(self):
        from . import chord
        blocked = settings_block_message(load())
        if blocked:
            raise RuntimeError(blocked)
        if self._recording["on"] or "请同时" in self.chord_var.get():
            self._stop_recording()
            raise RuntimeError("按键还没设好。请点「改按键」，同时按住两个键，比如 C 和 K。")
        keys = chord.validate([k.strip() for k in re.split(r"[+＋]", self.chord_var.get())])
        extra = {}
        hv = getattr(self, "hotkey_var", None)
        if hv is not None:
            from .hotkey import validate_hotkey
            if "请同时" in hv.get():
                raise RuntimeError("黑屏快捷键还没设好。请点「改快捷键」，同时按住两个键，比如 C 和 K。")
            text = hv.get().strip()
            extra["night_hotkey"] = [] if text == "关闭" else validate_hotkey(
                [k.strip() for k in re.split(r"[+＋]", text)])
        B.set_autostart(self.autostart_var.get())
        update(unlock_chord=keys, lock=self.lock_var.get(),
               autostart=self.autostart_var.get(), autostart_preference=self.autostart_var.get(), **extra)
        self._note("设置已保存。")
        self.show_page("home")

    # ---------- 行为（和旧版一致） ----------

    def safe(self, fn):
        try:
            result = fn()
            if isinstance(result, str) and result.startswith("error:"):
                raise RuntimeError(result[6:].strip())
            if result == "locked":
                raise RuntimeError(lock_hint())
            if self.root.winfo_exists():
                self.refresh(schedule=False, animate_ring=True)
            return result
        except Exception as e:
            log("界面操作失败：%s" % e)
            messagebox.showerror(APP, str(e), parent=self.root)

    def _show_saved_time(self, cfg):
        sh, sm = hm_parts(cfg.get("start"), (19, 30))
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
        cfg = load()
        if format_hm(self.start_h.get(), self.start_m.get()) == cfg.get("start") and \
                format_hm(self.end_h.get(), self.end_m.get()) == cfg.get("end"):
            return
        try:
            self.save_times()
        except Exception:
            self._show_saved_time(load())
            raise
        if load().get("enabled"):
            self._note("时间已改。每天自动黑屏会按这个时间走。")
        else:
            self._note("时间已经记下。打开「每天自动黑屏」后生效。")

    def install_driver(self):
        from .app import ensure_environment
        report = ensure_environment(force=True)
        if report.get("virtual_display"):
            messagebox.showinfo(APP, "已经修好了。先点「测试 15 秒」，看远程还能不能看到画面。", parent=self.root)
        else:
            messagebox.showinfo(APP, "远程屏幕这次没装上。程序会改用黑幕或关屏，这台电脑仍然能黑。", parent=self.root)

    def _note(self, text, seconds=4):
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
                APP, "屏幕会马上变黑。\n要亮回来，同时按住 %s，大约半秒。\n\n确定现在就黑屏吗？" % keys,
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
        from .app import pause, resume
        if load().get("enabled"):
            pause()
            self._note("每天自动黑屏已暂停。开机后程序仍会在后台运行。")
            return None
        # 恢复时绝不能把界面下拉的当前值写回配置（暂停前改过界面、或控件未同步都会把时段推迟/改掉）。
        cfg = load()
        self._show_saved_time(cfg)
        resume()
        self._note("已经启用。仍按保存的时间：%s。" % describe_ranges(load()))
        return self.act("reconcile")

    # ---------- 刷新 ----------

    def _viewable(self):
        try:
            return bool(self.root.winfo_viewable())
        except tk.TclError:
            return False

    def _schedule_refresh(self):
        if self._refresh_job is None:
            with contextlib.suppress(tk.TclError):
                self._refresh_job = self.root.after(1000, self._tick)

    def _tick(self):
        self._refresh_job = None
        # 窗口藏到托盘后不再每秒读配置、重画圆环；<Map> 时再接上。
        if not self._viewable():
            return
        self.refresh(schedule=False)
        self._schedule_refresh()

    def _on_map(self, event):
        if event.widget is self.root:
            self.refresh(schedule=False, animate_ring=True)
            self._schedule_refresh()

    def refresh(self, schedule=True, animate_ring=False):
        try:
            cfg = load()
            night = cfg["state"] == "night"
            if night:
                self._notice = ""
            elif self._last_state == "night":
                self._note("已恢复显示")
            self._last_state = cfg["state"]
            keys = fmt(cfg.get("unlock_chord") or DEFAULT_CHORD)
            _put(self.unlock_text, "亮回来：同时按住 " + keys)
            _put(self.state_text, "正在黑屏" if night else "屏幕正常显示")
            self.dot.delete("all")
            self.dot.create_oval(1, 1, T.S(9), T.S(9), outline="",
                                 fill=T.ACCENT if night else (T.GREEN if cfg["enabled"] else T.FAINT))
            span = describe_ranges(cfg)
            plan = "每天自动黑屏已开" if cfg["enabled"] else "每天自动黑屏还没开"
            _put(self.detail_text, "%s  ·  %s" % (span, plan))
            _put(self.plan_text, ("已开 · " if cfg["enabled"] else "已关 · ") + span)
            if self.enabled_var.get() != bool(cfg["enabled"]):
                self.enabled_var.set(bool(cfg["enabled"]))
            names = {
                "virtual_only": "你这边黑屏，远程还能用",
                "overlay": "你这边盖上黑幕，远程还能用",
                "screen_off": "关掉屏幕。动一下鼠标会亮",
            }
            _put(self.mode_text, ("方式：" + names[cfg["night_mode"]]) if night and cfg.get("night_mode") in names else "")
            tested = cfg.get("last_test_mode")
            if night and cfg.get("test_id"):
                message = "正在测试。大约 15 秒后自己亮回来。"
            elif night:
                if cfg.get("night_mode") == "screen_off":
                    message = "现在是关屏，碰一下会亮。要亮着不动，同时按住 %s。" % keys
                elif cfg.get("lock", True):
                    message = "你这边是黑的。同时按住 %s 才会亮。" % keys
                else:
                    message = "你这边是黑的。要亮回来，点「恢复并暂停」。"
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
            self.home_enable.set_text("暂停每天自动黑屏" if cfg["enabled"] else "启用每天自动黑屏")
            self.home_enable.set_kind("secondary" if cfg["enabled"] else "primary")
            if self.page == "home":
                frac, big, small, color = ring_state(cfg)
                colors = {"accent": T.ACCENT, "green": T.GREEN, "orange": T.ORANGE, "faint": T.FAINT}
                self.ring.show(frac, big, small, colors[color], animate_to=animate_ring)
        except Exception as e:
            log("界面状态刷新失败：%s" % e)
        finally:
            if schedule:
                self._schedule_refresh()

    # ---------- 日志 / 更新 / 卸载 ----------

    def logs(self):
        window = tk.Toplevel(self.root, bg=T.BG)
        window.title("运行日志")
        window.transient(self.root)
        T.dark_titlebar(window)
        bar = tk.Frame(window, bg=T.BG)
        bar.pack(fill="x", padx=T.S(12), pady=T.S(10))
        T.Button(bar, "导出日志", lambda: self.safe(lambda: self.export_saved_logs(window)), bg=T.BG).pack(side="left")
        T.label(bar, "最近的记录。导出会把保存下来的日志全部存成一个文件。", font="ND.Small", fg=T.SUB, bg=T.BG).pack(
            side="left", padx=(T.S(10), 0))
        text = tk.Text(window, width=88, height=24, wrap="word", bg=T.CARD, fg=T.TEXT, insertbackground=T.TEXT,
                       relief="flat", highlightthickness=0, padx=T.S(10), pady=T.S(8), font="ND.Small")
        text.pack(fill="both", expand=True, padx=T.S(12), pady=(0, T.S(12)))
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
                self._offer_update(result, quiet=quiet)
            elif not quiet:
                messagebox.showinfo(APP, "当前已是最新正式版本。", parent=self.root)

        self.root.after(100, poll)

    def _offer_update(self, result, quiet=False):
        """发现新版本：一点确认 → 下载 → cleanup_old → 静默安装 → 拉起新版。"""
        from . import updates
        ver = result["version"]
        setup = (result.get("setup_url") or "") if IS_WIN else ""
        if not setup:
            if messagebox.askyesno(APP, "发现新版本 %s，打开下载页？" % ver, parent=self.root):
                webbrowser.open(result["url"])
            return
        msg = ("发现新版本 %s。\n\n立即更新？\n（自动下载、清理旧版、静默安装，完成后自己打开）"
               % ver)
        if not messagebox.askyesno(APP, msg, parent=self.root):
            return
        self._note("正在下载并安装 %s…" % ver)
        # 藏到托盘，少打扰；安装脚本会结束本进程再拉新版。
        with contextlib.suppress(Exception):
            self.root.withdraw()
        box = queue.Queue()

        def work():
            try:
                path = updates.download_setup(setup)
                updates.install_silent(path)
                box.put((True, path))
            except Exception as e:
                box.put((False, str(e)))

        threading.Thread(target=work, daemon=True).start()

        def poll():
            try:
                ok, payload = box.get_nowait()
            except queue.Empty:
                self.root.after(200, poll)
                return
            if not ok:
                log("自动更新失败：%s" % payload)
                with contextlib.suppress(Exception):
                    self.root.deiconify()
                messagebox.showerror(
                    APP,
                    "更新失败：%s\n\n可到设置里再点「检查更新」，或手动打开下载页。" % payload,
                    parent=self.root)
                if messagebox.askyesno(APP, "打开下载页？", parent=self.root):
                    webbrowser.open(result["url"])
                return
            log("已启动静默安装：%s（cleanup_old + /VERYSILENT）" % payload)
            self._note("正在安装新版本，稍后会自动打开…")

        self.root.after(200, poll)


    def remove(self):
        from .app import uninstall
        if load().get("state") == "night" and load().get("lock", True):
            raise RuntimeError(lock_hint())
        uninstall(self.overlay)
        self.root.destroy()

    def on_close(self):
        # 点窗口 X：强制进托盘后台，不退出。计划、开机自启、守护任务都还在。
        # 真正退出只在托盘「退出 NightDisplay」。
        if not self.has_tray:
            icon = start_tray(self.root, self.act)
            self.has_tray = bool(icon)
            if icon:
                self._tray_icon = icon
        if not self.has_tray:
            # 托盘起不来就别把窗口藏死，否则用户找不到入口。
            messagebox.showwarning(
                APP,
                "托盘图标没起来，窗口不能藏到后台。\n请留着这个窗口，或再开一次程序。",
                parent=self.root)
            return
        try:
            self.root.withdraw()
        except tk.TclError:
            return
        if self._refresh_job is not None:
            with contextlib.suppress(tk.TclError):
                self.root.after_cancel(self._refresh_job)
            self._refresh_job = None
        import gc
        gc.collect()
        if not self._hide_noted:
            self._hide_noted = True
            log("窗口已隐藏到托盘（点 X 强制后台）。每天的时间和开机登录都还在。")
            if self._on_user_hide:
                self._on_user_hide()

    def open_settings(self):
        self.show_page("settings", animate=False)


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
    # 深色圆底 + 白色月牙，深浅任务栏上都看得见。
    draw.ellipse((2, 2, 62, 62), fill=(44, 44, 46, 255), outline=(90, 90, 96, 255), width=2)
    draw.ellipse((14, 12, 50, 48), fill=(245, 245, 247, 255))
    draw.ellipse((24, 6, 58, 40), fill=(44, 44, 46, 255))
    menu = pystray.Menu(
        pystray.MenuItem("打开 NightDisplay", lambda: act("show"), default=True),
        pystray.MenuItem("现在就黑屏", lambda: act("night")),
        pystray.MenuItem("现在恢复屏幕", lambda: act("day")),
        pystray.MenuItem("恢复屏幕并暂停", lambda: act("emergency")),
        pystray.MenuItem("暂停每天自动黑屏", lambda: act("pause")),
        pystray.MenuItem("设置…", lambda: act("settings")),
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

