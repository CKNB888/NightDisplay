"""Windows 显示配置：QueryDisplayConfig 完整备份 / SetDisplayConfig 校验+应用 / 分级回滚。

依据微软公开文档独立实现（未复制 RustDesk 代码）：
https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-setdisplayconfig
路径/模式数组按原始字节保存（DISPLAYCONFIG_PATH_INFO=72 字节, DISPLAYCONFIG_MODE_INFO=64 字节），
还原时原样交回 SetDisplayConfig，避免字段解析误差。
"""
import ctypes
import re
import struct
from ctypes import wintypes

QDC_ALL_PATHS = 0x1
QDC_ONLY_ACTIVE_PATHS = 0x2
SDC_TOPOLOGY_INTERNAL = 0x1
SDC_TOPOLOGY_CLONE = 0x2
SDC_TOPOLOGY_EXTEND = 0x4
SDC_TOPOLOGY_EXTERNAL = 0x8
SDC_TOPOLOGY_SUPPLIED = 0x10
SDC_USE_SUPPLIED_DISPLAY_CONFIG = 0x20
SDC_VALIDATE = 0x40
SDC_APPLY = 0x80
SDC_SAVE_TO_DATABASE = 0x200
SDC_ALLOW_CHANGES = 0x400
SDC_ALLOW_PATH_ORDER_CHANGES = 0x2000
PATH_ACTIVE = 0x1
MODE_IDX_INVALID = 0xFFFFFFFF
PATH_SIZE, MODE_SIZE = 72, 64
GET_TARGET_NAME, GET_ADAPTER_NAME, GET_SOURCE_NAME = 2, 4, 1

ERRORS = {0: "成功", 87: "参数无效(87)", 50: "不支持(50)", 5: "拒绝访问(5)", 31: "设备未工作(31)",
          1610: "配置无效(1610)", 122: "缓冲区不足(122)"}

_u32 = None


def _user32():
    global _u32
    if _u32 is None:
        _u32 = ctypes.WinDLL("user32", use_last_error=True)
        _u32.GetDisplayConfigBufferSizes.argtypes = [wintypes.UINT, ctypes.POINTER(wintypes.UINT), ctypes.POINTER(wintypes.UINT)]
        _u32.QueryDisplayConfig.argtypes = [wintypes.UINT, ctypes.POINTER(wintypes.UINT), ctypes.c_void_p,
                                            ctypes.POINTER(wintypes.UINT), ctypes.c_void_p, ctypes.c_void_p]
        _u32.SetDisplayConfig.argtypes = [wintypes.UINT, ctypes.c_void_p, wintypes.UINT, ctypes.c_void_p, wintypes.UINT]
        _u32.DisplayConfigGetDeviceInfo.argtypes = [ctypes.c_void_p]
        for f in ("GetDisplayConfigBufferSizes", "QueryDisplayConfig", "SetDisplayConfig", "DisplayConfigGetDeviceInfo"):
            getattr(_u32, f).restype = ctypes.c_long
    return _u32


class DisplayError(RuntimeError):
    pass


def _err(code):
    return ERRORS.get(code, "错误码 %s" % code)


def query(flags=QDC_ONLY_ACTIVE_PATHS):
    """返回 (paths:list[bytes], modes:list[bytes])。"""
    u = _user32()
    for _ in range(5):
        np_, nm = wintypes.UINT(), wintypes.UINT()
        r = u.GetDisplayConfigBufferSizes(flags, ctypes.byref(np_), ctypes.byref(nm))
        if r:
            raise DisplayError("GetDisplayConfigBufferSizes " + _err(r))
        pb = ctypes.create_string_buffer(PATH_SIZE * max(1, np_.value))
        mb = ctypes.create_string_buffer(MODE_SIZE * max(1, nm.value))
        r = u.QueryDisplayConfig(flags, ctypes.byref(np_), pb, ctypes.byref(nm), mb, None)
        if r == 122:
            continue
        if r:
            raise DisplayError("QueryDisplayConfig " + _err(r))
        paths = [pb.raw[i * PATH_SIZE:(i + 1) * PATH_SIZE] for i in range(np_.value)]
        modes = [mb.raw[i * MODE_SIZE:(i + 1) * MODE_SIZE] for i in range(nm.value)]
        return paths, modes
    raise DisplayError("QueryDisplayConfig 反复返回缓冲区不足")


def set_config(paths, modes, flags):
    if any(len(p) != PATH_SIZE for p in paths) or any(len(m) != MODE_SIZE for m in modes):
        raise DisplayError("显示配置备份的结构长度不正确")
    u = _user32()
    pb = ctypes.create_string_buffer(b"".join(paths), PATH_SIZE * len(paths)) if paths else None
    mb = ctypes.create_string_buffer(b"".join(modes), MODE_SIZE * len(modes)) if modes else None
    return u.SetDisplayConfig(len(paths), pb, len(modes), mb, flags)


def set_topology(flag):
    return _user32().SetDisplayConfig(0, None, 0, None, SDC_APPLY | flag)

# ---- 解析 ----

def parse_path(b):
    s_luid_lo, s_luid_hi, s_id, s_mode, s_flags = struct.unpack_from("<IiIII", b, 0)
    t_luid_lo, t_luid_hi, t_id, t_mode, tech, rot, scal, rn, rd, scan, avail, t_flags = struct.unpack_from("<IiIIIIIIIIiI", b, 20)
    flags, = struct.unpack_from("<I", b, 68)
    return dict(src_adapter=(s_luid_lo, s_luid_hi), src_id=s_id, src_mode=s_mode,
                tgt_adapter=(t_luid_lo, t_luid_hi), tgt_id=t_id, tgt_mode=t_mode, tech=tech,
                available=bool(avail), active=bool(flags & PATH_ACTIVE), flags=flags)


def build_path(b, src_mode=MODE_IDX_INVALID, tgt_mode=MODE_IDX_INVALID, active=True):
    ba = bytearray(b)
    struct.pack_into("<I", ba, 12, src_mode)
    struct.pack_into("<I", ba, 32, tgt_mode)
    fl, = struct.unpack_from("<I", ba, 68)
    fl = (fl | PATH_ACTIVE) if active else (fl & ~PATH_ACTIVE)
    struct.pack_into("<I", ba, 68, fl)
    return bytes(ba)


def target_name(adapter, tgt_id):
    """DISPLAYCONFIG_TARGET_DEVICE_NAME：返回 (友好名, 设备路径)。"""
    size = 20 + 4 + 4 + 2 + 2 + 4 + 64 * 2 + 128 * 2
    buf = ctypes.create_string_buffer(size)
    struct.pack_into("<IIIiI", buf, 0, GET_TARGET_NAME, size, adapter[0], adapter[1], tgt_id)
    r = _user32().DisplayConfigGetDeviceInfo(buf)
    if r:
        return ("", "")
    raw = buf.raw
    fname = raw[36:36 + 128].decode("utf-16-le", "ignore").split("\0")[0]
    dpath = raw[164:164 + 256].decode("utf-16-le", "ignore").split("\0")[0]
    return fname, dpath


def adapter_name(adapter):
    size = 20 + 128 * 2
    buf = ctypes.create_string_buffer(size)
    struct.pack_into("<IIIiI", buf, 0, GET_ADAPTER_NAME, size, adapter[0], adapter[1], 0)
    if _user32().DisplayConfigGetDeviceInfo(buf):
        return ""
    return buf.raw[20:].decode("utf-16-le", "ignore").split("\0")[0]

# ---- 高层操作 ----

# 不用没有边界的 Virtual，避免普通适配器名字里带这几个字母就被当成虚拟屏。
VIRTUAL_RE = r"MttVDD|Virtual Display|\bVDD\b|IddSample|Parsec|\bUU\b"


def describe(flags=QDC_ALL_PATHS):
    """列出每个目标显示器（去重）。"""
    paths, _ = query(flags)
    seen, out = set(), []
    for b in paths:
        p = parse_path(b)
        key = (p["tgt_adapter"], p["tgt_id"])
        if key in seen:
            if p["active"]:
                for o in out:
                    if (o["tgt_adapter"], o["tgt_id"]) == key:
                        o["active"] = True
            continue
        seen.add(key)
        fname, dpath = target_name(p["tgt_adapter"], p["tgt_id"])
        p.update(name=fname, device_path=dpath, adapter_path=adapter_name(p["tgt_adapter"]))
        out.append(p)
    return out


def is_virtual(d, match=""):
    pat = match or VIRTUAL_RE
    hay = " ".join([d.get("name", ""), d.get("device_path", ""), d.get("adapter_path", "")])
    # DISPLAYCONFIG_OUTPUT_TECHNOLOGY_INDIRECT_VIRTUAL = 17；16(INDIRECT_WIRED) 可能是 USB 实体屏，不算
    return bool(re.search(pat, hay, re.I)) or d.get("tech") == 17


def backup_active():
    paths, modes = query(QDC_ONLY_ACTIVE_PATHS)
    return {"paths": [p.hex() for p in paths], "modes": [m.hex() for m in modes],
            "displays": [{k: v for k, v in d.items() if k in ("name", "device_path", "adapter_path", "tgt_id", "active", "tech")}
                         for d in describe(QDC_ONLY_ACTIVE_PATHS)]}


def restore(backup, log=print):
    """分级回滚：1 原样还原备份 → 2 仅电脑屏幕 → 3 扩展 → 4 克隆。返回使用的级别。"""
    tries = []
    paths = []
    saved = (backup or {}).get("displays") or []
    backup_virtual = bool(saved) and all(is_virtual(d) for d in saved)
    if backup_virtual:
        log("备份里只有虚拟屏，跳过该备份，避免写进系统显示数据库")
    elif backup and backup.get("paths"):
        try:
            paths = [bytes.fromhex(x) for x in backup["paths"]]
            modes = [bytes.fromhex(x) for x in backup["modes"]]
            if any(len(p) != PATH_SIZE for p in paths) or any(len(m) != MODE_SIZE for m in modes):
                raise ValueError("结构长度错误")
            fl = SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_SAVE_TO_DATABASE | SDC_ALLOW_CHANGES
            tries.append(("还原完整备份", lambda: set_config(paths, modes, SDC_APPLY | fl)))
            tries.append(("还原备份(仅拓扑)", lambda: set_config(
                [build_path(p) for p in paths], [], SDC_APPLY | SDC_TOPOLOGY_SUPPLIED | SDC_ALLOW_PATH_ORDER_CHANGES)))
        except (ValueError, TypeError, KeyError) as e:
            log("备份不可用，尝试系统恢复：%s" % e)
            paths = []
    tries += [("仅电脑屏幕 SDC_TOPOLOGY_INTERNAL", lambda: set_topology(SDC_TOPOLOGY_INTERNAL)),
              ("扩展 SDC_TOPOLOGY_EXTEND", lambda: set_topology(SDC_TOPOLOGY_EXTEND)),
              ("复制 SDC_TOPOLOGY_CLONE", lambda: set_topology(SDC_TOPOLOGY_CLONE))]
    for i, (name, fn) in enumerate(tries, 1):
        try:
            r = fn()
        except (DisplayError, OSError) as e:
            log("恢复第%d级失败：%s" % (i, e))
            continue
        log("恢复第%d级 %s：%s" % (i, name, _err(r)))
        if r == 0:
            # A successful API return is insufficient if only the virtual
            # monitor remains active. Verify the expected targets as well.
            active = describe(QDC_ONLY_ACTIVE_PATHS)
            if not active:
                log("恢复后没有活动显示器，继续下一级")
                continue
            if paths and i <= 2:
                expected = {(parse_path(p)["tgt_adapter"], parse_path(p)["tgt_id"]) for p in paths}
                actual = {(p["tgt_adapter"], p["tgt_id"]) for p in active}
                if expected != actual:
                    log("恢复后目标显示器不匹配，继续下一级")
                    continue
            if not any(not is_virtual(p) for p in active):
                log("恢复后仍只有虚拟屏，继续下一级")
                continue
            return i
    raise DisplayError("所有恢复级别均失败，请按 Win+P 或进入安全模式")


def plan_virtual_only(match=""):
    """生成"只保留虚拟屏"的路径数组（不应用）。"""
    paths, _ = query(QDC_ALL_PATHS)
    infos = [(b, parse_path(b)) for b in paths]
    named = {}
    for _, p in infos:
        k = (p["tgt_adapter"], p["tgt_id"])
        if k not in named:
            fn, dp = target_name(*k)
            named[k] = dict(name=fn, device_path=dp, adapter_path=adapter_name(k[0]), tech=p["tech"])
    cands = [(b, p) for b, p in infos if p["available"] and is_virtual(named[(p["tgt_adapter"], p["tgt_id"])], match)]
    if not cands:
        raise DisplayError("没有找到可用的虚拟显示器（VDD）。请先安装虚拟显示驱动。")
    physical = [k for k, d in named.items() if not is_virtual(d, match)]
    def priority(bp):
        p = bp[1]
        d = named[(p["tgt_adapter"], p["tgt_id"])]
        stable = bool(re.search(r"MttVDD|Virtual Display Driver", " ".join(str(v) for v in d.values()), re.I))
        return (not stable, not p["active"], p["src_id"])
    cands.sort(key=priority)
    b, p = cands[0]
    return [build_path(b)], named[(p["tgt_adapter"], p["tgt_id"])], len(physical)


def apply_virtual_only(match="", log=print, dry_run=False):
    paths, info, nphys = plan_virtual_only(match)
    r = -1
    for fl in (SDC_TOPOLOGY_SUPPLIED | SDC_ALLOW_PATH_ORDER_CHANGES,
               SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES):
        r = set_config(paths, [], SDC_VALIDATE | fl)
        log("校验“仅虚拟屏”配置（目标：%s，物理屏 %d 块，flags=0x%x）：%s" % (info["name"] or info["device_path"], nphys, fl, _err(r)))
        if r == 0:
            break
    if r:
        raise DisplayError("SetDisplayConfig 校验失败：" + _err(r))
    if dry_run:
        return info
    # Night mode is temporary. Persisting it can leave the physical monitor
    # disabled after a reboot before NightDisplay starts.
    r = set_config(paths, [], SDC_APPLY | fl)
    log("应用“仅虚拟屏”：%s" % _err(r))
    if r:
        raise DisplayError("SetDisplayConfig 应用失败：" + _err(r))
    return info


def verify_virtual_only(match=""):
    act = describe(QDC_ONLY_ACTIVE_PATHS)
    return bool(act) and all(is_virtual(d, match) for d in act)
