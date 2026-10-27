import json
from pathlib import Path

import pytest

from nightdisplay import backends as B
from nightdisplay import core, updates
from nightdisplay import win_power as P


@pytest.fixture(autouse=True)
def isolated(tmp_path, monkeypatch):
    monkeypatch.setenv("NIGHTDISPLAY_HOME", str(tmp_path))


def fake_power(monkeypatch, fail=None):
    original = "11111111-2222-3333-4444-555555555555"
    state = {"active": original, "calls": []}
    def run(*args):
        state["calls"].append(args)
        if fail and args[0] == fail:
            raise RuntimeError("powercfg failed")
        if args[0] == "/getactivescheme":
            return "Power Scheme GUID: " + state["active"]
        if args[0] == "/setactive":
            state["active"] = args[1]
        return ""
    monkeypatch.setattr(P, "_run", run)
    return original, state


def test_night_power_is_temporary_and_restores(monkeypatch):
    original, state = fake_power(monkeypatch)
    P.enter()
    assert state["active"] != original
    assert any(c[:3] == ("/setacvalueindex", state["active"], "SUB_SLEEP") for c in state["calls"])
    assert any(c[-2:] == ("LIDACTION", "0") for c in state["calls"])
    P.restore()
    assert state["active"] == original and not Path(P._path()).exists()
    assert not any("/hibernate" in c for c in state["calls"])


def test_power_configuration_failure_rolls_back(monkeypatch):
    original, state = fake_power(monkeypatch, fail="/setdcvalueindex")
    with pytest.raises(RuntimeError):
        P.enter()
    assert state["active"] == original and not Path(P._path()).exists()


def test_restore_respects_users_new_power_plan(monkeypatch):
    original, state = fake_power(monkeypatch)
    P.enter()
    user = "99999999-2222-3333-4444-555555555555"
    state["active"] = user
    P.restore()
    assert state["active"] == user


def test_screen_off_fallback_does_not_disable_display_idle(monkeypatch):
    original, state = fake_power(monkeypatch)
    P.enter(keep_display_on=False)
    assert not any("VIDEOIDLE" in c for c in state["calls"])
    P.restore()


def test_repeated_power_entry_keeps_original_plan(monkeypatch):
    original, state = fake_power(monkeypatch)
    P.enter(); P.enter()
    assert sum(c[0] == "/duplicatescheme" for c in state["calls"]) == 1
    P.restore()
    assert state["active"] == original


def test_corrupt_power_state_does_not_block_night(monkeypatch):
    Path(P._path()).write_text("{", encoding="utf-8")
    original, state = fake_power(monkeypatch)
    P.enter()
    assert state["active"] != original
    P.restore()
    assert state["active"] == original


def test_auto_fallback_after_verified_rollback(monkeypatch):
    monkeypatch.setattr(B, "resolve_mode", lambda *a: "virtual_only")
    monkeypatch.setattr(B, "IS_WIN", True)
    monkeypatch.setattr(B, "win_build", lambda: 22621)
    seen = []
    def attempt(mode, overlay, dry_run):
        seen.append(mode)
        if mode == "virtual_only":
            raise RuntimeError("virtual unavailable")
        return mode
    monkeypatch.setattr(B, "_go_night", attempt)
    assert B.go_night("auto", lambda on: True) == "overlay"
    assert seen == ["virtual_only", "overlay"]


def test_auto_does_not_fallback_when_rollback_failed(monkeypatch):
    monkeypatch.setattr(B, "resolve_mode", lambda *a: "virtual_only")
    monkeypatch.setattr(B, "IS_WIN", True)
    monkeypatch.setattr(B, "win_build", lambda: 22621)
    seen = []
    def attempt(mode, overlay, dry_run):
        seen.append(mode)
        core.update(state="night", recovery_pending=True)
        raise RuntimeError("rollback failed")
    monkeypatch.setattr(B, "_go_night", attempt)
    with pytest.raises(RuntimeError):
        B.go_night("auto", lambda on: True)
    assert seen == ["virtual_only"]


@pytest.mark.parametrize("tag, expected", [("v3.0.2", (3, 0, 2)), ("3.10.0", (3, 10, 0)),
                                          ("v3.0.2-rc1", None), ("v3.0", None), ("bad", None)])
def test_version_parse(tag, expected):
    assert updates.version_tuple(tag) == expected


def test_update_url_cannot_redirect_to_arbitrary_download(monkeypatch):
    class Response:
        def __enter__(self): return self
        def __exit__(self, *args): pass
        def read(self, limit):
            return json.dumps({"tag_name": "v3.0.5", "html_url": "https://attacker.invalid"}).encode()
    monkeypatch.setattr(updates.urllib.request, "urlopen", lambda *a, **kw: Response())
    data = updates.check()
    assert data["newer"]
    assert data["url"] == "https://github.com/CKNB888/NightDisplay/releases/tag/v3.0.5"
