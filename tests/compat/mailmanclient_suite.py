#!/usr/bin/env python3
"""The mailmanclient 3.3.5 doctest (`docs/using.rst`), section by section,
against a running listmngr: every call the doctest makes, with the value
the doctest prints, on this server's own disposable fixture.

Where listmngr deliberately differs from Mailman the check says so and
asserts listmngr's behaviour instead; each such place is a `deviation()`
so the run's summary counts them. Only the doctest's observable surface
is exercised — no database is read.
"""
import os
import time
from importlib.metadata import version
from operator import itemgetter
from urllib.error import HTTPError

from mailmanclient import Client

EXPECTED_VERSION = "3.3.5"
actual_version = version("mailmanclient")
if actual_version != EXPECTED_VERSION:
    raise SystemExit(f"mailmanclient {EXPECTED_VERSION} required, got {actual_version}")

base_url = os.environ.get("LISTMNGR_COMPAT_URL", "http://127.0.0.1:8000/3.1")
client = Client(base_url, os.environ["LISTMNGR_COMPAT_USER"], os.environ["LISTMNGR_COMPAT_SECRET"])

COM = "suite-example.invalid"
EDU = "suite-example-edu.invalid"
NET = "suite-example-net.invalid"
ORG = "suite-example-org.invalid"
HOSTS = (COM, EDU, NET, ORG)

checks = 0
deviations = []
sections = []


def check(label, actual, expected):
    global checks
    checks += 1
    if actual != expected:
        raise AssertionError(f"{label}: expected {expected!r}, got {actual!r}")


def deviation(label, actual, expected_here, mailman_prints):
    """A place listmngr differs from the doctest on purpose."""
    deviations.append(f"{label}: Mailman prints {mailman_prints!r}, listmngr {expected_here!r}")
    check(label, actual, expected_here)


def http_error(label, operation, status):
    global checks
    checks += 1
    try:
        operation()
    except HTTPError as error:
        if error.code != status:
            raise AssertionError(f"{label}: expected HTTP {status}, got {error.code}") from error
        return
    raise AssertionError(f"{label}: expected HTTP {status}, got success")


def raises(label, operation, exception):
    global checks
    checks += 1
    try:
        operation()
    except exception:
        return
    raise AssertionError(f"{label}: expected {exception.__name__}")


def section(name):
    sections.append(name)
    print(f"  section {name}", flush=True)


def ours(items, attribute="mail_host"):
    return [item for item in items if getattr(item, attribute) in HOSTS]


def our_lists(items):
    return [item for item in items if item.mail_host in HOSTS]


def our_members(items):
    return [item for item in items if item.list_id.split(".", 1)[1] in HOSTS]


# ---------------------------------------------------------------- Domains

section("domains")
example_dot_com = client.create_domain(COM)
check("domain description", example_dot_com.description, None)
check("domain mail_host", example_dot_com.mail_host, COM)
check("domain alias_domain", example_dot_com.alias_domain, None)
example_dot_edu = client.create_domain(EDU, alias_domain=f"x.{EDU}")
check("alias mail_host", example_dot_edu.mail_host, EDU)
check("alias alias_domain", example_dot_edu.alias_domain, f"x.{EDU}")
example = client.get_domain(COM)
check("get_domain", example.mail_host, COM)
example_net = client.create_domain(NET)
example_org = client.create_domain(ORG)
check("created org", example_org.mail_host, ORG)
check("domains listed", sorted(d.mail_host for d in ours(client.domains)), sorted(HOSTS))
example_org.delete()
check("domain deleted", sorted(d.mail_host for d in ours(client.domains)), sorted((COM, EDU, NET)))

# ---------------------------------------------------------- Mailing lists

section("lists")
test_one = example.create_list("test-1")
check("fqdn_listname", test_one.fqdn_listname, f"test-1@{COM}")
check("list mail_host", test_one.mail_host, COM)
check("list_name", test_one.list_name, "test-1")
check("display_name", test_one.display_name, "Test-1")
test_two = example.create_list("test-announce", style_name="legacy-announce")
check("announce fqdn", test_two.fqdn_listname, f"test-announce@{COM}")
styles = client.styles
check(
    "styles",
    [style["name"] for style in sorted(styles["styles"], key=itemgetter("name"))],
    ["legacy-announce", "legacy-default", "private-default"],
)
check("default style", styles["default"], "legacy-default")
my_list = client.get_list(f"test-1@{COM}")
check("get_list", my_list.fqdn_listname, f"test-1@{COM}")
check("create test-2", example.create_list("test-2").fqdn_listname, f"test-2@{COM}")
domain = client.get_domain(NET)
check("create test-3 net", domain.create_list("test-3").fqdn_listname, f"test-3@{NET}")
check("create test-3 com", example.create_list("test-3").fqdn_listname, f"test-3@{COM}")
check(
    "client.lists",
    sorted(m.fqdn_listname for m in our_lists(client.lists)),
    sorted([f"test-1@{COM}", f"test-2@{COM}", f"test-3@{COM}", f"test-3@{NET}", f"test-announce@{COM}"]),
)
my_list.settings["advertised"] = False
my_list.settings.save()
check(
    "get_lists(advertised)",
    sorted(m.fqdn_listname for m in our_lists(client.get_lists(advertised=True))),
    sorted([f"test-2@{COM}", f"test-3@{COM}", f"test-3@{NET}", f"test-announce@{COM}"]),
)
page = client.get_list_page(count=2, page=1, mail_host=COM)
check("list page nr", page.nr, 1)
check("list page len", len(page), 2)
check("list page total", page.total_size, 4)
first_two = [m.fqdn_listname for m in page]
page = page.next
check("list page next nr", page.nr, 2)
check("list pages disjoint", set(first_two).isdisjoint(m.fqdn_listname for m in page), True)
check(
    "list page advertised",
    [m.fqdn_listname for m in client.get_list_page(count=2, page=1, advertised=True, mail_host=COM)],
    [f"test-2@{COM}", f"test-3@{COM}"],
)
check("list page by host", [m.fqdn_listname for m in client.get_list_page(mail_host=NET)], [f"test-3@{NET}"])
check(
    "domain.lists",
    sorted(m.fqdn_listname for m in example.lists),
    sorted([f"test-1@{COM}", f"test-2@{COM}", f"test-3@{COM}", f"test-announce@{COM}"]),
)
check(
    "domain.get_lists(advertised)",
    sorted(m.fqdn_listname for m in example.get_lists(advertised=True)),
    sorted([f"test-2@{COM}", f"test-3@{COM}", f"test-announce@{COM}"]),
)
check(
    "domain.get_list_page",
    [m.fqdn_listname for m in example.get_list_page(count=2, page=1, advertised=True)],
    [f"test-2@{COM}", f"test-3@{COM}"],
)
client.get_list(f"test-3@{NET}").delete()
client.delete_list(f"test-3@{COM}")
check(
    "lists after delete",
    sorted(m.fqdn_listname for m in our_lists(client.lists)),
    sorted([f"test-1@{COM}", f"test-2@{COM}", f"test-announce@{COM}"]),
)

# ------------------------------------------------------------- Membership

section("membership")
check("no members", our_members(client.members), [])
test_two = client.get_list(f"test-2@{COM}")
check("subscription_policy", test_two.settings["subscription_policy"], "confirm")
data = test_one.subscribe("unverified@example.invalid", "Unverified")
check("unverified token", data["token"] is not None, True)
check("unverified owner", data["token_owner"], "subscriber")
data = test_one.subscribe("invitee@example.invalid", "Invitee", invitation=True)
check("invitee token", data["token"] is not None, True)
check("invitee owner", data["token_owner"], "subscriber")
data = test_one.subscribe("unconfirmed@example.invalid", "Unconfirmed", pre_verified=True)
check("unconfirmed token", data["token"] is not None, True)
check("unconfirmed owner", data["token_owner"], "subscriber")
check(
    "anna",
    str(test_one.subscribe("anna@example.invalid", "Anna", pre_verified=True, pre_confirmed=True)),
    f'Member "anna@example.invalid" on "test-1.{COM}"',
)
check(
    "bill",
    str(test_one.subscribe("bill@example.invalid", "Bill", pre_verified=True, pre_confirmed=True)),
    f'Member "bill@example.invalid" on "test-1.{COM}"',
)
check(
    "anna two",
    str(test_two.subscribe("anna@example.invalid", pre_verified=True, pre_confirmed=True)),
    f'Member "anna@example.invalid" on "test-2.{COM}"',
)
check(
    "cris two",
    str(test_two.subscribe("cris@example.invalid", "Cris", pre_verified=True, pre_confirmed=True)),
    f'Member "cris@example.invalid" on "test-2.{COM}"',
)
check(
    "client.members",
    [str(m) for m in our_members(client.members)],
    [
        f'Member "anna@example.invalid" on "test-1.{COM}"',
        f'Member "bill@example.invalid" on "test-1.{COM}"',
        f'Member "anna@example.invalid" on "test-2.{COM}"',
        f'Member "cris@example.invalid" on "test-2.{COM}"',
    ],
)
check(
    "list.members",
    [str(m) for m in test_one.members],
    [f'Member "anna@example.invalid" on "test-1.{COM}"', f'Member "bill@example.invalid" on "test-1.{COM}"'],
)
check("member display names", [m.display_name for m in test_one.members], ["Anna", "Bill"])
page = client.get_member_page(count=2, page=1)
check("member page nr", page.nr, 1)
check("member page total", page.total_size >= 4, True)
page = test_one.get_member_page(count=1, page=1)
check("list member page nr", page.nr, 1)
check("list member page total", page.total_size, 2)
check("list member page 1", [str(m) for m in page], [f'Member "anna@example.invalid" on "test-1.{COM}"'])
page = page.next
check("list member page 2 nr", page.nr, 2)
check("list member page 2 total", page.total_size, 2)
check("list member page 2", [str(m) for m in page], [f'Member "bill@example.invalid" on "test-1.{COM}"'])
cris_test_two = test_two.get_member("cris@example.invalid")
check("get_member", str(cris_test_two), f'Member "cris@example.invalid" on "test-2.{COM}"')
check("member role", cris_test_two.role, "member")
check("member display_name", cris_test_two.display_name, "Cris")
check(
    "client.get_member",
    str(client.get_member(f"test-2@{COM}", "cris@example.invalid")),
    f'Member "cris@example.invalid" on "test-2.{COM}"',
)
prefs = cris_test_two.preferences
for key in (
    "delivery_mode",
    "acknowledge_posts",
    "delivery_status",
    "hide_address",
    "preferred_language",
    "receive_list_copy",
    "receive_own_postings",
):
    check(f"member pref {key} unset", prefs.get(key), None)
# listmngr registers a user only when an account is made (CLI, REST
# create_user, or web signup); a bare subscription is an address without
# a user, so the member carries no `user` link until then.
deviation("member.user before an account exists", "user" in cris_test_two.rest_data, False, "a User")
raises("unsubscribe nomember", lambda: test_two.unsubscribe("nomember@example.invalid"), ValueError)
# listmngr's REST keeps the confirmation workflow for a bare DELETE: an
# administrative removal states pre_confirmed and pre_approved.
raises("bare unsubscribe refused", lambda: test_one.unsubscribe("anna@example.invalid"), ValueError)
deviations.append("unsubscribe: Mailman removes on a bare DELETE, listmngr asks for pre_confirmed and pre_approved")
test_one.unsubscribe("anna@example.invalid", pre_confirmed=True, pre_approved=True)
check(
    "after unsubscribe",
    [str(m) for m in our_members(client.members)],
    [
        f'Member "bill@example.invalid" on "test-1.{COM}"',
        f'Member "anna@example.invalid" on "test-2.{COM}"',
        f'Member "cris@example.invalid" on "test-2.{COM}"',
    ],
)
cris_test_two.unsubscribe()
check(
    "after member.unsubscribe",
    [str(m) for m in our_members(client.members)],
    [f'Member "bill@example.invalid" on "test-1.{COM}"', f'Member "anna@example.invalid" on "test-2.{COM}"'],
)
raises("unsubscribe nomember again", lambda: test_one.unsubscribe("nomember@example.invalid"), ValueError)
for name in ("Jack", "Jill", "Hans"):
    test_one.subscribe(f"{name.lower()}@example.invalid", name, pre_verified=True, pre_confirmed=True)
outcome = test_one.mass_unsubscribe(
    ["jack@example.invalid", "hans@example.invalid", "jill@example.invalid", "bully@example.invalid"]
)
check("mass_unsubscribe answers", isinstance(outcome, dict), True)
check("after mass_unsubscribe", [str(m) for m in test_one.members], [f'Member "bill@example.invalid" on "test-1.{COM}"'])
check("nonmembers", list(test_one.nonmembers), [])

# ------------------------------------------------------ Users, addresses

section("users")
# Mailman auto-registers a user per subscriber; here the accounts are made
# explicitly and each adopts the address its subscription already created.
deviation(
    "users before accounts",
    [u.display_name for u in client.users if u.display_name in ("Anna", "Bill", "Cris")],
    [],
    "Unverified, Invitee, Unconfirmed, Anna, Bill, Cris, Jack, Jill, Hans",
)
for name in ("Unverified", "Invitee", "Unconfirmed", "Anna", "Bill", "Cris", "Jack", "Jill", "Hans"):
    client.create_user(email=f"{name.lower()}@example.invalid", password="Orbit!Cobalt7-River", display_name=name)
names = [u.display_name for u in client.users]
for expected in ("Unverified", "Invitee", "Unconfirmed", "Anna", "Bill", "Cris", "Jack", "Jill", "Hans"):
    check(f"user {expected} registered", expected in names, True)
check("member.user after an account exists", test_two.get_member("anna@example.invalid").user.display_name, "Anna")
page = client.get_user_page(count=4, page=1)
check("user page nr", page.nr, 1)
check("user page total", page.total_size >= 9, True)
check("user page len", len(page), 4)
page = page.next
check("user page next", page.nr, 2)
page = page.previous
check("user page previous", page.nr, 1)
cris = client.get_user("cris@example.invalid")
check("get_user", cris.display_name, "Cris")
addresses = list(cris.addresses)
check("user addresses", [str(a) for a in addresses], ["cris@example.invalid"])
check("address display_name", addresses[0].display_name, "Cris")
check("address registered_on", addresses[0].registered_on is not None, True)
check("add_address", str(cris.add_address("cris.person@example.invalid")), "cris.person@example.invalid")
check("get_address", str(client.get_address("cris.person@example.invalid")), "cris.person@example.invalid")
check("addresses after add", sorted(str(a) for a in cris.addresses), ["cris.person@example.invalid", "cris@example.invalid"])
# listmngr's password policy (zxcvbn) refuses the doctest's `somepass`.
http_error("weak password refused", lambda: client.create_user(email="weak@example.invalid", password="somepass"), 400)
deviations.append("create_user: Mailman accepts 'somepass', listmngr's password policy refuses it (400)")
dana = client.create_user(email="dana@example.invalid", password="Orbit!Cobalt7-River", display_name="Dana")
check("create_user", dana.display_name, "Dana")
http_error("add_address belonging to another user", lambda: cris.add_address("dana@example.invalid"), 400)
check("absorb_existing", str(cris.add_address("dana@example.invalid", absorb_existing=True)), "dana@example.invalid")
check(
    "addresses after absorb",
    sorted(str(a) for a in cris.addresses),
    ["cris.person@example.invalid", "cris@example.invalid", "dana@example.invalid"],
)
# The web account mails its preferred address (second factor, notices), so
# listmngr makes the account's first address preferred at creation.
deviation("preferred_address at creation", str(cris.preferred_address), "cris@example.invalid", "None")
address = client.get_address("cris.person@example.invalid")
address.verify()
check("verify", address.verified, True)
cris.preferred_address = "cris.person@example.invalid"
check("preferred_address set", str(cris.preferred_address), "cris.person@example.invalid")
cris.preferred_address = "cris@example.invalid"
check("preferred_address moved", str(cris.preferred_address), "cris@example.invalid")
cris.preferred_address = None
check("preferred_address cleared", cris.preferred_address, None)

section("addresses")
address = client.get_address("dana@example.invalid")
check("address", str(address), "dana@example.invalid")
check("address display_name (Dana)", address.display_name, "Dana")
check("not verified", address.verified, False)
address.verify()
check("verified", address.verified, True)
address.unverify()
check("unverified", address.verified, False)
cris.addresses.remove("dana@example.invalid")
check("addresses after remove", sorted(str(a) for a in cris.addresses), ["cris.person@example.invalid", "cris@example.invalid"])
ler = client.create_user(email="ler@suite-primus.invalid", password="Orbit!Cobalt7-River", display_name="Ler")
check("create_user Ler", ler.display_name, "Ler")
ler = client.get_user("ler@suite-primus.invalid")
deviation("user.password", ler.password, None, "$... (the stored hash)")
check("Ler display_name", ler.display_name, "Ler")
ler.display_name = "Sir Ler"
ler.save()
check("user display_name saved", client.get_user("ler@suite-primus.invalid").display_name, "Sir Ler")
ler.password = "Quartz$Meadow9-Lantern"
ler.save()
check("password saved", client.get_user("ler@suite-primus.invalid").display_name, "Sir Ler")
bill = client.get_user("bill@example.invalid")
check("subscriptions", [str(s) for s in bill.subscriptions], [f'Member "bill@example.invalid" on "test-1.{COM}"'])
check("subscription_list_ids", list(bill.subscription_list_ids), [f"test-1.{COM}"])

# ---------------------------------------------------------- List settings

section("settings")
settings = test_one.settings
check("settings iterable", len(sorted(settings)) > 30, True)
check("settings display_name", settings["display_name"], "Test-1")
check("settings fqdn_listname", settings["fqdn_listname"], f"test-1@{COM}")
check("settings description", settings["description"], "")
settings["description"] = "A very meaningful description."
settings["display_name"] = "Test Numero Uno"
settings.save()
settings_new = test_one.settings
check("description saved", settings_new["description"], "A very meaningful description.")
check("display_name saved", settings_new["display_name"], "Test Numero Uno")
check("settings.get default", settings_new.get("OhNoIForgotTheKey", "HowGoodIPlacedOneUnderTheDoormat"), "HowGoodIPlacedOneUnderTheDoormat")
# One representative key per Postorius settings group, saved and read back.
GROUPS = {
    "identity": ("subject_prefix", "[uno] "),
    "responses": ("autorespond_postings", "respond_and_continue"),
    "messages": ("reply_goes_to_list", "point_to_list"),
    "dmarc": ("dmarc_mitigate_action", "munge_from"),
    "digest": ("digest_size_threshold", 45.5),
    "acceptance": ("max_message_size", 512),
    "archiving": ("archive_policy", "private"),
    "members": ("subscription_policy", "moderate"),
    "bounces": ("bounce_score_threshold", 7.5),
}
for group, (key, value) in GROUPS.items():
    settings = test_one.settings
    settings[key] = value
    try:
        settings.save()
    except HTTPError as error:
        raise AssertionError(f"group {group}: saving {key}={value!r} answered HTTP {error.code}") from error
    check(f"group {group}: {key}", test_one.settings[key], value)
settings = test_one.settings
settings["subscription_policy"] = "confirm"
settings["archive_policy"] = "public"
settings.save()

# ------------------------------------------------------------ Preferences

section("preferences")
global_prefs = client.preferences
check("global acknowledge_posts", global_prefs["acknowledge_posts"], False)
check("global delivery_mode", global_prefs["delivery_mode"], "regular")
check("global delivery_status", global_prefs["delivery_status"], "enabled")
check("global hide_address", global_prefs["hide_address"], True)
check("global preferred_language", global_prefs["preferred_language"], "en")
check("global receive_list_copy", global_prefs["receive_list_copy"], True)
check("global receive_own_postings", global_prefs["receive_own_postings"], True)
prefs = test_two.get_member("anna@example.invalid").preferences
prefs["delivery_status"] = "by_user"
prefs.save()
check("member pref saved", test_two.get_member("anna@example.invalid").preferences["delivery_status"], "by_user")

# --------------------------------------------------- Pipelines and chains

section("pipelines")
check("pipelines", sorted(client.pipelines["pipelines"]), ["default-owner-pipeline", "default-posting-pipeline", "virgin"])
check(
    "chains",
    sorted(client.chains["chains"]),
    ["accept", "default-owner-chain", "default-posting-chain", "discard", "dmarc-mitigation", "header-match", "hold", "moderation", "reject"],
)

# --------------------------------------------------- Owners and moderators

section("owners")
check("no owners", list(test_one.owners), [])
check("no moderators", list(test_one.moderators), [])
test_one.add_owner("foo@example.invalid", display_name="Foo")
check("owners", [o.email for o in test_one.owners], ["foo@example.invalid"])
check("members exclude owner", [str(m) for m in test_one.members], [f'Member "bill@example.invalid" on "test-1.{COM}"'])
test_one.add_moderator("bar@example.invalid", display_name="Bar")
check("moderators", [m.email for m in test_one.moderators], ["bar@example.invalid"])
check("members exclude moderator", [str(m) for m in test_one.members], [f'Member "bill@example.invalid" on "test-1.{COM}"'])
check(
    "moderator subscribes",
    str(test_one.subscribe("bar@example.invalid", "Bar", pre_verified=True, pre_confirmed=True)),
    f'Member "bar@example.invalid" on "test-1.{COM}"',
)
test_four_net = example_net.create_list("test-4")
test_four_net.add_owner("foo@example.invalid", display_name="Foo")
roles = sorted(f"{m}: {m.role}" for m in our_members(client.members))
check(
    "members with roles",
    roles,
    sorted(
        [
            f'Member "bill@example.invalid" on "test-1.{COM}": member',
            f'Member "foo@example.invalid" on "test-1.{COM}": owner',
            f'Member "bar@example.invalid" on "test-1.{COM}": moderator',
            f'Member "bar@example.invalid" on "test-1.{COM}": member',
            f'Member "anna@example.invalid" on "test-2.{COM}": member',
            f'Member "foo@example.invalid" on "test-4.{NET}": owner',
        ]
    ),
)
check("find_lists member", [m.fqdn_listname for m in client.find_lists("bill@example.invalid", "member")], [f"test-1@{COM}"])
check("find_lists moderator", [m.fqdn_listname for m in client.find_lists("bar@example.invalid", "moderator")], [f"test-1@{COM}"])
check(
    "find_lists owner",
    sorted(m.fqdn_listname for m in client.find_lists("foo@example.invalid", "owner")),
    [f"test-1@{COM}", f"test-4@{NET}"],
)
check(
    "find_lists owner by host",
    [m.fqdn_listname for m in client.find_lists("foo@example.invalid", "owner", mail_host=NET)],
    [f"test-4@{NET}"],
)
test_one.remove_owner("foo@example.invalid")
check("owner removed", list(test_one.owners), [])

# ------------------------------------------------- Subscription moderation

section("requests")
confirm_first = example_dot_com.create_list("confirm-first")
settings = confirm_first.settings
settings["subscription_policy"] = "confirm_then_moderate"
settings.save()
confirm_first = client.get_list(f"confirm-first.{COM}")
check("policy confirm_then_moderate", confirm_first.settings["subscription_policy"], "confirm_then_moderate")
check("no requests", list(confirm_first.requests), [])
data = confirm_first.subscribe("groucho@example.invalid", pre_verified=True, pre_confirmed=True)
check("groucho token_owner", data["token_owner"], "moderator")
check("requests count", confirm_first.get_requests_count(), 1)
request_1 = confirm_first.requests[0]
check("request email", request_1["email"], "groucho@example.invalid")
check("request token", request_1["token"] is not None, True)
check("request token_owner", request_1["token_owner"], "moderator")
check("request date", request_1["request_date"] is not None, True)
check("request list_id", request_1["list_id"], f"confirm-first.{COM}")
confirm_first.subscribe("harpo@example.invalid", pre_verified=True, pre_confirmed=True)
confirm_first.subscribe("zeppo@example.invalid", pre_verified=True, pre_confirmed=False)
check("requests count 3", confirm_first.get_requests_count(), 3)
check("moderator requests", confirm_first.get_requests_count(token_owner="moderator"), 2)
check("subscriber requests", confirm_first.get_requests_count(token_owner="subscriber"), 1)
check("get_requests subscriber", confirm_first.get_requests(token_owner="subscriber")[0]["email"], "zeppo@example.invalid")
confirm_first.moderate_request(request_1["token"], "accept")
check("after accept", confirm_first.get_requests_count(), 2)
request_2, request_3 = confirm_first.requests[0], confirm_first.requests[1]
check("remaining requests", sorted([request_2["email"], request_3["email"]]), ["harpo@example.invalid", "zeppo@example.invalid"])
confirm_first.moderate_request(request_2["token"], "reject")
check("after reject", confirm_first.get_requests_count(), 1)
confirm_first.moderate_request(request_3["token"], "discard")
check("after discard", confirm_first.get_requests_count(), 0)
check("groucho member", [str(m) for m in confirm_first.members], [f'Member "groucho@example.invalid" on "confirm-first.{COM}"'])

# ------------------------------------------------------ Message moderation

section("held")
msg = f"""From: nomember@example.invalid
To: test-1@{COM}
Subject: Something
Message-ID: <moderated_01@example.invalid>

Some text.

"""
inq = client.queues["in"]
inq.inject(f"test-1.{COM}", msg)
deadline = time.monotonic() + 30
while test_one.get_held_count() == 0:
    if time.monotonic() > deadline:
        raise AssertionError("held: the injected post never reached the held queue")
    time.sleep(0.1)
all_held = test_one.held
check("one held", len(all_held), 1)
heldmsg = test_one.get_held_message(all_held[0].request_id)
check("held subject", heldmsg.subject, "Something")
check("held reason", heldmsg.reason, "The message is not from a list member")
check("held sender", heldmsg.sender, "nomember@example.invalid")
check("held msg", "Message-ID: <moderated_01@example.invalid>" in heldmsg.msg, True)
check("defer", test_one.defer_message(heldmsg.request_id).status_code, 204)
check("still held", len(test_one.held), 1)
check("discard", heldmsg.discard().status_code, 204)
check("held gone", len(test_one.held), 0)
bill_member = test_one.get_member("bill@example.invalid")
check("moderation_action default", bill_member.moderation_action, None)
bill_member.moderation_action = "hold"
bill_member.save()
check("moderation_action saved", test_one.get_member("bill@example.invalid").moderation_action, "hold")

# ------------------------------------------------------------------ Bans

section("bans")
check("no site bans", [b.email for b in client.bans if b.email.startswith(("anna", "bill"))], [])
banned_anna = client.bans.add("anna@example.invalid")
check("site ban", str(banned_anna), "anna@example.invalid")
check("anna in bans", "anna@example.invalid" in client.bans, True)
check("site ban bill", str(client.bans.add("bill@example.invalid")), "bill@example.invalid")
check("site bans", sorted(b.email for b in client.bans if b.email.endswith("@example.invalid")), ["anna@example.invalid", "bill@example.invalid"])
check("site bans page 1", len(list(client.get_bans_page(count=1, page=1))), 1)
check("site bans page 2", len(list(client.get_bans_page(count=1, page=2))), 1)
banned_anna.delete()
check("anna unbanned", "anna@example.invalid" in client.bans, False)
client.bans.remove("bill@example.invalid")
check("bill unbanned", "bill@example.invalid" in client.bans, False)
check("no list bans", list(test_one.bans), [])
banned_anna = test_one.bans.add("anna@example.invalid")
check("anna list-banned", "anna@example.invalid" in test_one.bans, True)
check("list ban bill", str(test_one.bans.add("bill@example.invalid")), "bill@example.invalid")
check("list bans", sorted(b.email for b in test_one.bans), ["anna@example.invalid", "bill@example.invalid"])
check("list bans page 1", [b.email for b in test_one.get_bans_page(count=1, page=1)], ["anna@example.invalid"])
check("list bans page 2", [b.email for b in test_one.get_bans_page(count=1, page=2)], ["bill@example.invalid"])
banned_anna.delete()
check("anna list-unbanned", "anna@example.invalid" in test_one.bans, False)
test_one.bans.remove("bill@example.invalid")
check("list bans empty", list(test_one.bans), [])

# ------------------------------------------------------------- Archivers

section("archivers")
archivers = test_one.archivers
# Mailman core alone knows three archivers; listmngr also carries `hyperkitty`
# (P6-ARCHIVER-HYPERKITTY), which Mailman lists only with the plugin installed.
deviation(
    "archivers", sorted(archivers.keys()),
    ["hyperkitty", "mail-archive", "mhonarc", "prototype"],
    "['mail-archive', 'mhonarc', 'prototype']",
)
deviation("archivers default", (archivers["mail-archive"], archivers["mhonarc"]), (False, False), "(True, True)")
archivers["mail-archive"] = True
archivers["mhonarc"] = True
archivers = test_one.archivers
check("archivers set", (archivers["mail-archive"], archivers["mhonarc"]), (True, True))
archivers["mail-archive"] = False
archivers["mhonarc"] = False
archivers = test_one.archivers
check("archivers cleared", (archivers["mail-archive"], archivers["mhonarc"]), (False, False))

# --------------------------------------------------------- Header matches

section("header_matches")
header_matches = test_one.header_matches
check("no header matches", len(header_matches), 0)
added = header_matches.add("Subject", "^test: ", "discard", "sometag")
check("header match added", (added.header, added.pattern, added.action, added.tag), ("subject", "^test: ", "discard", "sometag"))
check("one header match", len(header_matches), 1)
check("find by tag", [hm.header for hm in header_matches.find(tag="sometag")], ["subject"])
del header_matches[0]
check("header match deleted", len(header_matches), 0)
header_matches.add("Subject", "^test: ", "discard")
header_matches[0].delete()
check("header matches re-read", len(test_one.header_matches), 0)

# ---------------------------------------------------------- Configuration

section("configuration")
cfg = client.configuration
check("configuration sections", "mailman" in cfg, True)
check("configuration mailman keys", len(list(cfg["mailman"])) > 0, True)

# -------------------------------------------------------- Templates, uris

section("templates")
check("no list templates", [t.name for t in test_one.templates], [])
test_one.set_template("list:user:notice:welcome", "https://templates.example.invalid/welcome.txt")
templates = list(test_one.templates)
check("list template set", [(t.name, t.uri) for t in templates], [("list:user:notice:welcome", "https://templates.example.invalid/welcome.txt")])
check("templates page", len(list(client.get_templates_page(count=25, page=1))) >= 0, True)
templates[0].delete()
check("list template deleted", [t.name for t in test_one.templates], [])
example.set_template("domain:admin:notice:new-list", "https://templates.example.invalid/new-list.txt")
check("domain template set", [t.name for t in example.templates], ["domain:admin:notice:new-list"])
list(example.templates)[0].delete()
check("domain template deleted", [t.name for t in example.templates], [])

# ------------------------------------------------------- Digest counters

section("digest")
digest = client._connection.call(f"lists/test-1.{COM}/digest")[1]
check("digest volume", digest["volume"], 1)
check("digest next number", digest["next_digest_number"], 1)
client._connection.call(f"lists/test-1.{COM}/digest", {"bump": True}, "POST")
digest = client._connection.call(f"lists/test-1.{COM}/digest")[1]
check("digest bumped", (digest["volume"], digest["next_digest_number"]), (2, 1))
check("digest send", client._connection.call(f"lists/test-1.{COM}/digest", {"send": True}, "POST")[0].status_code, 202)

# --------------------------------------------------------------- Cleanup

section("cleanup")
for mlist in our_lists(client.lists):
    mlist.delete()
check("lists gone before the domains", [m.fqdn_listname for m in our_lists(client.lists)], [])
for domain in ours(client.domains):
    try:
        domain.delete()
    except HTTPError as error:
        raise AssertionError(f"deleting {domain.mail_host}: HTTP {error.code}; lists {[m.fqdn_listname for m in domain.lists]}") from error
# Only the accounts this suite made; the gate's own token owner also lives
# under example.invalid and must survive.
SUITE_USERS = {f"{name.lower()}@example.invalid" for name in ("Unverified", "Invitee", "Unconfirmed", "Anna", "Bill", "Cris", "Jack", "Jill", "Hans")}
SUITE_USERS |= {"dana@example.invalid", "ler@suite-primus.invalid"}
for email in sorted(SUITE_USERS):
    try:
        client.get_user(email).delete()
    except HTTPError as error:
        if error.code != 404:
            raise
check("lists gone", our_lists(client.lists), [])
check("domains gone", ours(client.domains), [])

print(
    f"mailmanclient {actual_version} suite PASS: {len(sections)} sections, {checks} checks, "
    f"{len(deviations)} documented deviations"
)
for line in deviations:
    print(f"  deviation: {line}")
