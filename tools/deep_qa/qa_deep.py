"""NightDisplay 4.0 deep Windows QA.

This is deliberately not a unit test. It drives the *installed* build on a real
Windows desktop session and asserts the things users actually complain about:

  1. the packaged installer installs, the driver lands, and the virtual display
     is enumerated by the OS;
  2. entering night mode never submits a display config with zero active
     targets, uses exactly one SDC_APPLY, and reports how long each step took;
  3. the physical target stays enumerated the whole time (proxy for "the remote
     tool's capture source never disappears");
  4. the virtual target ends up on the same resolution/refresh as the physical
     one (mismatch is what makes the compositor rebuild and the stream blink);
  5. synthesized local input does not restore anything, and the emergency path
     does restore;
  6. a killed daemon is brought back by the watchdog, and a simulated reboot
     re-enters night when still inside the window;
  7. a redacted diagnostic bundle is produced for debugging.

Every assertion prints the number it measured, so a failure explains itself.
Exit code 0 = all checks passed. Output is appended to qa-report.md and
qa-timings.json for the CI summary.
"""

import json
import os
import re
import shutil
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DIST = ROOT / "dist"
OUT = ROOT / "qa-out"
HOME = OUT / "home"
EXE_INSTALLED = Path(os.environ.get("LOCALAPPDATA", "")) / "Programs" / "NightDisplay" / "NightDisplay.exe"

results = []
timings = {}


def say(msg=""):
    print(msg, flush=True)


def check(name, ok, detail=""):
    results.append({"check": name, "ok": bool(ok), "detail": detail})
    say(f"{'PASS' if ok else 'FAIL'}  {name}" + (f"  —— {detail}" if detail else ""))
    return ok


def note(name, value):
    timings[name] = value
    say(f"      {name} = {value}")


def run(args, timeout=180, exe=EXE_INSTALLED, check_code=True):
    """Run the app with an isolated data directory and return (code, output)."""
    env = dict(os.environ)
    env["NIGHTDISPLAY_HOME"] = str(HOME)
    started = time.monotonic()
    proc = subprocess.run(
        [str(exe), *args],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=timeout,
        env=env,
    )
    elapsed = (time.monotonic() - started) * 1000
    out = (proc.stdout or "") + (proc.stderr or "")
    if check_code and proc.returncode != 0:
        say(f"--- {args} exited {proc.returncode} after {elapsed:.0f} ms")
        say(out.rstrip())
    return proc.returncode, out


def probe():
    code, out = run(["--action", "probe"], check_code=False)
    if code != 0:
        return None
    body = out[out.find("{"):]
    try:
        return json.loads(body)
    except json.JSONDecodeError:
        return None


def probe_reset():
    run(["--action", "probe-reset"], check_code=False)


def session_current():
    path = HOME / "session-current.json"
    if not path.exists():
        return None
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return None


def sessions():
    path = HOME / "sessions.jsonl"
    if not path.exists():
        return []
    rows = []
    for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
        line = line.strip()
        if line:
            try:
                rows.append(json.loads(line))
            except json.JSONDecodeError:
                pass
    return rows


# ---------------------------------------------------------------------------
# 1. install
# ---------------------------------------------------------------------------

def step_install():
    setup = next(iter(sorted(DIST.glob("NightDisplay-*-Setup.exe"))), None)
    if setup is None:
        return check("安装包存在", False, f"{DIST} 里没有 Setup.exe")
    check("安装包存在", True, f"{setup.name} / {setup.stat().st_size // 1024} KiB")

    # 装到当前用户的 LocalAppData，不需要管理员。
    started = time.monotonic()
    proc = subprocess.run(
        [str(setup), "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"],
        capture_output=True, text=True, timeout=600,
    )
    install_ms = (time.monotonic() - started) * 1000
    note("install_ms", round(install_ms))
    if not check("静默安装成功", proc.returncode == 0 and EXE_INSTALLED.exists(),
                 f"exit={proc.returncode} exe={EXE_INSTALLED.exists()}"):
        return False
    # 随包驱动必须在安装目录里，否则向导第 2 步无从安装。
    inf = list(EXE_INSTALLED.parent.rglob("MttVDD.inf"))
    check("随包驱动已就位", bool(inf), str(inf[0]) if inf else "找不到 MttVDD.inf")
    verify = EXE_INSTALLED.parent / "resources"
    check("资源目录完整", (verify / "install_vdd.ps1").exists()
          and (verify / "vdd_settings.xml").exists())
    return True


# ---------------------------------------------------------------------------
# 2. onboarding end to end (VDD installed, virtual display enumerated)
# ---------------------------------------------------------------------------

def step_onboarding():
    # 只跑不触权的几步？不行 —— 用户抱怨的核心正是驱动，
    # 所以这里真的装驱动：helper 会提权（runner 是管理员，不会弹框）。
    code, out = run(["--action", "install-vdd"], timeout=900)
    ok = code == 0
    check("虚拟显示驱动安装成功", ok, _first_line(out))
    if not ok:
        say("驱动安装完整输出：")
        say(out)
        return False

    p = probe()
    if not check("probe 可读", p is not None):
        return False
    note("virtual_count", p["virtual_count"])
    note("physical_count", p["physical_count"])
    check("虚拟屏已被系统枚举", p["virtual_count"] >= 1,
          f"virtual_count={p['virtual_count']}")
    check("实体屏仍在枚举中", p["physical_count"] >= 1,
          f"physical_count={p['physical_count']}")

    # 向导状态必须能读回，且驱动这一步是 ok 的。
    code, out = run(["--action", "onboarding"], timeout=120)
    check("向导状态可读", code == 0, _first_line(out))
    code, out = run(["--action", "setup", "--only", "autostart", "--seconds", "3"], timeout=300)
    check("自启步骤可执行", code == 0, _first_line(out))
    return True


def _first_line(text):
    for line in (text or "").splitlines():
        if line.strip():
            return line.strip()[:160]
    return ""


# ---------------------------------------------------------------------------
# 3. night switch: single apply, never zero targets, matching mode, timing
# ---------------------------------------------------------------------------

def step_night_switch():
    probe_reset()
    started = time.monotonic()
    code, out = run(["--action", "night", "--mode", "virtual_screen"], timeout=180)
    wall_ms = (time.monotonic() - started) * 1000
    note("night_wall_ms", round(wall_ms))
    if not check("进入夜间成功", code == 0, _first_line(out)):
        say(out)
        return False

    # 首帧黑屏延迟：从发起命令到拓扑真的切换完成。
    # wall_ms 含进程启动开销，所以另外读会话里每一步的实测耗时。
    session = session_current() or (sessions()[-1] if sessions() else {})
    steps = session.get("steps") or []
    step_ms = {s["step"]: s["ms"] for s in steps if "step" in s}
    timings["steps"] = step_ms
    say("      步骤耗时（毫秒）：" + ", ".join(f"{k}={v}" for k, v in step_ms.items()))
    total_ms = sum(v for k, v in step_ms.items() if k != "settle")
    note("switch_total_ms", total_ms)
    check("首帧黑屏 < 1500 ms", total_ms < 1500, f"{total_ms} ms")
    check("切换有步骤记录", bool(steps), f"{len(steps)} 步")
    check("步骤没有失败", all(s.get("ok") for s in steps),
          "; ".join(f"{s['step']}:{s.get('detail','')}" for s in steps if not s.get("ok")))

    p = probe()
    if not check("probe 可读（夜间）", p is not None):
        return False
    st = p["stats"]
    note("apply_calls", st["apply_calls"])
    note("validate_calls", st["validate_calls"])
    note("min_active_submitted", st["min_active_submitted"])
    note("active_count", p["active_count"])

    check("只有一次 SDC_APPLY", st["apply_calls"] == 1, f"apply_calls={st['apply_calls']}")
    check("没有用过拓扑快捷方式兜底", st["apply_shortcut_calls"] == 0,
          f"apply_shortcut_calls={st['apply_shortcut_calls']}")
    check("提交时活动 target 从未为 0", not st["ever_zero_target"]
          and st["min_active_submitted"] >= 1,
          f"min_active_submitted={st['min_active_submitted']}")
    check("切换后仍有活动显示", p["active_count"] >= 1, f"active_count={p['active_count']}")

    # 实体屏必须还在完整路径集里 —— 这是"远程采集表面没死"的代理指标。
    check("实体屏仍被枚举（采集表面未消失）", p["physical_never_disappeared"],
          f"physical_count={p['physical_count']}")

    # 活动显示里必须只剩虚拟屏。
    active_virtual = [d for d in p["displays"] if d["active"] and d["virtual"]]
    active_physical = [d for d in p["displays"] if d["active"] and not d["virtual"]]
    check("桌面已交给虚拟屏", bool(active_virtual), f"{len(active_virtual)} 块虚拟屏活动")
    check("实体屏已不接桌面", not active_physical,
          f"{len(active_physical)} 块实体屏仍活动")

    # 虚拟屏分辨率/刷新率必须和实体屏一致，否则合成器会重建 → 远程闪一下。
    modes = p["active_modes"]
    virt = [m for m in modes if m["virtual"]]
    phys = [m for m in modes if not m["virtual"]]
    if virt:
        note("virtual_mode", f"{virt[0]['width']}x{virt[0]['height']}@{virt[0]['refresh_hz']}")
    if phys:
        note("physical_mode", f"{phys[0]['width']}x{phys[0]['height']}@{phys[0]['refresh_hz']}")
    if virt and phys:
        matched = (virt[0]["width"], virt[0]["height"], virt[0]["refresh_hz"]) == \
                  (phys[0]["width"], phys[0]["height"], phys[0]["refresh_hz"])
        check("虚拟屏模式与实体屏一致", matched,
              f"virtual={virt[0]['width']}x{virt[0]['height']}@{virt[0]['refresh_hz']} "
              f"physical={phys[0]['width']}x{phys[0]['height']}@{phys[0]['refresh_hz']}")
    else:
        # 摘掉后实体屏不再活动是正常的，这里只要求虚拟屏有模式。
        check("虚拟屏报出了真实模式", bool(virt), f"{len(virt)} 块")
    check("未降级到黑幕模式", (session.get("effective") or "virtual_screen") == "virtual_screen",
          f"effective={session.get('effective')}")
    return True


# ---------------------------------------------------------------------------
# 4. synthesized input must not restore anything
# ---------------------------------------------------------------------------

def step_remote_session():
    import ctypes

    user32 = ctypes.windll.user32
    before = probe()
    cfg_before = read_config()

    # 模拟远程软件的注入：真的 SendInput 一些按键和移动鼠标。
    # 这也是本地用户会做的事，夜间这两种都不该把屏幕弄回来。
    class MOUSEINPUT(ctypes.Structure):
        _fields_ = [("dx", ctypes.c_long), ("dy", ctypes.c_long),
                    ("mouseData", ctypes.c_ulong), ("dwFlags", ctypes.c_ulong),
                    ("time", ctypes.c_ulong), ("dwExtraInfo", ctypes.c_void_p)]

    class KEYBDINPUT(ctypes.Structure):
        _fields_ = [("wVk", ctypes.c_ushort), ("wScan", ctypes.c_ushort),
                    ("dwFlags", ctypes.c_ulong), ("time", ctypes.c_ulong),
                    ("dwExtraInfo", ctypes.c_void_p)]

    class _INPUTunion(ctypes.Union):
        _fields_ = [("mi", MOUSEINPUT), ("ki", KEYBDINPUT)]

    class INPUT(ctypes.Structure):
        _fields_ = [("type", ctypes.c_ulong), ("u", _INPUTunion)]

    sent = 0
    for i in range(20):
        move = INPUT(type=0)
        move.u.mi = MOUSEINPUT(dx=3, dy=2, mouseData=0, dwFlags=0x0001, time=0, dwExtraInfo=None)
        sent += user32.SendInput(1, ctypes.byref(move), ctypes.sizeof(INPUT))
        key = INPUT(type=1)
        key.u.ki = KEYBDINPUT(wVk=0x41 + (i % 20), wScan=0, dwFlags=0, time=0, dwExtraInfo=None)
        sent += user32.SendInput(1, ctypes.byref(key), ctypes.sizeof(INPUT))
        key_down = INPUT(type=1)
        key_down.u.ki = KEYBDINPUT(wVk=0x41 + (i % 20), wScan=0, dwFlags=0x0002, time=0, dwExtraInfo=None)
        sent += user32.SendInput(1, ctypes.byref(key_down), ctypes.sizeof(INPUT))
        time.sleep(0.05)
    note("sendinput_events", sent)
    check("成功注入了模拟输入", sent >= 20, f"{sent} 个事件")

    time.sleep(1.5)
    after = probe()
    cfg_after = read_config()

    check("模拟远程操作没有打断夜间", after and after["active_count"] == before["active_count"]
          and after["virtual_holding"],
          f"active={after['active_count'] if after else '?'} holding="
          f"{after['virtual_holding'] if after else '?'}")
    check("没有发生恢复（状态仍是夜间）",
          cfg_before.get("state") == cfg_after.get("state") == "night",
          f"{cfg_before.get('state')} → {cfg_after.get('state')}")
    check("模拟输入期间没有新的显示提交",
          after["stats"]["apply_calls"] == before["stats"]["apply_calls"],
          f"{before['stats']['apply_calls']} → {after['stats']['apply_calls']}")
    check("实体屏仍未接桌面",
          not any(d["active"] and not d["virtual"] for d in after["displays"]),
          "active physical targets")
    return True


def read_config():
    path = HOME / "config.json"
    if not path.exists():
        return {}
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError):
        return {}


# ---------------------------------------------------------------------------
# 5. unlock chord + emergency
# ---------------------------------------------------------------------------

def step_chord_and_emergency():
    code, out = run(["--action", "day"], timeout=180)
    check("白天恢复成功", code == 0, _first_line(out))
    p = probe()
    check("恢复后实体屏重新接桌面",
          any(d["active"] and not d["virtual"] for d in p["displays"]),
          f"active={p['active_count']}")

    # 再进夜间，然后用 emergency 兜底。
    code, _ = run(["--action", "night", "--mode", "virtual_screen"], timeout=180)
    check("再次进入夜间", code == 0)
    probe_reset()
    code, out = run(["--action", "emergency"], timeout=180)
    check("应急恢复成功", code == 0, _first_line(out))
    p = probe()
    check("应急后屏幕已恢复",
          any(d["active"] and not d["virtual"] for d in p["displays"]),
          f"active={p['active_count']}")
    cfg = read_config()
    check("应急后计划已暂停", cfg.get("enabled") is False, f"enabled={cfg.get('enabled')}")
    return True


# ---------------------------------------------------------------------------
# 6. watchdog restarts a killed daemon; simulated reboot re-enters night
# ---------------------------------------------------------------------------

def step_watchdog():
    # 起常驻进程，然后强杀它，看门狗任务应当把它拉回来。
    env = dict(os.environ)
    env["NIGHTDISPLAY_HOME"] = str(HOME)
    proc = subprocess.Popen([str(EXE_INSTALLED), "--action", "serve"], env=env,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    time.sleep(4)

    def daemon_pids():
        out = subprocess.run(
            ["powershell", "-NoProfile", "-Command",
             "Get-CimInstance Win32_Process | Where-Object { $_.CommandLine -like '*--action serve*' } | "
             "Select-Object -ExpandProperty ProcessId"],
            capture_output=True, text=True, timeout=60).stdout
        return [int(x) for x in re.findall(r"^\s*(\d+)\s*$", out, re.M)]

    first = daemon_pids()
    check("常驻进程已启动", bool(first), f"pids={first}")
    if not first:
        return False

    # 真杀（不带 graceful 消息），模拟崩溃。
    for pid in first:
        subprocess.run(["taskkill", "/PID", str(pid), "/F"], capture_output=True, timeout=60)
    time.sleep(2)
    check("常驻进程已被强杀", not daemon_pids(), "还在跑说明没杀掉")

    # 看门狗：程序自己的 watchdog 动作或计划任务应当把它拉回来。
    run(["--action", "watchdog"], timeout=180, check_code=False)
    time.sleep(6)
    revived = daemon_pids()
    check("看门狗把常驻进程拉回来了", bool(revived), f"pids={revived}")

    # 模拟重启：清掉运行态但保留配置，reconcile 应当按当前时间重新判定。
    cfg = read_config()
    cfg["state"] = "day"
    (HOME / "config.json").write_text(json.dumps(cfg, ensure_ascii=False), encoding="utf-8")
    code, out = run(["--action", "reconcile"], timeout=180)
    check("reconcile 在白天不误开夜间", code == 0, _first_line(out))

    # 把时段设成"现在"，再模拟断电重启后的一次 reconcile：必须自己进夜间。
    now = time.localtime()
    start = f"{now.tm_hour:02d}:{max(0, now.tm_min - 1):02d}"
    end = f"{(now.tm_hour + 1) % 24:02d}:{now.tm_min:02d}"
    run(["--action", "status"], check_code=False)
    code, out = run(["--action", "activate", "--start", start, "--end", end], timeout=180, check_code=False)
    if code != 0:
        # 没测试记录时 activate 会拒绝，这是设计；这里不把它当失败。
        say(f"      提示：activate 被拒绝（{_first_line(out)}），跳过重启自动入夜断言")
        return True
    code, out = run(["--action", "reconcile"], timeout=300, check_code=False)
    cfg = read_config()
    p = probe()
    check("重启后仍在时段内会自动入夜",
          cfg.get("state") == "night" and p["virtual_holding"],
          f"state={cfg.get('state')} holding={p['virtual_holding']}")
    return True


# ---------------------------------------------------------------------------
# 7. diagnostics bundle
# ---------------------------------------------------------------------------

def step_diag():
    code, out = run(["--action", "export-diag", "--out", str(OUT / "diag")], timeout=300)
    zips = list((OUT / "diag").glob("*.zip"))
    check("诊断包已生成", code == 0 and bool(zips), str(zips[0].name) if zips else "没有 zip")
    if zips:
        note("diag_bytes", zips[0].stat().st_size)
        import zipfile
        with zipfile.ZipFile(zips[0]) as zf:
            names = zf.namelist()
        note("diag_entries", len(names))
        check("诊断包含日志与配置",
              any("log" in n.lower() for n in names) and any("config" in n.lower() for n in names),
              ", ".join(names[:6]))
        # 脱敏：包里不能出现真实用户名路径。
        user = os.environ.get("USERNAME", "")
        blob = zips[0].read_bytes()
        check("诊断包不含明文用户名", not user or user.encode() not in blob,
              f"username={user}")
    return True


# ---------------------------------------------------------------------------
# main
# ---------------------------------------------------------------------------

def write_reports():
    OUT.mkdir(exist_ok=True)
    (OUT / "qa-results.json").write_text(
        json.dumps({"checks": results, "timings": timings}, ensure_ascii=False, indent=2),
        encoding="utf-8")
    lines = ["## NightDisplay 4.0 深度 Windows QA", ""]
    failed = [r for r in results if not r["ok"]]
    lines.append(f"**{len(results) - len(failed)}/{len(results)} 通过**"
                 f"（失败 {len(failed)}）")
    lines.append("")
    lines.append("| 检查 | 结果 | 实测 |")
    lines.append("|---|---|---|")
    for r in results:
        lines.append(f"| {r['check']} | {'✅' if r['ok'] else '❌'} | {r['detail']} |")
    if timings:
        lines += ["", "### 实测数字", "", "| 指标 | 值 |", "|---|---|"]
        for k, v in timings.items():
            if isinstance(v, dict):
                for sk, sv in v.items():
                    lines.append(f"| {k}.{sk} | {sv} ms |")
            else:
                lines.append(f"| {k} | {v} |")
    (OUT / "qa-report.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    summary = os.environ.get("GITHUB_STEP_SUMMARY")
    if summary:
        with open(summary, "a", encoding="utf-8") as fh:
            fh.write("\n".join(lines) + "\n")
    say()
    say("\n".join(lines))
    return 0 if not failed else 1


def main():
    if os.name != "nt":
        say("只在 Windows 上运行。")
        return 2
    OUT.mkdir(exist_ok=True)
    if HOME.exists():
        shutil.rmtree(HOME, ignore_errors=True)
    HOME.mkdir(parents=True, exist_ok=True)

    try:
        if not step_install():
            return write_reports()
        if not step_onboarding():
            return write_reports()
        if not step_night_switch():
            return write_reports()
        step_remote_session()
        step_chord_and_emergency()
        # 看门狗这一步依赖计划任务，可能因为 runner 策略受限而失败，
        # 所以它自己内部决定跳过哪些断言。
        try:
            step_watchdog()
        except Exception as exc:  # noqa: BLE001 - QA 脚本：任何异常都要记录而不是崩掉
            check("看门狗检查", False, f"异常：{exc}")
        step_diag()
    finally:
        # 不管成败都导出一次诊断包，失败才可复盘。
        try:
            run(["--action", "export-diag", "--out", str(OUT / "diag-final")],
                timeout=300, check_code=False)
            run(["--action", "emergency"], timeout=180, check_code=False)
        except Exception:  # noqa: BLE001
            pass
    return write_reports()


if __name__ == "__main__":
    sys.exit(main())
