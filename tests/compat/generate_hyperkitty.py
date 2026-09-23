#!/usr/bin/env python3
"""Build a real HyperKitty database for `crates/import/tests/hyperkitty.rs`.

The database is made by HyperKitty's own Django migrations and filled
through its own models (`hyperkitty.lib.incoming.add_to_list` for the
posts, so each Message-ID-Hash and thread is the one HyperKitty computes),
so the importer is tested against the real schema rather than hand-written
tables. No Mailman core is needed: `HYPERKITTY_BATCH_MODE` skips the
lookup HyperKitty would otherwise make.

Run in a disposable environment (never against a production archive):

    uv venv hk && uv pip install --python hk/bin/python hyperkitty==1.3.12 whoosh==2.7.4
    hk/bin/python tests/compat/generate_hyperkitty.py

Writes `crates/import/tests/fixtures/hyperkitty/hyperkitty.db`: three posts
in two threads, three votes, two tags, a category and a favourite. With
`HYPERKITTY_PG_NAME` (and `HYPERKITTY_PG_USER`, `HYPERKITTY_PG_PASSWORD`,
`HYPERKITTY_PG_HOST`; `psycopg2-binary` in the environment) the same
archive goes into that disposable PostgreSQL database instead, for the
ignored `hyperkitty_reads_a_real_database` test.
"""
import os
import sys
from email.message import EmailMessage
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "crates/import/tests/fixtures/hyperkitty/hyperkitty.db"
LIST = "rust-users@example.invalid"


def configure():
    os.environ.setdefault("DJANGO_SETTINGS_MODULE", "hyperkitty.tests.settings_test")
    import django
    from django.conf import settings

    if os.environ.get("HYPERKITTY_PG_NAME"):
        settings.DATABASES["default"] = {
            "ENGINE": "django.db.backends.postgresql",
            "NAME": os.environ["HYPERKITTY_PG_NAME"],
            "USER": os.environ.get("HYPERKITTY_PG_USER", ""),
            "PASSWORD": os.environ.get("HYPERKITTY_PG_PASSWORD", ""),
            "HOST": os.environ.get("HYPERKITTY_PG_HOST", "127.0.0.1"),
        }
    else:
        settings.DATABASES["default"]["NAME"] = str(OUT)
    settings.HYPERKITTY_BATCH_MODE = True
    django.setup()


def post(message_id, sender, date, subject, body, in_reply_to=None):
    message = EmailMessage()
    message["From"] = sender
    message["To"] = LIST
    message["Subject"] = subject
    message["Date"] = date
    message["Message-ID"] = f"<{message_id}>"
    if in_reply_to:
        message["In-Reply-To"] = f"<{in_reply_to}>"
        message["References"] = f"<{in_reply_to}>"
    message.set_content(body)
    return message


def main():
    postgres = bool(os.environ.get("HYPERKITTY_PG_NAME"))
    if not postgres:
        OUT.parent.mkdir(parents=True, exist_ok=True)
        OUT.unlink(missing_ok=True)
    configure()
    from django.contrib.auth.models import User
    from django.core.management import call_command
    from hyperkitty.lib.incoming import add_to_list
    from hyperkitty.models import Email, Favorite, Tag, Tagging, ThreadCategory, Vote

    call_command("migrate", verbosity=0, interactive=False)
    add_to_list(LIST, post("root-1@example.invalid", "alice@example.invalid",
                           "Mon, 21 Sep 2026 09:00:00 +0000", "Hello archive",
                           "The first post."))
    add_to_list(LIST, post("reply-1@example.invalid", "bob@example.invalid",
                           "Mon, 21 Sep 2026 10:00:00 +0000", "Re: Hello archive",
                           "A reply.", in_reply_to="root-1@example.invalid"))
    add_to_list(LIST, post("root-2@example.invalid", "carol@elsewhere.invalid",
                           "Tue, 22 Sep 2026 08:30:00 +0000", "Another thread",
                           "A second thread."))
    alice = User.objects.create_user("alice", "alice@example.invalid")
    bob = User.objects.create_user("bob", "bob@example.invalid")
    root1 = Email.objects.get(message_id="root-1@example.invalid")
    root2 = Email.objects.get(message_id="root-2@example.invalid")
    Vote.objects.create(email=root1, user=alice, value=1)
    Vote.objects.create(email=root1, user=bob, value=-1)
    Vote.objects.create(email=root2, user=alice, value=1)
    release = Tag.objects.create(name="release")
    bug = Tag.objects.create(name="bug")
    Tagging.objects.create(thread=root1.thread, tag=release, user=alice)
    Tagging.objects.create(thread=root1.thread, tag=bug, user=bob)
    category = ThreadCategory.objects.create(name="announcements", color="#ff0000")
    root1.thread.category = category
    root1.thread.save()
    Favorite.objects.create(thread=root2.thread, user=alice)
    if postgres:
        print(f"wrote {os.environ['HYPERKITTY_PG_NAME']}", file=sys.stderr)
        return
    from django.db import connection

    with connection.cursor() as cursor:
        cursor.execute("VACUUM")
    print(f"wrote {OUT.relative_to(ROOT)}", file=sys.stderr)


if __name__ == "__main__":
    main()
