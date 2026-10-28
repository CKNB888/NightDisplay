"""Failure paths that previously reported success or restored the wrong session."""
import datetime as dt
import json
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


def test_short_manual_night_is_not_skipped():
    cfg = dict(core.DEFAULT, enabled=True, start="20:00", end="20:01")
    until = core.manual_night_until(cfg, dt.datetime(2026, 10, 27, 19, 0))
    assert until == dt.datetime(2026, 10, 27, 20, 1)


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


def test_plain_virtual_word_is_not_a_virtual_display():
    assert not W.is_virtual({"name": "Generic Virtual Adapter", "device_path": "", "tech": 0})
    assert W.is_virtual({"name": "Virtual Display Driver", "tech": 0})
    assert W.is_virtual({"name": "Panel", "tech": 17})


def test_windows_restore_skips_all_virtual_backup(monkeypatch):
    called = []
    monkeypatch.setattr(W, "set_config", lambda *a, **k: called.append("config") or 0)
    monkeypatch.setattr(W, "set_topology", lambda f: called.append("topo") or 0)
    monkeypatch.setattr(W, "describe", lambda *a, **k: [{"tech": 5, "name": "Panel", "tgt_adapter": 1, "tgt_id": 2}])
    level = W.restore({
        "paths": [bytes(72).hex()],
        "modes": [bytes(64).hex()],
        "displays": [{"name": "VDD", "tech": 17, "active": True}],
    }, lambda s: None)
    assert level == 1 and called == ["topo"]


def test_matching_virtual_targets_are_not_a_restore(monkeypatch):
    monkeypatch.setattr(W, "set_config", lambda *a, **k: 0)
    monkeypatch.setattr(W, "set_topology", lambda f: 0)
    monkeypatch.setattr(W, "parse_path", lambda b: {"tgt_adapter": 1, "tgt_id": 2, "active": True, "tech": 17})
    monkeypatch.setattr(W, "describe", lambda *a, **k: [{
        "tgt_adapter": 1, "tgt_id": 2, "tech": 17, "name": "VDD", "active": True,
    }])
    with pytest.raises(W.DisplayError):
        W.restore({"paths": [bytes(72).hex()], "modes": [bytes(64).hex()]}, lambda s: None)


def _freeze(monkeypatch, moment):
    class Frozen(dt.datetime):
        @classmethod
        def now(cls, tz=None):
            return moment.replace(tzinfo=tz) if tz is not None else moment

    monkeypatch.setattr(core.dt, "datetime", Frozen)
    monkeypatch.setattr(app.dt, "datetime", Frozen)


def test_failed_chord_unlock_still_blocks_the_rest_of_the_night(monkeypatch):
    core.update(enabled=True, state="night", night_mode="screen_off",
                start="20:00", end="10:30", hold_night_until="2026-10-28T10:30:00")
    _freeze(monkeypatch, dt.datetime(2026, 10, 27, 21, 0))
    monkeypatch.setattr(B, "go_day", lambda *a, **k: (_ for _ in ()).throw(RuntimeError("display")))
    with pytest.raises(RuntimeError):
        app.chord_unlock()
    cfg = core.load()
    assert cfg["hold_night_until"] == ""
    assert cfg["override_until"].startswith("2026-10-28T10:30")


def test_explicit_day_and_repeat_night_follow_override(monkeypatch):
    core.update(enabled=True, state="night", start="20:00", end="10:30", hold_night_until="x")
    _freeze(monkeypatch, dt.datetime(2026, 10, 27, 21, 0))
    app.remember_explicit_day()
    assert core.load()["override_until"].startswith("2026-10-28T10:30")
    assert core.load()["hold_night_until"] == ""
    app.forget_unlock_override()
    assert core.load()["override_until"] == ""


def test_linux_backup_refuses_virtual_only_topology(monkeypatch):
    monkeypatch.setattr(B, "IS_WIN", False)
    monkeypatch.setattr(B, "IS_LINUX", True)
    monkeypatch.setattr(L, "backup", lambda: {"outputs": [{"name": "VIRTUAL1", "active": True}]})
    with pytest.raises(RuntimeError, match="虚拟屏"):
        B._backup_display()


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


def test_overlay_geometry_covers_negative_virtual_origin():
    assert app.overlay_geometry(0, 0, 1920, 1080) == "1920x1080+0+0"
    assert app.overlay_geometry(-1920, -120, 3840, 1200) == "3840x1200-1920-120"
    with pytest.raises(ValueError):
        app.overlay_geometry(0, 0, 0, 1080)


def test_borderless_style_strips_caption_and_keeps_popup():
    assert app.borderless_style(0x00CF0000) == 0x80000000
    assert app.borderless_style(0x10000000 | 0x00C00000) & 0x10000000
    assert app.borderless_style(0x10000000 | 0x00C00000) & 0x00CF0000 == 0
    assert app.overlay_dwm_attributes(19041) == ((2, 1), (3, 1))
    assert app.overlay_dwm_attributes(22000) == ((2, 1), (3, 1), (33, 1), (34, 0xFFFFFFFE))


def test_settings_window_stays_hidden_for_tray_startup():
    assert app.settings_visibility(True, True, False) == (True, False, True)
    assert app.settings_visibility(False, True, True) == (False, False, True)
    assert app.settings_visibility(False, False, True) == (False, True, False)
    assert app.settings_visibility(False, False, False) == (False, False, False)
    assert app.settings_visibility(False, True, False) == (False, False, False)


def test_blank_cursor_replaces_every_system_cursor_once():
    class User32:
        def __init__(self):
            self.calls = []
            self._next = 1

        def GetSystemMetrics(self, index):
            return 32

        def CreateCursor(self, inst, x, y, w, h, and_mask, xor_mask):
            self.calls.append(("create", w, h))
            self._next += 1
            return self._next

        def CopyIcon(self, handle):
            self.calls.append(("copy", handle))
            self._next += 1
            return self._next

        def SetSystemCursor(self, handle, cursor_id):
            self.calls.append(("set", handle, cursor_id))
            return True

        def DestroyCursor(self, handle):
            self.calls.append(("destroy", handle))
            return True

    user32 = User32()
    app.hide_system_cursor(user32)
    assert [item[2] for item in user32.calls if item[0] == "set"] == list(app.CURSOR_IDS)
    assert sum(item[0] == "copy" for item in user32.calls) == len(app.CURSOR_IDS)
    assert user32.calls[-1][0] == "destroy"

    class Restore:
        def SystemParametersInfoW(self, action, param, value, flags):
            self.got = (action, param, value, flags)
            return True

    restore = Restore()
    app.restore_system_cursor(restore)
    assert restore.got == (0x0057, 0, None, 0)


def test_physical_restore_is_saved_only_after_it_is_confirmed(monkeypatch):
    flags = []
    monkeypatch.setattr(W, "set_config", lambda paths, modes, fl: flags.append(fl) or 0)
    monkeypatch.setattr(W, "set_topology", lambda flag: pytest.fail("topology"))
    monkeypatch.setattr(W, "parse_path", lambda raw: {"tgt_adapter": (1, 0), "tgt_id": 0, "active": True, "tech": 5})
    monkeypatch.setattr(W, "describe", lambda *args, **kwargs: [{
        "tgt_adapter": (1, 0), "tgt_id": 0, "tech": 5, "name": "Internal", "active": True,
    }])
    level = W.restore({"paths": [bytes(72).hex()], "modes": [bytes(64).hex()]}, lambda _s: None)
    assert level == 1 and len(flags) == 2
    assert flags[0] & W.SDC_APPLY and not flags[0] & W.SDC_SAVE_TO_DATABASE
    assert flags[1] & W.SDC_SAVE_TO_DATABASE


def test_virtual_result_is_never_saved_without_display_metadata(monkeypatch):
    flags = []
    monkeypatch.setattr(W, "set_config", lambda paths, modes, fl: flags.append(fl) or 0)
    monkeypatch.setattr(W, "set_topology", lambda flag: 0)
    monkeypatch.setattr(W, "parse_path", lambda raw: {"tgt_adapter": (1, 0), "tgt_id": 0, "active": True, "tech": 17})
    monkeypatch.setattr(W, "describe", lambda *args, **kwargs: [{
        "tgt_adapter": (1, 0), "tgt_id": 0, "tech": 17, "name": "VDD", "active": True,
    }])
    with pytest.raises(W.DisplayError):
        W.restore({"paths": [bytes(72).hex()], "modes": [bytes(64).hex()]}, lambda _s: None)
    assert flags
    assert all(not fl & W.SDC_SAVE_TO_DATABASE for fl in flags)


def test_virtual_only_lets_the_panel_power_down(monkeypatch):
    monkeypatch.setattr(B, "IS_WIN", True)
    monkeypatch.setattr(B.time, "sleep", lambda _seconds: None)
    monkeypatch.setattr(B, "_backup_display", lambda: None)
    monkeypatch.setattr(B, "keep_awake", lambda *args, **kwargs: True)
    seen = {}
    monkeypatch.setattr(W, "plan_virtual_only", lambda match: None)
    monkeypatch.setattr(W, "apply_virtual_only", lambda *args, **kwargs: None)
    monkeypatch.setattr(W, "verify_virtual_only", lambda match: True)

    def enter(keep_display_on=True):
        seen["keep"] = keep_display_on

    monkeypatch.setattr("nightdisplay.win_power.enter", enter)
    core.update(state="day", mode="virtual_only", keep_awake=False)
    assert B._go_night("virtual_only") == "virtual_only"
    assert seen["keep"] is False
    assert B.windows_execution_flags("virtual_only") == 0x80000001
    assert B.windows_execution_flags("screen_off") == 0x80000001
    assert B.windows_execution_flags("overlay") == 0x80000003


def test_ensure_night_virtual_does_not_force_the_panel_on(monkeypatch):
    monkeypatch.setattr(B, "IS_WIN", True)
    core.update(state="night", night_mode="virtual_only", keep_awake=False, virtual_match="")
    monkeypatch.setattr(W, "verify_virtual_only", lambda match: True)
    seen = {}

    def enter(keep_display_on=True):
        seen["keep"] = keep_display_on

    monkeypatch.setattr("nightdisplay.win_power.enter", enter)
    B.ensure_night()
    assert seen["keep"] is False


def test_screen_off_is_reissued_and_failure_stays_in_night(monkeypatch):
    monkeypatch.setattr(B, "IS_WIN", True)
    core.update(state="night", night_mode="screen_off", keep_awake=False)
    calls = []
    monkeypatch.setattr(B, "screen_off", lambda quiet=False: calls.append(quiet))
    B.ensure_night()
    assert calls == [True]

    def boom(quiet=False):
        raise RuntimeError("rejected")

    monkeypatch.setattr(B, "screen_off", boom)
    B.ensure_night()
    assert core.load()["state"] == "night"


def test_windows_tasks_catch_up_after_sleep(monkeypatch):
    monkeypatch.setattr(B, "IS_WIN", True)
    monkeypatch.setattr(B, "_win_delete_tasks", lambda: 0)
    monkeypatch.setattr(B, "set_autostart", lambda on: None)
    calls = []

    def fake_run(cmd, check=True, timeout=60, env=None):
        calls.append((cmd, env))
        return SimpleNamespace(returncode=0, stdout="", stderr="")

    monkeypatch.setattr(B, "run", fake_run)
    core.update(start="20:00", end="10:30", enabled=False)
    B.schedule_install(core.load())
    assert calls[0][0][0] == "powershell"
    script = calls[0][0][-1]
    assert "StartWhenAvailable" in script
    assert "LogonType Interactive" in script
    assert "WindowsIdentity" in script
    assert "InvariantCulture" in script
    payload = json.loads(calls[0][1]["NIGHTDISPLAY_TASKS"])
    assert [task["name"] for task in payload["tasks"]] == ["NightDisplay_Night", "NightDisplay_Day"]
    assert [task["time"] for task in payload["tasks"]] == ["10:30", "20:00"]
    assert not any(call[0][0] == "schtasks" for call in calls)


def test_windows_tasks_fall_back_when_powershell_fails(monkeypatch):
    monkeypatch.setattr(B, "IS_WIN", True)
    monkeypatch.setattr(B, "_win_delete_tasks", lambda: 0)
    monkeypatch.setattr(B, "set_autostart", lambda on: None)
    calls = []

    def fake_run(cmd, check=True, timeout=60, env=None):
        if cmd[0] == "powershell":
            raise RuntimeError("no scheduled task module")
        calls.append(cmd)
        return SimpleNamespace(returncode=0, stdout="", stderr="")

    monkeypatch.setattr(B, "run", fake_run)
    core.update(start="20:00", end="10:30", extra_ranges=[["12:00", "13:00"]], enabled=False)
    B.schedule_install(core.load())
    names = [cmd[cmd.index("/TN") + 1] for cmd in calls]
    assert names == ["NightDisplay_Night", "NightDisplay_Day", "NightDisplay_T02", "NightDisplay_T03"]
    assert all("/IT" in cmd for cmd in calls)


def test_environment_report_does_not_install_or_raise():
    from nightdisplay import app

    report = app.environment_report()
    assert report["pure_black"] is True
    assert isinstance(report["available_modes"], list)
    assert report["ready_mode"] in ("auto", "virtual_only", "overlay", "screen_off", "")
    assert "virtual_display" in report
    assert "registry_writable" in report


def test_supported_cursor_ids_match_the_documented_set():
    assert 32640 not in app.CURSOR_IDS
    assert 32641 not in app.CURSOR_IDS
    assert app.CURSOR_IDS == (32512, 32513, 32514, 32515, 32516, 32642, 32643,
                              32644, 32645, 32646, 32648, 32649, 32650)


def test_partial_cursor_hide_rolls_back_and_does_not_free_the_failed_copy():
    class User32:
        def __init__(self):
            self.changed = []
            self.destroyed = []
            self.restores = 0
            self._next = 10

        def GetSystemMetrics(self, index):
            return 32

        def CreateCursor(self, *args):
            self._next += 1
            self.blank = self._next
            return self.blank

        def CopyIcon(self, handle):
            self._next += 1
            return self._next

        def SetSystemCursor(self, handle, cursor_id):
            if cursor_id == 32514:
                self.failed = handle
                return False
            self.changed.append(cursor_id)
            return True

        def DestroyCursor(self, handle):
            self.destroyed.append(handle)
            return True

        def SystemParametersInfoW(self, action, param, value, flags):
            assert (action, param, value, flags) == (0x0057, 0, None, 0)
            self.restores += 1
            self.changed.clear()
            return True

    user = User32()
    with pytest.raises(RuntimeError, match="无法隐藏系统光标"):
        app.hide_system_cursor(user)
    assert user.changed == []
    assert user.restores == 1
    assert user.destroyed == [user.blank]
    assert user.failed not in user.destroyed
    assert not app.cursor_is_dirty()


def test_fresh_process_reloads_cursor_scheme(monkeypatch):
    import ctypes
    monkeypatch.setattr(app, "IS_WIN", True)
    monkeypatch.setattr(ctypes, "windll", SimpleNamespace(user32=object()), raising=False)
    app._cursor["hidden"] = False
    calls = []
    monkeypatch.setattr(app, "restore_system_cursor", lambda user: calls.append(True))
    try:
        app.apply_cursor(False)
        assert calls == [True]
        assert app._cursor["hidden"] is False
    finally:
        app._cursor["hidden"] = False


def test_environment_report_keeps_driver_retry_timestamp(monkeypatch):
    from unittest.mock import MagicMock
    monkeypatch.setattr(app, "IS_WIN", True)
    monkeypatch.setattr(app, "IS_MAC", False)
    monkeypatch.setitem(sys.modules, "winreg", MagicMock())
    write_json(os.path.join(core.home(), "env.json"), {"driver_auto_setup_at": 1234567.0})
    report = app.environment_report()
    with open(os.path.join(core.home(), "env.json"), encoding="utf-8") as handle:
        saved = json.load(handle)
    assert report["driver_auto_setup_at"] == 1234567.0
    assert saved["driver_auto_setup_at"] == 1234567.0


def test_failed_driver_install_waits_seven_days(monkeypatch):
    from unittest.mock import MagicMock
    monkeypatch.setattr(app, "IS_WIN", True)
    monkeypatch.setattr(app, "IS_MAC", False)
    monkeypatch.setitem(sys.modules, "winreg", MagicMock())
    monkeypatch.setattr(B, "has_virtual_display", lambda match="": False)
    monkeypatch.setattr(app, "bundled_driver_present", lambda: True)
    monkeypatch.setattr(B, "autostart_status", lambda: True)
    monkeypatch.setattr(B, "set_autostart", lambda on: None)
    calls = []
    monkeypatch.setattr(app, "install_vdd", lambda op: calls.append(op) or 1)
    app.ensure_environment()
    app.ensure_environment()
    assert calls == ["install"]


def test_existing_virtual_display_still_applies_autostart(monkeypatch):
    from unittest.mock import MagicMock
    monkeypatch.setattr(app, "IS_WIN", True)
    monkeypatch.setattr(app, "IS_MAC", False)
    monkeypatch.setitem(sys.modules, "winreg", MagicMock())
    monkeypatch.setattr(app, "bundled_driver_present", lambda: True)
    monkeypatch.setattr(B, "has_virtual_display", lambda match="": True)
    monkeypatch.setattr(B, "autostart_status", lambda: False)
    writes = []
    monkeypatch.setattr(B, "set_autostart", lambda on: writes.append(on))
    core.update(autostart_preference=True)
    app.ensure_environment()
    assert writes == [True]


def test_null_display_backup_uses_system_topology(monkeypatch):
    calls = []
    monkeypatch.setattr(W, "set_topology", lambda flag: calls.append(flag) or 0)
    monkeypatch.setattr(W, "describe", lambda *args, **kwargs: [{"name": "Physical", "tech": 5, "active": True}])
    assert W.restore({"displays": [None]}, lambda *_args: None) == 1
    assert calls == [W.SDC_TOPOLOGY_INTERNAL]


def test_idle_bundled_vdd_beats_an_active_remote_display(monkeypatch):
    import struct
    def raw(src, tgt, active):
        buf = bytearray(W.PATH_SIZE)
        struct.pack_into("<IiIII", buf, 0, 1, 0, src, 0, 1)
        struct.pack_into("<IiIIIIIIIIiI", buf, 20, 9, 0, tgt, 0, 17, 0, 0, 0, 0, 0, 1, 1)
        struct.pack_into("<I", buf, 68, W.PATH_ACTIVE if active else 0)
        return bytes(buf)
    remote = raw(1, 7, True)
    bundled = raw(2, 8, False)
    monkeypatch.setattr(W, "query", lambda flags: ([remote, bundled], []))
    def names(adapter, tgt):
        if tgt == 7:
            return "UU Virtual Display", r"\\?\DISPLAY#UU#1"
        return "VDD by MTT", r"\\?\DISPLAY#MTT0001"
    monkeypatch.setattr(W, "target_name", names)
    monkeypatch.setattr(W, "adapter_name", lambda adapter: r"Root\MttVDD" if adapter == (9, 0) else r"Root\UU")
    _paths, info, physical = W.plan_virtual_only()
    assert info["name"] == "VDD by MTT"
    assert physical == 0
    idle = {"name": "VDD by MTT", "device_path": "DISPLAY#MTT1"}
    busy = {"name": "VDD by MTT", "device_path": "DISPLAY#MTT2"}
    other = {"name": "UU Virtual Display", "device_path": "DISPLAY#UU"}
    assert W.virtual_candidate_key(idle, False, 3) < W.virtual_candidate_key(busy, True, 1)
    assert W.virtual_candidate_key(busy, True, 1) < W.virtual_candidate_key(other, False, 0)


def test_verified_auto_fallback_is_saved(monkeypatch):
    now = dt.datetime.now().isoformat()
    core.update(last_test_ok=True, last_test_mode="overlay", last_test_requested="auto",
                last_test_capability="same", last_recovery_mode="overlay",
                last_test_at=now, last_recovery_at=now, enabled=False)
    monkeypatch.setattr(B, "capability_fingerprint", lambda: "same")
    monkeypatch.setattr(B, "resolve_mode", lambda mode, match="": "virtual_only" if mode == "auto" else mode)
    monkeypatch.setattr(B, "available_modes", lambda: ["auto", "virtual_only", "overlay", "screen_off"])
    seen = []
    monkeypatch.setattr(B, "schedule_install", lambda cfg: seen.append(cfg["mode"]))
    app.activate("auto", "20:00", "10:30")
    assert seen == ["overlay"]
    assert core.load()["mode"] == "overlay"
    assert core.load()["enabled"] is True


def test_changed_capability_still_requires_a_new_test(monkeypatch):
    now = dt.datetime.now().isoformat()
    core.update(last_test_ok=True, last_test_mode="overlay", last_test_requested="auto",
                last_test_capability="old", last_recovery_mode="overlay",
                last_test_at=now, last_recovery_at=now)
    monkeypatch.setattr(B, "capability_fingerprint", lambda: "new")
    monkeypatch.setattr(B, "resolve_mode", lambda mode, match="": "virtual_only" if mode == "auto" else mode)
    monkeypatch.setattr(B, "available_modes", lambda: ["auto", "virtual_only", "overlay", "screen_off"])
    monkeypatch.setattr(B, "schedule_install", lambda cfg: pytest.fail("schedule"))
    with pytest.raises(RuntimeError, match="完成一次测试"):
        app.activate("auto", "20:00", "10:30")


def test_orphaned_virtual_night_is_restored_on_startup(monkeypatch):
    core.update(state="night", night_mode="virtual_only", enabled=False, recovery_pending=False)
    calls = []
    monkeypatch.setattr(B, "go_day", lambda *args, **kwargs: calls.append("day") or "virtual_only")
    assert app.recover_orphaned_night() == "recovered"
    cfg = core.load()
    assert calls == ["day"]
    assert cfg["state"] == "day" and cfg["night_mode"] == ""
    assert cfg["recovery_pending"] is False and cfg["hold_night_until"] == ""


def test_failed_orphan_recovery_does_not_pretend_it_is_day(monkeypatch):
    core.update(state="night", night_mode="screen_off", enabled=False)
    monkeypatch.setattr(B, "go_day", lambda *args, **kwargs: (_ for _ in ()).throw(RuntimeError("display")))
    assert app.recover_orphaned_night() == "failed"
    assert core.load()["state"] == "night"


def test_clean_startup_does_not_touch_the_display(monkeypatch):
    core.update(state="day", night_mode="")
    monkeypatch.setattr(B, "go_day", lambda *args, **kwargs: pytest.fail("day"))
    assert app.recover_orphaned_night() == "clean"


def test_test_recovery_restores_a_dirty_cursor(monkeypatch):
    core.update(test_id="t", state="night", night_mode="overlay",
                last_test_night_ok=True, last_test_mode="overlay")
    app.mark_cursor_dirty()
    monkeypatch.setattr(app, "IS_WIN", True)
    monkeypatch.setattr(B, "go_day", lambda *args, **kwargs: core.update(state="day", night_mode=""))
    called = []
    monkeypatch.setattr(app, "apply_cursor", lambda hidden: called.append(hidden))
    try:
        assert app.finish_test("t") == "ok"
        assert called == [False]
    finally:
        app.clear_cursor_marker()
        app._cursor["hidden"] = False


def test_virtual_test_recovery_does_not_reset_custom_cursors(monkeypatch):
    core.update(test_id="t", state="night", night_mode="virtual_only",
                last_test_night_ok=True, last_test_mode="virtual_only")
    app._cursor["hidden"] = False
    monkeypatch.setattr(app, "IS_WIN", True)
    monkeypatch.setattr(B, "go_day", lambda *args, **kwargs: core.update(state="day", night_mode=""))
    monkeypatch.setattr(app, "apply_cursor", lambda hidden: pytest.fail("cursor"))
    assert app.finish_test("t") == "ok"


def test_linux_bad_output_records_fall_through(monkeypatch):
    monkeypatch.setattr(L, "_run", lambda cmd: None)
    monkeypatch.setattr(L, "query", lambda: [{"name": "HDMI-1", "active": True, "connected": True}])
    assert L.restore({"outputs": [None]}, lambda *_args: None) == 2
