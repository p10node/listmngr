#!/usr/bin/env python3
"""Write Mailman 2.1 `config.pck` fixtures for `crates/import/tests/import21.rs`.

Mailman 2.1 pickles its `MailList.__dict__` with protocol 2: a dict whose
keys and text values are Python 2 `str` (bytes here), the same keys as
Mailman 3's own importer fixtures (`mailman/testing/config.pck`). Mailman
2.1 itself is Python 2 and cannot run here, so these are written by this
script with the same layout; `import21_reads_mailman3s_own_fixture` in the
same test file reads Mailman 3's real fixture when `MAILMAN_TESTING_DIR`
names the installed package's `testing/` directory.

Two fixtures:

    mailman21-full.pck     every setting set away from its default, three
                           regular members and one digest member with
                           options, an owner, a moderator, nonmember lists,
                           bans, header filter rules, templates, topics, and
                           `bounce_info` instances written with the `OBJ`
                           opcode as Mailman 2.1 wrote them
    mailman21-minimal.pck  a list with the 2.1 defaults and no members

Run: python3 tests/compat/generate_mailman21_config.py
"""
import pickle
import struct
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "crates/import/tests/fixtures"


def b(value):
    """Mailman 2.1 wrote Python 2 str: bytes."""
    if isinstance(value, str):
        return value.encode("utf-8")
    if isinstance(value, list):
        return [b(item) for item in value]
    if isinstance(value, tuple):
        return tuple(b(item) for item in value)
    if isinstance(value, dict):
        return {b(key): b(item) for key, item in value.items()}
    return value


MINIMAL = {
    "real_name": "Test",
    "host_name": "example.invalid",
    "web_page_url": "http://example.invalid/mailman/",
    "description": "",
    "info": "",
    "subject_prefix": "[Test] ",
    "preferred_language": "en",
    "available_languages": ["en"],
    "advertised": True,
    "anonymous_list": False,
    "admin_immed_notify": True,
    "admin_notify_mchanges": False,
    "administrivia": True,
    "require_explicit_destination": True,
    "respond_to_post_requests": 1,
    "send_welcome_msg": True,
    "send_goodbye_msg": True,
    "include_list_post_header": 1,
    "include_rfc2369_headers": 1,
    "archive": True,
    "archive_private": 0,
    "archive_volume_frequency": 1,
    "autorespond_admin": 0,
    "autoresponse_admin_text": "",
    "autorespond_postings": 0,
    "autoresponse_postings_text": "",
    "autorespond_requests": 0,
    "autoresponse_request_text": "",
    "autoresponse_graceperiod": 90,
    "bounce_processing": True,
    "bounce_score_threshold": 5.0,
    "bounce_info_stale_after": 604800,
    "bounce_you_are_disabled_warnings": 3,
    "bounce_you_are_disabled_warnings_interval": 604800,
    "bounce_notify_owner_on_disable": True,
    "bounce_notify_owner_on_removal": True,
    "bounce_unrecognized_goes_to_list_owner": True,
    "collapse_alternatives": True,
    "convert_html_to_plaintext": True,
    "filter_action": 0,
    "filter_content": False,
    "filter_filename_extensions": ["exe", "bat", "cmd", "com", "pif", "scr", "vbs", "cpl"],
    "filter_mime_types": [],
    "pass_filename_extensions": [],
    "pass_mime_types": ["multipart/mixed", "multipart/alternative", "text/plain"],
    "default_member_moderation": False,
    "member_moderation_action": 0,
    "member_moderation_notice": "",
    "generic_nonmember_action": 1,
    "nonmember_rejection_notice": "",
    "dmarc_moderation_action": 0,
    "dmarc_moderation_addresses": [],
    "dmarc_moderation_notice": "",
    "dmarc_wrapped_message_text": "",
    "from_is_list": 0,
    "digestable": True,
    "digest_is_default": False,
    "digest_send_periodic": True,
    "digest_size_threshhold": 30,
    "digest_volume_frequency": 1,
    "next_digest_number": 1,
    "volume": 1,
    "digest_header": "",
    "digest_footer": "",
    "msg_header": "",
    "msg_footer": "",
    "welcome_msg": "",
    "goodbye_msg": "",
    "emergency": 0,
    "first_strip_reply_to": False,
    "reply_goes_to_list": 0,
    "reply_to_address": "",
    "personalize": 0,
    "subscribe_policy": 1,
    "unsubscribe_policy": 0,
    "private_roster": 1,
    "max_message_size": 40,
    "max_num_recipients": 10,
    "mod_password": None,
    "password": "0beec7b5ea3f0fdbc95d0dd47f3c5bc275da8a33",
    "gateway_to_mail": 0,
    "gateway_to_news": 0,
    "linked_newsgroup": "",
    "news_moderation": 0,
    "news_prefix_subject_too": 1,
    "nntp_host": "",
    "topics": [],
    "topics_bodylines_limit": 5,
    "topics_enabled": 0,
    "topics_userinterest": {},
    "acceptable_aliases": "\n",
    "ban_list": [],
    "header_filter_rules": [],
    "accept_these_nonmembers": [],
    "hold_these_nonmembers": [],
    "reject_these_nonmembers": [],
    "discard_these_nonmembers": [],
    "members": {},
    "digest_members": {},
    "user_options": {},
    "usernames": {},
    "passwords": {},
    "language": {},
    "delivery_status": {},
    "owner": [],
    "moderator": [],
    "bounce_info": {},
    "created_at": 1270420777.274485,
    "last_post_time": 0,
    "digest_last_sent_at": 0,
    "post_id": 1.0,
    "data_version": 97,
}

FULL = dict(MINIMAL)
FULL.update(
    {
        "real_name": "Rust-Users",
        "description": "Rust users of Example",
        "info": "Long description\nwith two lines",
        "subject_prefix": "[Rust]",
        "preferred_language": "vi",
        "advertised": False,
        "anonymous_list": True,
        "admin_immed_notify": False,
        "admin_notify_mchanges": True,
        "administrivia": False,
        "require_explicit_destination": False,
        "respond_to_post_requests": 0,
        "send_welcome_msg": False,
        "send_goodbye_msg": False,
        "include_list_post_header": 0,
        "include_rfc2369_headers": 0,
        "archive": True,
        "archive_private": 1,
        "autorespond_admin": 1,
        "autoresponse_admin_text": "Owner reply %(real_name)s",
        "autorespond_postings": 1,
        "autoresponse_postings_text": "Posting reply",
        "autorespond_requests": 2,
        "autoresponse_request_text": "Request reply",
        "autoresponse_graceperiod": 3,
        "bounce_processing": False,
        "bounce_score_threshold": 7.5,
        "bounce_info_stale_after": 1209600,
        "bounce_you_are_disabled_warnings": 2,
        "bounce_you_are_disabled_warnings_interval": 259200,
        "bounce_notify_owner_on_disable": False,
        "bounce_notify_owner_on_removal": False,
        "bounce_unrecognized_goes_to_list_owner": False,
        "collapse_alternatives": False,
        "convert_html_to_plaintext": False,
        "filter_action": 2,
        "filter_content": True,
        "filter_filename_extensions": ["exe", "bat"],
        "filter_mime_types": ["image/jpeg", "application/octet-stream"],
        "pass_filename_extensions": ["txt", "pdf"],
        "pass_mime_types": ["multipart/mixed", "text/plain"],
        "default_member_moderation": True,
        "member_moderation_action": 1,
        "generic_nonmember_action": 3,
        "dmarc_moderation_action": 1,
        "dmarc_moderation_addresses": ["^.*@yahoo\\.com$", "friend@example.invalid"],
        "dmarc_moderation_notice": "Your domain publishes a DMARC policy.",
        "dmarc_wrapped_message_text": "The original post is attached.",
        "from_is_list": 2,
        "digest_send_periodic": False,
        "digest_size_threshhold": 45,
        "digest_volume_frequency": 3,
        "next_digest_number": 4,
        "volume": 2,
        "digest_header": "Digest header for %(real_name)s",
        "digest_footer": "_______________________________________________\n%(real_name)s mailing list\n%(real_name)s@%(host_name)s\n%(web_page_url)slistinfo%(cgiext)s/%(_internal_name)s\n",
        "msg_header": "Header of %(list_name)s",
        "msg_footer": "Footer for %(user_address)s of %(real_name)s",
        "goodbye_msg": "Goodbye from %(real_name)s, %(user_name)s",
        "emergency": 1,
        "first_strip_reply_to": True,
        "reply_goes_to_list": 2,
        "reply_to_address": "replies@example.invalid",
        "personalize": 1,
        "subscribe_policy": 3,
        "unsubscribe_policy": 1,
        "private_roster": 2,
        "max_message_size": 120,
        "max_num_recipients": 25,
        "mod_password": "d033e22ae348aeb5660fc2140aec35850c4da997",
        "gateway_to_mail": 1,
        "gateway_to_news": 1,
        "linked_newsgroup": "comp.lang.rust.lists",
        "news_moderation": 2,
        "news_prefix_subject_too": 0,
        "topics": [
            ("release", "^subject:.*release", "Releases", 0),
            ("bugs", "^subject:.*bug", "Bug reports", 0),
        ],
        "topics_bodylines_limit": 7,
        "topics_enabled": 1,
        "acceptable_aliases": "rust-users-alias@example.invalid\n^announce-.*@example\\.invalid\n\n",
        "ban_list": ["spammer@example.invalid", "^.*@spam\\.invalid", "^[unclosed"],
        "header_filter_rules": [
            ("X-Spam-Flag: YES\nSubject: .*viagra.*", 3, False),
            ("^X-Trusted: yes", 6, False),
            ("List-Post:", 0, False),
            ("nonsense-without-separator", 7, False),
            ("Subject: [", 2, False),
        ],
        "accept_these_nonmembers": ["friend@example.invalid", "^.*@partner\\.invalid", "@otherlist"],
        "hold_these_nonmembers": ["suspect@example.invalid"],
        "reject_these_nonmembers": ["^.*@reject\\.invalid"],
        "discard_these_nonmembers": ["noise@example.invalid"],
        "members": {
            "alice@example.invalid": 0,
            "bob@example.invalid": "Bob@Example.invalid",
            "spammer@example.invalid": 0,
            "carol@elsewhere.invalid": 0,
        },
        "digest_members": {"dave@example.invalid": 0, "erin@example.invalid": 0},
        "user_options": {
            "alice@example.invalid": 2 | 4 | 16 | 128,
            "bob@example.invalid": 256,
            "carol@elsewhere.invalid": 0,
            "dave@example.invalid": 8,
            "erin@example.invalid": 0,
        },
        "usernames": {
            "alice@example.invalid": "Alice Nguyễn",
            "bob@example.invalid": "Bob",
        },
        "language": {"alice@example.invalid": "vi", "bob@example.invalid": "xx"},
        "delivery_status": {
            "bob@example.invalid": (2, 1270420777.0),
            "carol@elsewhere.invalid": (4, 1270420777.0),
            "dave@example.invalid": (3, 1270420777.0),
        },
        "owner": ["owner@example.invalid", "alice@example.invalid"],
        "moderator": ["mod@example.invalid"],
    }
)


def bounce_info(entries):
    """`bounce_info` as Mailman 2.1 pickled it: old-style `_BounceInfo`
    instances through the protocol 1 `OBJ` opcode (class inside the mark,
    then `BUILD` with the instance `__dict__`), which Python 3 never emits.
    """
    out = b"}("
    for email, score in entries:
        out += b"U" + bytes([len(email)]) + email.encode()
        out += b"(cMailman.Bouncer\n_BounceInfo\no}("
        out += b"U\x05score" + b"G" + struct.pack(">d", score)
        out += b"U\x0bnoticesleftK\x03"
        out += b"U\x06member" + b"U" + bytes([len(email)]) + email.encode()
        out += b"ub"
    return out + b"u"


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    for name, data in (("mailman21-full.pck", FULL), ("mailman21-minimal.pck", MINIMAL)):
        raw = pickle.dumps(b(data), protocol=2)
        assert raw.endswith(b"u."), "one SETITEMS batch closes the dict"
        if name == "mailman21-full.pck":
            raw = raw[:-2] + b"U\x0bbounce_info" + bounce_info(
                [("alice@example.invalid", 1.0), ("carol@elsewhere.invalid", 5.0)]
            ) + b"u."
        with open(OUT / name, "wb") as fp:
            fp.write(raw)
        print(name, (OUT / name).stat().st_size, "bytes")


if __name__ == "__main__":
    main()
