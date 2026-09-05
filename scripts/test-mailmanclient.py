#!/usr/bin/env python3
"""Run the pinned real client against an isolated, disposable Phase 1 server.

Build the binary first and install tests/compat/requirements-mailmanclient.txt.
No development configuration, database, or credentials are consumed.
"""

import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request


ROOT = Path(__file__).resolve().parents[1]


def main():
    binary = Path(os.environ.get("LISTMNGR_TEST_BINARY", ROOT / "target/debug/listmngr")).resolve()
    if not binary.is_file():
        raise SystemExit("Build first: cargo build --locked --workspace")
    env = {key: value for key, value in os.environ.items() if not key.startswith("LISTMNGR")}
    env["RUST_LOG"] = "warn"
    with tempfile.TemporaryDirectory(prefix="listmngr-compat-") as directory:
        env["LISTMNGR__DATABASE__URL"] = f"sqlite://{directory}/compat.sqlite?mode=rwc"
        env["LISTMNGR__API__COMPAT_BASIC_AUTH"] = "true"
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            port = reservation.getsockname()[1]
        env["LISTMNGR__WEB__LISTEN"] = f"127.0.0.1:{port}"

        def cli(*args, stdin=None):
            result = subprocess.run(
                [str(binary), *args], env=env, cwd=directory,
                capture_output=True, text=True, timeout=30, input=stdin,
            )
            if result.returncode:
                # Never dump captured token/config output or credential-bearing argv.
                raise RuntimeError(f"fixture CLI {args[0]} failed (exit {result.returncode})")
            return result.stdout.strip()

        cli("migrate")
        user = json.loads(cli(
            "user", "create", "compat-owner@example.invalid", "--display-name", "Compat gate",
            "--password-stdin", "--server-owner", stdin="Orbit!Cobalt7-River$Quartz\n",
        ))
        token = cli("token", "create", user["id"], "compat-gate", "--scopes", "admin")
        prefix, token_id, secret = token.split("_", 2)
        if prefix != "lm":
            raise RuntimeError("unexpected issued-token format")
        client_env = dict(env, LISTMNGR_COMPAT_URL=f"http://127.0.0.1:{port}/3.1",
                          LISTMNGR_COMPAT_USER=token_id, LISTMNGR_COMPAT_SECRET=secret)
        server = subprocess.Popen(
            [str(binary), "serve"], env=env, cwd=directory,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        )
        try:
            deadline = time.monotonic() + 30
            while True:
                if server.poll() is not None:
                    raise RuntimeError("fixture server exited before readiness")
                try:
                    with urllib.request.urlopen(f"http://127.0.0.1:{port}/readyz", timeout=1) as response:
                        if response.status == 200:
                            break
                except (urllib.error.URLError, TimeoutError):
                    pass
                if time.monotonic() >= deadline:
                    raise RuntimeError("fixture server readiness timed out")
                time.sleep(0.1)
            result = subprocess.run(
                [sys.executable, str(ROOT / "tests/compat/mailmanclient_phase1.py")],
                env=client_env, cwd=directory, timeout=60,
            )
            if result.returncode:
                raise RuntimeError(f"real-client gate failed (exit {result.returncode})")
        finally:
            server.terminate()
            try:
                server.wait(timeout=10)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait(timeout=10)
        print("compat fixture: server stopped; temporary database removed on exit")


if __name__ == "__main__":
    main()
