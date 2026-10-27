"""Per-data-directory IPC. A lost reply must never repeat a display action."""
import hmac
import json
import os
import secrets
import socket
import socketserver
import threading

from .core import home
from .storage import file_lock, write_json

MAX_PACKET = 8192


def endpoint_path():
    return os.path.join(home(), "ipc.json")


def send(command, timeout=60):
    try:
        with open(endpoint_path(), encoding="utf-8") as f:
            endpoint = json.load(f)
        port, token = endpoint["port"], endpoint["token"]
        if type(port) is not int or not 1 <= port <= 65535 or not isinstance(token, str):
            raise ValueError("invalid endpoint")
    except (FileNotFoundError, ValueError, KeyError, TypeError):
        return None
    try:
        s = socket.create_connection(("127.0.0.1", port), timeout=2)
    except OSError:
        return None
    with s:
        try:
            s.settimeout(timeout)
            s.sendall(json.dumps({"token": token, "command": command}).encode() + b"\n")
            with s.makefile("rb") as f:
                raw = f.readline(MAX_PACKET + 1)
            if not raw.endswith(b"\n") or len(raw) > MAX_PACKET:
                raise ValueError("incomplete response")
            result = json.loads(raw)["result"]
            if not isinstance(result, str):
                raise ValueError("invalid result")
            return result
        except (OSError, ValueError, KeyError) as e:
            raise RuntimeError("常驻进程未返回完整结果；操作可能仍在进行，请先查看状态") from e


class Server:
    def __init__(self, dispatch):
        self.dispatch = dispatch
        self.token = secrets.token_hex(32)
        self.server = None
        self.serving = False
        self.lock = file_lock(os.path.join(home(), "daemon.lock"), timeout=0)
        self._locked = False

    def start(self):
        if self.server is not None:
            raise RuntimeError("IPC 服务已经启动")
        self.lock.__enter__()
        self._locked = True
        outer = self

        class Handler(socketserver.StreamRequestHandler):
            def handle(self):
                self.connection.settimeout(3)
                try:
                    raw = self.rfile.readline(MAX_PACKET + 1)
                    if not raw.endswith(b"\n") or len(raw) > MAX_PACKET:
                        return
                    data = json.loads(raw)
                    if not isinstance(data, dict):
                        return
                    token = data.get("token", "")
                    if not isinstance(token, str) or not hmac.compare_digest(token, outer.token):
                        return
                    command = data.get("command")
                    if not isinstance(command, (str, dict)):
                        return
                    result = outer.dispatch(command)
                    self.wfile.write(json.dumps({"result": result}).encode() + b"\n")
                except (OSError, ValueError, TypeError):
                    return

        class TCPServer(socketserver.ThreadingTCPServer):
            daemon_threads = True
            allow_reuse_address = False

        try:
            self.server = TCPServer(("127.0.0.1", 0), Handler)
            write_json(endpoint_path(), {"port": self.server.server_address[1], "token": self.token})
            threading.Thread(target=self.server.serve_forever, daemon=True).start()
            self.serving = True
        except Exception:
            self.close()
            raise

    def close(self):
        if self.server is not None:
            if self.serving:
                self.server.shutdown()
            self.server.server_close()
            self.server = None
            self.serving = False
        try:
            with open(endpoint_path(), encoding="utf-8") as f:
                current = json.load(f)
            if current.get("token") == self.token:
                os.unlink(endpoint_path())
        except (OSError, ValueError):
            pass
        if self._locked:
            self._locked = False
            self.lock.__exit__(None, None, None)
