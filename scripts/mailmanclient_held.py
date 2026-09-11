"""Bounded real-client held acceptance; only disposable fixture state is used."""
from contextlib import closing, contextmanager
from datetime import datetime
from email import message_from_bytes, message_from_string
from importlib.metadata import version
import smtplib
import socket
import socketserver
import sqlite3
import threading
import time
from urllib.error import HTTPError

from mailmanclient import Client


def wait_for(predicate, label):
    deadline = time.monotonic() + 20
    while not predicate():
        if time.monotonic() >= deadline:
            raise AssertionError(f"timed out: {label}")
        time.sleep(0.1)


class SinkHandler(socketserver.StreamRequestHandler):
    def handle(self):
        self.connection.settimeout(5)
        self.wfile.write(b"220 held-sink.invalid ESMTP\r\n")
        recipients = []
        while line := self.rfile.readline():
            command = line.upper()
            if command.startswith(b"EHLO"):
                self.wfile.write(b"250 held-sink.invalid\r\n")
            elif command.startswith(b"MAIL FROM:"):
                recipients = []
                self.wfile.write(b"250 ok\r\n")
            elif command.startswith(b"RCPT TO:"):
                recipients.append(line.decode().strip()[8:].strip("<>"))
                self.wfile.write(b"250 ok\r\n")
            elif command == b"DATA\r\n":
                self.wfile.write(b"354 send\r\n")
                body = bytearray()
                while True:
                    data = self.rfile.readline()
                    if not data:
                        return
                    if data == b".\r\n":
                        break
                    body.extend(data[1:] if data.startswith(b"..") else data)
                with self.server.lock:
                    self.server.deliveries.append((recipients[:], bytes(body)))
                self.wfile.write(b"250 accepted\r\n")
            elif command == b"QUIT\r\n":
                self.wfile.write(b"221 bye\r\n")
                return
            else:
                self.wfile.write(b"500 unsupported\r\n")


@contextmanager
def mail_role(env):
    with socketserver.ThreadingTCPServer(("127.0.0.1", 0), SinkHandler) as sink:
        sink.deliveries = []
        sink.lock = threading.Lock()
        with socket.socket() as reservation:
            reservation.bind(("127.0.0.1", 0))
            lmtp_port = reservation.getsockname()[1]
        env.update({
            "LISTMNGR__MAILMAN__DEFAULT_NONMEMBER_ACTION": "hold",
            "LISTMNGR__MTA__ENABLED": "true",
            "LISTMNGR__MTA__LMTP_LISTEN": f"127.0.0.1:{lmtp_port}",
            "LISTMNGR__MTA__SMTP_RELAY": f"127.0.0.1:{sink.server_address[1]}",
            "LISTMNGR__MTA__SMTP_TLS": "plaintext_trusted_relay",
            "LISTMNGR__MTA__LOCAL_HOSTNAME": "held-fixture.invalid",
            "LISTMNGR__MTA__COMMAND_TIMEOUT_SECS": "5",
            "LISTMNGR__SECURITY__RATE_LIMIT__API": "1000/min",
        })
        thread = threading.Thread(target=sink.serve_forever)
        thread.start()
        try:
            yield lmtp_port, sink
        finally:
            sink.shutdown()
            thread.join(timeout=10)


def expect_http(status, operation):
    try:
        operation()
    except HTTPError as error:
        assert error.code == status, (error.code, status)
    else:
        raise AssertionError(f"expected HTTP {status}")


def run_held(url, token, cli, user_id, directory, lmtp_port, sink):
    assert version("mailmanclient") == "3.3.5"

    def client_for(credential):
        prefix, identity, secret = credential.split("_", 2)
        assert prefix == "lm"
        return Client(url, identity, secret,
                      request_hooks=[lambda params: dict(params, timeout=5)])

    client = client_for(token)
    domain = client.create_domain("held-client.invalid")
    mailing_list = domain.create_list("held")
    subscriber = "subscriber@example.invalid"
    mailing_list.subscribe(subscriber, pre_verified=True, pre_confirmed=True, pre_approved=True)
    # Current posting policy uses the explicit global fixture default below;
    # per-list default_nonmember_action is not a supported settings field.
    assert mailing_list.get_held_count() == 0
    assert mailing_list.held == []

    # Read-only corroboration, never synthetic message/queue/held insertion.
    with closing(sqlite3.connect(f"file:{directory}/compat.sqlite?mode=ro", uri=True)) as db:
        def rows(sql, args=()):
            return db.execute(sql, args).fetchall()

        def deliveries():
            with sink.lock:
                return list(sink.deliveries)

        def submit(label, expected_count):
            raw = (f"From: outsider@example.invalid\r\nTo: {mailing_list.fqdn_listname}\r\n"
                   f"Message-ID: <held-{label}@example.invalid>\r\nSubject: Held {label}\r\n"
                   f"\r\nUnique held body {label}\r\n")
            with smtplib.LMTP("127.0.0.1", lmtp_port, timeout=5) as lmtp:
                assert lmtp.sendmail("outsider@example.invalid", [mailing_list.fqdn_listname], raw) == {}
            wait_for(lambda: mailing_list.get_held_count() == expected_count, "LMTP nonmember hold")
            matches = [item for item in mailing_list.held if item.subject == f"Held {label}"]
            assert len(matches) == 1
            item = matches[0]
            fetched = mailing_list.get_held_message(item.request_id)
            for prop in ("hold_date", "message_id", "msg", "reason", "request_id",
                         "self_link", "sender", "subject", "type"):
                assert getattr(fetched, prop) == getattr(item, prop), prop
            assert datetime.fromisoformat(item.hold_date).tzinfo is not None
            assert item.message_id == f"held-{label}@example.invalid"
            assert item.sender == "outsider@example.invalid"
            assert item.type == "held_message"
            assert item.reason == "moderation policy"
            assert item.msg == raw
            assert message_from_string(item.msg).get_payload() == f"Unique held body {label}\r\n"
            assert rows("SELECT q.state FROM queue_jobs q JOIN held_messages h ON h.message_id=q.message_id WHERE h.id=? AND q.queue='in'", (item.request_id,)) == [("done",)]
            return fetched

        held = submit("accept", 1)
        assert deliveries() == []
        wrong_token = cli("token", "create", user_id, "held-wrong-scope", "--scopes", "lists:read")
        restricted = client_for(wrong_token).get_list(mailing_list.fqdn_listname)
        for operation in (restricted.get_held_count, lambda: restricted.held,
                          lambda: restricted.get_held_message(held.request_id).msg,
                          lambda: restricted.moderate_message(held.request_id, "accept")):
            expect_http(403, operation)
        assert rows("SELECT disposition FROM held_messages WHERE id=?", (held.request_id,)) == [(None,)]
        assert rows("SELECT action FROM moderation_log WHERE held_id=?", (held.request_id,)) == []
        assert deliveries() == []

        # The installed public API has action/comment only, no forwarding option.
        # Probe unsupported wire fields with its actual connection, not a mock.
        expect_http(400, lambda: client._connection.call(held.self_link,
                    {"action": "accept", "forward": "other@example.invalid"}, "POST"))
        expect_http(400, lambda: held.moderate("forward"))
        comment = "deferred + nguyên & reviewed"
        assert mailing_list.moderate_message(held.request_id, "defer", comment=comment).status_code == 204
        assert mailing_list.get_held_count() == 1
        assert rows("SELECT action,reason FROM moderation_log WHERE held_id=?", (held.request_id,)) == [("defer", comment)]
        assert deliveries() == []
        assert held.accept().status_code == 204
        wait_for(lambda: len(deliveries()) == 1, "accepted SMTP delivery")
        wait_for(lambda: rows("SELECT status FROM delivery_recipients") == [("sent",)], "durable sent outcome")
        assert deliveries()[0][0] == [subscriber]
        assert b"Unique held body accept" in deliveries()[0][1]
        # SMTP completion does not mean the digest/archive consumers are done.
        # Observe quiescence before comparing all queue states across replay;
        # otherwise unrelated, legitimate child-job progress races this assertion.
        wait_for(lambda: rows("SELECT count(*) FROM queue_jobs WHERE state!='done'") == [(0,)],
                 "accepted child-job quiescence before replay")
        before = rows("SELECT id,queue,state FROM queue_jobs ORDER BY id")
        expect_http(409, held.accept)
        assert rows("SELECT id,queue,state FROM queue_jobs ORDER BY id") == before
        assert rows("SELECT count(*) FROM moderation_log WHERE held_id=? AND action='accept'", (held.request_id,)) == [(1,)]
        assert mailing_list.get_held_count() == 0

        rejected = submit("reject", 1)
        discarded = submit("discard", 2)
        assert rejected.reject("not allowed + nguyên").status_code == 204
        assert discarded.discard().status_code == 204
        assert mailing_list.get_held_count() == 0
        assert mailing_list.held == []
        assert rows("SELECT disposition FROM held_messages ORDER BY subject") == [("accepted",), ("discarded",), ("rejected",)]
        assert rows("SELECT reason FROM moderation_log WHERE held_id=? AND action='rejected'", (rejected.request_id,)) == [("not allowed + nguyên",)]
        assert rows("SELECT count(*) FROM queue_jobs WHERE queue='out'") == [(2,)]
        assert rows("SELECT count(*) FROM queue_jobs q JOIN held_messages h ON h.message_id=q.message_id WHERE h.disposition IN ('rejected','discarded') AND q.queue!='in'") == [(0,)]
        # One subscriber post plus one rejection notice, never a discarded post.
        wait_for(lambda: rows("SELECT count(*) FROM queue_jobs WHERE state!='done'") == [(0,)], "queue quiescence")
        assert len(deliveries()) == 2
        recipients, raw_notice = deliveries()[1]
        assert recipients == ["outsider@example.invalid"]
        notice = message_from_bytes(raw_notice)
        assert notice["Auto-Submitted"] == "auto-generated"
        assert notice["List-Post"] is None
        payload = notice.get_payload(decode=True)
        assert isinstance(payload, bytes)
        decoded = payload.decode("utf-8")
        assert "not allowed + nguyên" in decoded
        assert "Unique held body" not in decoded
    print("mailmanclient 3.3.5 held PASS: LMTP hold/count/list/get/msg, scoped denial, fail-closed options, defer comment, accept/replay one subscriber delivery, one rejection notice, silent discard; read-only DB corroboration")
