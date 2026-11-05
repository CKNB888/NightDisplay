"""把真实运行中的主窗口截成 PNG 和巡览 GIF（README 用）。

用的就是 nightdisplay.ui.MainWindow 本体，只是把执行动作换成空函数，
配置写进临时目录，不碰本机计划任务和显示器。Windows CI 上跑出来的是 Windows 真实界面。
用法：python tools/capture_ui.py 输出目录
"""
import json
import os
import sys
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
os.environ["NIGHTDISPLAY_HOME"] = tempfile.mkdtemp(prefix="nd-shot-")


def main(out):
    os.makedirs(out, exist_ok=True)
    if sys.platform == "win32":
        import ctypes
        ctypes.windll.user32.SetProcessDPIAware()
    import tkinter as tk

    from PIL import ImageGrab

    from nightdisplay import core, ui
    core.update(enabled=True, start="19:30", end="10:30", extra_ranges=[["12:00", "13:30"]],
                last_test_ok=True, last_test_requested="auto", last_test_mode="virtual_only")
    root = tk.Tk()
    win = ui.MainWindow(root, lambda on, **k: True, lambda c: "ok")
    win.check_update = lambda quiet=False: None
    root.update()
    root.lift()
    root.attributes("-topmost", True)
    frames = []

    def settle(ms=400):
        end = time.time() + ms / 1000
        while time.time() < end:
            root.update()
            time.sleep(0.01)

    def grab(name=None):
        root.update()
        x, y = root.winfo_rootx(), root.winfo_rooty()
        img = ImageGrab.grab(bbox=(x, y, x + root.winfo_width(), y + root.winfo_height()), all_screens=True) \
            if sys.platform == "win32" else ImageGrab.grab(bbox=(x, y, x + root.winfo_width(), y + root.winfo_height()))
        if name:
            img.save(os.path.join(out, name))
        return img.convert("RGB")

    def go(page, name, hold=6):
        win.show_page(page)
        for _ in range(4):  # 截几帧切页动画
            root.update()
            time.sleep(0.04)
            frames.append((grab(), 60))
        settle()
        frames.append((grab(name), 1800))

    settle(800)
    frames.append((grab("home.png"), 1800))
    go("schedule", "schedule.png")
    go("settings", "settings.png")
    core.update(state="night", night_mode="virtual_only")
    go("home", "home-night.png")
    win.refresh(schedule=False)
    settle()
    grab("home-night.png")
    core.update(state="day", night_mode="", enabled=False)
    win.refresh(schedule=False)
    settle()
    grab("home-off.png")
    core.update(enabled=True)
    win.refresh(schedule=False, animate_ring=True)
    settle()
    imgs = [f.quantize(colors=128) for f, _ in frames]
    imgs[0].save(os.path.join(out, "ui-tour.gif"), save_all=True, append_images=imgs[1:],
                 duration=[d for _, d in frames], loop=0, optimize=True)
    root.destroy()
    print(json.dumps(sorted(os.listdir(out))))


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "shots")
