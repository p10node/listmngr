#!/usr/bin/env python3
"""Produce a real Mailman 3.3 digest pair (RFC 1153 and MIME) from fixed
inputs, as fixtures for `crates/mail/tests/digest_snapshot.rs`.

Runs Mailman core's own digesters under its test configuration (an
in-memory SQLite database and a temporary var directory), so the output
is what Mailman itself would send; nothing is mocked. Writes three files
into `crates/mail/tests/fixtures/digests/`:

    mailman-<version>-plain.eml   the RFC 1153 issue
    mailman-<version>-mime.eml    the MIME issue
    inputs.json                   the list, volume, issue, templates and
                                  the two posts, for the Rust side

Run: uv run --python 3.12 --with mailman==3.3.10 python tests/compat/generate_mailman_digest.py
"""
import json
import os
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "crates/mail/tests/fixtures/digests"

# Fixed inputs: a UTF-8 Vietnamese post and a post with a non-text
# attachment (the RFC 1153 issue scrubs it, the MIME issue keeps it). The
# list's own name stays ASCII: Mailman's `en` charset is us-ascii and its
# MIME digester encodes the masthead with it, so a non-ASCII display name
# raises UnicodeEncodeError in Mailman itself (3.3.10).
POSTS = [
    (
        "Message-ID: <a1@example.invalid>\n"
        "From: An Nguyễn <a@example.invalid>\n"
        "To: news@example.invalid\n"
        "Date: Mon, 01 Jan 2024 10:00:00 +0000\n"
        "Subject: [News] Xin chào\n"
        "Content-Type: text/plain; charset=utf-8\n"
        "Content-Transfer-Encoding: 8bit\n"
        "\n"
        "Nội dung đầu tiên.\n"
    ),
    (
        "Message-ID: <b1@example.invalid>\n"
        "From: b@example.invalid\n"
        "To: news@example.invalid\n"
        "Date: Mon, 01 Jan 2024 11:00:00 +0000\n"
        "Subject: Re: [News] Second\n"
        "MIME-Version: 1.0\n"
        "Content-Type: multipart/mixed; boundary=\"part\"\n"
        "\n"
        "--part\n"
        "Content-Type: text/plain; charset=us-ascii\n"
        "\n"
        "Different body\n"
        "--part\n"
        "Content-Type: application/octet-stream; name=\"notes.bin\"\n"
        "Content-Disposition: attachment; filename=\"notes.bin\"\n"
        "Content-Transfer-Encoding: base64\n"
        "\n"
        "AAECAw==\n"
        "--part--\n"
    ),
]
HEADER = "Read the list rules before replying.\n"
FOOTER = "News mailing list -- news@example.invalid\n"


def main():
    os.environ.setdefault("MAILMAN_TESTING", "1")
    from mailman.testing.layers import ConfigLayer

    ConfigLayer.setUp()
    try:
        from zope.component import getUtility
        from mailman.app.lifecycle import create_list
        from mailman.config import config
        from mailman.database.transaction import transaction
        from mailman.interfaces.domain import IDomainManager
        from mailman.interfaces.template import ITemplateManager
        from mailman.runners.digest import MIMEDigester, RFC1153Digester
        from mailman.testing.helpers import specialized_message_from_string as mfs
        from mailman.version import VERSION

        with tempfile.TemporaryDirectory() as directory:
            header = Path(directory) / "header.txt"
            footer = Path(directory) / "footer.txt"
            header.write_text(HEADER, encoding="utf-8")
            footer.write_text(FOOTER, encoding="utf-8")
            with transaction():
                getUtility(IDomainManager).add("example.invalid")
                mlist = create_list("news@example.invalid")
                mlist.display_name = "News"
                mlist.subject_prefix = "[News] "
                manager = getUtility(ITemplateManager)
                manager.set("list:member:digest:header", mlist.list_id, header.as_uri())
                manager.set("list:member:digest:footer", mlist.list_id, footer.as_uri())
            digests = {}
            for kind, cls in (("plain", RFC1153Digester), ("mime", MIMEDigester)):
                digester = cls(mlist, 2, 3)
                messages = [mfs(post) for post in POSTS]
                for count, msg in enumerate(messages, 1):
                    digester.add_to_toc(msg, count)
                digester.add_toc(len(messages))
                for count, msg in enumerate(messages, 1):
                    digester.add_message(msg, count)
                digests[kind] = digester.finish().as_bytes()
        OUT.mkdir(parents=True, exist_ok=True)
        for kind, raw in digests.items():
            (OUT / f"mailman-{VERSION}-{kind}.eml").write_bytes(raw)
        (OUT / "inputs.json").write_text(
            json.dumps(
                {
                    "mailman": VERSION,
                    "list_id": "news.example.invalid",
                    "display_name": "News",
                    "subject_prefix": "[News] ",
                    "volume": 2,
                    "number": 3,
                    "header": HEADER,
                    "footer": FOOTER,
                    "plain_keep_headers": config.digests.plain_digest_keep_headers.split(),
                    "mime_keep_headers": config.digests.mime_digest_keep_headers.split(),
                    "posts": POSTS,
                },
                ensure_ascii=False,
                indent=1,
            )
            + "\n",
            encoding="utf-8",
        )
        print(f"wrote Mailman {VERSION} digests into {OUT}")
    finally:
        ConfigLayer.tearDown()


if __name__ == "__main__":
    sys.exit(main())
