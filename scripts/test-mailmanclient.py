#!/usr/bin/env python3
"""Run the pinned real client against an isolated Phase 1 + bounded held server.

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


from mailmanclient_held import mail_role, run_held

ROOT = Path(__file__).resolve().parents[1]


def main():
    binary = Path(os.environ.get("LISTMNGR_TEST_BINARY", ROOT / "target/debug/listmngr")).resolve()
    if not binary.is_file():
        raise SystemExit("Build first: cargo build --locked --workspace")
    env = {key: value for key, value in os.environ.items() if not key.startswith("LISTMNGR")}
    env["RUST_LOG"] = "warn"
    with tempfile.TemporaryDirectory(prefix="listmngr-compat-") as directory, mail_role(env) as (lmtp_port, sink):
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
        # The server's log (RUST_LOG=warn: correlation ids and error
        # messages, never secrets) is shown only when a flow fails.
        server_log = open(Path(directory) / "server.log", "w+b")
        server = subprocess.Popen(
            [str(binary), "serve"], env=env, cwd=directory,
            stdout=subprocess.DEVNULL, stderr=server_log,
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
            def flow(script, label):
                result = subprocess.run(
                    [sys.executable, str(ROOT / script)],
                    env=client_env, cwd=directory, timeout=300,
                )
                if result.returncode:
                    server_log.flush()
                    server_log.seek(0)
                    tail = server_log.read().decode(errors="replace").splitlines()[-40:]
                    print("--- fixture server log (tail) ---", file=sys.stderr)
                    print("\n".join(tail), file=sys.stderr)
                    raise RuntimeError(f"{label} failed (exit {result.returncode})")

            flow("tests/compat/mailmanclient_phase1.py", "real-client gate")
            # The held flow counts deliveries on the sink, so it runs before
            # the suite, whose subscriptions and moderation send mail too.
            run_held(client_env["LISTMNGR_COMPAT_URL"], token, cli, user["id"],
                     directory, lmtp_port, sink)
            flow("tests/compat/mailmanclient_suite.py", "doctest-equivalent suite")
        finally:
            server.terminate()
            try:
                server.wait(timeout=10)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait(timeout=10)
            server_log.close()
        print("compat fixture: server stopped; temporary database removed on exit")


if __name__ == "__main__":
    main()
