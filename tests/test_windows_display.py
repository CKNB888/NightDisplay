import sys, pytest, struct
pytestmark = pytest.mark.skipif(not sys.platform.startswith("win"), reason="Windows only")

def test_sizes_against_api():
    from nightdisplay import win_display as W
    paths, modes = W.query(W.QDC_ONLY_ACTIVE_PATHS)
    assert len(paths) >= 1
    assert all(len(p) == 72 for p in paths) and all(len(m) == 64 for m in modes)
    p = W.parse_path(paths[0])
    assert p["active"]

def test_describe_and_backup_restore_roundtrip():
    from nightdisplay import win_display as W
    ds = W.describe()
    print("DISPLAYS", ds)
    b = W.backup_active()
    before = W.query(W.QDC_ONLY_ACTIVE_PATHS)
    lvl = W.restore(b, print)
    assert lvl == 1, "full backup should restore at level 1"
    after = W.query(W.QDC_ONLY_ACTIVE_PATHS)
    assert len(before[0]) == len(after[0])

def test_validate_supplied_config():
    from nightdisplay import win_display as W
    paths, modes = W.query(W.QDC_ONLY_ACTIVE_PATHS)
    r = W.set_config(paths, modes, W.SDC_VALIDATE | W.SDC_USE_SUPPLIED_DISPLAY_CONFIG)
    assert r == 0

def test_build_path_flags():
    from nightdisplay import win_display as W
    b = bytes(72)
    nb = W.build_path(b)
    p = W.parse_path(nb)
    assert p["active"] and p["src_mode"] == W.MODE_IDX_INVALID and p["tgt_mode"] == W.MODE_IDX_INVALID

