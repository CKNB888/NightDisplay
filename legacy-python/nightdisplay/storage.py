"""Small, process-safe state files. No third-party dependency is needed."""
import contextlib
import json
import os
import tempfile
import threading
import time

_threads = {}
_guard = threading.Lock()
_held = threading.local()


@contextlib.contextmanager
def file_lock(path, timeout=60):
    """A reentrant thread + process lock; timeout=0 is a nonblocking probe."""
    path = os.path.abspath(path)
    with _guard:
        lock = _threads.setdefault(path, threading.RLock())
    if not lock.acquire(timeout=max(0, timeout)):
        raise TimeoutError("另一个 NightDisplay 操作仍在进行")
    held = getattr(_held, "paths", None)
    if held is None:
        held = _held.paths = set()
    f = None
    try:
        if path in held:
            yield
            return
        os.makedirs(os.path.dirname(path), exist_ok=True)
        f = open(path, "a+b")  # noqa: SIM115 - the lock must stay held across yield
        if os.fstat(f.fileno()).st_size == 0:
            f.write(b"\0"); f.flush()
        deadline = time.monotonic() + timeout
        while True:
            try:
                if os.name == "nt":
                    import msvcrt
                    f.seek(0)
                    msvcrt.locking(f.fileno(), msvcrt.LK_NBLCK, 1)
                else:
                    import fcntl
                    fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except OSError:
                if time.monotonic() >= deadline:
                    raise TimeoutError("另一个 NightDisplay 操作仍在进行") from None
                time.sleep(0.02)
        held.add(path)
        try:
            yield
        finally:
            held.remove(path)
            if os.name == "nt":
                import msvcrt
                f.seek(0)
                msvcrt.locking(f.fileno(), msvcrt.LK_UNLCK, 1)
            else:
                import fcntl
                fcntl.flock(f, fcntl.LOCK_UN)
    finally:
        if f is not None:
            f.close()
        lock.release()


def _replace_file(tmp, path):
    """换成新文件。Windows 正在读旧文件时，改名会拒绝访问，先重试再直接写入。"""
    last = None
    for _attempt in range(30):
        try:
            os.replace(tmp, path)
            return
        except PermissionError as blocked:
            last = blocked
            time.sleep(0.05)
    if os.name != "nt":
        raise last
    # 旧文件被打开着时，Windows 不允许覆盖改名。内容写进原文件，避免状态停在夜间。
    with open(tmp, encoding="utf-8") as src, open(path, "w", encoding="utf-8") as dest:
        dest.write(src.read())
        dest.flush()
        os.fsync(dest.fileno())


def write_json(path, data):
    """Replace in the same directory; readers see either complete version."""
    fd, tmp = tempfile.mkstemp(prefix=".nd-", dir=os.path.dirname(path) or ".")
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            json.dump(data, f, ensure_ascii=False, indent=2)
            f.flush()
            os.fsync(f.fileno())
        _replace_file(tmp, path)
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)
