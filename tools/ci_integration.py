# -*- coding: utf-8 -*-
"""CI 集成测试：在真实 Windows / macOS / Linux 机器上跑核心流程，输出 PASS/FAIL/SKIP 证据表。
用法: python tools/ci_integration.py [--vdd]   （硬失败时退出码 1）"""
import os, sys, json, time, shutil, subprocess, traceback, platform
sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
os.environ.setdefault("NIGHTDISPLAY_HOME", os.path.join(os.getcwd(), "ci-home"))
from nightdisplay import core, backends as B
from nightdisplay.core import IS_WIN, IS_MAC

R = []
def check(name, fn, hard=True):
    try:
        ev = fn()
        R.append((name, "PASS", str(ev)[:300])); print("PASS", name, "|", ev)
    except SkipTest as e:
        R.append((name, "SKIP", str(e))); print("SKIP", name, "|", e)
    except Exception as e:
        R.append((name, "FAIL" if hard else "WARN", repr(e)[:300])); print("FAIL" if hard else "WARN", name); traceback.print_exc()
class SkipTest(Exception): pass
def sh(cmd, **kw):
    p = subprocess.run(cmd, capture_output=True, text=True, errors="replace", **kw)
    return p.returncode, (p.stdout or "") + (p.stderr or "")
PY = [sys.executable, "-m", "nightdisplay"]

def schedule_roundtrip():
    cfg = core.update(start="20:00", end="10:30")
    B.schedule_install(cfg)
    st = B.schedule_status()
    if IS_WIN:
        rc, out = sh(["schtasks", "/Query", "/FO", "CSV"])
        assert "NightDisplay_Night" in out, out[-400:]
    elif IS_MAC:
        assert "已加载" in st, st
        rc, out = sh(["plutil", "-lint", os.path.expanduser("~/Library/LaunchAgents/com.nightdisplay.agent.plist")]); assert rc == 0, out
    else:
        d = B._systemd_dir()
        if shutil.which("systemd-analyze"):
            rc, out = sh(["systemd-analyze", "--user", "verify", os.path.join(d, "nightdisplay.timer")])
            st += " | verify rc=%s %s" % (rc, out.strip()[:120])
    B.schedule_remove()
    st2 = B.schedule_status()
    assert "已注册" not in st2.replace("未注册", "") and "已加载" not in st2 and "已写入" not in st2, st2
    return st + " -> removed: " + st2

def awake_check():
    core.update(state="night")
    try:
        B.keep_awake(True, 120); time.sleep(4)
        if IS_WIN:
            rc, out = sh(["powercfg", "/requests"])
            ok = "python" in out.lower() or "nightdisplay" in out.lower()
            ev = "powercfg rc=%s python-in-SYSTEM-requests=%s" % (rc, ok)
            assert ok, out
        elif IS_MAC:
            rc, out = sh(["pmset", "-g", "assertions"]); assert "caffeinate" in out, out; ev = "caffeinate assertion present"
        else:
            if not shutil.which("systemd-inhibit"): raise SkipTest("no systemd-inhibit")
            rc, out = sh(["systemd-inhibit", "--list", "--no-pager"])
            ev = "inhibit list rc=%s has NightDisplay=%s" % (rc, "NightDisplay" in out)
            if rc: raise SkipTest("logind 不可用（容器/CI）：" + out.strip()[:100])
        return ev
    finally:
        core.update(state="day"); B.keep_awake(False)

def display_roundtrip():
    rc, out = sh(PY + ["--action", "displays"]); print(out)
    assert rc == 0, out
    rc, out = sh(PY + ["--action", "backup"]); assert rc == 0, out
    rc, out = sh(PY + ["--action", "restore"]); assert rc == 0 and "level 1" in out, out
    return out.strip().splitlines()[-1]

def screen_off_on():
    core.update(state="day")
    rc, out = sh(PY + ["--action", "night", "--mode", "screen_off"])
    rc2, out2 = sh(PY + ["--action", "emergency"])
    assert rc == 0 and rc2 == 0, out + out2
    return "night rc=%s day rc=%s" % (rc, rc2)

def overlay_night_day():
    if not IS_WIN or B.win_build() < 19041: raise SkipTest("仅 Win10 2004+")
    core.update(mode="overlay", state="day")
    g = subprocess.Popen(PY + ["--action", "gui", "--selftest", "25"])
    try:
        time.sleep(8)
        rc, out = sh(PY + ["--action", "night"])
        c = core.load()
        assert rc == 0 and c.get("state") == "night" and c.get("night_mode") == "overlay", (out, c)
        time.sleep(2)
        rc2, out2 = sh(PY + ["--action", "emergency"])
        assert rc2 == 0 and core.load().get("state") == "day", out2
        return "overlay night via IPC ok, day ok; log tail: " + core.read_log_tail(400).replace("\n", " / ")
    finally:
        g.wait(60); core.update(mode="auto")

def vdd_virtual_only():
    if not IS_WIN: raise SkipTest("Windows only")
    script = os.path.join(os.path.dirname(__file__), "install_vdd.ps1")
    rc, out = sh(["powershell", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", script], timeout=240)
    print(out[-3000:])
    assert rc == 0, "install_vdd rc=%s" % rc
    time.sleep(8)
    rc, out = sh(PY + ["--action", "displays"]); print(out)
    rc, plan = sh(PY + ["--action", "plan-virtual"]); print(plan)
    assert rc == 0, plan
    rc, n = sh(PY + ["--action", "night", "--mode", "virtual_only"]); print(n)
    rc2, d = sh(PY + ["--action", "emergency"]); print(d)
    rc3, after = sh(PY + ["--action", "displays"]); print(after)
    assert rc == 0, n
    return "virtual_only night rc=%s day rc=%s" % (rc, rc2)

def gui_selftest():
    rc, out = sh(PY + ["--action", "gui", "--selftest", "5"], timeout=60)
    assert rc == 0, out
    return "rc=0"

if __name__ == "__main__":
    print("platform:", platform.platform(), sys.version)
    check("schedule register/query/remove", schedule_roundtrip)
    check("keep-awake helper", awake_check)
    check("display backup/restore round-trip", display_roundtrip, hard=not IS_MAC)
    check("screen_off -> day", screen_off_on, hard=False)
    check("GUI selftest", gui_selftest)
    check("overlay (exclude-from-capture) GUI", overlay_night_day, hard=False)
    if "--vdd" in sys.argv:
        check("VDD install + virtual_only + restore", vdd_virtual_only, hard=False)
    print("\n| test | result | evidence |\n|---|---|---|")
    for n, r, e in R:
        print("| %s | %s | %s |" % (n, r, e.replace("|", "/").replace("\n", " ")))
    with open("ci-results-%s.json" % platform.system(), "w", encoding="utf-8") as f:
        json.dump(R, f, ensure_ascii=False, indent=1)
    sys.exit(1 if any(r == "FAIL" for _, r, _ in R) else 0)
