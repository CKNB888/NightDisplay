import datetime as dt

import pytest

pytest.importorskip("tkinter")


from nightdisplay import ui


def cfg(**kw):
    base = {"enabled": True, "state": "day", "start": "19:30", "end": "10:30", "extra_ranges": []}
    base.update(kw)
    return base


def test_ring_counts_down_to_blackout_in_day():
    frac, big, small, color = ui.ring_state(cfg(), dt.datetime(2026, 1, 1, 18, 30))
    assert (big, small, color) == ("1:00", "后黑屏", "green")
    assert 0.85 < frac < 1.0


def test_ring_counts_down_to_end_at_night():
    frac, big, small, color = ui.ring_state(cfg(state="night"), dt.datetime(2026, 1, 1, 22, 30))
    assert (big, small, color) == ("12:00", "后亮回来", "accent")
    assert 0.0 < frac < 0.3


def test_ring_when_plan_off():
    assert ui.ring_state(cfg(enabled=False))[1] == "未启用"


def test_fmt_span():
    assert ui.fmt_span(59) == "0:59"
    assert ui.fmt_span(3600 * 2 + 60 * 5) == "2:05"
