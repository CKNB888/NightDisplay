import datetime as dt
import pytest
from nightdisplay import core

D = lambda h, m: dt.datetime(2026, 1, 1, h, m)

@pytest.mark.parametrize("h,m,exp", [(0, 0, 1), (10, 29, 1), (10, 30, 0), (19, 59, 0), (20, 0, 1), (23, 59, 1)])
def test_overnight(h, m, exp):
    assert core.is_night("20:00", "10:30", D(h, m)) == bool(exp)

def test_sameday_and_equal():
    assert core.is_night("09:00", "17:00", D(12, 0))
    assert not core.is_night("09:00", "17:00", D(17, 0))
    assert not core.is_night("08:00", "08:00", D(8, 0))

def test_next_switch():
    cfg = dict(core.DEFAULT)
    assert core.next_switch(cfg, D(19, 0)) == ("night", 3600)
    assert core.next_switch(cfg, D(23, 0)) == ("day", 11.5 * 3600)

def test_bad_time():
    with pytest.raises(ValueError):
        core.parse_hm("25:00")

def test_config_and_backup_roundtrip():
    core.update(start="21:15")
    assert core.load()["start"] == "21:15"
    for i in range(12):
        core.save_backup("display", {"i": i})
    assert core.load_backup("display") == {"i": 11}
    import glob, os
    assert len(glob.glob(os.path.join(core.home(), "backups", "display-2*.json"))) == 10

def test_auto_mode_resolution(monkeypatch):
    from nightdisplay import backends as B
    monkeypatch.setattr(B, "has_virtual_display", lambda m="": True)
    if core.IS_MAC:
        assert B.resolve_mode("auto") == "screen_off"
    else:
        assert B.resolve_mode("auto") == "virtual_only"
    monkeypatch.setattr(B, "has_virtual_display", lambda m="": False)
    assert B.resolve_mode("auto") in ("overlay", "screen_off")
    assert B.resolve_mode("screen_off") == "screen_off"
