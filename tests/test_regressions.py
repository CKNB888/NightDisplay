"""Failure paths that previously reported success or restored the wrong session."""
import datetime as dt
import os
import subprocess
import sys
from concurrent.futures import ThreadPoolExecutor
from types import SimpleNamespace

import pytest

from nightdisplay import app, core, ipc
from nightdisplay import backends as B
from nightdisplay import linux_display as L
from nightdisplay import win_display as W
from nightdisplay.storage import write_json


@pytest.fixture(autouse=True)
def isolated_home(tmp_path, monkeypatch):
    monkeypatch.setenv("NIGHTDISPLAY_HOME", str(tmp_path / "state"))


@pytest.mark.parametrize("value", ["", "12", "12:1", "-1:10", "24:00", "00:60", None])
def test_invalid_times(value):
    with pytest.raises(ValueError):
        core.parse_hm(value)


def test_equal_normalized_start_rejected():
    with pytest.raises(RuntimeError, match="不能相同"):
        app.activate("auto", "9:00", "09:00")


def test_full_day_has_no_false_boundary():
    c = dict(core.DEFAULT, start="00:00", end="12:00", extra_ranges=[["12:00", "00:00"]])
    assert core.next_switch(c, dt.datetime(2026, 1, 1, 5)) == (None, 86400)


def test_manual_night_holds_until_next_morning():
    cfg = dict(core.DEFAULT, enabled=True, start="20:00", end="10:30")
    now = dt.datetime(2026, 10, 27, 10, 46)
    until = core.manual_night_until(cfg, now)
    assert until == dt.datetime(2026, 10, 28, 10, 30)
    assert core.manual_night_until(cfg, dt.datetime(2026, 10, 27, 21, 0)) is None


def test_reconcile_keeps_manual_night_outside_schedule(monkeypatch):
    core.update(enabled=True, state="night", night_mode="screen_off",
                start="20:00", end="10:30",
                hold_night_until="2026-10-28T10:30:00")
    calls = []
    monkeypatch.setattr(B, "go_day", lambda *a, **k: calls.append("day"))
    monkeypatch.setattr(B, "go_night", lambda *a, **k: calls.append("night"))
    monkeypatch.setattr(B, "ensure_night", lambda: calls.append("ensure"))

    class Frozen(dt.datetime):
        @classmethod
        def now(cls, tz=None):
            base = cls(2026, 10, 27, 10, 46)
            return base.replace(tzinfo=tz) if tz is not None else base

    monkeypatch.setattr(core.dt, "datetime", Frozen)
    monkeypatch.setattr(app.dt, "datetime", Frozen)
    assert app.reconcile() == "unchanged"
    assert calls == ["ensure"]
    assert core.load()["hold_night_until"] == "2026-10-28T10:30:00"


def test_expired_manual_night_returns_to_day(monkeypatch):
    core.update(enabled=True, state="night", night_mode="screen_off",
                start="20:00", end="10:30",
                hold_night_until="2026-10-27T10:30:00")
    calls = []

    def go_day(*a, **k):
        calls.append("day")
        core.update(state="day", night_mode="", hold_night_until="")

    monkeypatch.setattr(B, "go_day", go_day)
    monkeypatch.setattr(B, "go_night", lambda *a, **k: calls.append("night"))

    class Frozen(dt.datetime):
        @classmethod
        def now(cls, tz=None):
            base = cls(2026, 10, 27, 10, 46)
            return base.replace(tzinfo=tz) if tz is not None else base

    monkeypatch.setattr(core.dt, "datetime", Frozen)
    monkeypatch.setattr(app.dt, "datetime", Frozen)
    assert app.reconcile() == "day"
    assert calls == ["day"]
    assert core.load()["hold_night_until"] == ""


def test_updates_survive_threads_and_processes():
    def task(i):
        core.update(**{"thread_%d" % i: i})
    with ThreadPoolExecutor(max_workers=8) as pool:
        list(pool.map(task, range(40)))
    script = "from nightdisplay.core import update; import sys; update(**{sys.argv[1]: True})"
    ps = [subprocess.Popen([sys.executable, "-c", script, "process_%d" % i]) for i in range(6)]
    assert all(p.wait(timeout=20) == 0 for p in ps)
    c = core.load()
    assert all(c["thread_%d" % i] == i for i in range(40))
    assert all(c["process_%d" % i] for i in range(6))


def test_atomic_writer_keeps_old_file_on_serialization_failure():
    core.update(start="21:00")
    with pytest.raises(TypeError):
        write_json(core.config_path(), {"oops": object()})
    assert core.load()["start"] == "21:00"


@pytest.mark.parametrize("data", [[], {"enabled": "true"}, {"extra_ranges": [["10:00"]]}, {"theme": "bad"}])
def test_corrupt_config_reported(data):
    write_json(core.config_path(), data)
    with pytest.raises(RuntimeError, match="配置文件损坏"):
        core.load()


def test_backup_corruption_does_not_block_fallback(monkeypatch):
    core.home()
    with open(os.path.join(core.home(), "backups", "display-lastgood.json"), "w") as f:
        f.write("{")
    got = []
    monkeypatch.setattr(B, "IS_WIN", False)
    monkeypatch.setattr(B, "IS_LINUX", True)
    monkeypatch.setattr(L, "restore", lambda b, lg: got.append(b) or 2)
    assert B.restore_display() == 2 and got == [None]


def test_restore_failure_preserves_night_and_awake(monkeypatch):
    core.update(state="night", night_mode="virtual_only")
    monkeypatch.setattr(B, "restore_display", lambda: (_ for _ in ()).throw(RuntimeError("driver failed")))
    monkeypatch.setattr(B, "screen_on", lambda: None)
    calls = []
    monkeypatch.setattr(B, "keep_awake", lambda on: calls.append(on))
    with pytest.raises(RuntimeError, match="driver failed"):
        B.go_day()
    assert core.load()["state"] == "night" and not calls


@pytest.mark.parametrize("mode", ["overlay", "screen_off"])
def test_day_does_not_apply_stale_virtual_backup(mode, monkeypatch):
    core.update(state="night", night_mode=mode)
    monkeypatch.setattr(B, "restore_display", lambda: pytest.fail("stale display restore"))
    monkeypatch.setattr(B, "screen_on", lambda: None)
    monkeypatch.setattr(B, "keep_awake", lambda on: None)
    assert B.go_day(lambda on: True, mode="all") == mode
    assert core.load()["state"] == "day"


def test_emergency_restores_even_if_task_cleanup_fails(monkeypatch):
    monkeypatch.setattr(B, "schedule_remove", lambda: (_ for _ in ()).throw(RuntimeError("scheduler failed")))
    restored = []
    monkeypatch.setattr(B, "go_day", lambda *a, **kw: restored.append(True))
    with pytest.raises(RuntimeError, match="scheduler failed"):
        app.emergency()
    assert restored and not core.load()["enabled"]


def test_activation_failure_does_not_leave_enabled(monkeypatch):
    now = dt.datetime.now().isoformat()
    core.update(last_test_mode="screen_off", last_recovery_mode="screen_off", last_test_at=now,
                last_test_ok=True, last_recovery_at=now)
    monkeypatch.setattr(B, "schedule_install", lambda c: (_ for _ in ()).throw(RuntimeError("denied")))
    monkeypatch.setattr(B, "schedule_remove", lambda: None)
    with pytest.raises(RuntimeError, match="denied"):
        app.activate("screen_off", "20:00", "10:30")
    assert core.load()["enabled"] is False


@pytest.mark.parametrize("seconds", [-1, 0, 1, 3601])
def test_invalid_test_duration_has_no_helper(seconds, monkeypatch):
    monkeypatch.setattr(B, "spawn_detached", lambda cmd: pytest.fail("spawned helper"))
    with pytest.raises(ValueError):
        app.start_test("screen_off", seconds)


def test_failed_test_cannot_be_marked_successful(monkeypatch):
    monkeypatch.setattr(B, "spawn_detached", lambda cmd: None)
    monkeypatch.setattr(B, "go_night", lambda *a: (_ for _ in ()).throw(RuntimeError("no monitor")))
    with pytest.raises(RuntimeError):
        app.start_test("screen_off", 15)
    assert not core.load()["test_id"] and not core.load()["last_test_ok"]


def test_old_test_recovery_cannot_restore_new_night(monkeypatch):
    core.update(test_id="new", state="night", night_mode="virtual_only")
    monkeypatch.setattr(B, "go_day", lambda *a, **kw: pytest.fail("old recovery ran"))
    assert app.finish_test("old") == "stale"
    assert core.load()["state"] == "night"


def test_test_recovery_failure_is_not_success(monkeypatch):
    core.update(test_id="current", state="night", last_test_night_ok=True)
    monkeypatch.setattr(B, "go_day", lambda *a, **kw: (_ for _ in ()).throw(RuntimeError("failed")))
    with pytest.raises(RuntimeError):
        app.finish_test("current")
    assert not core.load()["last_test_ok"] and core.load()["state"] == "night"


def test_cli_mode_reaches_resident(monkeypatch):
    got = []
    monkeypatch.setattr(app, "send_ipc", lambda cmd: got.append(cmd) or "ok")
    assert app.main(["--action", "night", "--mode", "screen_off"]) == 0
    assert got == [{"action": "night", "mode": "screen_off"}]


def test_ipc_error_is_nonzero(monkeypatch):
    monkeypatch.setattr(app, "send_ipc", lambda cmd: "error: rejected")
    assert app.main(["--action", "night"]) == 1


def test_ipc_timeout_never_repeats_night_action(monkeypatch):
    monkeypatch.setattr(app, "send_ipc", lambda cmd: (_ for _ in ()).throw(RuntimeError("timeout")))
    monkeypatch.setattr(B, "go_night", lambda *a: pytest.fail("repeated action"))
    assert app.main(["--action", "night"]) == 1


def test_ipc_roundtrip_and_home_isolation(tmp_path, monkeypatch):
    server = ipc.Server(lambda cmd: "ok:" + cmd)
    server.start()
    try:
        assert ipc.send("hello") == "ok:hello"
        with monkeypatch.context() as m:
            m.setenv("NIGHTDISPLAY_HOME", str(tmp_path / "another"))
            assert ipc.send("hello") is None
    finally:
        server.close()
    assert ipc.send("hello") is None


def test_self_cmd_works_outside_repo():
    cmd = B.self_cmd() + ["--action", "selfcheck"]
    p = subprocess.run(cmd, cwd=core.home(), capture_output=True, text=True,
                       encoding="utf-8", errors="replace", timeout=20)
    assert p.returncode == 0, p.stderr
    assert core.home() in cmd


def test_windows_default_prefers_virtual_and_has_compatible_fallback(monkeypatch):
    monkeypatch.setattr(B, "IS_WIN", True)
    monkeypatch.setattr(B, "has_virtual_display", lambda match="": True)
    assert B.resolve_mode("auto") == "virtual_only"
    monkeypatch.setattr(B, "has_virtual_display", lambda match="": False)
    monkeypatch.setattr(B, "win_build", lambda: 19041)
    assert B.resolve_mode("auto") == "overlay"
    monkeypatch.setattr(B, "win_build", lambda: 18362)
    assert B.resolve_mode("auto") == "screen_off"


def test_windows_invalid_flags_not_used_or_persisted(monkeypatch):
    calls = []
    monkeypatch.setattr(W, "plan_virtual_only", lambda m: ([bytes(72)], {"name": "VDD"}, 1))
    def set_config(p, m, flags):
        calls.append(flags)
        return 1610 if flags & W.SDC_TOPOLOGY_SUPPLIED else 0
    monkeypatch.setattr(W, "set_config", set_config)
    W.apply_virtual_only(log=lambda s: None)
    assert len(calls) == 3
    assert not calls[-1] & W.SDC_SAVE_TO_DATABASE
    assert not (calls[-1] & W.SDC_ALLOW_PATH_ORDER_CHANGES)


def test_malformed_windows_backup_falls_back(monkeypatch):
    calls = []
    monkeypatch.setattr(W, "set_topology", lambda f: calls.append(f) or 0)
    monkeypatch.setattr(W, "describe", lambda *a: [{"tech": 5, "name": "Physical"}])
    assert W.restore({"paths": ["garbage"], "modes": []}, lambda s: None) == 1
    assert calls == [W.SDC_TOPOLOGY_INTERNAL]


def test_windows_restore_rejects_virtual_only_success(monkeypatch):
    monkeypatch.setattr(W, "set_topology", lambda f: 0)
    monkeypatch.setattr(W, "describe", lambda *a: [{"tech": 17}])
    with pytest.raises(W.DisplayError):
        W.restore(None, lambda s: None)


def test_xrandr_negative_offsets_and_rotation():
    text = "DP-1 connected primary 1080x1920-1080+0 left (normal left inverted right x axis y axis)\n   1920x1080 60.00*+\n"
    display = L.parse_xrandr(text)[0]
    assert display["active"] and display["pos"] == (-1080, 0)
    assert display["mode"] == "1920x1080" and display["rotate"] == "left"


def test_wayland_rejected_even_with_xwayland(monkeypatch):
    monkeypatch.setenv("WAYLAND_DISPLAY", "wayland-0")
    monkeypatch.setenv("DISPLAY", ":1")
    monkeypatch.setattr(L.shutil, "which", lambda s: s)
    with pytest.raises(RuntimeError, match="Wayland"):
        L._need("xrandr")


def test_no_kill_from_stale_awake_pid(monkeypatch):
    with open(os.path.join(core.home(), "awake.pid"), "w") as f:
        f.write("12345:python")
    monkeypatch.setattr(os, "kill", lambda *a: pytest.fail("killed disk PID"))
    B.keep_awake(False)
    assert core.load()["awake_token"] == ""


def test_frozen_helpers_get_independent_extraction(monkeypatch):
    monkeypatch.setattr(sys, "frozen", True, raising=False)
    seen = {}
    monkeypatch.setattr(B.subprocess, "Popen", lambda cmd, **kwargs: seen.update(kwargs) or object())
    B.spawn_detached(["NightDisplay", "--action", "day"])
    assert seen["env"]["PYINSTALLER_RESET_ENVIRONMENT"] == "1"


def test_external_commands_do_not_inherit_bundled_libraries(monkeypatch):
    monkeypatch.setattr(sys, "frozen", True, raising=False)
    monkeypatch.setenv("LD_LIBRARY_PATH", "/tmp/_MEI/bundled")
    monkeypatch.setenv("LD_LIBRARY_PATH_ORIG", "/usr/lib/original")
    assert B.native_env()["LD_LIBRARY_PATH"] == "/usr/lib/original"


def test_tray_only_start_does_not_show_existing_window(monkeypatch):
    calls = []
    monkeypatch.setattr(app, "send_ipc", lambda cmd: calls.append(cmd) or "ok")
    monkeypatch.setattr(app, "run_app", lambda **kw: pytest.fail("started a second GUI"))
    assert app.main(["--tray"]) == 0
    assert calls == ["ping"]


def test_gui_start_shows_existing_window(monkeypatch):
    calls = []
    monkeypatch.setattr(app, "send_ipc", lambda cmd: calls.append(cmd) or "ok")
    monkeypatch.setattr(app, "run_app", lambda **kw: pytest.fail("started a second GUI"))
    assert app.main(["--action", "gui"]) == 0
    assert calls == ["show"]


def test_tray_only_starts_hidden_without_resident(monkeypatch):
    seen = {}
    monkeypatch.setattr(app, "send_ipc", lambda cmd: None)
    monkeypatch.setattr(app, "run_app", lambda **kw: seen.update(kw) or 0)
    assert app.main(["--tray"]) == 0
    assert seen["show_window"] is False


def test_activate_rejects_new_auto_resolution(monkeypatch):
    now = dt.datetime.now().isoformat()
    core.update(last_test_mode="screen_off", last_test_requested="auto",
                last_recovery_mode="screen_off", last_test_at=now,
                last_test_ok=True, last_recovery_at=now)
    monkeypatch.setattr(B, "resolve_mode", lambda mode, match="": "virtual_only" if mode == "auto" else mode)
    monkeypatch.setattr(B, "available_modes", lambda: ["auto", "virtual_only", "screen_off"])
    monkeypatch.setattr(B, "schedule_install", lambda cfg: pytest.fail("enabled an untested mode"))
    with pytest.raises(RuntimeError, match="完成一次测试"):
        app.activate("auto", "20:00", "10:30")


def test_abort_test_marks_interrupted_test_unsuccessful(monkeypatch):
    core.update(test_id="t", state="night", night_mode="screen_off",
                last_test_night_ok=True, last_test_ok=False)
    monkeypatch.setattr(B, "go_day", lambda *a, **kw: core.update(state="day", night_mode=""))
    assert app.abort_test() == "aborted"
    cfg = core.load()
    assert not cfg["test_id"] and not cfg["last_test_ok"] and not cfg["test_aborted"]


def test_aborted_test_recovery_is_not_success(monkeypatch):
    core.update(test_id="t", state="night", night_mode="screen_off",
                last_test_mode="screen_off", last_test_night_ok=True,
                last_test_ok=False, test_aborted=True)
    monkeypatch.setattr(B, "go_day", lambda *a, **kw: core.update(state="day", night_mode=""))
    assert app.finish_test("t") == "ok"
    cfg = core.load()
    assert not cfg["test_id"] and not cfg["last_test_ok"] and not cfg["test_aborted"]


def test_abort_test_failure_keeps_helper_session(monkeypatch):
    core.update(test_id="t", state="night", night_mode="screen_off", last_test_night_ok=True)
    monkeypatch.setattr(B, "go_day", lambda *a, **kw: (_ for _ in ()).throw(RuntimeError("failed")))
    with pytest.raises(RuntimeError, match="failed"):
        app.abort_test()
    cfg = core.load()
    assert cfg["test_id"] == "t" and cfg["test_aborted"] is True


def test_windows_available_modes_gate_overlay(monkeypatch):
    monkeypatch.setattr(B, "IS_WIN", True)
    monkeypatch.setattr(B, "win_build", lambda: 18362)
    assert "overlay" not in B.available_modes()
    with pytest.raises(RuntimeError, match="不支持的模式"):
        B.validate_mode("overlay")
    monkeypatch.setattr(B, "win_build", lambda: 19041)
    assert "overlay" in B.available_modes()


def test_schedule_status_lists_extra_tasks(monkeypatch):
    monkeypatch.setattr(B, "IS_WIN", True)
    csv = '"NightDisplay_Night","N/A","Ready"\n"NightDisplay_T02","N/A","Ready"\n'
    monkeypatch.setattr(B, "run", lambda *a, **kw: SimpleNamespace(stdout=csv, returncode=0))
    status = B.schedule_status()
    assert "NightDisplay_Night：已注册" in status
    assert "NightDisplay_T02：已注册" in status
    assert "NightDisplay_Day：未注册" in status


@pytest.mark.parametrize("kw", [
    {"virtual_match": 1},
    {"last_test_at": 1},
    {"last_test_mode": []},
    {"last_test_requested": "bad"},
    {"extra_ranges": "x"},
    {"test_aborted": "yes"},
    {"virtual_match": "["},
])
def test_config_validation_rejects_bad_runtime_state(kw):
    with pytest.raises(ValueError):
        core.validate_config(dict(core.DEFAULT, **kw))


def test_config_normalizes_unlock_chord():
    cfg = dict(core.DEFAULT, unlock_chord=["ctrl", "k"])
    core.validate_config(cfg)
    assert cfg["unlock_chord"] == ["Ctrl", "K"]


def test_non_object_display_backup_falls_back(monkeypatch):
    write_json(os.path.join(core.home(), "backups", "display-lastgood.json"), ["bad"])
    got = []
    monkeypatch.setattr(B, "IS_WIN", False)
    monkeypatch.setattr(B, "IS_LINUX", True)
    monkeypatch.setattr(L, "restore", lambda b, lg: got.append(b) or 2)
    assert B.restore_display() == 2 and got == [None]


def test_ipc_close_is_idempotent():
    server = ipc.Server(lambda cmd: "ok")
    server.start()
    server.close()
    server.close()
    assert ipc.send("hello") is None


def test_ipc_server_cannot_start_twice():
    server = ipc.Server(lambda cmd: "ok")
    server.start()
    try:
        with pytest.raises(RuntimeError, match="已经启动"):
            server.start()
    finally:
        server.close()


def test_log_does_not_raise_when_data_directory_fails(monkeypatch):
    monkeypatch.setattr(core, "home", lambda: (_ for _ in ()).throw(OSError("denied")))
    core.log("still alive")


def test_transition_frames_cover_full_progress():
    from nightdisplay.ui import transition_frames

    frames = transition_frames(2.5)
    assert len(frames) == 10
    assert frames[0] == (1, 10, 1)
    assert frames[-1] == (10, 10, 2)
    assert [step for step, _, _ in frames] == list(range(1, 11))
    assert all(1 <= dots <= 4 for _, _, dots in frames)


def test_environment_report_does_not_install_or_raise():
    from nightdisplay import app

    report = app.environment_report()
    assert report["pure_black"] is True
    assert isinstance(report["available_modes"], list)
    assert report["ready_mode"] in ("auto", "virtual_only", "overlay", "screen_off", "")
    assert "virtual_display" in report
    assert "registry_writable" in report
