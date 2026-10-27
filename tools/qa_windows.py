# -*- coding: utf-8 -*-
"""打包产物端到端 QA（Windows）：对解压后的 NightDisplay.exe 跑多轮完整流程。
用法: python tools/qa_windows.py <NightDisplay.exe> [轮数]"""
import os, sys, json, time, shutil, subprocess, ctypes, tempfile, winreg
sys.path.insert(0, os.path.join(os.path.dirname(__file__), ".."))
from nightdisplay import win_display as W

EXE, ROUNDS = sys.argv[1], int(sys.argv[2]) if len(sys.argv) > 2 else 3
results = []

def cfgp(home): return os.path.join(home, "config.json")
def cfg(home): return json.load(open(cfgp(home), encoding="utf-8"))
def setcfg(home, **kw):
    c = cfg(home) if os.path.exists(cfgp(home)) else {}
    c.update(kw); os.makedirs(home, exist_ok=True)
    json.dump(c, open(cfgp(home), "w", encoding="utf-8"), ensure_ascii=False)
def run(exe, *a, env=None):
    return subprocess.run([exe, *a], env=env, timeout=120).returncode
def press(keys, hold=0.8):
    u = ctypes.windll.user32
    for k in keys: u.keybd_event(k, 0, 0, 0)
    time.sleep(hold)
    for k in reversed(keys): u.keybd_event(k, 0, 2, 0)
def tasks():
    return subprocess.run(["schtasks", "/Query", "/TN", "NightDisplay_Night"], capture_output=True).returncode == 0
def runkey():
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, r"Software\Microsoft\Windows\CurrentVersion\Run") as k:
            return bool(winreg.QueryValueEx(k, "NightDisplay")[0])
    except OSError:
        return False
def topo():
    return sorted((W.parse_path(p)["tgt_id"], W.parse_path(p)["src_id"]) for p in W.query(W.QDC_ONLY_ACTIVE_PATHS)[0])

def round_(i):
    ev, ok = [], True
    def chk(name, cond, info=""):
        nonlocal ok
        ok &= bool(cond); ev.append("%s %s %s" % ("PASS" if cond else "FAIL", name, info)); print(ev[-1], flush=True)
    d = tempfile.mkdtemp(prefix="ndqa%d-" % i)
    exe = os.path.join(d, "NightDisplay.exe"); shutil.copy(EXE, exe)          # 安装（免安装版：解压/复制）
    home = os.path.join(d, "home"); env = dict(os.environ, NIGHTDISPLAY_HOME=home)
    before = topo()
    chk("install/status", run(exe, "--action", "status", env=env) == 0 and os.path.exists(cfgp(home)))
    from datetime import datetime
    now = datetime.now().isoformat()
    setcfg(home, mode="overlay", last_test_mode="overlay", last_test_ok=True, last_test_at=now, last_recovery_at=now, unlock_chord=["C", "K"], lock=True)
    chk("schedule register", run(exe, "--action", "activate", "--start", "20:00", "--end", "10:30", env=env) == 0 and tasks() and runkey(),
        "task=%s runkey=%s" % (tasks(), runkey()))
    g = subprocess.Popen([exe, "--action", "gui", "--selftest", "90"], env=env); time.sleep(10)
    chk("enter black (overlay via IPC)", run(exe, "--action", "night", env=env) == 0 and cfg(home)["state"] == "night", cfg(home).get("night_mode"))
    time.sleep(3)
    rc = run(exe, "--action", "day", env=env)
    chk("local restore blocked while locked", rc == 2 and cfg(home)["state"] == "night", "rc=%s" % rc)
    press([0x43, 0x4B]); time.sleep(3)                                          # 按住 C+K（SendInput）
    chk("C+K chord unlock", cfg(home)["state"] == "day")
    run(exe, "--action", "night", env=env); time.sleep(3)
    chk("re-enter black", cfg(home)["state"] == "night")
    run(exe, "--action", "emergency", env=env); time.sleep(2)
    chk("remote emergency restore", cfg(home)["state"] == "day" and not cfg(home)["enabled"])
    chk("display config restored", topo() == before, "%s" % topo())
    chk("autostart/task removed by emergency(pause)", not tasks() and not runkey())
    setcfg(home, last_test_at=datetime.now().isoformat())
    run(exe, "--action", "activate", "--start", "21:00", "--end", "07:00", env=env)
    chk("autostart re-register", tasks() and runkey())
    run(exe, "--action", "uninstall", env=env); time.sleep(2)
    chk("uninstall clean", not tasks() and not runkey() and cfg(home)["state"] == "day")
    try: g.wait(100)
    except Exception: g.kill()
    log = "".join(open(os.path.join(home, "logs", f), encoding="utf-8").read() for f in os.listdir(os.path.join(home, "logs")))
    print(log[-1500:])
    results.append({"round": i, "ok": ok, "evidence": ev})

for i in range(1, ROUNDS + 1):
    print("===== ROUND %d =====" % i, flush=True); round_(i)
json.dump(results, open("qa-windows.json", "w", encoding="utf-8"), ensure_ascii=False, indent=1)
print("\n| round | result |\n|---|---|")
for r in results: print("| %d | %s |" % (r["round"], "PASS" if r["ok"] else "FAIL"))
sys.exit(0 if all(r["ok"] for r in results) else 1)
