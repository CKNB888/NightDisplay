# -*- coding: utf-8 -*-
"""解锁组合键检测：只“读取”按键状态，不拦截、不吞掉任何按键（输入照常传给系统）。
Windows: GetAsyncKeyState；macOS: CGEventSourceKeyState（需“输入监控”权限）；Linux X11: XQueryKeymap。"""
import ctypes, ctypes.util, sys, time, threading

IS_WIN, IS_MAC = sys.platform.startswith("win"), sys.platform == "darwin"
MODS = {"CTRL": "Ctrl", "CONTROL": "Ctrl", "ALT": "Alt", "OPTION": "Alt", "SHIFT": "Shift", "WIN": "Win", "CMD": "Win", "SUPER": "Win", "META": "Win"}
DEFAULT_CHORD = ["C", "K"]


def normalize(name):
    n = str(name).strip()
    u = n.upper().replace("_L", "").replace("_R", "")
    if u in MODS:
        return MODS[u]
    if len(n) == 1 and n.isalnum():
        return n.upper()
    if u.startswith("F") and u[1:].isdigit() and 1 <= int(u[1:]) <= 12:
        return u
    if u in ("SPACE", "TAB", "ESCAPE", "ESC"):
        return {"ESCAPE": "Esc", "ESC": "Esc"}.get(u, u.title())
    raise ValueError("不支持的按键：%s（支持字母、数字、F1-F12、Ctrl/Alt/Shift/Win、Space/Tab/Esc）" % name)


def validate(keys):
    ks = []
    for k in keys:
        k = normalize(k)
        if k not in ks:
            ks.append(k)
    if not 2 <= len(ks) <= 3:
        raise ValueError("组合键需要 2~3 个不同的按键")
    if all(k in ("Ctrl", "Alt", "Shift", "Win") for k in ks):
        raise ValueError("组合键至少要包含一个普通按键（字母/数字/F 键等）")
    if set(ks) in ({"Ctrl", "Alt", "Esc"}, {"Alt", "Tab"}, {"Alt", "F4"}):
        raise ValueError("该组合与系统快捷键冲突，请换一个")
    return ks


def fmt(keys):
    return " + ".join(keys)

# ---------- 平台读取 ----------
_WIN_VK = {"Ctrl": 0x11, "Alt": 0x12, "Shift": 0x10, "Win": 0x5B, "Space": 0x20, "Tab": 0x09, "Esc": 0x1B}
_MAC_KC = {"A": 0, "S": 1, "D": 2, "F": 3, "H": 4, "G": 5, "Z": 6, "X": 7, "C": 8, "V": 9, "B": 11, "Q": 12, "W": 13, "E": 14,
           "R": 15, "Y": 16, "T": 17, "1": 18, "2": 19, "3": 20, "4": 21, "6": 22, "5": 23, "9": 25, "7": 26, "8": 28, "0": 29,
           "O": 31, "U": 32, "I": 34, "P": 35, "L": 37, "J": 38, "K": 40, "N": 45, "M": 46, "Tab": 48, "Space": 49, "Esc": 53,
           "Win": 55, "Shift": 56, "Alt": 58, "Ctrl": 59, "F1": 122, "F2": 120, "F3": 99, "F4": 118, "F5": 96, "F6": 97,
           "F7": 98, "F8": 100, "F9": 101, "F10": 109, "F11": 103, "F12": 111}


class Reader:
    def __init__(self, keys):
        self.keys = keys
        if IS_WIN:
            self.u = ctypes.windll.user32
            self.codes = [_WIN_VK.get(k) or (0x70 + int(k[1:]) - 1 if k.startswith("F") and len(k) > 1 else ord(k)) for k in keys]
        elif IS_MAC:
            self.q = ctypes.CDLL("/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices")
            self.q.CGEventSourceKeyState.restype = ctypes.c_bool
            self.q.CGEventSourceKeyState.argtypes = [ctypes.c_int32, ctypes.c_uint16]
            self.codes = [_MAC_KC[k] for k in keys]
        else:
            x = ctypes.CDLL(ctypes.util.find_library("X11") or "libX11.so.6")
            x.XOpenDisplay.restype = ctypes.c_void_p
            x.XOpenDisplay.argtypes = [ctypes.c_char_p]
            x.XStringToKeysym.restype = ctypes.c_ulong
            x.XKeysymToKeycode.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
            x.XQueryKeymap.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
            self.x, self.d = x, x.XOpenDisplay(None)
            if not self.d:
                raise RuntimeError("无法连接 X11（Wayland 下不支持全局组合键检测）")
            names = {"Ctrl": "Control_L", "Alt": "Alt_L", "Shift": "Shift_L", "Win": "Super_L", "Esc": "Escape", "Space": "space", "Tab": "Tab"}
            self.codes = [x.XKeysymToKeycode(self.d, x.XStringToKeysym(names.get(k, k.lower()).encode())) for k in keys]
            self.buf = ctypes.create_string_buffer(32)

    def all_down(self):
        if IS_WIN:
            return all(self.u.GetAsyncKeyState(c) & 0x8000 for c in self.codes)
        if IS_MAC:
            return all(self.q.CGEventSourceKeyState(1, c) for c in self.codes)  # 1 = HIDSystemState
        self.x.XQueryKeymap(self.d, self.buf)
        return all(self.buf.raw[c // 8] & (1 << (c % 8)) for c in self.codes)


def request_permission():
    """macOS：请求“输入监控”权限（系统会弹窗）。"""
    if IS_MAC:
        try:
            iokit = ctypes.CDLL("/System/Library/Frameworks/IOKit.framework/IOKit")
            return bool(iokit.IOHIDRequestAccess(1))  # kIOHIDRequestTypeListenEvent
        except Exception:
            return False
    return True


class Watcher:
    """黑屏期间以 20Hz 读取按键状态；按住组合键 ≥0.3 秒触发 on_chord。非黑屏时休眠，不占 CPU。"""
    def __init__(self, get_keys, on_chord):
        self.get_keys, self.on_chord = get_keys, on_chord
        self.active = threading.Event()
        threading.Thread(target=self._loop, daemon=True).start()

    def _loop(self):
        while True:
            self.active.wait()
            try:
                r = Reader(self.get_keys())
            except Exception as e:
                from .core import log
                log("组合键检测不可用：%s" % e)
                self.active.clear()
                continue
            since = None
            while self.active.is_set():
                if r.all_down():
                    since = since or time.time()
                    if time.time() - since >= 0.3:
                        self.active.clear()
                        self.on_chord()
                        break
                else:
                    since = None
                time.sleep(0.05)


def wait_chord(keys, timeout):
    """测试按钮用：timeout 秒内检测到组合键返回 True。"""
    r = Reader(keys)
    end = time.time() + timeout
    while time.time() < end:
        if r.all_down():
            return True
        time.sleep(0.05)
    return False
