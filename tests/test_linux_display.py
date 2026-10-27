from nightdisplay import backends as B
from nightdisplay import core
from nightdisplay import linux_display as L

SAMPLE = """Screen 0: minimum 8 x 8, current 3840 x 1080, maximum 32767 x 32767
eDP-1 connected primary 1920x1080+0+0 (normal left inverted right x axis y axis) 344mm x 193mm
   1920x1080     60.02*+  48.00
HDMI-1 disconnected (normal left inverted right x axis y axis)
VIRTUAL1 connected 1920x1080+1920+0 (normal left inverted right x axis y axis) 0mm x 0mm
   1920x1080     59.96*
"""

def test_parse():
    o = L.parse_xrandr(SAMPLE)
    assert [x["name"] for x in o] == ["eDP-1", "HDMI-1", "VIRTUAL1"]
    assert o[0]["primary"] and o[0]["mode"] == "1920x1080" and o[0]["rate"] == "60.02"
    assert not o[1]["active"] and o[2]["pos"] == (1920, 0)

def test_restore_cmd():
    c = L.restore_cmd(L.parse_xrandr(SAMPLE))
    s = " ".join(c)
    assert "--output eDP-1 --mode 1920x1080 --pos 0x0 --rotate normal --rate 60.02 --primary" in s
    assert "--output HDMI-1 --off" in s

def test_virtual_only_cmd():
    cmd, name = L.virtual_only_cmd(L.parse_xrandr(SAMPLE))
    assert name == "VIRTUAL1"
    assert cmd[:5] == ["xrandr", "--output", "VIRTUAL1", "--auto", "--primary"]
    assert "eDP-1" in cmd and cmd[-1] == "--off"

def test_no_virtual():
    import pytest
    with pytest.raises(RuntimeError):
        L.virtual_only_cmd(L.parse_xrandr(SAMPLE.replace("VIRTUAL1 connected 1920x1080+1920+0", "VIRTUAL1 disconnected")))

def test_systemd_units():
    svc, tim = B.systemd_units(dict(core.DEFAULT, start="21:05", end="07:30"))
    assert "OnCalendar=*-*-* 21:05:00" in tim and "OnCalendar=*-*-* 07:30:00" in tim
    assert "--action reconcile" in svc and "Persistent=true" in tim

def test_launchd_plist():
    p = B.launchd_plist(dict(core.DEFAULT, start="20:00", end="10:30"))
    assert p["StartCalendarInterval"] == [{"Hour": 10, "Minute": 30}, {"Hour": 20, "Minute": 0}]
    assert p["ProgramArguments"][-2:] == ["--action", "reconcile"]
