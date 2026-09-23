#!/usr/bin/env python3
"""Import GNU Mailman 3's translations into listmngr's shipped catalogs.

Mailman ships each notice body per language as
`mailman/templates/<lang>/<name>.txt` and each subject line it generates in
code as a `msgid` of `mailman/messages/<lang>/LC_MESSAGES/mailman.po`. This
tool copies the ones listmngr can use as they are:

* a body only for a template whose listmngr English text *is* Mailman's
  English text (up to line wrapping; `IMPORTED` below, checked again by
  `crates/mail/tests/mailman_catalog.rs` against the vendored English), and
  only when the translation is not empty, differs from the English and uses
  no placeholder the English does not;
* a subject only for a Fluent key whose English value is Mailman's `msgid`
  once its placeholders are renamed (`SUBJECTS` below), from a translated,
  non-fuzzy `msgstr` with the same placeholders.

Everything else keeps listmngr's own English (or Vietnamese) text, because
listmngr reworded it: no member passwords, confirmation over HTTP, nothing
attached that is not attached.

The translations are Mailman's, GPL-3.0-or-later, by the Mailman translators
(see each `.po` header and https://hosted.weblate.org/projects/gnu-mailman/);
listmngr is AGPL-3.0-or-later, which GPLv3 section 13 lets them be combined
with. `crates/mail/catalog/mailman/SOURCE` records the release.

Run in a disposable environment:

    uv venv mm --python 3.12 && uv pip install --python mm/bin/python mailman==3.3.10
    mm/bin/python tests/compat/import_mailman_templates.py

Rewrites `crates/mail/catalog/mailman/`, `crates/mail/src/templates_mailman.rs`,
`crates/i18n/locales/mailman/` and `crates/i18n/src/mailman.rs`.
"""
import re
import shutil
import subprocess
import sys
from importlib.metadata import version
from pathlib import Path

import mailman

ROOT = Path(__file__).resolve().parents[2]
MAILMAN = Path(mailman.__file__).resolve().parent
BODIES = ROOT / "crates/mail/catalog/mailman"
BODIES_RS = ROOT / "crates/mail/src/templates_mailman.rs"
SUBJECTS_DIR = ROOT / "crates/i18n/locales/mailman"
SUBJECTS_RS = ROOT / "crates/i18n/src/mailman.rs"

# Templates whose listmngr English is Mailman's English up to wrapping
# (`list:admin:notice:removal` says "removed" where Mailman says
# "unsubscribed", the same event).
IMPORTED = [
    "domain:admin:notice:new-list",
    "list:admin:action:post",
    "list:admin:action:subscribe",
    "list:admin:action:unsubscribe",
    "list:admin:notice:pending",
    "list:admin:notice:removal",
    "list:admin:notice:subscribe",
    "list:admin:notice:unrecognized",
    "list:admin:notice:unsubscribe",
    "list:member:digest:header",
    "list:member:digest:masthead",
    "list:member:generic:footer",
    "list:member:regular:header",
    "list:user:notice:hold",
    "list:user:notice:no-more-today",
    "list:user:notice:post",
    "list:user:notice:refuse",
]

# Fluent key -> (Mailman msgid, Mailman placeholder -> Fluent variable).
# A positional `{}` is named by its place in the list of `{}` names.
SUBJECTS = {
    "notice-welcome-subject": (
        'Welcome to the "${mlist.display_name}" mailing list${digmode}',
        {"mlist.display_name": "display_name", "digmode": None},
    ),
    "notice-goodbye-subject": (
        "You have been unsubscribed from the ${mlist.display_name} mailing list",
        {"mlist.display_name": "display_name"},
    ),
    "notice-autoresponse-subject": (
        'Auto-response for your message to the "${display_name}" mailing list',
        {"display_name": "display_name"},
    ),
    "notice-rejected-subject": (
        'Request to mailing list "${display_name}" rejected',
        {"display_name": "display_name"},
    ),
    "notice-hold-subject": (
        "Your message to ${mlist.fqdn_listname} awaits moderator approval",
        {"mlist.fqdn_listname": "listname"},
    ),
    "notice-admin-post-subject": (
        "${mlist.fqdn_listname} post from ${msg.sender} requires approval",
        {"mlist.fqdn_listname": "listname", "msg.sender": "sender"},
    ),
    "notice-bounce-disable-subject": (
        "${member}'s subscription disabled on ${mlist.display_name}",
        {"member": "member", "mlist.display_name": "listname"},
    ),
    "notice-bounce-increment-subject": (
        "${member}'s bounce score incremented on ${mlist.display_name}",
        {"member": "member", "mlist.display_name": "listname"},
    ),
    "notice-bounce-removal-subject": (
        "${member} unsubscribed from ${mlist.display_name} mailing list due to bounces",
        {"member": "member", "mlist.display_name": "listname"},
    ),
    "notice-warning-subject": (
        "Your subscription for ${mlist.display_name} mailing list has been disabled",
        {"mlist.display_name": "listname"},
    ),
    "notice-pending-subject": (
        "The {} list has {} moderation requests waiting.",
        {"{0}": "listname", "{1}": "count"},
    ),
    "notice-admin-subscribe-subject": (
        "${mlist.display_name} subscription notification",
        {"mlist.display_name": "display_name"},
    ),
    "notice-admin-unsubscribe-subject": (
        "${mlist.display_name} unsubscription notification",
        {"mlist.display_name": "display_name"},
    ),
    "notice-post-ack-subject": (
        "${display_name} post acknowledgment",
        {"display_name": "display_name"},
    ),
    "notice-content-filter-subject": ("Content filter message notification", {}),
    "notice-forward-subject": ("Forward of moderated message", {}),
    "notice-no-subject": ("(no subject)", {}),
}

# Each language's own name, as a language picker shows it.
ENDONYMS = {
    "ar": "العربية", "bg": "Български", "bn": "বাংলা", "ca": "Català",
    "cs": "Čeština", "de": "Deutsch", "el": "Ελληνικά", "eo": "Esperanto",
    "es": "Español", "fa": "فارسی", "fi": "Suomi", "fr": "Français",
    "he": "עברית", "hu": "Magyar", "id": "Bahasa Indonesia", "it": "Italiano",
    "ja": "日本語", "ko": "한국어", "nb-NO": "Norsk bokmål", "nl": "Nederlands",
    "oc": "Occitan", "pl": "Polski", "pt": "Português", "pt-BR": "Português (Brasil)",
    "ru": "Русский", "si": "සිංහල", "sk": "Slovenčina", "sq": "Shqip",
    "sv": "Svenska", "tr": "Türkçe", "uk": "Українська",
    "zh-Hans": "简体中文", "zh-Hant": "繁體中文",
}

PLACEHOLDER = re.compile(r"\$\{([A-Za-z_][A-Za-z0-9_.]*)\}|\$([A-Za-z_][A-Za-z0-9_]*)|\{\}")


def tag(mailman_code):
    """Mailman's `pt_BR`/`zh_Hans` as the BCP 47 tag this site stores."""
    return mailman_code.replace("_", "-")


def names(text):
    return {m.group(1) or m.group(2) for m in PLACEHOLDER.finditer(text) if m.group(0) != "{}"}


def read_po(path):
    """`{msgid: msgstr}` for every translated, non-fuzzy singular entry."""
    entries, fuzzy, field, current = {}, False, None, {}

    def flush():
        nonlocal current, fuzzy
        msgid, msgstr = current.get("msgid"), current.get("msgstr")
        if msgid and msgstr and not fuzzy and "msgid_plural" not in current:
            entries[msgid] = msgstr
        current, fuzzy = {}, False

    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line:
            flush()
            field = None
        elif line.startswith("#,"):
            fuzzy = fuzzy or "fuzzy" in line
        elif line.startswith("#"):
            continue
        elif line.startswith('"'):
            if field:
                current[field] += unquote(line)
        else:
            key, _, rest = line.partition(" ")
            if key.startswith("msgid") and "msgid" in current and key == "msgid":
                flush()
            field = key
            current[field] = unquote(rest)
    flush()
    return entries


def unquote(text):
    text = text.strip()
    assert text.startswith('"') and text.endswith('"'), text
    return re.sub(r'\\(["\\nt])', lambda m: {"n": "\n", "t": "\t"}.get(m.group(1), m.group(1)), text[1:-1])


def fluent_english(msgid, renames):
    """Mailman's msgid in Fluent's spelling, or `None` if a name is unmapped."""
    position = iter(range(100))

    def replace(match):
        if match.group(0) == "{}":
            name = f"{{{next(position)}}}"
        else:
            name = match.group(1) or match.group(2)
        if name not in renames:
            raise KeyError(name)
        target = renames[name]
        return "" if target is None else f"{{ ${target} }}"

    try:
        return PLACEHOLDER.sub(replace, msgid)
    except KeyError:
        return None


def english_ftl():
    values = {}
    for line in (ROOT / "crates/i18n/locales/en.ftl").read_text(encoding="utf-8").splitlines():
        key, sep, value = line.partition(" = ")
        if sep and re.fullmatch(r"[a-z][a-z0-9-]*", key):
            values[key] = value
    return values


def bodies(languages):
    shutil.rmtree(BODIES, ignore_errors=True)
    english = MAILMAN / "templates/en"
    (BODIES / "en").mkdir(parents=True)
    for name in IMPORTED:
        shutil.copyfile(english / f"{name}.txt", BODIES / "en" / f"{name}.txt")
    table, skipped = {}, []
    for code in languages:
        for name in IMPORTED:
            source = MAILMAN / "templates" / code / f"{name}.txt"
            if not source.is_file():
                continue
            text = source.read_text(encoding="utf-8")
            reference = (english / f"{name}.txt").read_text(encoding="utf-8")
            if not text.strip() or text == reference:
                continue
            if not names(text) <= names(reference):
                skipped.append(f"{code}/{name}: {sorted(names(text) - names(reference))}")
                continue
            if len(text.encode()) > 65_536:
                skipped.append(f"{code}/{name}: too large")
                continue
            target = BODIES / tag(code) / f"{name}.txt"
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(text, encoding="utf-8")
            table.setdefault(tag(code), []).append(name)
    return table, skipped


def subjects(languages):
    shutil.rmtree(SUBJECTS_DIR, ignore_errors=True)
    SUBJECTS_DIR.mkdir(parents=True)
    english = english_ftl()
    usable = {}
    for key, (msgid, renames) in SUBJECTS.items():
        converted = fluent_english(msgid, renames)
        if converted is None or english.get(key) != converted:
            sys.exit(f"{key}: Mailman's {msgid!r} is not {english.get(key)!r}")
        usable[key] = (msgid, renames)
    table, skipped = {}, []
    for code in languages:
        po = MAILMAN / "messages" / code / "LC_MESSAGES/mailman.po"
        if not po.is_file():
            continue
        entries = read_po(po)
        lines = []
        for key, (msgid, renames) in usable.items():
            msgstr = entries.get(msgid)
            if not msgstr or msgstr == msgid:
                continue
            wanted = names(msgid)
            if names(msgstr) != wanted or msgstr.count("{}") != msgid.count("{}"):
                skipped.append(f"{code}/{key}: placeholders differ")
                continue
            value = fluent_english(msgstr, renames)
            stripped = PLACEHOLDER.sub("", msgstr)
            if value is None or "\n" in value or re.search(r"[{}]", stripped) or value[:1] in "[*.":
                skipped.append(f"{code}/{key}: not a one-line Fluent value")
                continue
            lines.append(f"{key} = {value.strip()}")
        if lines:
            header = (
                f"# {ENDONYMS[tag(code)]}: notice subjects translated for GNU Mailman "
                f"{version('mailman')}\n# (GPL-3.0-or-later), imported by "
                "tests/compat/import_mailman_templates.py. Do not edit.\n"
            )
            (SUBJECTS_DIR / f"{tag(code)}.ftl").write_text(header + "\n".join(lines) + "\n", encoding="utf-8")
            table[tag(code)] = len(lines)
    return table, skipped


GENERATED = "// Generated by tests/compat/import_mailman_templates.py from GNU Mailman\n// {release} (GPL-3.0-or-later translations). Do not edit.\n"


def write_bodies_rs(table, release):
    languages = sorted(table)
    out = [GENERATED.format(release=release)]
    out.append("//! Mailman's translated bodies for the templates whose English listmngr\n")
    out.append("//! keeps as Mailman wrote it; see `IMPORTED`.\n\n")
    out.append("/// The templates a Mailman translation may serve.\n")
    out.append("pub const IMPORTED: &[&str] = &[\n")
    out += [f'    "{name}",\n' for name in IMPORTED]
    out.append("];\n\n/// The languages with at least one translated body.\n")
    out.append("pub const LANGUAGES: &[&str] = &[\n")
    out += [f'    "{language}",\n' for language in languages]
    out.append("];\n\n/// Mailman's English for each imported template, as vendored.\n")
    out.append("#[must_use]\n#[allow(clippy::match_same_arms)] // One arm per vendored file.\n")
    out.append("pub fn english(name: &str) -> Option<&'static str> {\n    Some(match name {\n")
    for name in IMPORTED:
        out.append(f'        "{name}" => include_str!("../catalog/mailman/en/{name}.txt"),\n')
    out.append("        _ => return None,\n    })\n}\n\n")
    out.append("/// Mailman's translation of `name` into `language`, when it has one.\n")
    out.append("#[must_use]\n// One arm per vendored file; some languages share a translation.\n")
    out.append("#[allow(clippy::too_many_lines, clippy::match_same_arms)]\n")
    out.append("pub fn builtin(language: &str, name: &str) -> Option<&'static str> {\n")
    out.append("    Some(match (language, name) {\n")
    for language in languages:
        for name in sorted(table[language]):
            out.append(
                f'        ("{language}", "{name}") => {{\n'
                f'            include_str!("../catalog/mailman/{language}/{name}.txt")\n        }}\n'
            )
    out.append("        _ => return None,\n    })\n}\n")
    BODIES_RS.write_text("".join(out), encoding="utf-8")


def write_subjects_rs(table, release, body_languages):
    languages = sorted(set(table) | set(body_languages))
    out = [GENERATED.format(release=release)]
    out.append("//! The languages Mailman's translators brought, for notices only.\n\n")
    out.append("/// Notice languages beyond the interface's, with their own names.\n")
    out.append("pub const LANGUAGES: &[(&str, &str)] = &[\n")
    out += [f'    ("{language}", "{ENDONYMS[language]}"),\n' for language in languages]
    out.append("];\n\n/// Every notice language as a picker option, the interface's first:\n")
    out.append("/// the code and the Fluent id of its name.\n")
    out.append("pub const OPTIONS: &[(&str, &str)] = &[\n")
    out += [f'    ("{language}", "web-language-{language}"),\n' for language in ["en", "vi"] + languages]
    out.append("];\n\n/// Their names, the same in every catalog.\n")
    out.append('pub const NAMES: &str = include_str!("../locales/mailman/languages.ftl");\n\n')
    out.append("/// Each language's translated subjects, as Fluent.\n")
    out.append("pub const CATALOGS: &[(&str, &str)] = &[\n")
    for language in sorted(table):
        out.append(f'    ("{language}", include_str!("../locales/mailman/{language}.ftl")),\n')
    out.append("];\n")
    SUBJECTS_RS.write_text("".join(out), encoding="utf-8")
    (SUBJECTS_DIR / "languages.ftl").write_text(
        "# Language names are endonyms in every catalog, as a language picker shows\n"
        "# them. Generated by tests/compat/import_mailman_templates.py. Do not edit.\n"
        + "".join(f"web-language-{language} = {ENDONYMS[language]}\n" for language in languages),
        encoding="utf-8",
    )


def main():
    release = version("mailman")
    languages = sorted(
        path.name for path in (MAILMAN / "templates").iterdir()
        if path.is_dir() and path.name not in ("en", "__pycache__")
    )
    for code in languages:
        assert tag(code) in ENDONYMS, code
    body_table, body_skipped = bodies(languages)
    subject_table, subject_skipped = subjects(languages)
    (BODIES / "SOURCE").write_text(
        f"GNU Mailman {release}, mailman/templates/<lang>/*.txt and\n"
        "mailman/messages/<lang>/LC_MESSAGES/mailman.po.\n"
        "Copyright (C) the GNU Mailman developers and translators.\n"
        "GPL-3.0-or-later; combined with listmngr (AGPL-3.0-or-later) under\n"
        "section 13 of the GNU GPL version 3. Imported unchanged by\n"
        "tests/compat/import_mailman_templates.py.\n",
        encoding="utf-8",
    )
    write_bodies_rs(body_table, release)
    write_subjects_rs(subject_table, release, body_table)
    subprocess.run(["rustfmt", "--edition", "2024", str(BODIES_RS), str(SUBJECTS_RS)], check=True)
    for line in body_skipped + subject_skipped:
        print(f"skipped {line}", file=sys.stderr)
    print(
        f"{len(body_table)} languages with {sum(map(len, body_table.values()))} bodies; "
        f"{len(subject_table)} with {sum(subject_table.values())} subjects",
        file=sys.stderr,
    )


if __name__ == "__main__":
    main()
