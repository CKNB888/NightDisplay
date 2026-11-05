"""Check GitHub Releases Latest; on Windows download Setup and silent-install.

Quiet / one-click path: user says yes once → download → cleanup_old.ps1 →
Setup /VERYSILENT → start new --tray. Download URLs must stay on GitHub.
"""
import json
import os
import re
import sys
import tempfile
import textwrap
import urllib.error
import urllib.request
from urllib.parse import urlparse

from . import VERSION

RELEASES = "https://github.com/CKNB888/NightDisplay/releases"
API_LATEST = "https://api.github.com/repos/CKNB888/NightDisplay/releases/latest"
SETUP_NAME = "NightDisplay-Windows-x64-Setup.exe"
APP_ID = "{C777459D-E0F3-45A3-96D4-83DC47EE3562}"
# browser_download_url redirects onto these hosts; reject anything else.
_ALLOWED_HOSTS = (
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
    "github-releases.githubusercontent.com",
)
_MAX_SETUP_BYTES = 80 * 1024 * 1024  # 80 MB hard cap


def version_tuple(tag):
    m = re.fullmatch(r"v?(\d+)\.(\d+)\.(\d+)", tag)
    return tuple(map(int, m.groups())) if m else None


def _allowed_url(url):
    try:
        p = urlparse(url)
    except Exception:
        return False
    if p.scheme != "https":
        return False
    host = (p.hostname or "").lower()
    return any(host == h or host.endswith("." + h) for h in _ALLOWED_HOSTS)


def check():
    req = urllib.request.Request(
        API_LATEST,
        headers={"User-Agent": "NightDisplay/" + VERSION, "Accept": "application/vnd.github+json"},
    )
    with urllib.request.urlopen(req, timeout=8) as response:
        raw = response.read(1024 * 1024 + 1)
    if len(raw) > 1024 * 1024:
        raise RuntimeError("更新信息过大，已停止读取")
    data = json.loads(raw)
    tag = data.get("tag_name", "")
    remote = version_tuple(tag)
    if not remote or data.get("prerelease") or data.get("draft"):
        raise RuntimeError("没有有效的正式版本信息")
    setup_url = ""
    setup_size = 0
    for asset in data.get("assets") or []:
        if asset.get("name") == SETUP_NAME:
            url = asset.get("browser_download_url") or ""
            if url and _allowed_url(url):
                setup_url = url
                try:
                    setup_size = int(asset.get("size") or 0)
                except (TypeError, ValueError):
                    setup_size = 0
            break
    return {
        "version": tag,
        "newer": remote > version_tuple(VERSION),
        "url": RELEASES + "/tag/" + tag,
        "setup_url": setup_url,
        "setup_name": SETUP_NAME,
        "setup_size": setup_size,
    }


def download_setup(url, dest_dir=None, on_progress=None):
    """Download the Windows Setup.exe. Returns the local path. Does not run it."""
    if not _allowed_url(url):
        raise RuntimeError("更新地址不在 GitHub，已拒绝下载")
    dest_dir = dest_dir or tempfile.mkdtemp(prefix="nd-update-")
    os.makedirs(dest_dir, exist_ok=True)
    dest = os.path.join(dest_dir, SETUP_NAME)
    partial = dest + ".part"
    req = urllib.request.Request(
        url,
        headers={"User-Agent": "NightDisplay/" + VERSION, "Accept": "application/octet-stream"},
    )
    try:
        with urllib.request.urlopen(req, timeout=60) as response:
            final = response.geturl()
            if not _allowed_url(final):
                raise RuntimeError("更新下载被重定向到非 GitHub 地址，已取消")
            total = response.headers.get("Content-Length")
            try:
                total_n = int(total) if total else 0
            except ValueError:
                total_n = 0
            if total_n > _MAX_SETUP_BYTES:
                raise RuntimeError("安装包过大（超过 80 MB），已取消")
            got = 0
            with open(partial, "wb") as out:
                while True:
                    chunk = response.read(256 * 1024)
                    if not chunk:
                        break
                    got += len(chunk)
                    if got > _MAX_SETUP_BYTES:
                        raise RuntimeError("安装包过大（超过 80 MB），已取消")
                    out.write(chunk)
                    if on_progress:
                        on_progress(got, total_n)
    except urllib.error.URLError as e:
        raise RuntimeError("下载失败：%s" % e) from e
    if got < 1024 * 100:
        raise RuntimeError("下载的安装包太小，可能不完整")
    os.replace(partial, dest)
    return dest


def cleanup_script_path():
    """Locate packaging/cleanup_old.ps1 shipped with the app (or repo checkout)."""
    candidates = []
    if getattr(sys, "frozen", False) and hasattr(sys, "_MEIPASS"):
        candidates.append(os.path.join(sys._MEIPASS, "nightdisplay", "resources", "cleanup_old.ps1"))
        candidates.append(os.path.join(sys._MEIPASS, "resources", "cleanup_old.ps1"))
    here = os.path.dirname(os.path.abspath(__file__))
    candidates.append(os.path.join(here, "resources", "cleanup_old.ps1"))
    candidates.append(os.path.join(here, "..", "packaging", "cleanup_old.ps1"))
    for path in candidates:
        path = os.path.abspath(path)
        if os.path.isfile(path):
            return path
    return ""


def default_install_dir():
    local = os.environ.get("LOCALAPPDATA")
    if not local:
        local = os.path.join(os.path.expanduser("~"), "AppData", "Local")
    return os.path.join(local, "Programs", "NightDisplay")

def install_silent(setup_path):
    """Windows: run cleanup_old.ps1, then Setup /VERYSILENT, then start new --tray.

    Runs in a detached PowerShell helper so cleanup can kill this process safely.
    Returns True once the helper has been started (not when install finished).
    """
    import subprocess

    if sys.platform != "win32":
        raise RuntimeError("自动安装只支持 Windows")
    setup_path = os.path.abspath(setup_path)
    if not os.path.isfile(setup_path) or not setup_path.lower().endswith(".exe"):
        raise RuntimeError("安装包路径无效")
    cleanup = cleanup_script_path()
    if not cleanup:
        raise RuntimeError("找不到 cleanup_old.ps1，无法安全清理旧版")
    new_dir = default_install_dir()
    helper = os.path.join(tempfile.mkdtemp(prefix="nd-upd-"), "apply_update.ps1")
    # Helper is not NightDisplay.exe, so Kill-All won't stop it mid-flight.
    script = textwrap.dedent(
        """\
        $ErrorActionPreference = 'Continue'
        $Setup = {setup!r}
        $Cleanup = {cleanup!r}
        $NewDir = {new_dir!r}
        $AppId = {app_id!r}
        $Log = Join-Path $env:TEMP ('NightDisplay-update-' + [guid]::NewGuid().ToString() + '.log')
        function W($m) {{ Add-Content -LiteralPath $Log -Value $m; Write-Output $m }}
        W "update start"
        & powershell.exe -NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File $Cleanup `
            -NewDir $NewDir -SetupExe $Setup -AppId $AppId *>> $Log
        W "cleanup exit $LASTEXITCODE"
        $p = Start-Process -FilePath $Setup -ArgumentList '/VERYSILENT','/SUPPRESSMSGBOXES','/NORESTART' `
            -PassThru -WindowStyle Hidden
        if (-not $p.WaitForExit(600000)) {{
            W "setup timeout"; & taskkill.exe /F /T /PID $p.Id | Out-Null; exit 2
        }}
        W "setup exit $($p.ExitCode)"
        if ($p.ExitCode -ne 0) {{ exit $p.ExitCode }}
        $exe = Join-Path $NewDir 'NightDisplay.exe'
        if (Test-Path -LiteralPath $exe) {{
            Start-Process -FilePath $exe -ArgumentList '--tray' -WindowStyle Hidden
            W "started $exe --tray"
        }} else {{
            W "missing $exe"
            exit 4
        }}
        exit 0
        """
    ).format(setup=setup_path, cleanup=cleanup, new_dir=new_dir, app_id=APP_ID)
    with open(helper, "w", encoding="utf-8") as f:
        f.write(script)
    flags = 0x00000008 | 0x00000200 | 0x08000000  # DETACHED|NEW_GROUP|NO_WINDOW
    subprocess.Popen(
        [
            "powershell.exe",
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-WindowStyle",
            "Hidden",
            "-File",
            helper,
        ],
        close_fds=True,
        creationflags=flags,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        stdin=subprocess.DEVNULL,
    )
    return True


# Back-compat name used by older call sites / tests.
def launch_installer(path, silent=True):
    if silent:
        return install_silent(path)
    import subprocess

    if sys.platform != "win32":
        raise RuntimeError("自动安装只支持 Windows")
    flags = 0x00000008 | 0x00000200
    subprocess.Popen([path], close_fds=True, creationflags=flags)
    return True
