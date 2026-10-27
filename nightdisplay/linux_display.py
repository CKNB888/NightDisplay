"""Linux(X11) 显示：xrandr 备份/还原/仅虚拟输出，xset dpms 关屏。Wayland 下仅提示。"""
import os
import re
import shutil
import subprocess

VIRTUAL_RE = r"^(VIRTUAL|DUMMY|Virtual|HDMI-A-?V|DVI-I-\d+-\d+)"  # evdi/ xf86-video-dummy / Intel VIRTUAL


def session_type():
    if os.environ.get("WAYLAND_DISPLAY") or os.environ.get("XDG_SESSION_TYPE") == "wayland":
        return "wayland"
    return "x11" if os.environ.get("DISPLAY") else "none"


def _need(tool):
    if not shutil.which(tool):
        raise RuntimeError("未找到 %s，请先安装：sudo apt install x11-xserver-utils（Fedora: xorg-x11-server-utils）" % tool)
    if session_type() == "none":
        raise RuntimeError("未检测到 X11 会话（DISPLAY 为空），请在图形桌面的 X11 会话中运行")
    if session_type() == "wayland":
        raise RuntimeError("Wayland 下不能控制真实屏幕，请在 X11 会话中运行")


def _run(args):
    _need(args[0])
    from .backends import native_env
    p = subprocess.run(args, capture_output=True, text=True, errors="replace", timeout=30, env=native_env())
    if p.returncode:
        raise RuntimeError("%s 失败: %s" % (" ".join(args), (p.stderr or p.stdout).strip()))
    return p.stdout


def parse_xrandr(text):
    outs, cur = [], None
    for line in text.splitlines():
        m = re.match(r"^(\S+) (connected|disconnected)( primary)?(?: (\d+)x(\d+)([+-]\d+)([+-]\d+))?(?: (left|right|inverted|normal))?", line)
        if m:
            cur = dict(name=m.group(1), connected=m.group(2) == "connected", primary=bool(m.group(3)),
                       active=m.group(4) is not None, mode=None, rate=None,
                       pos=(int(m.group(6)), int(m.group(7))) if m.group(4) else None,
                       rotate=m.group(8) or "normal")
            if cur["active"]:
                cur["mode"] = "%sx%s" % (m.group(4), m.group(5))
            outs.append(cur)
            continue
        if cur is not None and line.startswith("   ") and "*" in line:
            parts = line.split()
            cur["mode"] = parts[0]  # rotated outputs report swapped geometry dimensions
            for r in parts[1:]:
                if "*" in r:
                    cur["rate"] = r.replace("*", "").replace("+", "")
    return outs


def query():
    return parse_xrandr(_run(["xrandr", "--query"]))


def restore_cmd(outs):
    cmd = ["xrandr"]
    for o in outs:
        cmd += ["--output", o["name"]]
        if o["active"]:
            cmd += ["--mode", o["mode"], "--pos", "%dx%d" % tuple(o["pos"]), "--rotate", o["rotate"]]
            if o.get("rate"):
                cmd += ["--rate", o["rate"]]
            if o["primary"]:
                cmd += ["--primary"]
        else:
            cmd += ["--off"]
    return cmd


def backup():
    return {"outputs": query()}


def restore(b, log=print):
    if b and b.get("outputs"):
        try:
            _run(restore_cmd(b["outputs"]))
            log("xrandr 已还原完整备份")
            return 1
        except Exception as e:
            log("还原备份失败: %s" % e)
    _run(["xrandr", "--auto"])
    log("xrandr --auto 已执行（第2级）")
    return 2


def virtual_only_cmd(outs, match=""):
    pat = match or VIRTUAL_RE
    v = [o for o in outs if o["connected"] and re.search(pat, o["name"])]
    if not v:
        raise RuntimeError("没有已连接的虚拟输出（VIRTUAL*/DUMMY*/evdi）。")
    cmd = ["xrandr", "--output", v[0]["name"], "--auto", "--primary"]
    for o in outs:
        if o is not v[0] and o["active"]:
            cmd += ["--output", o["name"], "--off"]
    return cmd, v[0]["name"]


def apply_virtual_only(match="", log=print, dry_run=False):
    cmd, name = virtual_only_cmd(query(), match)
    if dry_run:
        log("预演（未执行）：%s" % " ".join(cmd))
        return name
    _run(cmd)
    log("已切换为仅虚拟输出 %s" % name)
    return name


def verify_virtual_only(match=""):
    active = [o for o in query() if o["active"]]
    return bool(active) and all(re.search(match or VIRTUAL_RE, o["name"]) for o in active)


def screen_off():
    st = session_type()
    if st == "none":
        raise RuntimeError("未检测到 X11 会话（DISPLAY 为空），请在图形桌面的 X11 会话中运行")
    if st != "x11":
        raise RuntimeError("Wayland 下无法通用关屏，请使用桌面环境的“息屏”设置。")
    if "DPMS is" not in _run(["xset", "q"]):
        raise RuntimeError("X 服务器不支持 DPMS，无法关屏")
    _run(["xset", "dpms", "force", "off"])


def screen_on():
    # Xvfb has no DPMS; resetting the saver is still a valid wake operation.
    if "DPMS is" in _run(["xset", "q"]):
        _run(["xset", "dpms", "force", "on"])
    _run(["xset", "s", "reset"])
