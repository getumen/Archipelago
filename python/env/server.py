"""Launching and stopping the `archipelago-api` binary as a subprocess.

Used by `ArchipelagoEnv(start_server=True)` (the default) and by the pytest
suite (docs/phase5-spec.md "Stage 5B の受け入れ基準": "python/env の単体テ
ストが通る（サーバをサブプロセスで起動して実行する）") so both a fresh
checkout and CI can run the Python tests without a server already running
somewhere.
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
import time
from pathlib import Path

_LISTEN_RE = re.compile(r"listening on http://(\S+)")


class ServerNotFoundError(RuntimeError):
    def __init__(self, searched: list[str]):
        locations = "\n".join(f"  - {p}" for p in searched)
        super().__init__(
            "Could not find the `archipelago-api` binary. Build it first:\n"
            "  cargo build --workspace\n"
            "(or `cargo build --workspace --release` for the release binary)\n"
            f"Searched:\n{locations}\n"
            "Set the ARCHIPELAGO_API_BIN environment variable to point at the "
            "binary directly if it lives somewhere else."
        )


def _repo_root() -> Path:
    # python/env/server.py -> python/env -> python -> repo root
    return Path(__file__).resolve().parents[2]


def find_server_binary() -> Path:
    """Locates the `archipelago-api` binary, preferring an explicit
    `ARCHIPELAGO_API_BIN` override, then a release build, then a debug
    build - never silently building it (a Python import shouldn't have the
    side effect of invoking `cargo`).
    """
    override = os.environ.get("ARCHIPELAGO_API_BIN")
    if override:
        path = Path(override)
        if path.is_file():
            return path
        raise ServerNotFoundError([f"{override} (from $ARCHIPELAGO_API_BIN, not found)"])

    root = _repo_root()
    exe = "archipelago-api.exe" if sys.platform == "win32" else "archipelago-api"
    candidates = [root / "target" / "release" / exe, root / "target" / "debug" / exe]
    for candidate in candidates:
        if candidate.is_file():
            return candidate
    raise ServerNotFoundError([str(c) for c in candidates])


class ManagedServer:
    """Owns one `archipelago-api` subprocess bound to an OS-assigned free
    port (`--bind 127.0.0.1:0`), discovered by parsing the "listening on"
    line the binary prints to stdout on startup - this avoids a
    port-scanning race between picking a "free" port and the server actually
    binding it.
    """

    def __init__(self, idle_timeout_secs: int = 1800, startup_timeout_secs: float = 15.0, binary: Path | None = None):
        self.binary = binary or find_server_binary()
        self.idle_timeout_secs = idle_timeout_secs
        self.startup_timeout_secs = startup_timeout_secs
        self._process: subprocess.Popen | None = None
        self.base_url: str | None = None

    def start(self) -> str:
        if self._process is not None:
            assert self.base_url is not None
            return self.base_url
        self._process = subprocess.Popen(
            [str(self.binary), "--bind", "127.0.0.1:0", "--idle-timeout", str(self.idle_timeout_secs)],
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            bufsize=1,
        )
        deadline = time.monotonic() + self.startup_timeout_secs
        assert self._process.stdout is not None
        lines: list[str] = []
        while time.monotonic() < deadline:
            line = self._process.stdout.readline()
            if not line:
                if self._process.poll() is not None:
                    break
                continue
            lines.append(line)
            match = _LISTEN_RE.search(line)
            if match:
                self.base_url = f"http://{match.group(1)}"
                return self.base_url
        self.stop()
        raise RuntimeError(
            f"`{self.binary}` did not report a listening address within {self.startup_timeout_secs}s. "
            f"Output so far:\n{''.join(lines)}"
        )

    def stop(self) -> None:
        process = self._process
        self._process = None
        if process is None or process.poll() is not None:
            return
        process.terminate()
        try:
            process.wait(timeout=5.0)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5.0)

    def __enter__(self) -> "ManagedServer":
        self.start()
        return self

    def __exit__(self, *exc_info) -> None:
        self.stop()

    def __del__(self):
        # Best-effort: don't leak a server process if the owner forgets to
        # call stop()/close the env. Not a substitute for explicit cleanup.
        try:
            self.stop()
        except Exception:
            pass
