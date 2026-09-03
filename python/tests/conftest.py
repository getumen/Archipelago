import sys
from pathlib import Path

import pytest

# Allow `import env` / `from env import ArchipelagoEnv` when pytest is run
# from anywhere, without requiring the package to be pip-installed.
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

from env.server import ManagedServer  # noqa: E402


@pytest.fixture(scope="session")
def server_url():
    """One `archipelago-api` subprocess shared by the whole test session
    (docs/phase5-spec.md "Stage 5B の受け入れ基準": tests start the server as
    a subprocess). Sessions created against it by individual tests are
    independent (`crates/api/src/session.rs`), so sharing the process is
    safe and much faster than spawning one server per test.
    """
    server = ManagedServer(idle_timeout_secs=120)
    url = server.start()
    yield url
    server.stop()
