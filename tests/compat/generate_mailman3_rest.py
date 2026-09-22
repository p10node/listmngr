#!/usr/bin/env python3
"""Populate a real Mailman 3 core over its REST API and dump the answers
as fixtures for `crates/import/tests/import3.rs`.

The fixtures are what `listmngr import3 --rest` reads, recorded from GNU
Mailman 3.3.10 so the importer is tested against the real shapes
(pagination envelopes, `http_etag`, the `config` and `preferences`
resources) rather than hand-written JSON.

Run against a disposable core (never a production one):

    mailman -C mailman.cfg start
    python3 tests/compat/generate_mailman3_rest.py \
        --url http://127.0.0.1:8199/3.1 --user restadmin --password restpass

Writes `crates/import/tests/fixtures/mailman3/<name>.json`. The core's own
database and message store are copied next to them afterwards, for the
`--db` path of the importer:

    cp var/mailman.db crates/import/tests/fixtures/mailman3/mailman.db
    sqlite3 crates/import/tests/fixtures/mailman3/mailman.db VACUUM
    cp -R var/messages crates/import/tests/fixtures/mailman3/var/
"""
import argparse
import base64
import time
import json
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
OUT = ROOT / "crates/import/tests/fixtures/mailman3"


class Rest:
    def __init__(self, url, user, password):
        self.url = url.rstrip("/")
        token = base64.b64encode(f"{user}:{password}".encode()).decode()
        self.auth = f"Basic {token}"

    def request(self, method, path, data=None):
        url = path if path.startswith("http") else f"{self.url}/{path.lstrip('/')}"
        body = None
        headers = {"Authorization": self.auth}
        if data is not None:
            body = "&".join(
                f"{key}={urllib.parse.quote(str(value))}" for key, value in data.items()
            ).encode()
            headers["Content-Type"] = "application/x-www-form-urlencoded"
        request = urllib.request.Request(url, data=body, headers=headers, method=method)
        try:
            with urllib.request.urlopen(request) as response:
                raw = response.read()
                return response.status, (json.loads(raw) if raw else None)
        except urllib.error.HTTPError as error:
            return error.code, error.read().decode()

    def get(self, path):
        status, body = self.request("GET", path)
        assert status == 200, (path, status, body)
        return body

    def post(self, path, **data):
        status, body = self.request("POST", path, data)
        assert status in (200, 201, 202, 204), (path, status, body)
        return body

    def patch(self, path, **data):
        status, body = self.request("PATCH", path, data)
        assert status in (200, 204), (path, status, body)
        return body

    def put(self, path, **data):
        status, body = self.request("PUT", path, data)
        assert status in (200, 204), (path, status, body)
        return body


def populate(rest):
    """A small site with everything the importer reads."""
    rest.post("domains", mail_host="example.invalid", description="Example")
    rest.post("domains", mail_host="other.invalid", description="Other")
    rest.post("lists", fqdn_listname="rust-users@example.invalid", style_name="legacy-default")
    rest.post("lists", fqdn_listname="announce@other.invalid", style_name="legacy-announce")
    rest.patch(
        "lists/rust-users.example.invalid/config",
        display_name="Rust-Users",
        description="Rust users of Example",
        info="Long description\nwith two lines",
        subject_prefix="[Rust] ",
        preferred_language="en",
        advertised="False",
        anonymous_list="True",
        administrivia="False",
        archive_policy="private",
        collapse_alternatives="False",
        convert_html_to_plaintext="True",
        default_member_action="reject",
        default_nonmember_action="discard",
        digest_size_threshold="45.0",
        dmarc_mitigate_action="wrap_message",
        dmarc_mitigate_unconditionally="True",
        dmarc_moderation_notice="Your domain publishes a DMARC policy.",
        dmarc_wrapped_message_text="The original post is attached.",
        emergency="True",
        first_strip_reply_to="True",
        max_message_size="120",
        max_num_recipients="25",
        member_roster_visibility="moderators",
        personalize="individual",
        reply_goes_to_list="explicit_header",
        reply_to_address="replies@example.invalid",
        require_explicit_destination="False",
        respond_to_post_requests="False",
        send_welcome_message="False",
        send_goodbye_message="False",
        subscription_policy="confirm_then_moderate",
        unsubscription_policy="confirm",
        acceptable_aliases="rust-users-alias@example.invalid\n^announce-.*@example\\.invalid",
        gateway_to_mail="True",
        gateway_to_news="True",
        linked_newsgroup="comp.lang.rust.lists",
        newsgroup_moderation="moderated",
        nntp_prefix_subject_too="False",
    )
    # Members, owners, moderators, a nonmember with an action.
    for email, name in [
        ("alice@example.invalid", "Alice Nguyễn"),
        ("bob@example.invalid", "Bob"),
        ("carol@elsewhere.invalid", ""),
    ]:
        rest.post(
            "members",
            list_id="rust-users.example.invalid",
            subscriber=email,
            display_name=name,
            role="member",
            pre_verified="True",
            pre_confirmed="True",
            pre_approved="True",
        )
    for role, email in [
        ("owner", "owner@example.invalid"),
        ("moderator", "mod@example.invalid"),
        ("nonmember", "stranger@example.invalid"),
    ]:
        rest.post(
            "members",
            list_id="rust-users.example.invalid",
            subscriber=email,
            role=role,
            pre_verified="True",
            pre_confirmed="True",
            pre_approved="True",
        )
    members = {
        entry["email"]: entry
        for entry in rest.get("lists/rust-users.example.invalid/roster/member")["entries"]
    }
    rest.patch(
        members["alice@example.invalid"]["self_link"] + "/preferences",
        acknowledge_posts="True",
        hide_address="True",
        receive_own_postings="False",
        delivery_mode="plaintext_digests",
    )
    rest.patch(members["bob@example.invalid"]["self_link"], moderation_action="hold")
    rest.patch(
        members["carol@elsewhere.invalid"]["self_link"] + "/preferences",
        delivery_status="by_bounces",
    )
    nonmember = rest.get("lists/rust-users.example.invalid/roster/nonmember")["entries"][0]
    rest.patch(nonmember["self_link"], moderation_action="discard")
    # Bans, header matches, one template.
    rest.post("lists/rust-users.example.invalid/bans", email="spammer@example.invalid")
    rest.post("lists/rust-users.example.invalid/bans", email="^.*@spam\\.invalid")
    rest.post("bans", email="global-spammer@example.invalid")
    rest.post(
        "lists/rust-users.example.invalid/header-matches",
        header="x-spam-flag",
        pattern="YES",
        action="discard",
    )
    rest.post(
        "lists/rust-users.example.invalid/header-matches",
        header="subject",
        pattern=".*viagra.*",
        action="reject",
    )
    rest.patch(
        "lists/rust-users.example.invalid/uris",
        **{"list:member:regular:footer": "http://example.invalid/footer.txt"},
    )
    # A user with two addresses, both verified, subscribed as the user so
    # the roster carries a user id instead of an address.
    rest.post("users", email="dave@example.invalid", display_name="Dave", password="x" * 12)
    user = rest.get("users/dave@example.invalid")
    rest.post(user["self_link"] + "/addresses", email="dave@work.invalid")
    for email in ("dave@example.invalid", "dave@work.invalid"):
        rest.post(f"addresses/{email}/verify")
    rest.post(user["self_link"] + "/preferred_address", email="dave@example.invalid")
    rest.post(
        "members",
        list_id="rust-users.example.invalid",
        subscriber=user["user_id"],
        role="member",
        pre_verified="True",
        pre_confirmed="True",
        pre_approved="True",
    )
    rest.patch(user["self_link"] + "/preferences", preferred_language="en", hide_address="True")
    # A second account: a server owner with an address it never verified.
    rest.post("users", email="eve@example.invalid", display_name="Eve", password="y" * 12)
    eve = rest.get("users/eve@example.invalid")
    rest.post(eve["self_link"] + "/addresses", email="eve@spare.invalid")
    rest.patch(eve["self_link"], is_server_owner="True")


def waiting(rest):
    """Leave a held message and an undecided subscription behind, the way
    a site being migrated has them: a post from a stranger to a list that
    holds non-member posts, and a join a moderator has not answered."""
    rest.patch(
        "lists/announce.other.invalid/config",
        default_nonmember_action="hold",
        subscription_policy="moderate",
    )
    rest.post(
        "queues/in",
        list_id="announce.other.invalid",
        text=(
            "From: stranger@example.invalid\n"
            "To: announce@other.invalid\n"
            "Subject: Please let me in\n"
            "Message-ID: <held-1@example.invalid>\n"
            "Date: Mon, 21 Sep 2026 09:00:00 +0000\n"
            "\n"
            "A post that waits for a moderator.\n"
        ),
    )
    rest.post(
        "members",
        list_id="announce.other.invalid",
        subscriber="wanted@example.invalid",
        display_name="Wanted",
        role="member",
        pre_verified="True",
        pre_confirmed="True",
    )
    # The `in` runner has to see the injected message before it is held.
    for _ in range(50):
        time.sleep(0.2)
        if rest.get("lists/announce.other.invalid/held")["total_size"]:
            return
    raise AssertionError("the core did not hold the injected message")


def dump(rest):
    OUT.mkdir(parents=True, exist_ok=True)
    wanted = {
        "domains": "domains",
        "lists": "lists?advertised=false",
        "list-config": "lists/rust-users.example.invalid/config",
        "roster-member": "lists/rust-users.example.invalid/roster/member",
        "roster-owner": "lists/rust-users.example.invalid/roster/owner",
        "roster-moderator": "lists/rust-users.example.invalid/roster/moderator",
        "roster-nonmember": "lists/rust-users.example.invalid/roster/nonmember",
        "bans-list": "lists/rust-users.example.invalid/bans",
        "bans-global": "bans",
        "header-matches": "lists/rust-users.example.invalid/header-matches",
        "uris": "lists/rust-users.example.invalid/uris",
        "members-page": "members?count=2&page=1",
        "users": "users",
        "announce-config": "lists/announce.other.invalid/config",
        "announce-roster-member": "lists/announce.other.invalid/roster/member",
        "announce-roster-owner": "lists/announce.other.invalid/roster/owner",
        "announce-roster-moderator": "lists/announce.other.invalid/roster/moderator",
        "announce-roster-nonmember": "lists/announce.other.invalid/roster/nonmember",
        "announce-bans": "lists/announce.other.invalid/bans",
        "announce-header-matches": "lists/announce.other.invalid/header-matches",
        "announce-uris": "lists/announce.other.invalid/uris",
        "held": "lists/rust-users.example.invalid/held",
        "requests": "lists/rust-users.example.invalid/requests",
        "announce-held": "lists/announce.other.invalid/held",
        "announce-requests": "lists/announce.other.invalid/requests",
    }
    for name, path in wanted.items():
        body = rest.get(path)
        (OUT / f"{name}.json").write_text(json.dumps(body, indent=2, sort_keys=True) + "\n")
        print(name, path)
    # Every user, with the resources that hang off it.
    addresses, preferences, preferred = {}, {}, {}
    for user in rest.get("users")["entries"]:
        uid = user["user_id"]
        addresses[uid] = rest.get(f"users/{uid}/addresses")
        preferences[uid] = rest.get(f"users/{uid}/preferences")
        status, body = rest.request("GET", f"users/{uid}/preferred_address")
        preferred[uid] = body if status == 200 else None
    for name, data in (
        ("user-addresses", addresses),
        ("user-preferences", preferences),
        ("user-preferred", preferred),
    ):
        (OUT / f"{name}.json").write_text(json.dumps(data, indent=2, sort_keys=True) + "\n")
        print(name, len(data))
    # Every member's preferences, by the member id the roster gives.
    preferences = {}
    for list_id in ("rust-users.example.invalid", "announce.other.invalid"):
        for role in ("member", "owner", "moderator", "nonmember"):
            for entry in rest.get(f"lists/{list_id}/roster/{role}").get("entries", []):
                preferences[entry["member_id"]] = rest.get(entry["self_link"] + "/preferences")
    (OUT / "preferences.json").write_text(
        json.dumps(preferences, indent=2, sort_keys=True) + "\n"
    )
    print("preferences", len(preferences))


MULTIPART = """From: alice@example.invalid
To: dev@example.invalid
Subject: =?utf-8?b?WGluIGNow6Bv?=
Message-ID: <multi@example.invalid>
MIME-Version: 1.0
Content-Type: multipart/mixed; boundary="=-=frontier=-="

preamble text
--=-=frontier=-=
Content-Type: text/plain; charset="utf-8"
Content-Transfer-Encoding: 8bit

Xin ch\u00e0o m\u1ecdi ng\u01b0\u1eddi.
--=-=frontier=-=
Content-Type: application/octet-stream
Content-Transfer-Encoding: base64
Content-Disposition: attachment; filename="a.bin"

AAECAw==
--=-=frontier=-=--
epilogue text
"""


def multipart_fixture():
    """A multipart message pickled the way the core's message store
    pickles one (`mailman.email.message.Message`), for the renderer test;
    needs the `mailman` package importable."""
    import pickle
    from email import message_from_string
    from mailman.email.message import Message

    (OUT / "var").mkdir(parents=True, exist_ok=True)
    with open(OUT / "var" / "multipart.pck", "wb") as fp:
        pickle.dump(message_from_string(MULTIPART, _class=Message), fp)
    print("var/multipart.pck")


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--url", default="http://127.0.0.1:8199/3.1")
    parser.add_argument("--user", default="restadmin")
    parser.add_argument("--password", default="restpass")
    parser.add_argument("--no-populate", action="store_true")
    parser.add_argument("--multipart", action="store_true", help="also write var/multipart.pck")
    args = parser.parse_args()
    rest = Rest(args.url, args.user, args.password)
    if not args.no_populate:
        populate(rest)
        waiting(rest)
    dump(rest)
    if args.multipart:
        multipart_fixture()


if __name__ == "__main__":
    main()
