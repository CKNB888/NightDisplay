"""Exercise the packaged Windows exe; no fabricated test-success records."""
import ctypes
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import winreg
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from nightdisplay import win_display as W
from nightdisplay.core import load


def wait_for(fn, timeout=45):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if fn():
            return
        time.sleep(.25)
    raise AssertionError("Timed out waiting for expected state")


def press_chord():
    user = ctypes.windll.user32
    try:
        for key in (0x43, 0x4B):
            user.keybd_event(key, 0, 0, 0)
        time.sleep(.8)
    finally:
        for key in (0x4B, 0x43):
            user.keybd_event(key, 0, 2, 0)


def active_targets():
    return sorted((d["tgt_adapter"], d["tgt_id"]) for d in W.describe(W.QDC_ONLY_ACTIVE_PATHS))


def has_runkey():
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, r"Software\Microsoft\Windows\CurrentVersion\Run") as key:
            return bool(winreg.QueryValueEx(key, "NightDisplay")[0])
    except OSError:
        return False


def has_tasks():
    p = subprocess.run(["schtasks", "/Query", "/TN", "NightDisplay_Night"], capture_output=True, timeout=20)
    return p.returncode == 0


def round_test(source, number):
    root = Path(tempfile.mkdtemp(prefix="nightdisplay-qa-"))
    exe = root / "NightDisplay.exe"
    shutil.copyfile(source, exe)
    os.environ["NIGHTDISPLAY_HOME"] = str(root / "state")
    events, gui = [], None
    def run(*args, expected=0, timeout=120):
        p = subprocess.run([str(exe), *args], timeout=timeout)
        assert p.returncode == expected, (args, p.returncode)
        events.append("PASS: " + " ".join(args))
    before = active_targets()
    try:
        run("--action", "status")
        if number == 1:
            run("--action", "install-vdd", timeout=320)
        assert any(W.is_virtual(d) and d["available"] for d in W.describe()), "No available virtual display"
        gui = subprocess.Popen([str(exe), "--action", "gui", "--selftest", "240"])
        from nightdisplay.app import send_ipc
        wait_for(lambda: send_ipc("show") == "ok")
        run("--action", "test", "--mode", "auto", "--seconds", "15")
        wait_for(lambda: load().get("last_test_ok"))
        assert load()["last_test_mode"] == "virtual_only", "Default silently fell back instead of testing VDD"
        assert load()["state"] == "day" and active_targets() == before
        events.append("PASS: real timed test + verified display recovery")
        # A daytime test interval prevents automatic reconciliation racing the test.
        run("--action", "activate", "--start", "20:00", "--end", "10:30")
        assert has_tasks() and has_runkey()
        run("--action", "night", "--mode", "virtual_only")
        assert load()["state"] == "night" and W.verify_virtual_only()
        run("--action", "day", expected=2)
        press_chord()
        wait_for(lambda: load()["state"] == "day")
        assert active_targets() == before
        events.append("PASS: C+K unlock + original targets")
        run("--action", "night", "--mode", "virtual_only")
        run("--action", "emergency")
        assert load()["state"] == "day" and not load()["enabled"]
        assert not has_tasks() and not has_runkey() and active_targets() == before
        assert not (root / "state" / "night-power.json").exists()
        events.append("PASS: emergency removes tasks, run key and temporary power scheme")
        run("--action", "uninstall")
        gui.wait(timeout=20)
        return {"round": number, "ok": True, "evidence": events}
    except Exception as e:
        return {"round": number, "ok": False, "error": repr(e), "evidence": events}
    finally:
        try:
            subprocess.run([str(exe), "--action", "emergency"], timeout=120)
        finally:
            if gui is not None and gui.poll() is None:
                try:
                    from nightdisplay.app import send_ipc
                    send_ipc("quit")
                    gui.wait(timeout=10)
                except Exception:
                    subprocess.run(["taskkill", "/F", "/T", "/PID", str(gui.pid)], capture_output=True, timeout=15)
            output = Path("qa-evidence") / ("round-%d" % number)
            output.parent.mkdir(exist_ok=True)
            if (root / "state").exists():
                shutil.copytree(root / "state", output, dirs_exist_ok=True)
                for f in sorted((root / "state" / "logs").glob("*.log")):
                    print("----- %s -----" % f.name)
                    print(f.read_text(encoding="utf-8", errors="replace")[-4000:])


if __name__ == "__main__":
    source = Path(sys.argv[1]).resolve()
    rounds = int(sys.argv[2]) if len(sys.argv) > 2 else 3
    results = [round_test(source, i) for i in range(1, rounds + 1)]
    Path("qa-windows.json").write_text(json.dumps(results, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps(results, ensure_ascii=False, indent=2))
    sys.exit(0 if all(result["ok"] for result in results) else 1)
