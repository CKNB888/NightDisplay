"""全局快捷键（Windows 用 RegisterHotKey）。

只在 Windows 上启用，窗口藏到托盘也照常生效；macOS / Linux 不注册。
应急恢复 Ctrl+Alt+Shift+R 走 RegisterHotKey；黑屏快捷键见 NightKey。
默认黑屏快捷键是 C + K：白天按一下立刻黑屏；黑屏中再按一下恢复
（开着黑屏锁定时，仍要按住 C + K 解锁，快捷键不绕过锁定）。可在设置里改或关掉。
"""
import sys
import threading

IS_WIN = sys.platform.startswith("win")
MOD_KEYS = ("Ctrl", "Alt", "Shift", "Win")
DEFAULT_NIGHT_HOTKEY = ["C", "K"]
EMERGENCY_HOTKEY = ["Ctrl", "Alt", "Shift", "R"]
_MOD_BITS = {"Alt": 0x1, "Ctrl": 0x2, "Shift": 0x4, "Win": 0x8}
MOD_NOREPEAT = 0x4000
_RESERVED = [{"Ctrl", "Alt", "Del"}, {"Alt", "F4"}, {"Alt", "Tab"}, {"Ctrl", "Alt", "Shift", "R"},
             {"Win", "L"}, {"Ctrl", "Shift", "Esc"}]


def validate_hotkey(keys):
    """返回规范化后的按键列表；空列表表示关闭。规则与解锁组合键相同：同时按 2～3 个键。"""
    from .chord import validate
    if not keys:
        return []
    ks = validate(keys)
    if set(ks) in _RESERVED:
        raise ValueError("这个组合系统或应急恢复在用（Ctrl + Alt + Shift + R）。请换一个，比如 C + K。")
    return ks


class NightKey:
    """黑屏快捷键（仅 Windows）。同时按住这些键 0.3 秒触发一次，松开后才会再次触发。

    默认 C + K 不带修饰键，RegisterHotKey 做不到，所以用 GetAsyncKeyState 每 50 毫秒
    读一次（只读按键状态，不装钩子、不拦截输入，打字照常）。0.3 秒的按住门槛避免打字时
    偶然同时按到 C 和 K 就黑屏。macOS 需要“输入监控”权限且 Tk 托盘程序拿不稳，不支持。
    """

    def __init__(self, get_keys, on_fire):
        self.get_keys, self.on_fire = get_keys, on_fire
        self.stopped = threading.Event()
        self.keys = None
        self.status = "off"
        if IS_WIN:
            threading.Thread(target=self._loop, daemon=True).start()

    def stop(self):
        self.stopped.set()

    def _loop(self):
        import time

        from .chord import Reader, fmt
        from .core import log
        reader, since, armed = None, None, True
        while not self.stopped.is_set():
            try:
                keys = list(self.get_keys() or [])
            except Exception:
                keys = self.keys or []
            if keys != self.keys:
                self.keys, reader, since, armed = keys, None, None, False
                if keys:
                    try:
                        reader = Reader(keys)
                        self.status = "ok"
                        log("黑屏快捷键：同时按住 %s（再按一次恢复）" % fmt(keys))
                    except Exception as e:  # noqa: BLE001
                        self.status = "busy"
                        log("黑屏快捷键不可用：%s" % e)
                else:
                    self.status = "off"
            if reader is None:
                self.stopped.wait(1)
                continue
            down = reader.all_down()
            if not down:
                since, armed = None, True
            elif armed:
                since = since or time.monotonic()
                if time.monotonic() - since >= 0.3:
                    armed = False
                    try:
                        self.on_fire()
                    except Exception as e:  # noqa: BLE001
                        log("黑屏快捷键动作失败：%s" % e)
            time.sleep(0.05)


def to_win(keys):
    """(修饰位, 虚拟键码)。"""
    mods = 0
    vk = 0
    for k in keys:
        if k in _MOD_BITS:
            mods |= _MOD_BITS[k]
        elif k.startswith("F") and k[1:].isdigit():
            vk = 0x70 + int(k[1:]) - 1
        elif k in ("Space", "Tab", "Esc"):
            vk = {"Space": 0x20, "Tab": 0x09, "Esc": 0x1B}[k]
        else:
            vk = ord(k.upper())
    return mods | MOD_NOREPEAT, vk


class Hotkeys:
    """一个后台线程持有消息循环。apply() 换键时停掉旧线程、再注册新的。"""

    def __init__(self, on_emergency, on_night):
        self.on = {1: on_emergency, 2: on_night}
        self.thread = None
        self.tid = 0
        self.night_keys = None
        self.status = {"emergency": "off", "night": "off"}
        self._lock = threading.Lock()

    def apply(self, night_keys):
        if not IS_WIN:
            return False
        night_keys = list(night_keys or [])
        with self._lock:
            if self.thread is not None and night_keys == self.night_keys:
                return True
            self._stop()
            self.night_keys = night_keys
            ready = threading.Event()
            self.thread = threading.Thread(target=self._loop, args=(night_keys, ready), daemon=True)
            self.thread.start()
            ready.wait(2)
        return True

    def _stop(self):
        if self.thread is not None and self.tid:
            import ctypes
            ctypes.windll.user32.PostThreadMessageW(self.tid, 0x0012, 0, 0)  # WM_QUIT
            self.thread.join(2)
        self.thread, self.tid = None, 0

    def stop(self):
        with self._lock:
            self._stop()

    def _loop(self, night_keys, ready):
        import ctypes
        from ctypes import wintypes

        from .core import log
        u = ctypes.windll.user32
        k = ctypes.windll.kernel32
        msg = wintypes.MSG()
        # 先建好本线程的消息队列，PostThreadMessage 才投得进来。
        u.PeekMessageW(ctypes.byref(msg), None, 0, 0, 0)
        self.tid = k.GetCurrentThreadId()
        names = {1: EMERGENCY_HOTKEY, 2: night_keys}
        registered = []
        for hid, keys in names.items():
            label = "emergency" if hid == 1 else "night"
            if not keys:
                self.status[label] = "off"
                continue
            mods, vk = to_win(keys)
            if u.RegisterHotKey(None, hid, mods, vk):
                registered.append(hid)
                self.status[label] = "ok"
                log("全局快捷键 %s 已注册（%s）" % (" + ".join(keys), "应急恢复" if hid == 1 else "黑屏 / 恢复"))
            else:
                self.status[label] = "busy"
                log("全局快捷键 %s 注册失败，可能被别的程序占用。可以在设置里换一个。" % " + ".join(keys))
        ready.set()
        try:
            while u.GetMessageW(ctypes.byref(msg), None, 0, 0) > 0:
                if msg.message == 0x0312 and msg.wParam in self.on:
                    try:
                        self.on[msg.wParam]()
                    except Exception as e:  # noqa: BLE001
                        log("快捷键动作失败：%s" % e)
        finally:
            for hid in registered:
                u.UnregisterHotKey(None, hid)
