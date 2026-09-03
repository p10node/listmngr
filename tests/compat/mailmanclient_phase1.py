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
finally:
    if mailing_list is not None:
        mailing_list.delete()
    if domain is not None:
        domain.delete()

print(f"mailmanclient Phase 1 flow PASS: {fqdn}")
