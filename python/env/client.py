"""Minimal HTTP JSON client for the Stage 5A `archipelago-api` server.

Deliberately stdlib-only (``urllib``): the project's dependency budget for
``python/`` is exactly ``gymnasium`` and ``numpy`` (docs/phase5-spec.md
"Stage 5B" -> "依存は gymnasium と numpy のみ"), so this file may not import
``requests`` or anything else that would grow ``requirements.txt``.
"""

from __future__ import annotations

import json
import urllib.error
import urllib.request
from typing import Any, Mapping


class ArchipelagoConnectionError(ConnectionError):
    """Raised when the Stage 5A server can't be reached at all.

    This is meant to be the first error a brand-new user of this package
    ever sees, so the message spells out exactly what to do next rather than
    just naming the failure.
    """

    def __init__(self, base_url: str, cause: Exception):
        message = (
            f"Could not reach the Archipelago API server at {base_url!r} ({cause}).\n"
            "\n"
            "Either:\n"
            "  1) Start it yourself first, from the repository root:\n"
            "       cargo build --workspace\n"
            "       ./target/debug/archipelago-api --bind 127.0.0.1:8080\n"
            "     then create the env with base_url='http://127.0.0.1:8080'.\n"
            "  2) Or let ArchipelagoEnv manage it for you: construct it with\n"
            "     start_server=True (the default) and no base_url - it will\n"
            "     build/locate the `archipelago-api` binary and launch it as a\n"
            "     subprocess automatically. See python/README.md."
        )
        super().__init__(message)
        self.base_url = base_url
        self.cause = cause


class ArchipelagoApiError(RuntimeError):
    """Raised for a well-formed-but-rejected HTTP request (4xx/5xx) that
    is not the "server unreachable" case above - e.g. an unknown
    `session_id`, or a malformed request body. This is a programming error
    in the caller (or a genuinely dead session), never a normal RL outcome -
    normal in-game rejections are reported through `info["rejected"]`
    instead, exactly per docs/phase5-spec.md ("エラーで落とさない").
    """

    def __init__(self, method: str, path: str, status: int, body: str):
        super().__init__(f"{method} {path} -> HTTP {status}: {body}")
        self.status = status
        self.body = body


class ApiClient:
    """Thin synchronous JSON-over-HTTP wrapper around one server instance."""

    def __init__(self, base_url: str, timeout: float = 30.0):
        self.base_url = base_url.rstrip("/")
        self.timeout = timeout

    def _request(self, method: str, path: str, body: Mapping[str, Any] | None = None) -> Any:
        url = f"{self.base_url}{path}"
        data = None
        headers = {}
        if body is not None:
            data = json.dumps(body).encode("utf-8")
            headers["Content-Type"] = "application/json"
        request = urllib.request.Request(url, data=data, headers=headers, method=method)
        try:
            with urllib.request.urlopen(request, timeout=self.timeout) as response:
                raw = response.read()
        except urllib.error.HTTPError as e:
            raw_body = e.read().decode("utf-8", errors="replace")
            raise ArchipelagoApiError(method, path, e.code, raw_body) from e
        except urllib.error.URLError as e:
            raise ArchipelagoConnectionError(self.base_url, e) from e
        except OSError as e:
            # A refused/reset connection on some platforms surfaces as a bare
            # OSError rather than a urllib.error.URLError.
            raise ArchipelagoConnectionError(self.base_url, e) from e
        if not raw:
            return None
        return json.loads(raw.decode("utf-8"))

    def get(self, path: str) -> Any:
        return self._request("GET", path)

    def post(self, path: str, body: Mapping[str, Any] | None = None) -> Any:
        return self._request("POST", path, body if body is not None else {})

    def delete(self, path: str, body: Mapping[str, Any] | None = None) -> Any:
        return self._request("DELETE", path, body if body is not None else {})

    def health_check(self) -> bool:
        try:
            result = self.get("/health")
        except ArchipelagoApiError:
            return False
        return isinstance(result, dict) and result.get("status") == "ok"
