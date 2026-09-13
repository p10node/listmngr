#!/usr/bin/env python3
"""Exercise the Phase 1 compatibility surface through real mailmanclient 3.3.5."""
import os
import sys
from importlib.metadata import version

from mailmanclient import Client

EXPECTED_VERSION = "3.3.5"
actual_version = version("mailmanclient")
if actual_version != EXPECTED_VERSION:
    raise SystemExit(f"mailmanclient {EXPECTED_VERSION} required, got {actual_version}")

base_url = os.environ.get("LISTMNGR_COMPAT_URL", "http://127.0.0.1:8000/3.1")
api_user = os.environ["LISTMNGR_COMPAT_USER"]
api_secret = os.environ["LISTMNGR_COMPAT_SECRET"]
commit = os.environ.get("LISTMNGR_SERVER_COMMIT", "unknown")
client = Client(base_url, api_user, api_secret)
host = "mailmanclient-phase1.invalid"
fqdn = f"compat@{host}"
subscriber = "subscriber@example.invalid"

print(f"mailmanclient={actual_version} server_commit={commit} base_url={base_url}")
domain = None
mailing_list = None
try:
    domain = client.create_domain(host, description="Phase 1 compatibility probe")
    mailing_list = domain.create_list("compat")
    member = mailing_list.subscribe(
        subscriber,
        pre_verified=True,
        pre_confirmed=True,
        pre_approved=True,
    )
    mailing_list.settings["description"] = "mailmanclient round trip"
    mailing_list.settings.save()
    if mailing_list.settings["description"] != "mailmanclient round trip":
        raise AssertionError("list setting did not round-trip")
    if not any(item.email == subscriber for item in mailing_list.members):
        raise AssertionError("subscriber absent from member roster")
    if member.email != subscriber:
        raise AssertionError("subscribe response address mismatch")

    # Header matches: add, read back, move by position, patch, find, delete.
    rules = mailing_list.header_matches
    first = rules.add("X-Spam-Flag", "^yes$", action="discard", tag="spam")
    rules.add("subject", "viagra")
    if first.header != "x-spam-flag" or first.action != "discard" or first.tag != "spam":
        raise AssertionError("header match did not read back as posted")
    if [rule.position for rule in rules] != [0, 1]:
        raise AssertionError("header matches are not numbered from zero")
    if rules[1].action is not None:
        raise AssertionError("a rule without a chain must read back as None")
    try:
        rules.add("Subject", "viagra")
    except Exception as error:  # HTTPError 400 is Mailman's answer
        if getattr(error, "code", None) != 400:
            raise
    else:
        raise AssertionError("duplicate header match was accepted")
    second = rules[1]
    second.position = 0
    second.tag = "ads"
    second.save()
    # The collection caches its page; re-read it the way Postorius does.
    rules = mailing_list.header_matches
    if [rule.header for rule in rules] != ["subject", "x-spam-flag"]:
        raise AssertionError("header match did not move")
    if rules[0].tag != "ads":
        raise AssertionError("header match patch did not apply")
    found = rules.find(tag="spam")
    if [rule.header for rule in found] != ["x-spam-flag"] or found[0].position != 1:
        raise AssertionError("find did not keep the real position")
    if rules.find(action="hold") != []:
        raise AssertionError("find with no match must be empty")
    del rules[0]
    rules = mailing_list.header_matches
    if [rule.header for rule in rules] != ["x-spam-flag"] or rules[0].position != 0:
        raise AssertionError("header match delete did not renumber")
    rules.clear()
    if len(mailing_list.header_matches) != 0:
        raise AssertionError("header matches were not cleared")
finally:
    if mailing_list is not None:
        mailing_list.delete()
    if domain is not None:
        domain.delete()

print(f"mailmanclient Phase 1 flow PASS: {fqdn}")
