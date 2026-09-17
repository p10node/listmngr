# Feature parity and acceptance evidence

## System page and audit log — bounded local acceptance verified

| Acceptance ID | Behavior | Evidence/status |
| --- | --- | --- |
| P4-SYSTEM | `/web/admin/system` for server owners (`privileged`, `server_owner_tx`; linked from `/web/admin` with the other site-wide pages through `site_admin_routes`): versions (the crate version, REST API `3.1`, `Config::database_backend`); every runner queue (`Queue::ALL`) with its ready/leased/done/shunted counts from `MailQueueRepo::stats` — the query `/metrics` and `queue stats` use — the oldest ready job's wait past its due time, the shunted total, and the runners holding leases (`locked_by` of leased jobs, counted; `browser_runner_status` in `crates/db/src/web_system.rs`); the MTA map status from the `MapWriter` the site built at start (`mta.incoming`, the map directory, the LMTP transport, the generation the `current` link names with its modification time, or "no generation published yet"; a note when `mta.incoming = none`); and the configuration from `Config::redacted_json` flattened to dotted keys with arrays as one JSON value and every value bounded to 512 characters — the same redaction the CLI's `conf` and REST `/system/configuration` use, so the database URL shows `[REDACTED]` and an SMTP password never reaches a page. `/web/admin/system/audit`: audit events newest first, twenty per page (`browser_audit`: `LIKE` on an escaped action prefix and target substring, `valid_offset`), each with the time, the actor (the user id, `token <id>`, or "system"), the peer address, the action, `target_type target_id` and the diff bounded to 512 characters; `?action=` (100 characters at most) and `?target=` (320) kept on page links (`Pagination::filtered`); an unknown parameter, an oversized filter or a page past the limit is 400. Strings `web-system-*`, `web-audit-*`, `web-title-system`, `web-title-audit` in `en` and `vi`. | **Bounded local acceptance verified.** RED first: `crates/api/tests/webui/system.rs` run against `main`'s sources (the branch's `crates/*/src`, templates, catalogs and `Cargo.lock` stashed) failed at its first door — the system page answered 404 rather than 401 — before the work (`cargo test --locked -p listmngr-api --test webui system::system_page_and_audit_log`). The matrix, with `mta.incoming = "postfix"`, a temporary map directory and an SMTP password in the configuration: both pages 401 anonymous and 403 to a plain reader, the link on the administration index; the page naming the crate version, `sqlite`, `[REDACTED]`, never `sqlite::memory:` nor the password, `mta.incoming` as `postfix`, "No job is waiting", "No runner holds a lease", "No generation published yet" and zero for the `in` queue; after one job is enqueued thirty seconds overdue, one ready in `in` and "has waited"; after `claim` by `in-runner-1`, zero ready and one leased with the runner named holding one. The audit viewer after a `user.update` with `Root <b>`: `list.create` and `user.update` rows, the diff escaped, "system" as the fixture actor, the newer event first; `action=list.` only list events naming `public.example.com`, with `target=private` only the private list, `action=nothing.` "No audit events match", a 101-character action, `page=10001` and an unknown parameter 400, `action=user.` naming the actor's id. `postgres_system_contract` runs the matrix on an isolated schema and is wired into `scripts/test-postgres.sh`. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` PASS; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 150 binaries, 985 passed, 0 failed, 73 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS (55 tests) and `scripts/test-postgres-all.sh` 70 passed, 0 failed on one disposable PostgreSQL 14.24 role and database, dropped afterwards (run before the final change, which touched only five catalog strings); Chromium 140.0.7339.16 acceptance PASS: the server-owner fixture opens System from the administration index, sees `[REDACTED]`, the note that no incoming MTA is configured and the queue table, opens the audit log, filters by `list.create` and sees the list it created and no `member.create` row; axe-core 4.13.0 finds no critical or serious violation on 17 pages (the system page and the audit log scanned); zero console errors. `scripts/test-mailmanclient.py` not run: no REST surface changed. **Limits and deviations:** the page is a snapshot with no auto-refresh; runner status is inferred from leased jobs, so an idle runner is invisible and no heartbeat table exists; no job actions (unshunt, retry) in the browser — the CLI and REST `/queues` keep them; the audit viewer shows actor ids rather than names and has no export; the configuration view is the whole redacted tree with no per-section page. Not Phase 4 completion. |

## Domains and accounts — bounded local acceptance verified

| Acceptance ID | Behavior | Evidence/status |
| --- | --- | --- |
| P4-DOMAINS-USERS | The server owner's site-wide pages, linked from `/web/admin` for server owners only (`browser_is_server_owner`; `server_owner_tx` from `web_list_settings.rs` is the one authority check). `/web/admin/domains` (`crates/db/src/web_domains.rs`, `crates/api/src/webui_domains.rs`): every domain with description, alias, owners (first address each) and list count, and an add form (mail host, description, optional alias) whose refusals — an invalid host or alias through `normalize_domain`, a taken host (`Conflict`) — come back inline on `mail_host` with the other values kept; success redirects to the new domain with `?saved=added`. `/web/admin/domains/{host}`: facts, the owners with a remove form each, an add-owner form by address (`UserRepo::get_by_email`; no account or a bad address refused inline on `email`, an existing owner "already owns"), the template catalogue of every non-`site:` name with the languages the domain overrides, the DKIM records from `[[mta.dkim_signing]]` entries for the host (`SigningKeys::dns_record`, new: the same file checks as `load`, PKCS#8 or PKCS#1 DER through `rsa`, the SPKI base64 as `v=DKIM1; k=rsa; p=…`; an unreadable key shows a note, never bytes), and a deletion form that needs the host typed back (mismatch inline) and an empty domain (`DomainRepo::delete_with_context`'s `Conflict` inline). The domain template editor reuses `TemplateEditor` with `browser_domain_template` (stored override plus `templates::resolve_domain_tx`, new: domain then site then built-in), `browser_domain_template_set`/`_delete` through `TemplateRepo::set_body_tx`/`delete_tx` in the browser write transaction. Owner changes use `DomainRepo::add_owner_with_context` and the new `remove_owner_with_context` (`domain.owner.remove`), after the authority check commits. `/web/admin/users` (`web_users.rs`, `webui_users.rs`): a search (`q`, 200 characters at most) over the display name and every address with paging that keeps the search; `/web/admin/users/{id}`: facts, a form saving the display name and server-owner flag in one audited transaction (`browser_user_update`, `user.update` with `source: browser-admin`; an empty name refused inline; demoting the last server owner with a verified address refused inline), the addresses with a verify/unverify form each (`browser_user_address_verify`: the address must be the account's, `address.verify`/`address.unverify` audited), and the memberships with a link to each member's options page. Strings `web-domains-*`, `web-domain-*`, `web-users-*`, `web-title-domains`, `web-title-users` in `en` and `vi`. | **Bounded local acceptance verified.** RED first: `crates/api/tests/webui/domains_users.rs` run against `main`'s sources (the branch's `crates/*/src`, templates, catalogs and `Cargo.lock` stashed) failed at its first assertion — no domains link on the administration index — before the work (`cargo test --locked -p listmngr-api --test webui domains_users::domains_and_accounts_administration`). The matrix, with a server owner, a plain member and a third account, and a DKIM key generated by `openssl genpkey` into a 0600 temporary file configured for `example.com`: the index links for the server owner only, both pages 401 anonymous and 403 to the plain member, a domain page 403 to them; the index naming `example.com` with two lists and the add form; `not a host!`, the taken `example.com` and a bad alias each a 400 with the refusal on `mail_host` and the description kept, no `domain.create` row; `Lists.Example.ORG` with an alias added (303 to the lowercased host with `saved=added`, one `domain.create` row), its page showing the notice, the escaped description, the alias, "No owners yet" and "No DKIM signing key"; an unknown host 404; `example.com`'s page showing `sel._domainkey.example.com` and `v=DKIM1; k=rsa; p=` and never `PRIVATE KEY`. Owners: an address without an account refused inline on `email` with the value kept; `Downer@Example.com` seated (`domain.owner.add` +1) and named on the domain page and the index; seating again "already owns"; removal (`domain.owner.remove` +1), a second removal 404, a form without CSRF 403. Templates: the welcome editor showing `builtin:en`; a preview rendering `$domain` and writing nothing; a save, the editor then showing the body and `domain:en`, `TemplateRepo::resolve` for a list on the domain answering the domain body with source `domain:en`, the catalogue naming `en`, an unknown name 404, removal restoring `builtin:en`. Deletion: a mistyped host refused inline, a domain with a list refused with "still carries lists", the empty domain deleted with the host typed in another case (303 to the index with `saved=deleted`, `domain.delete` +1), the page then 404 and the index showing the notice. Accounts: the search listing both accounts with links, `q=PLAIN` one, `q=absent` "No accounts match", a 201-character query 400, an unknown id 404; the account page with the address, the membership and its role; marking the address unverified (303, `address.unverify` +1, `verified_on` cleared, the page offering "Mark verified") and verified again, another account's address 404, a bad flag 400; saving `Plain <b>` as server owner (303, `user.update` +1, the row updated, the page showing the escaped value), a blank name refused inline, the account demoted again, then the server owner's own demotion refused as the last owner with the flag intact. `postgres_domains_users_contract` runs the matrix on an isolated schema and is wired into `scripts/test-postgres.sh`. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` PASS; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 150 binaries, 984 passed, 0 failed, 72 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS (54 tests) and `scripts/test-postgres-all.sh` 69 passed, 0 failed on one disposable PostgreSQL 14.24 role and database, dropped afterwards (run before the final change, which touched only two catalog strings and one table header); Chromium 140.0.7339.16 acceptance PASS: the server-owner fixture opens Domains from the administration index, adds `lists.example.org`, seats itself as owner and removes itself, is refused a deletion with the host mistyped (an announced inline 400), deletes the empty domain and sees it gone from the table, searches accounts (none for `newcomer`, itself for `browser@`), opens its own account with its memberships, renames it and sees the new name as the heading; axe-core 4.13.0 finds no critical or serious violation on 15 pages (the domain index, the domain page and the account search scanned); zero console errors. `scripts/test-mailmanclient.py` not run: no REST surface changed. **Limits and deviations:** a domain's description and alias cannot be edited afterwards (REST has no domain update either); DKIM keys are configuration — the page shows the DNS record of a configured key and generates or rotates none (the plan's `dkim_keys` table stays open); the owner search is by exact address; the account page neither deletes nor merges accounts (`P4-GDPR`), resets passwords nor manages second factors; the account search is a substring match without sorting. Not Phase 4 completion. |

## List creation and the directory — bounded local acceptance verified

| Acceptance ID | Behavior | Evidence/status |
| --- | --- | --- |
| P4-LIST-CREATE-INDEX | The directory, list creation and the list summary. `/web` (`crates/db/src/web_lists.rs::browser_directory`, `crates/api/src/webui_lists.rs`) takes `?q=` (a case-insensitive `LIKE … ESCAPE '!'` substring of the id, display name or description, 200 characters at most), `?domain=` (an exact mail host, offered as a select of the site's domains) and `?show=all`; a visitor sees advertised lists only and gets no cookie; a signed-in reader sees their highest verified-address role on each list as a badge (owner, moderator, member) and, with `show=all`, also the unadvertised lists they have a role on — every list for a verified server owner — each marked "not in the public directory"; `Pagination::filtered` keeps the filters on page links; a create link appears for readers with at least one creatable domain (`browser_creatable_domains`: every domain for a server owner, else the reader's `domain_owners` rows, under the browser write transaction and live session authority), also on `/web/admin`. `/web/lists/new` (GET/POST, `privileged`, 403 with no creatable domain): list name, domain, display name, owner address (the reader's first verified address offered), style (the built-in three), advertised, description; each refusal inline on its field with the other values kept (`id="{field}-error"`: a bad or empty name, a taken id, a domain not offered, a non-mailbox owner, an unknown style); a domain that exists but is not the reader's is 403. `browser_create_list` is one transaction: domain existence (`NotFound` → inline), authority (`Forbidden`), a free id (`Conflict` → inline "already exists"), `ListRepo::create_tx` (new: the former `create_with_context` body inside a caller's transaction, the domain checked there), `ListRepo::update_tx` for `advertised`/`description` through the REST validator, the owner row (address created when unknown, `user_id` from the address) and the `list.create`, `list.update`, `member.create` (`source: browser-create`) events, the session re-checked before commit; then `refresh_mta_maps` and a redirect to the new list's settings. `/web/lists/{id}` (`browser_list_summary`) is a page for anyone when advertised and for readers with a role or server owners when not (404 otherwise, checked before a cookie is issued); it lists the posting address, the owners' address, the domain, the archive and subscription policies, the reader's role and, for those who administer, the settings link, above the unchanged subscribe form. Strings `web-create-list-*`, `web-directory-*`, `web-badge-*`, `web-list-*` in `en` and `vi`; a `control` template macro renders a `SettingField` outside the settings groups. | **Bounded local acceptance verified.** RED first: `crates/api/tests/webui/list_create_index.rs` failed on the directory lacking the search form before the work (`cargo test --locked -p listmngr-api --test webui list_create_index::`). The matrix, with a server owner, the owner of a second domain `other.example`, a plain member of the public list and the owner of the unadvertised private list: a visitor's directory with the public list only, the search form and domain select, no create link; `q=PUBLIC`, `q=alert` (the display name) one entry, `q=absent` none with "No lists match", `q=%25` none (a literal percent), `domain=example.com` one, `domain=other.example` none, filters combined with `page=0`; a 201-character `q`, a 254-character `domain` and `show=maybe` 400; `show=all` as a visitor never lists the private list; the plain member's badge "Member", no create link, `show=all` still without the private list; the private owner sees the private list only with `show=all`, with "Owner" and "not in the public directory"; the server owner sees every list and the create link on `/web` and `/web/admin`. The form: 401 anonymous, 403 for the plain member, the domain owner offered `other.example` only with their address prefilled, the server owner offered both domains and all three styles. Refusals from the server owner: `bad name!`, an empty name, `public` (taken, "already exists"), `unknown.example`, `not-an-address`, `no-such-style` each a 400 with the refusal on its field and the display name kept; the domain owner posting `example.com` 403; no or a wrong CSRF 403; no `list.create` row and no list afterwards. Creation: the server owner's `announce` on `example.com` (announce style, unadvertised, description) answers 303 to `/web/lists/announce.example.com/settings`; the row carries the display name, description, style and `advertised=false`; one owner row for `root@example.com`; `list.create` and `member.create` each +1; the list 404 to a visitor and a page with "not in the public directory" for the server owner; the settings page shows the escaped name. The domain owner's `Team` on `other.example` with owner `Lead@Example.ORG` seats `lead@example.org` (address created); a visitor finds it with `domain=other.example`, opens the summary with `team@other.example` and `team-owner@other.example`, and a join request answers 202 with one `subscription_workflows` row. The summary: the public list's addresses and both policies for a visitor without "Your role", "Your role: Member" for the plain member, the private list 404 for them and a page with "Owner", the posting address and the settings link for its owner, and the owners' address for the server owner. The legacy `directory_is_usable_public_only_and_escaped` and the directory paging matrix still pass on the rebuilt page. `postgres_list_create_index_contract` runs the matrix on an isolated schema and is wired into `scripts/test-postgres.sh`. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` PASS; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 150 binaries, 983 passed, 0 failed, 71 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS (53 tests) and `scripts/test-postgres-all.sh` 68 passed, 0 failed on one disposable PostgreSQL 14.24 role and database, dropped afterwards; Chromium 140.0.7339.16 acceptance PASS: the server-owner fixture opens "Create a list" from the administration index (the owner address prefilled), creates `browser-made.example.com` with the announce style and a description, lands on its settings, is refused inline with "already exists" on the same id, finds the list with `q=browser-made&domain=example.com` as the only entry with the "Owner" badge and the escaped description, gets "No lists match." on another domain, sees the private list with `show=all`, opens the summary with the posting address, "Your role", "Owner" and the settings link; the Rust harness confirms the row, style, `advertised` and the creator's owner row; axe-core 4.13.0 finds no critical or serious violation on 12 pages (the create form and the summary scanned); zero console errors. `scripts/test-mailmanclient.py` not run: no REST surface changed. **Limits and deviations:** who may create is server owners and domain owners only (no site policy such as Mailman's `create_list` scope for other users); one owner at creation, seated unverified as on REST, and no welcome or notice; the directory offers search, domain and scope but no sorting or role filter, and the role badge is the reader's highest role; the summary shows policies as their configuration words; the domain select on the directory lists every domain of the site, including those with no advertised list. Not Phase 4 completion. |

## Moderator queues — bounded local acceptance verified

| Acceptance ID | Behavior | Evidence/status |
| --- | --- | --- |
| P4-HELD-QUEUE | The moderator's queues. `/web/moderation` lists the lists the reader moderates with the counts of held posts and undecided requests (`browser_moderated_lists`). `/web/lists/{id}/held` (`crates/db/src/web_moderation.rs::browser_held_queue`, `crates/api/src/webui_moderation.rs`): each post with sender, reason, hold time, the decoded `From`/`To`/`Date`, the first text body rendered as text (`mail_parser`), the attachment count and the raw source (first 64 KiB) folded; a per-post decision form (keep held, accept, reject, discard) with a comment — the rejection's reason to the author — and `forward_to` (`browser_review_forwarding`, `ModerationRepo::review_tx`'s `forward_to`; a forward to the list's own address refused inline on the post's field); a bulk form applying one decision to the ticked posts in one transaction (`browser_review_many`: posts decided meanwhile or not on this list are skipped and counted, `?done=N&skipped=M` reported, an empty selection refused inline); per post a **Sender** section showing the sender's standing (`role`, posting policy, or not on the list), a "moderate sender" form (`browser_moderate_sender`: the member's own override, or a nonmember row created for the purpose with `member.create`, then `member.update` with the held id), a "ban sender" form (`browser_ban_sender` through `BanRepo::create_tx`, hidden with "already banned" once banned) and a link to the header rules with a `From` rule for the escaped address prefilled (the rules page now takes `?header=&pattern=`). The page loads `/web/moderation.js` (`Shell::with_script`; the shell records one script path now, and the passkey pages use the same mechanism): `j`/`k` move between posts, `a`/`r`/`d`/`h` choose, `s` submits the focused post's decision, `?` shows the list; nothing depends on it. `/web/lists/{id}/requests` (`browser_requests`, `browser_decide_request` through the new `WorkflowRepo::decide_tx` after checking the request belongs to the list): join/leave requests waiting for a moderator or for the address, with name and time, accepted/rejected (reason in the audit event)/discarded/kept. `moderator_tx` is the one authority check (server owner, or owner/moderator of the list through a verified address), now shared with `browser_review`. Strings `web-held-*`, `web-requests-*`, `web-moderation-waiting` in `en` and `vi`; the outdated note that rejections send no notice corrected. | **Bounded local acceptance verified.** RED first: `crates/api/tests/webui/held_queue.rs` failed on the queue lacking the rendered body and the new forms before the work (`cargo test --locked -p listmngr-api --test webui held_queue::`). The matrix, as a moderator with a plain member as the outsider: the queue with the escaped subject, the rendered body (`Untrusted <b>body</b>` escaped in `pre.preview`), the sender, the forward field, the bulk selection, the sender and ban forms, the prefilled header-rule link, the script tag and `data-held`, 403 to the outsider, the script served as `text/javascript`; a `defer` with a forward to `reviewer@example.org` logging `forward_to` and queuing one job while the post stays held, a forward to `public@example.com` refused inline (`id="forward_to-error"`), a rejection with "Off topic" logged, and "1 decided" after the redirect; bulk accept of two of three posts (`done=2`), then a selection with one already-decided post giving `done=1&skipped=1` and "1 decided … 1 skipped", three `moderation.*` audit rows, an empty selection 400; "moderate sender" creating a nonmember row with `discard` then updating it to `hold` (one row), the queue showing the current action selected, "ban sender" adding the ban and the page then saying "already banned"; the requests queue on a `moderate` list listing two moderated requests and an invitation "waiting for the address", 403 to the outsider, one accepted into membership, one rejected with the reason in the audit row, a request of another list 404, the queue then without the decided ones; the index reporting "1 held" and "1 request". The legacy `moderator_review_is_scoped_verified_escaped_and_has_real_queue_effects` and `moderator_preview_is_bounded_and_queue_pages_are_reachable` matrices still pass. `postgres_held_queue_contract` runs the matrix on an isolated schema and is wired into `scripts/test-postgres.sh`. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` PASS; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 150 binaries, 982 passed, 0 failed, 70 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS (52 tests) and `scripts/test-postgres-all.sh` 67 passed, 0 failed on one disposable PostgreSQL 14.24 role and database, dropped afterwards; Chromium 140.0.7339.16 acceptance PASS: the server-owner fixture sees "3 held, 0 requests" and "0 held, 1 requests" on the index, the rendered body in the queue, sets the sender's posting policy to hold from the post and sees "nonmember, posting policy hold", discards two posts in bulk ("2 decided, 0 skipped"), opens the shortcuts with `?`, chooses accept with `j` then `a`, adds a comment and submits with `s` ("1 decided", queue empty), then accepts the private list's pending request from the requests queue; the Rust harness confirms the accepted and discarded dispositions, the nonmember row and the new membership; axe-core 4.13.0 finds no critical or serious violation on 10 pages; zero console errors. `scripts/test-mailmanclient.py` not run: no REST surface changed. **Limits and deviations:** the preview shows the first text part only (HTML-only posts show "no text part"; attachments are counted, not listed); "moderate sender" and "ban sender" are open to moderators as well as owners; the header-rule shortcut is a prefilled form, not a one-click rule; no per-post "add to accept/hold/reject/discard nonmember lists" (the nonmember row's own policy serves); the requests queue is bounded to 200 rows without paging; the keyboard shortcuts are latin-keyboard single keys with no remapping. Not Phase 4 completion. |

## Member management — bounded local acceptance verified

| Acceptance ID | Behavior | Evidence/status |
| --- | --- | --- |
| P4-MEMBERS | The owner's member management at `/web/lists/{id}/members`. Rosters of every role (`?role=member|owner|moderator|nonmember`) with the email-substring search and paging; per row the display name, the member-level delivery mode/status when set, the bounce score when above zero, the inline posting-policy form (now for nonmembers too: `browser_member_policy` accepts both roles) and a link to the options page. The roster is the first page to load the vendored htmx (`Shell::with_htmx`, CSP widened to `script-src 'self'` by `scripted`, a `<meta name="htmx-config">` switching off htmx's injected indicator stylesheet): the search form carries `hx-get`/`hx-target="#roster"`/`hx-push-url`, and `webui_members::roster` renders only the `Roster` sub-template when the request carries `HX-Request: true`, the whole page otherwise, so scripts off lose nothing. `crates/db/src/web_members.rs`: `browser_roster` (joined member-level preferences), `browser_member_detail` (stored and resolved preferences), `browser_member_options` (member row + `PreferencesRepo::set_tx` + one `member.update` event, `source: browser-admin`; the role can change), `browser_member_bounce_reset` (score, last bounce and warnings cleared, delivery re-enabled when bounces disabled it, `bounce.recover` with `source: owner`), `browser_mass_subscribe` (authority checked, then per address: an existing row on that roster is reported, members go through `WorkflowRepo::subscribe` with `AdminSubscription` so the policy and `pre_verified`/`pre_confirmed`/`pre_approved`/`invitation` behave as on REST, other roles through `MemberRepo::subscribe_with_context`; each address commits alone and reports Subscribed/Held/AlreadyMember/Invalid/Refused), `browser_mass_remove` (ids and member-role addresses resolved to this list's rows, each deleted through `delete_member_with_goodbye` with its own `member.delete` event, all in one transaction, rows of other lists ignored, the count returned), `browser_export_members` (10,000 rows at most). Pages: `…/members/{member}` (options form with **Inherit** for member-level values, the effective values resolved, bounce section, removal), `…/members/subscribe` (textarea or a plain-text file — the handler reads `multipart/form-data` through axum's `Multipart`, a new feature, or the urlencoded form; `Name <address>` lines; 1000 addresses and 256 KiB at most; a duplicate line reported), `…/members/remove` (checkboxes attached to a form outside the rows through the `form` attribute, plus pasted addresses; `?removed=N` notice), `…/members/export.csv?role=` (`text/csv`, attachment named `{list}-{role}.csv`, RFC 4180 quoting). `web-members-*`, `web-role-*`, `web-member-*`, `web-mass-*` strings in `en` and `vi`. | **Bounded local acceptance verified.** RED first: `crates/api/tests/webui/members_admin.rs` failed on the roster lacking the role tabs before the work (`cargo test --locked -p listmngr-api --test webui members_admin::`). The matrix, as a list owner with a plain member as the outsider and a moderator and a nonmember seeded: every role tab, the subscribe and export links and the bulk selection on the roster, each role's roster naming its own rows only, nonmembers with a posting policy, a bogus role 400, every page 403 to the outsider; the search answered with the whole page (containing `<html` and the htmx script) and, with `HX-Request: true`, a fragment without `<html` under a CSP allowing `script-src 'self'`; the options page listing every control, a save persisting posting policy, display name, delivery mode/status, acknowledgements, list copy, own posts and language (member-level `None` for Inherit) with one more `member.update` row and the values read back selected, a bad delivery mode refused inline (`id="delivery_mode-error"`), a role change moving the row to the moderators' roster with the member-level values cleared, a member of another list 404; bounce score 5.5 and last bounce shown, the roster saying `by_bounces`, the reset zeroing the score and last bounce, re-enabling delivery and auditing `bounce.recover` for the owner; mass subscription from the textarea on a `moderate` list with all three flags subscribing `Mass One <mass-one@example.org>` (display name kept) and `mass-two`, naming the bad line and the duplicate, then without `pre_approved` a held request (one open `subscription_workflows` row), an invitation held, a moderator added directly, `subscription.subscribe` and `member.create` rows; the same form as `multipart/form-data` with the addresses in a file subscribing both; removal by selected ids deleting this list's row and leaving the other list's alone with one `member.delete` row, then by pasted addresses (two more rows, an unknown address ignored) and "Removed 2" after the redirect; the export as `text/csv` with the attachment name, the header row, this roster's rows only, the owners' roster on `?role=owner`, and 403 to the outsider. The existing `member_admin`, `member_search` and `admin_pagination` matrices still pass on the rebuilt page. `postgres_members_admin_contract` runs the matrix on an isolated schema and is wired into `scripts/test-postgres.sh`. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` PASS; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 150 binaries, 981 passed, 0 failed, 69 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok after `cargo update -p precis-profiles` (0.1.14 was yanked upstream during this increment; 0.1.13 and `precis-core` 0.1.11 are what `webauthn_rp` resolves to now); `cargo audit --ignore RUSTSEC-2023-0071` clean (the `multer` dependency of axum's `multipart` feature is new); `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS (51 tests) and `scripts/test-postgres-all.sh` 66 passed, 0 failed on one disposable PostgreSQL 14.24 role and database, dropped afterwards; Chromium 140.0.7339.16 acceptance PASS: the server-owner fixture searches the roster with htmx swapping the rows in place (a marker set on the document survives and the URL is pushed), opens the owners' roster, adds two members and a bad line from the mass form and reads the outcomes, opens a member's options, changes the delivery mode and display name and sees them saved, resets the bounce score, fetches the CSV export with the renamed member, removes one member from its page and another by pasted address; axe-core 4.13.0 finds no critical or serious violation on 8 pages (the roster and the options page scanned signed in); zero console errors. `scripts/test-mailmanclient.py` not run: no REST surface changed. **Limits and deviations:** the mass subscription is not one transaction — each address commits alone (as a REST client would) so accepted addresses stay when a later one is refused; the roster search is the only htmx interaction (paging, policy saves and removals are full-page forms); no per-member `send_welcome_message` override on the mass form (the list's setting applies, as on REST); the export is capped at 10,000 rows without saying so in the file; the options page edits member-level preferences only (address and account layers are the reader's own); no "sync members" (replace the roster with a file) operation. Not Phase 4 completion. |

## List settings — bounded local acceptance verified

| Acceptance ID | Behavior | Evidence/status |
| --- | --- | --- |
| P4-LIST-SETTINGS | Every list setting the REST configuration accepts, editable in the browser in Postorius's nine groups at `/web/lists/{id}/settings/{identity,responses,messages,dmarc,digest,acceptance,archiving,members,bounces}` (the essentials form at `/web/lists/{id}/settings` stays, with the group navigation). `crates/api/src/webui_list_settings.rs` describes each group as fields with a kind (text, textarea, one-entry-per-line, select over wire values, moderation action with `default`, whole number, decimal); one template (`settings_group.html`) renders any group; the handler turns the form into the same JSON patch the REST API takes, so `ListRepo`'s patch engine is the only validator — `validate_patch` (new, on a copy) for **Preview changes**, which shows a before/after table of what a save would change and writes nothing; `browser_update_list_settings` (locked row, `list.config` audit with the patch) for **Save**. A refused value comes back inline on its field with the submitted values kept and a 400 (`inline_refusal`: a marker header the browser error-page middleware strips instead of replacing the body — every other client error still shows the generic page). The identity group shows the derived addresses, `no_reply_address`, creation and last-post times, post count and volume read-only. `crates/db/src/web_list_settings.rs`: header rules (`browser_header_match_change`: append, update in place, move, remove — each rewriting the ordered set through `HeaderMatchRepo::edit_tx`, one `list.header_matches` audit event), list bans and site bans (`BanRepo::{create,delete,site_create,site_delete}_tx`; site bans need a live server owner), templates (`browser_template` resolves the effective body and its source; `browser_template_set`/`browser_template_delete` through `TemplateRepo::{set_body,delete}_tx`), archivers (`ListRepo::set_archiver_tx`, names from a catalogue: `mail-archive`, `mhonarc`, `prototype`), digest bump (`DigestRepo::bump_tx`) and send (the digest flush after an authority check), and `browser_delete_list` (`ListRepo::delete_tx`, attributed `list.delete`) — every write one transaction with the owner's live authority rechecked. Pages: `…/header-matches` (rules with inline edit, move up/down, remove, an add form, and **Test a header value** evaluating the stored patterns with the pipeline's `compile_header_pattern`), `…/bans` and `/web/admin/bans` (paged, add, lift), `…/templates` (catalogue of every `list:*` template with stored languages) and `…/templates/{name}?language=` (editor, source, effective text, placeholder reference from the new `templates::PLACEHOLDER_NAMES`, **Preview** through `templates::expand` with sample values), digest and archiver actions after their groups, `…/delete` (consequences with the archive policy; the id typed back). `web-ls-*`, `web-rules-*`, `web-bans-*`, `web-templates-*`, `web-delete-list-*` strings in `en` and `vi`; styles for the group navigation, inline refusals, facts and tables. | **Bounded local acceptance verified.** RED first: `crates/api/tests/webui/list_settings_groups.rs` failed on the overview lacking the group links before the work (`cargo test --locked -p listmngr-api --test webui list_settings_groups::`). The matrix, as a list owner with a plain member as the outsider: every group and page linked from the overview and refused (403) to the outsider, an unknown group 404; identity showing the posting and owner addresses, a preview rendering the escaped old and new display names and `preferred_language` with the row unchanged afterwards, an identical form previewing "Nothing would change", a subject prefix with a line break refused inline (`id="subject_prefix-error"`, submitted values kept, no audit row), then a save persisting `display_name`, `info`, `subject_prefix` and `preferred_language` with exactly one more `list.config` row and "Saved" after the redirect; one representative form per remaining group saved and read back through `ListRepo::get` (autoresponse action and grace period, MIME/extension token lists, reply munging, personalization, anonymous list, DMARC action/unconditional/addresses, digest threshold 45.5/weekly/next number, member and nonmember actions with `default` → none, nonmember lists, aliases, explicit destination, limits, archive policy and rendering, subscription policies and roster visibility, bounce threshold 7.5, staleness, unrecognized-bounce disposition) and a bad MIME type refused inline with the old list intact; header rules added, a bad pattern refused inline (`id="pattern-error"`), order shown, moved up, edited in place (pattern, chain, tag read back), tested (`Rule 1 matches: the post would reject.` / `No rule matches this value.`), removed, ≥ 4 `list.header_matches` rows; list bans added (normalized), a bad regex refused inline, one lifted, three `ban.*` rows, the site page 403 to a list owner and working for a server owner (add, listed, removed, `list_id IS NULL` rows); templates: the catalogue naming `list:user:notice:welcome`, the editor with the placeholder reference and language select, a preview expanding `$display_name` and `$listname` and leaving `$$` as `$` with nothing stored, a Vietnamese body saved and shown back, an unknown name 404, removal, one `template.set` and one `template.delete` row; digest bump (volume +1, numbering reset, `digest.bump`) and send accepted; an archiver stored and read back, an unknown archiver 400; deletion refused for a wrong id (400) and for the outsider (403), then done for the owner (303 to `/web/admin`) with the list, members, header matches and bans gone and `list.delete` attributed to the owner. `postgres_settings_groups_contract` runs the matrix on an isolated schema and is wired into `scripts/test-postgres.sh`. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` PASS; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 150 binaries, 980 passed, 0 failed, 68 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS (50 tests) and `scripts/test-postgres-all.sh` 65 passed, 0 failed on one disposable PostgreSQL 14.24 role and database, dropped afterwards; Chromium 140.0.7339.16 acceptance PASS: the server-owner fixture previews a change in the acceptance group (the before/after table), has a negative size refused inline, saves twice with the "Saved" notice, adds a header rule, tests a value against it ("Rule 1 matches") and removes it, bans and lifts an address, previews, saves and removes a welcome template, and has a wrong id refused on the delete page; axe-core 4.13.0 finds no critical or serious violation on 6 pages (the two settings pages scanned signed in); zero console errors beyond the two announced inline refusals. `scripts/test-mailmanclient.py` not run: no REST surface changed. **Limits and deviations:** the browser has no equivalent of `moderator_password` (the Approved: posting key stays REST-only); the DMARC *policy* itself is evaluated by the pipeline, the page only stores the mitigation; the archiver toggles are recorded only until `P5-REMOTE-ARCHIVERS`; the template editor offers `en` and `vi` (the languages the site ships) and edits list scope only — domain and site templates stay REST-only; **Send the digest now** publishes and returns to the page without saying how many digests went; a deleted list's archive messages stay stored but unreachable, as with the REST delete; no htmx partials — every action is a full-page form. Not Phase 4 completion. |

## OpenID Connect sign-in — bounded local acceptance verified

| Acceptance ID | Behavior | Evidence/status |
| --- | --- | --- |
| P4-OIDC | Sign in through an OpenID Connect provider. `[[web.oidc]]` entries (`crates/core/src/lib.rs`: `OidcProviderConfig`, validated in `Config::load` — unique lowercase slug, non-empty display name, an https issuer or an http loopback one for development, `client_id`, exactly one of `client_secret` or `client_secret_file` read with the same 0600 check as `database.url_file`, the secret held in the redacting `SmtpAuthSecret` type so `Debug` and serialization print `[REDACTED]`, `openid` added to the scopes). `crates/api/src/oidc.rs`: the relying-party client — discovery and JWKS cached per provider, Authorization Code with PKCE (S256), fresh `state` and `nonce` per ceremony, the token exchange with `client_secret_post` and the code verifier, ID-token verification (header `alg`/`kid`, JWKS lookup with one refresh on an unknown key, RS256 via `rsa` and ES256 via `p256`, issuer with a trailing slash forgiven, audience as string or array, `exp`, `iat` within five minutes, a constant-time nonce). Migration `0047`: `user_oidc` (`UNIQUE(provider, subject)`, the asserted email, timestamps), `web_sessions.oidc_state` (the parked ceremony as JSON, taken and cleared by the callback) and `user_credentials.usable` (0 for a just-in-time account's random password; set to 1 by a reset or a password change). `crates/db/src/web_oidc.rs`, one transaction each: `browser_oidc_login` signs in the account linked to `(provider, subject)`; with nothing linked and `email_verified`, the account owning that verified address (linked on the spot) or, for an unowned address or none, a new account (`user.jit`) with the provider's name as display name, a random Argon2 hash marked unusable and the address verified — an unverified email or an address an account never verified is refused with nothing written; like the password, a provider sign-in is `LoginOutcome::SecondFactor` for an account with TOTP enrolled, and it audits `web.login` / `web.login.password` with `method: oidc`. `browser_oidc_link` (purpose `link`, from a CSRF-protected POST) links to the signed-in account and refuses a subject linked elsewhere; `browser_login_methods` and `browser_oidc_unlink` re-verify the password on the login bucket when the account has a usable one and refuse the last way in (no usable password, no passkey, no other link). Pages: the login page lists the providers; `/web/login/oidc/{name}` starts (404 for an unknown name), `/web/login/oidc/{name}/callback` finishes — a callback with no parked ceremony, another provider's, an expired one or a mismatched `state` is a 400, and the provider's own `error` is a 400 without echoing it; `/web/account/oidc` links or unlinks, with the unlink form hidden and the reason shown when the link is the last way in. The `web-oidc-*` strings ship in `en` and `vi`. | **Bounded local acceptance verified.** RED first: `crates/api/tests/webui/oidc.rs` failed on the login page lacking the provider link before the handlers existed (`cargo test --locked -p listmngr-api --test webui oidc::`); `crates/core/tests/domain.rs::oidc_providers_are_validated_and_secrets_come_from_files` and the `oidc.rs` unit test (`rs256_and_es256_signatures_verify_and_tampering_fails`) pin config validation and signature verification. A mock provider on a loopback port (`crates/api/tests/webui/mock_oidc.rs`: discovery, an authorization endpoint that checks the request and redirects back with a code, a token endpoint that checks `grant_type`, `redirect_uri`, client id and secret and the S256 challenge against the verifier, ES256 ID tokens with a JWKS) drives the matrix: the login page naming the provider; an unknown provider 404; a just-in-time sign-in for a verified email creating one user with a verified, linked address, one `user_oidc` row, `user.jit` and a `web.login` row naming `oidc`, and `usable = 0`, then the same subject coming back to the same account (two tokens issued, one user); an unverified email refused (403) with nothing created; a stray callback on a fresh session and a mismatched `state` on a started one refused (400); a verified email matching an existing verified address linking to that account instead of creating one; linking from the account page (POST with CSRF → provider → callback → `/web/account/oidc`, one `user.oidc.link` row) and the linked subject then signing that owner in; unlinking refused on a wrong password (401) and done on the right one (one `user.oidc.unlink` row); the just-in-time account's only link refused as the last way in (400) with the form withheld; and an account with TOTP enrolled landing on `/web/login/totp` with a pending session that answers 401. `postgres_oidc_contract` runs the matrix on an isolated schema and is wired into `scripts/test-postgres.sh`; the `oidc-schema.snapshot` fixture pins the schema in both chains. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` PASS; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 150 binaries, 979 passed, 0 failed, 67 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS (49 tests) and `scripts/test-postgres-all.sh` 64 passed, 0 failed on one disposable PostgreSQL 14.24 role and database, dropped afterwards; Chromium 140.0.7339.16 acceptance PASS (regression of the login and account pages; no provider configured in that fixture), zero console errors. `scripts/test-mailmanclient.py` not run: no REST surface changed. **Limits and deviations:** generic OpenID Connect only — Google works through its standard discovery document, but GitHub is OAuth2 without an ID token and is not supported; no browser-level run against a real provider (the mock signs ES256; RS256 is covered by the unit test); the `client_secret_post` method only (no `private_key_jwt`); no logout propagation to the provider and no refresh tokens (the ID token is used once, at sign-in); a provider's asserted name is trusted for the display name of a just-in-time account; an existing account with an unverified matching address is refused rather than prompted to verify; no notice is mailed on link or unlink; a just-in-time account reaches a password only through `/web/reset`. Not Phase 4 completion. |

## Passkeys — bounded local acceptance verified

| Acceptance ID | Behavior | Evidence/status |
| --- | --- | --- |
| P4-WEBAUTHN | Passkeys over WebAuthn with `webauthn_rp` 0.3.0 (pure Rust: `p256`, `ed25519-dalek`, `rsa` for verification — no OpenSSL, so the static musl build is untouched). Migration `0046`: `user_passkeys` (credential id, `StaticState`/`DynamicState` in the library's binary encoding, name, timestamps), `users.webauthn_handle` (unique 64-byte handle) and `web_sessions.webauthn_state`. `crates/db/src/web_passkeys.rs`: the relying party is the host of `site.base_url` (which `listmngr_api::router` now carries into the `Database` handle with the site name and owner), refusing an IP literal; `browser_passkey_register_start` mints the user handle once, excludes the account's existing credentials, asks for `residentKey: required`, `userVerification: required` and credential protection without enforcement, and parks the ceremony on the session prefixed by purpose; `browser_passkey_register_finish` takes and clears that state, verifies with the origin pinned to `site.base_url`, checks the returned handle is the account's, stores the credential (a duplicate id is a conflict) and audits `user.passkey.add`; `browser_passkey_login_start` on an anonymous session parks a discoverable ceremony; `browser_passkey_login_finish` resolves the assertion's credential and handle to one account, verifies, stores the advanced counter and `last_used_at`, rotates into a signed-in session and audits `web.login` with `method: passkey` — no second step follows, and `browser_second_factor_missing` counts a passkey as a second factor; `browser_passkey_remove` needs the password. Pages: `/web/account/passkeys` (list, name form, removal) and the login page's "Sign in with a passkey" button; JSON ceremonies at `/web/account/passkeys/register/{start,finish}` and `/web/login/passkey/{start,finish}` with the CSRF token in `X-CSRF-Token` and the same Origin check as forms, bodies capped at 64 KiB, the login ceremonies on the pre-auth bucket. `crates/web/assets/passkeys.js` is the only script any page loads — first-party, `parseCreationOptionsFromJSON`/`toJSON()`, buttons hidden until it runs — and those two pages carry `script-src 'self'; connect-src 'self'` while every other page keeps `default-src 'none'`. `deny.toml` now ignores RUSTSEC-2023-0071 with the reason (public-key verification only), matching the `cargo audit` gate. | **Bounded local acceptance verified.** RED first: `crates/api/tests/webui/passkeys.rs` failed on a 404 route before the work (`cargo test --locked -p listmngr-api --test webui passkeys::`). A software ES256 authenticator (`crates/api/tests/webui/soft_authenticator.rs`: CBOR COSE key, `none` attestation, UP+UV flags, incrementing counter, DER signatures) drives the ceremonies over HTTP; the matrix, on a fixture with the second-factor policy on, covers: the page loading the first-party script under the widened CSP and the script served as `text/javascript`; creation options naming `localhost`, the account's address, `residentKey: required` and no exclusions; a wrong CSRF header refused (403); a response signed for `https://evil.example` refused (400) with nothing stored and the ceremony consumed; a good response storing one credential, one `user.passkey.add` row and the user handle, the key listed with its name escaped, and a second start excluding it; the un-enrolled server owner refused from `/web/admin` before and admitted after; passwordless login — request options with `rpId: localhost` and `userVerification: required`, the assertion rotating the anonymous session into one that reaches `/web/account` and `/web/admin`, one `web.login` row naming `passkey`, `last_used_at` set — then the same assertion replayed against a fresh session refused (401) and a fresh assertion accepted; removal refused on a wrong password (401), done on the right one with one `user.passkey.remove` row, and the policy gate returning. `postgres_passkeys_contract` runs the matrix on an isolated schema and is wired into `scripts/test-postgres.sh`; the `passkeys-schema.snapshot` fixture pins the schema in both chains; `crates/web/tests/assets.rs` checks the script refers to no other origin. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` PASS; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 150 binaries, 976 passed, 0 failed, 66 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS and `scripts/test-postgres-all.sh` 63 passed, 0 failed on one disposable PostgreSQL 14.24 role and database, dropped afterwards; `scripts/test-mailmanclient.py` PASS; Chromium 140.0.7339.16 acceptance PASS with the harness now serving `http://localhost:<port>` and CDP's virtual authenticator (CTAP2, resident key, user verification): the server-owner fixture registers a passkey from the page — the page's own script running the ceremony — sees it listed and confirmed resident on the authenticator, later signs in with it alone from the login page straight to a signed-in account and an open administration page, then removes it with the password; zero console errors throughout. **Limits and deviations:** `webauthn_rp` brings `rsa` for RS256 verification, and RUSTSEC-2023-0071 (a private-key timing side channel) is now ignored in `deny.toml` with the reason, since listmngr holds no RSA private key; attestation is `none` (no attestation-CA trust, as for most passkey deployments); browsers without `parseCreationOptionsFromJSON` (older than Chrome 129 / Firefox 119 / Safari 17.4) see no passkey buttons; no conditional UI (autofill) and no "remember this browser"; `site.base_url` must name a domain — an IP address refuses every ceremony with a validation error; no notice is mailed on registration or removal; deleting the account cascades the credentials but does not revoke them on the device. Not Phase 4 completion. |

## Two-step sign-in — bounded local acceptance verified

| Acceptance ID | Behavior | Evidence/status |
| --- | --- | --- |
| P4-TOTP | Two-step sign-in. `crates/db/src/totp.rs`: RFC 6238 over HMAC-SHA1 (RFC 4226), six digits, 30-second steps, the RFC 4648 base32 alphabet with forgiving decoding, `verify` accepting steps `now-1..=now+1` later than the last accepted one, the `otpauth://` provisioning URI with percent-encoded issuer and account, and an inline SVG QR code from the `qrcode` crate (XML declaration stripped). Migration `0045`: `user_totp` (secret, `confirmed_at`, `last_counter`), `user_recovery_codes` (SHA-256 digests, `used_at`), and `web_sessions.pending_user_id` / `second_factor_attempts`. `crates/db/src/web_totp.rs`: `browser_totp_status` starts enrolment idempotently with a 160-bit pending secret; `browser_totp_confirm` accepts the first matching code, records its step, mints ten `XXXXX-XXXXX` recovery codes and audits `user.totp.enable`; `browser_login` now returns `LoginOutcome` — for an enrolled account a session that is anonymous to every page, names its pending user, expires in ten minutes and audits `web.login.password`; `browser_second_factor` verifies CSRF and the credential version, accepts a fresh step (recording it) or an unused recovery code (marking it), rotates into a signed-in session and audits `web.login` with the factor, and counts failures — the fifth deletes the pending session; `browser_totp_recovery_codes` and `browser_totp_disable` re-verify the password on the login bucket. `webui::privileged` enforces `security.require_2fa_for`: a server owner without a confirmed factor gets 403 from `/web/admin`, list settings and members, `/web/moderation`, held queues and token minting, and the account page explains; `Config::load` now rejects any role other than `server_owner` in that list. `security.rate_limit.login` is honoured by the browser surface instead of a fixed five per minute. Pages: `/web/account/totp` (enrol or manage), `/web/account/totp/confirm|recovery|disable`, `/web/login/totp`. | **Bounded local acceptance verified.** RED first: `crates/api/tests/webui/totp.rs` did not compile before the work (no `listmngr_db::totp`), and `crates/db/src/totp.rs` unit tests pin RFC 6238 appendix B vectors, base32 round trips, drift, replay and the URI. The HTTP matrix (`cargo test --locked -p listmngr-api --test webui totp::`, on a fixture with the policy on) covers: an un-enrolled server owner refused (403) from administration and moderation while an ordinary member is not, with the account page naming the requirement; the enrolment page with a 32-character secret, an `otpauth://` URI and an inline SVG, stable across reloads; a wrong code refused with nothing confirmed; the right code confirming, ten well-formed recovery codes shown once, one `user.totp.enable` row, and the administration page opening; after logout, the password alone redirecting to `/web/login/totp` with a session that answers 401 everywhere, five wrong codes ending it; the next-step code (drift) completing into a rotated session with one `web.login` row naming `totp`, and that code refused on replay; a recovery code completing once, refused on reuse, with case and spacing forgiven; regeneration refused on a wrong password and replacing every code on the right one; disabling refused on a wrong password and, on the right one, deleting both tables so login is one step again and the policy gate returns. `postgres_totp_contract` runs the matrix on an isolated schema and is wired into `scripts/test-postgres.sh`; `crates/core/tests/domain.rs` covers the config validation; the `totp-schema.snapshot` fixture pins the schema in both chains. The shared browser fixture now switches the policy and the five-per-minute login budget off explicitly (each has its own test), and the `LoginOutcome` change is threaded through the three db/api tests that log in directly. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` PASS; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 150 binaries, 975 passed, 0 failed, 65 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS and `scripts/test-postgres-all.sh` 62 passed, 0 failed on one disposable PostgreSQL 14.24 role and database, dropped afterwards; `scripts/test-mailmanclient.py` PASS; Chromium 140.0.7339.16 acceptance PASS on the production defaults: the server-owner fixture sees administration closed (403) and the warning, enrols from the shown secret with a code the script computes as an app would, reads ten recovery codes (redacted before the screenshot), then completes a later login with the next step's code and another with a recovery code. **Limits and deviations:** the shared secret is stored as the authenticator holds it (it must be readable to verify; the database is the trust boundary, as for the password hashes' pepperless Argon2); no WebAuthn yet (`P4-WEBAUTHN`), no per-user "remember this browser", no notice when the factor is enabled or disabled, and API tokens are not gated by the policy (a server owner's tokens minted before enrolment keep working); `require_2fa_for` recognizes `server_owner` only; the browser fixture's `Config::default()` login budget of five a minute still holds for the Chromium run. Not Phase 4 completion. |

## Account deletion — bounded local acceptance verified

| Acceptance ID | Behavior | Evidence/status |
| --- | --- | --- |
| P4-ACCOUNT-DELETE | Self-service account deletion at `/web/account/delete`, linked from the account page: a confirmation page that spells out what goes and what stays, and a form that requires the password again. `Database::browser_delete_account` (`crates/db/src/web_delete.rs`) verifies the password with Argon2 before the writer reservation (on the login bucket, like every credential check), rebinds the session under `browser_write_tx` + `browser_user_tx`, refuses a server owner when no other server owner with a verified address exists, and then in one transaction ends every membership held through the user or any of their addresses (each with its own `member.delete` audit event carrying list, role, address and the reason), deletes the address rows (pending verification and reset tokens cascade) and their preferences, the API tokens, the credential, the sessions and any domain ownership, sets `held_messages.moderator_id` and `moderation_log.moderator_id` to NULL (both are `ON DELETE RESTRICT`), deletes the user row and its preferences, and audits `user.delete` with `by: self` and the counts. The handler clears the cookie and renders the page in the browser's language, since the account's own language is gone. | **Bounded local acceptance verified.** RED first: `crates/api/tests/webui/delete_account.rs` failed on the missing account-page link before the work (`cargo test --locked -p listmngr-api --test webui delete_account::`). It covers the last server owner refused (400) and still present; a wrong password refused (401) and a missing CSRF token refused (403) with the account intact and no `user.delete` row; and the real deletion: 200 with the cookie expired and the "Account deleted" page, the user gone, the address row and its membership gone, the pre-created API token gone, one `user.delete` and one `member.delete` audit row, the session refused afterwards with zero session rows left, while a bystander keeps their account, address and membership. `postgres_delete_account_contract` runs the matrix on an isolated schema and is wired into `scripts/test-postgres.sh`. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` PASS; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 150 binaries, 969 passed, 0 failed, 64 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS and `scripts/test-postgres-all.sh` 61 passed, 0 failed on one disposable PostgreSQL 14.24 role and database, dropped afterwards; `scripts/test-mailmanclient.py` PASS; Chromium 140.0.7339.16 acceptance PASS reaching the confirmation page (the fixture account is the last server owner, so the deletion itself is exercised over HTTP, not in the browser). **Limits and deviations:** this is erasure of the account, not of everything the person ever sent — posted messages stay in archives, held-message and moderation rows stay with the moderator link removed, and `audit_log.actor_user_id` keeps the id as text, all of which `P4-GDPR` will revisit with export and a broader erase; the moderator-link clearing is not exercised by a test, since no fixture creates a moderated message cheaply; no notice is mailed to the person or to list owners; the REST `DELETE /users/{id}` keeps its older semantics (unlink addresses, keep memberships) as an administrative surface; Mailman's own user deletion removes memberships too but keeps addresses unlinked, which listmngr does not do because `account_tokens` and enumeration safety depend on unowned verified addresses not lingering. `P4-ACCOUNTS` is now closed as a set: sessions, profile, site notices, signup, reset, addresses, tokens and deletion. |

## Own API tokens — bounded local acceptance verified

| Acceptance ID | Behavior | Evidence/status |
| --- | --- | --- |
| P4-ACCOUNT-TOKENS | The reader's own API tokens at `/web/account/tokens`, linked from the account page: every token with name, scopes, binding, creation, expiry, last use and a revoke form (or a revoked mark), and a form to mint one. Because the API authorizes on a token's scopes and bounds alone, `Database::browser_create_token` (`crates/db/src/web_tokens.rs`) computes the reader's authority inside the writer transaction — a verified server owner, and the lists they own by the same predicate the admin pages use — and allows a non-owner only `lists:read`, `lists:write`, `members:read`, `members:write` and `moderation`, bound to one of those lists (any other scope → 400, another list → 403, no list → 400), while a server owner may mint any scope and leave the token unbound (`admin` unbound only). Names are 1–64 control-free characters; lifetime is 1–365 days or, for a server owner, none. The row and its `token.create` audit event are written by `insert_token_tx`, now shared with the CLI path; the secret is returned once and shown on the page that follows, never stored or shown again. `browser_revoke_token` is an ownership-guarded `UPDATE … revoked_at` with a `token.revoke` audit event; a stranger's or an already-revoked token is a 404. The form's repeated `scopes` fields are read from the raw pairs with the same Origin and CSRF checks as every browser write. | **Bounded local acceptance verified.** RED first: `crates/api/tests/webui/tokens.rs` failed on a 404 route before the work (`cargo test --locked -p listmngr-api --test webui tokens::`). It covers a list owner seeing only their list and no `admin` scope; eight refused submissions (admin, system and unknown scopes, no scope, a list not owned, no list, a blank name, a 400-day lifetime) minting nothing, and a wrong CSRF token (403); a bound `members:read`+`lists:read` token whose secret starts with `lm_`, one row bound to the list, one `token.create` audit row with the reader as actor, the token answering 200 on its own list, 403 on another list and 403 on a `system:read` route, and the listing naming the token without the secret; a server owner unable to revoke it (404); revocation making the same bearer a 401 with one `token.revoke` audit row, a second revocation 404, and the listing marking it revoked; and a server owner minting an unbound `admin` token with a 7-day expiry that answers 200 on `/api/v1/domains`. `postgres_tokens_contract` runs the matrix on an isolated schema and is wired into `scripts/test-postgres.sh`. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` PASS; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 150 binaries, 968 passed, 0 failed, 63 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS and `scripts/test-postgres-all.sh` 60 passed, 0 failed on one disposable PostgreSQL 14.24 role and database, dropped afterwards; `scripts/test-mailmanclient.py` PASS; Chromium 140.0.7339.16 acceptance PASS with the new flow (the server-owner reader sees the administrative scope offered, mints a token bound to one list, reads the secret once, finds it absent from the listing, revokes it) and the harness confirming the bound, revoked row. **Limits and deviations:** authority is decided at minting time only — a token outlives the role that justified it until it expires or is revoked, and losing list ownership does not revoke tokens bound to that list; password change and reset do not revoke tokens, which the page says; a token's binding is one list (domain-bound tokens stay a CLI matter); the list picker is capped at 100 owned lists; `last_used_at` is whatever the API records; Mailman has no per-user API tokens (its REST is a single administrative credential), so this is listmngr's own surface. `P4-ACCOUNTS` is complete except deletion (`P4-ACCOUNT-DELETE`). |

## Own addresses — bounded local acceptance verified

| Acceptance ID | Behavior | Evidence/status |
| --- | --- | --- |
| P4-ACCOUNT-ADDRESSES | The reader's own addresses at `/web/account/addresses`, linked from the account page: primary first, each marked primary, verified or unverified, with only the applicable actions offered. `Database::browser_add_address` (`crates/db/src/web_addresses.rs`) validates the mailbox and, under `browser_write_tx` + `browser_user_tx`, inserts a new address linked unverified, links an address nobody owns while clearing any earlier `verified_on`, re-mails an unverified own address, and commits nothing for an address another account owns or an already-verified own one; the token and mail come from the `issue_verification` helper shared with signup (digest-only row, 24-hour expiry, one per address per hour, `site:user:action:verify`), and `address.add` is audited when something changed. `browser_primary_address` sets `preferred_address_id` only to a verified address of the reader (404 for a stranger's, 400 for an unverified one), auditing `address.primary`. `browser_remove_address` refuses the primary and the last verified address, otherwise unlinks, clears `verified_on`, deletes the address's pending tokens and audits `address.remove`; the address row and its memberships stay. Signup's `claim_account` now treats a verified address nobody owns the same way — linked, but proven again — so an administrative verification can never become an account for someone who did not read the mailbox. | **Bounded local acceptance verified.** RED first: `crates/api/tests/webui/addresses.rs` failed on a 404 route before the work (`cargo test --locked -p listmngr-api --test webui addresses::`). It covers the listing with the primary marked; a wrong CSRF token (403) and a non-mailbox (400); an address another verified account owns accepted with nothing listed, no token and no mail; an unowned address created by a nonmember row linked, listed unverified, with one token, one mail and one `address.add` row, and a repeat within the hour mailing nothing more; the mailed token verifying through `/web/verify` and the new address then usable to log in; the page offering promotion for the verified non-primary address and no removal for the primary; removal of the primary refused (400); promotion changing `preferred_address_id` with one `address.primary` row; a stranger's id refused for both actions (404); removal of the old primary leaving it unowned and unverified with its nonmember membership intact and one `address.remove` row, the removed address no longer able to log in while the kept one can; and removal of the last verified address refused. `postgres_addresses_contract` runs the matrix on an isolated schema and is wired into `scripts/test-postgres.sh`. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` PASS; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 150 binaries, 967 passed, 0 failed, 62 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS and `scripts/test-postgres-all.sh` 59 passed, 0 failed on one disposable PostgreSQL 14.24 role and database, dropped afterwards; `scripts/test-mailmanclient.py` PASS; Chromium 140.0.7339.16 acceptance PASS with the new flow (the reader opens the page from the account links, sees the primary address, adds a second one and sees it listed unverified) and the harness confirming the added address linked and unverified with its live token. **Limits and deviations:** removal does not touch `as_user` memberships, which deliver to whatever the primary address is, nor per-address preferences; an address's `display_name` is not editable here; there is no confirmation step before removal; a removed address that is later re-added starts unverified even for the same account, by design; the REST `/addresses` resource keeps its administrative semantics (an admin may still verify without mailbox proof); Postorius offers the same four actions, but its "remove" deletes the address row, which listmngr does not do because memberships reference it. Not `P4-ACCOUNTS` completion: tokens and deletion remain. |

## Password reset by mailbox proof — bounded local acceptance verified

| Acceptance ID | Behavior | Evidence/status |
| --- | --- | --- |
| P4-ACCOUNT-RESET | Password reset by mailbox proof at `/web/reset` and `/web/reset/confirm`, linked from the login page (whose stale "signup and reset are not available" note is replaced by a help line). `Database::browser_reset_request` (`crates/db/src/web_reset.rs`) accepts any mailbox, re-checks the anonymous CSRF-anchoring session on the writer's connection, and only for a verified address joined to a user with a credential issues a `password_reset` row in `account_tokens` (digest only, 24-hour expiry, one per address per hour) and mails `site:user:action:reset` through `site_notices().enqueue_tx` in the account's own language, auditing `user.reset.request`; every other case commits nothing. `browser_reset_confirm` validates and hashes the new password before the writer reservation, consumes the token with one guarded `UPDATE … RETURNING`, requires the address to still be verified and owned, replaces the credential (bumping `password_updated_at`, which invalidates every session's `credential_version`), deletes the account's sessions, retires sibling tokens and audits `user.password` with `"by":"mailed token"`. The handler renders the same 202 page for every request, rate limits both forms on the pre-auth bucket, and requires Origin and CSRF. | **Bounded local acceptance verified.** RED first: `crates/api/tests/webui/reset.rs` failed on the missing login-page link before the work (`cargo test --locked -p listmngr-api --test webui reset::`). It covers unknown and unverified addresses accepted with no mail and no token; a verified account (typed in mixed case) getting exactly one mail, one live token and one `user.reset.request` row, with a second request in the hour accepted and silent; the confirmation page prefilling the token; a wrong token, a weak password and a mismatch all refused with the token still live and the old password still working; the right token and a strong password ending with the old password refused, the new one accepted, a session from before the reset answering 401, one `user.password` audit row for the right user carrying the cause (the audit layer redacts the `password` value), and the token refused on reuse. `postgres_reset_contract` runs the matrix on an isolated schema and is wired into `scripts/test-postgres.sh`. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` PASS; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 150 binaries, 966 passed, 0 failed, 61 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS and `scripts/test-postgres-all.sh` 58 passed, 0 failed on one disposable PostgreSQL 14.24 role and database, dropped afterwards; `scripts/test-mailmanclient.py` PASS; Chromium 140.0.7339.16 acceptance PASS with the new flow (reset reached from the login page, a request for the verified browser account accepted, the confirmation page prefilled from the link) and the harness confirming one live reset token afterwards. **Limits and deviations:** the reset mail is queued, not delivered, unless the out runner and a relay are configured; `$reset_url` is `site.base_url` + `/web/reset/confirm`; a reset does not revoke API tokens (`P4-ACCOUNT-TOKENS` will decide that) and sends no "your password was changed" notice; a token is entered by hand or via `?token=`, never consumed by a bare link; no per-IP limit beyond the shared pre-auth bucket; Mailman itself has no reset (Django does), so the flow and wording are listmngr's. Not `P4-ACCOUNTS` completion: addresses, tokens and deletion remain. |

## Self-service signup and mailbox proof — bounded local acceptance verified

| Acceptance ID | Behavior | Evidence/status |
| --- | --- | --- |
| P4-ACCOUNT-SIGNUP | Self-service accounts at `/web/signup` and `/web/verify`, on by default and removed entirely (route and login-page link) by `web.signup = false`. `Database::browser_signup` (`crates/db/src/web_signup.rs`) validates the mailbox, name (shared `validate_display_name`) and password (zxcvbn, `security.password_min_score`), hashes with Argon2 before the writer reservation, re-checks the anonymous CSRF-anchoring session on the writer's connection, and decides per address: create the account; link an address nobody owns; re-arm an account that was never proven (new name and password — the address belongs to whoever proves it); or, for an address a verified account owns, commit nothing. Otherwise it issues a 32-byte token into `account_tokens` (migration `0044`: SHA-256 digest only, purpose, 24-hour expiry, `consumed_at`; at most one per address per hour) and mails `site:user:action:verify` through `site_notices().enqueue_tx` with `$token` and `$verify_url`, then audits `user.signup` — one transaction. `browser_verify_address` consumes the token with one guarded `UPDATE … RETURNING`, verifies the address, retires sibling tokens and audits `address.verify`. The handler always renders the same 202 "Check your email" page, rate limits both forms on the pre-auth bucket, and requires Origin + CSRF like every other browser write; new accounts start in the negotiated interface language. | **Bounded local acceptance verified.** RED first: `crates/api/tests/webui/signup.rs` did not compile before the work (`web.signup` did not exist) and its first assertion was a 404 route. It covers the login-page link; five rejected submissions (missing CSRF → 403; weak password, mismatch, non-mailbox, blank name → 400) creating nothing; an accepted signup with the typed spelling creating exactly one user, one unverified linked address, one `user.signup` audit row, one `workflow_notices` job and one live token, with login refused (401) until verification and the token read from the queued message (`From: postmaster@example.com`, `To:` as typed, `/web/verify` in the body); a repeat within the hour that is accepted but mails nothing; `/web/verify?token=` prefilling, a wrong token → 400, the right token verifying once (address `verified_on`, `address.verify` audit) and refused on reuse; login succeeding afterwards; and a stranger's repeat signup on the verified address that is accepted, creates nothing, mails nothing and does not change the password. `signup_can_be_switched_off` covers the 404s and the missing link. `postgres_signup_contract` runs the matrix on an isolated schema and is wired into `scripts/test-postgres.sh`; the `account-tokens-schema.snapshot` fixture pins the new table in both schema-contract chains. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` PASS; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 150 binaries, 965 passed, 0 failed, 60 ignored (a first run saw `notice_language::a_member_preference_beats_the_list_language` count an expired 100 ms test lease twice under full-suite load; the test helper now leases for 60 s, unrelated to this behavior); `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS and `scripts/test-postgres-all.sh` 57 passed, 0 failed on one disposable PostgreSQL 14.24 role and database, dropped afterwards; `scripts/test-mailmanclient.py` PASS; Chromium 140.0.7339.16 acceptance PASS with the new flow (an anonymous visitor reaches the form from the login page, is accepted, and the verification page prefills a token from the link) and the harness confirming one unverified account with one live token afterwards. **Limits and deviations:** the verification mail is queued, not delivered, unless the out runner and a relay are configured — as for every notice; `$verify_url` is `site.base_url` + `/web/verify`, so an empty `base_url` yields a relative path in the mail; a verified address that signs up again receives no courtesy notice, so a person who forgot they have an account learns nothing until password reset ships (`P4-ACCOUNT-RESET`); the token is entered by hand or via `?token=` on a page, never consumed by a bare link, so a preview-fetching mail client cannot verify on the reader's behalf; there is no CAPTCHA or per-IP signup limit beyond the shared pre-auth bucket; Mailman itself has no signup (Django does), so the flow and wording are listmngr's. Not `P4-ACCOUNTS` completion. |

## Site notices — bounded local acceptance verified

| Acceptance ID | Behavior | Evidence/status |
| --- | --- | --- |
| P4-SITE-NOTICES | Mail the site sends to a person outside any list. `Database::site_notices()` (`crates/db/src/site_notices.rs`) renders a `site:*` template at the site scope then the built-in (`resolve_site_tx`), in the recipient's negotiated language, with `$site_name`, `$domain` and `$base_url` defaults under the caller's placeholders; serializes it with `From:` and a Message-ID at `site.site_owner` (carried by `Database::with_site`, applied by `serve` from `[site]`); and enqueues it through the same `enqueue_raw_notice` store→message→`out` job→`workflow_notices` path every generated notice uses, in the caller's transaction (`enqueue_tx`) or its own with a `site.notice` audit event (`enqueue`). The message context is `{"site": true, "site_mail_host": …}` with no `list_id`. Two templates ship in `en` and `vi`, `site:user:action:verify` and `site:user:action:reset`, with subjects `notice-site-verify-subject` / `notice-site-reset-subject`; the REST `/uris` resource can override them at the site scope. In the out runner, `sign_delivery` signs for `site_mail_host` when the context has no `list_id` and the job's notice row exists; every other step already tolerated a missing `list_id`. | **Bounded local acceptance verified.** RED first: `crates/runners/src/site_notice_tests.rs` did not compile before the work (no producer, no `with_site`). It runs the real out runner against an SMTP sink and covers: `MAIL FROM:<>`, `RCPT TO:<person@example.net>`, `From: postmaster@lists.example.com`, the catalog subject, `Auto-Submitted`, a Message-ID at the site domain, expanded `$token`/`$verify_url`, no `List-*` or `DKIM-Signature` header without a key, zero `message_delivery_bindings`, one `site.notice` audit row; a `DKIM-Signature` with `d=lists.example.com` when `[[mta.dkim_signing]]` has a key for the owner's domain; a Vietnamese recipient getting the RFC 2047 subject and a site-scoped stored template override (base64 body decoded and checked); and an unknown template or an unusable `site_owner` enqueueing nothing. `postgres_site_notice_contract` runs the first matrix on an isolated schema and is wired into `scripts/test-postgres.sh`. `crates/mail` catalog tests keep the new names and placeholders consistent across `en` and `vi`; `crates/i18n` tests cover the new subjects. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` PASS; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 150 binaries, 963 passed, 0 failed, 59 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS and `scripts/test-postgres-all.sh` 56 passed, 0 failed on one disposable PostgreSQL 14.24 role and database, dropped afterwards; `scripts/test-mailmanclient.py` PASS. **Limits and deviations:** no production producer calls this yet — `P4-ACCOUNT-SIGNUP` and `P4-ACCOUNT-RESET` are the first, so no browser or CLI surface changed and the Chromium acceptance was not rerun; site notices are one recipient per message with no VERP or personalization; `site.site_owner` must be an ASCII mailbox, or every site notice is refused with a validation error rather than sent from an unusable address; the site-scope `/uris` override uses the existing REST resource and has no browser editor; Mailman has no equivalent (Django owns these mails), so the template names and bodies are listmngr's. Not `P4-ACCOUNTS` completion. |

## Own profile and interface language — bounded local acceptance verified

| Acceptance ID | Behavior | Evidence/status |
| --- | --- | --- |
| P4-ACCOUNT-PROFILE | The reader's own profile at `/web/account/profile`: display name, interface language and IANA time zone, prefilled and selected from the row. `listmngr_db::web_profile::Profile::validate` is the one validator — a non-empty name of at most 256 characters without control characters, a language `listmngr_i18n` serves, a zone name from `chrono-tz` 0.10.4 — and it now also guards the REST user patch (`UserRepo::update`), which previously stored any string. The browser write runs under `browser_write_tx` + `browser_user_tx` with one `user.profile` audit event in the same transaction. `webui::reader_language` makes the chosen language win over `Accept-Language` on every page a signed-in reader opens (account, admin, members, settings, password, sessions, profile, leave/recover, moderation, held, archive when signed in, and the login page when a signed-in session opens it); anonymous pages and the failure page keep the browser negotiation; the compatibility archive path stays English. The account page links the new page. | **Bounded local acceptance verified.** RED first: `crates/api/tests/webui/profile.rs` failed on a 404 route before the work (`cargo test --locked -p listmngr-api --test webui profile::`). It covers the prefilled form, seven rejected writes (missing and wrong CSRF → 403; an unshipped locale, an unknown zone, a control character, a blank name and a 257-character name → 400) with no row change and no audit row, then one accepted edit that persists all three fields, audits once, escapes the new name on the account page, renders `<html lang="vi">` and the Vietnamese shell for a browser that asks for English, keeps the selections on reload, and leaves the anonymous directory following the browser. The same matrix runs as `postgres_profile_contract` on an isolated schema and is wired into `scripts/test-postgres.sh`. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` PASS; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 150 binaries, 959 passed, 0 failed, 58 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS and `scripts/test-postgres-all.sh` 55 passed, 0 failed on one disposable PostgreSQL 14.24 role and database, dropped afterwards; `scripts/test-mailmanclient.py` PASS; Chromium 140.0.7339.16 acceptance PASS including the new flow — the reader opens the profile from the account links, saves a name with markup, Vietnamese and `Asia/Ho_Chi_Minh`, lands on a Vietnamese page with `lang="vi"` and the zone kept, then switches back to English through the Vietnamese form. **Limits and deviations:** the interface language is `users.locale`, not the Mailman `preferred_language` preference that chooses notice language — the page says so, and a later package may offer to set both; the zone is stored and validated but nothing renders in it yet (session times still print UTC); the picker is one `<select>` of ~600 zones, with no grouping or search; the REST user patch is stricter than before, which the existing REST test (valid values only) does not notice; no notice is mailed on a profile change. Not `P4-ACCOUNTS` completion. |

## Own browser sessions — bounded local acceptance verified

| Acceptance ID | Behavior | Evidence/status |
| --- | --- | --- |
| P4-ACCOUNT-SESSIONS | The reader's own browser sessions, at `/web/account/sessions`: every live session with when it was issued and when it expires, the one making the request marked, and buttons to end one session or every other one. Migration `0043` gives `web_sessions` an opaque `id` and a `created_at` (existing rows are deleted rather than backfilled from the token digest, so no credential hash becomes a public identifier; every browser signs in once more after the upgrade). `Database::browser_sessions` reads under a live-session check; `browser_revoke_session` and `browser_revoke_other_sessions` write under `browser_write_tx` + `browser_user_tx` — the same live session, CSRF binding and writer reservation every other browser write uses — and commit one `web.session.revoke` audit event in the same transaction. An id that is not this reader's is a 404 that writes nothing; ending the current session also clears its cookie so the browser stops presenting a deleted credential. The account page links the new page, and `P4-ACCOUNTS` is split into per-vertical ids in `docs/PLAN.md` §7. | **Bounded local acceptance verified.** RED first: `crates/api/tests/webui/sessions.rs` failed on a 404 route before the work (`cargo test --locked -p listmngr-api --test webui sessions::`). It covers both sessions of one reader listed with the current one marked, no session token anywhere in the page, a stranger's session neither listed nor revocable (404, stranger still signed in), missing and wrong CSRF rejected with no audit row, revocation ending exactly the named session while the revoking session keeps working, one audit row per revocation, and `revoke-others` leaving exactly one row. The same matrix runs as `postgres_session_inventory_contract` on an isolated schema and is wired into `scripts/test-postgres.sh`. `crates/db/tests/fixtures/session-inventory-schema.snapshot` pins the new columns and unique index in both schema-contract chains. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` PASS; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 150 binaries, 958 passed, 0 failed, 57 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS and `scripts/test-postgres-all.sh` 54 passed, 0 failed on one disposable PostgreSQL 14.24 role and database, dropped afterwards; `scripts/test-mailmanclient.py` PASS; Chromium 140.0.7339.16 acceptance PASS including the new page — the reader opens it from the account links, sees this browser marked, ends it, lands on the directory with no session cookie and a 401 on `/web/account`, then signs in again so the pre-existing logout flow is unchanged. **Limits and deviations:** the list shows only what the row holds — no IP address, user agent, or last-seen time, because none is recorded (adding them is a privacy decision of its own); times render as fixed UTC, since locale formatting in the browser would need script; the page is capped at 100 sessions; there is no confirmation step before ending a session, and no session-ended notice is mailed (site-scoped mail arrives with `P4-SITE-NOTICES`); API tokens are a separate surface (`P4-ACCOUNT-TOKENS`) and the page says so; the axe scan still covers the four public pages, not this authenticated one. Not `P4-ACCOUNTS` completion: signup, verification, reset, addresses, profile, tokens and deletion remain open as their own ids. |

## API reference served from this origin — bounded local acceptance verified

| Acceptance ID | Behavior | Evidence/status |
| --- | --- | --- |
| P1-API-DOCS-ORIGIN | `/api/docs` stops loading Swagger UI from unpkg. `crates/api/src/api_docs.rs` renders the reference from the same `serde_json` value `/openapi.json` returns — title, version, and per path every method with its summary, required scopes, parameters and response codes — into `crates/web/templates/api_docs.html`, so the page cannot drift from the document it describes. It links `/openapi.json` for machine consumers and `/web/style.css` for styling, both from this origin, contains no script element, and carries the browser security headers (`no-store`, `strict-origin`, `nosniff`, `DENY`) with the same strict CSP the browser surface uses (`default-src 'none'; style-src 'self'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'`). The source-level markup ban from `P4-SHELL` now covers every module of `listmngr-api`, not only the browser modules. | **Bounded local acceptance verified.** RED first: `crates/api/tests/api_docs.rs` (3) failed 3/3 before the work (`cargo test --locked -p listmngr-api --test api_docs`), and covers that the page names no third-party origin, CDN, script element or the embedded explorer; that it carries the four security headers and the exact CSP; and that it contains every path and method of the live OpenAPI document (44+ paths) plus the bearer scheme. `crates/api/tests/webui_shell.rs::browser_handlers_build_no_html_by_hand` was widened to the whole crate. `rest.rs::health_ready_metrics_and_openapi_are_public` still passes, so the route stays public and 200. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` PASS; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 150 binaries, 957 passed, 0 failed, 56 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS and `scripts/test-postgres-all.sh` 53 passed, 0 failed on one disposable PostgreSQL 14.24 role and database, dropped afterwards; `scripts/test-mailmanclient.py` PASS with the pinned 3.3.5 client; Chromium acceptance PASS unchanged. **Limits and deviations:** this replaces an interactive explorer with a read-only reference — a reader who wants to issue requests from a UI points their own client at `/openapi.json`, and the page says so; the page is English only (an operator surface, not a member one) and is not part of the Fluent catalog; it renders summaries, scopes, parameters and response codes, not request or response schemas, examples or the component models, which stay in `/openapi.json`; it is one long document with no search or deep links per operation. No change to any API route, authorization, validation or payload. |

## Browser shell and template rendering — bounded local acceptance verified

| Acceptance ID | Behavior | Evidence/status |
| --- | --- | --- |
| P4-SHELL | One rendering path for the browser surface. `listmngr-web` becomes the template crate: Askama 0.16 with compile-time auto-escaping through a project escaper that writes the same named character references the surface always wrote (`crate::HtmlEscaper`, `crates/web/askama.toml`), one `base.html` shell (document `lang`, skip link, landmark header/`main`/footer, navigation with `aria-current="page"` on the page being read) and 21 page templates; handlers build view models only. Every string in the shell and in all 21 pages comes from the shared Fluent catalog (`listmngr_i18n`, 147 new `web-*` ids in `en` and `vi`), negotiated per request from the reader's ordered `Accept-Language` preferences, then `site.default_language`, then English — never from the list, the `Host` or a forwarded header. The stylesheet moves to `crates/web/assets/style.css` with design tokens both colour schemes bind (`color-scheme: light dark` plus a `prefers-color-scheme: dark` block that redefines only the tokens). htmx 2.0.10 is vendored (0BSD) at `crates/web/assets/htmx.min.js`, served from this origin at `/web/htmx.min.js` and pinned by SHA-384; no page loads it yet, so pages keep `default-src 'none'` and stay script-free until a package needs progressive enhancement. The RFC 8058 one-click pages and the HyperKitty-compatible `/archives/{list}` page render through templates too, so no module on the browser surface concatenates markup. | **Bounded local acceptance verified.** RED first: `crates/api/tests/webui_shell.rs` (5) failed 5/5 before the work (`cargo test --locked -p listmngr-api --test webui_shell`), and covers the source-level ban on hand-built markup in every browser module, `Accept-Language` negotiation to `vi` with English fallback for an unshipped tag, the marked current navigation item, the design tokens and dark scheme, and htmx served from this origin while `/web` keeps its exact CSP and no `<script>`. `crates/web/tests/templates.rs` fails if any `shell.t("…")` id is missing from either catalog; `crates/web/tests/assets.rs` pins the htmx SHA-384 and asserts no asset refers to another origin; `crates/i18n/tests/catalog.rs` gained a resolution test over every placeable. The 85 existing `--test webui` assertions and the `web_*` suites passed unchanged, which is the byte-level parity evidence. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` PASS; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 149 binaries, 954 passed, 0 failed, 56 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean after bumping `rustls` 0.23.43 → 0.23.45 for RUSTSEC-2026-0285 (a lockfile-only transitive bump, unrelated to this behavior); `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS and `scripts/test-postgres-all.sh` 53 passed, 0 failed on one disposable PostgreSQL 14.24 role and database, dropped afterwards; `WEBUI_BROWSER_*` Chromium 140.0.7339.16 acceptance PASS with the existing flows unchanged plus the new shell checks (document language, one `aria-current`, a distinct dark scheme, a Vietnamese-negotiated page, the exact CSP header and origin-served htmx), and `AXE PASS: no critical or serious violations on 4 pages`. **Limits and deviations:** axe-core is passed in through `WEBUI_AXE_SCRIPT` rather than vendored, so a run without that variable prints `AXE SKIPPED` and scans nothing; only `critical` and `serious` impacts fail; the scan covers four representative pages, not all 21; only `en` and `vi` ship, and a signed-in user's stored `preferred_language` does not yet override `Accept-Language`; the delivery mode and status inside one account sentence are still raw wire values (`regular`, `enabled`) as before; the one-click page lost its `<strong>` emphasis so the sentence can stay one translatable unit; the HyperKitty-compatible archive page keeps its English literals and minimal document until Phase 5 renders it; `/api/docs` still builds its Swagger page in `crates/api/src/lib.rs` and loads `swagger-ui-dist@5` from unpkg — it is outside this row and outside the scan, and needs its own acceptance ID. No new route, authorization, validation, transaction or delivery behavior. Not Phase 4 completion. |

## Chain and rule engine — bounded acceptance verified

| Acceptance ID | Behavior | Evidence/status |
| --- | --- | --- |
| P2-TEMPLATES | Mailman-named notice templates: the 29 `docs/PLAN.md` §4.9 names plus listmngr's `list:user:notice:help` and `list:user:notice:receipt`, an English built-in catalog ported from Mailman 3 (`listmngr_mail::templates`), `$placeholder`/`${placeholder}`/`$$` expansion with Python `string.Template.safe_substitute` semantics, `mailman:///`, `file:///` and `https://` URI parsing with bounded UTF-8 file loading, and resolution list → domain → site → built-in with per-scope language fallback to `en` (`listmngr_db::templates`). A stored template that cannot be loaded is logged (name and scope only) and skipped, so notices never block the subscription or moderation that needs them. All eight hardcoded notices — confirmation challenge (`list:user:action:subscribe`/`unsubscribe`), email-command help, welcome, goodbye, confirmation receipt, moderator rejection, owner bounce disable/increment/removal, and the disabled-member warning — now render through templates via one shared serializer (`listmngr_db::notices`) with ASCII-validated transport headers, RFC 2047 subjects and base64 bodies when needed. New hold notices: `list:user:notice:hold` to a safe non-list envelope sender when `respond_to_post_requests`, and `list:admin:action:post` to every owner/moderator when `admin_immed_notify` (migration `0030`, both default true as in Mailman), enqueued in the hold transaction. Mailman's `/uris` resource on list, domain and site scopes (`GET`, `PATCH` name=uri pairs with optional credentials, `PUT` replace, `DELETE`, and `/uris/{name}` `GET`/`PATCH`/`DELETE`) on both prefixes, plus `PUT`/`DELETE /lists/{id}/templates/{name}` inline bodies with a language. Subjects follow Mailman's wording. | **Bounded local acceptance verified.** `crates/mail/tests/templates.rs` (6) covers catalog completeness, expansion semantics, URI parsing and loaders; `crates/db/src/notices.rs` unit tests (4) cover 7bit/base64 selection, RFC 2047 subjects, header-injection resistance and unsafe envelopes; `crates/db/tests/templates.rs` (5) covers scope/language precedence, credential redaction in audit, file loading, fallback on a missing file, bounds and CRLF normalization; `crates/db/tests/hold_notices.rs` (4) covers both hold notices, their switches, unsafe/list-owned senders and a list-scoped Vietnamese override; `crates/api/tests/rest/template_uris.rs` (4) covers every scope and prefix, form and JSON, validation, and scope authorization; existing welcome/goodbye/bounce/rejection/e2e tests were updated to the Mailman wording and, where they count post fan-out, to exclude or disable hold notices. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 116 binaries, 748 passed, 0 failed, 50 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Limits and deviations:** `https://` templates are accepted and stored but never fetched (network I/O inside notice transactions would violate the single-transaction invariant), so they resolve to the next candidate; the built-in catalog shipped only in English at this WP (Vietnamese and per-recipient language selection arrived with `P3-I18N`); built-in bodies are ported from memory of Mailman 3.3's `en` templates with product wording adjusted, not byte-identical; the `$listinfo_uri`/`$user_options_uri` placeholders are not yet supplied (no base URL in the database layer); the hold notice to the poster is a known backscatter vector that operators can switch off per list; member/nonmember hold reasons still read `moderation policy`; the confirmation `Subject` must stay `confirm TOKEN`. Not Phase 2 completion. |
| P3-I18N | Recipient-language notices. A Fluent catalog crate (`listmngr_i18n`: `en` and `vi` embedded, `negotiate` by exact tag then primary subtag, `choose` over an ordered preference list, `message` with `en` and then id fallback) plus a Vietnamese built-in template catalog (`listmngr_mail::templates::builtin_in`). Every generated notice decides its language inside the producer's transaction from the member's `preferred_language` (member → address → user layers), then the list's `preferred_language`, then `site.default_language` (carried by `Database::with_default_language`), and renders both subject and body in it; template resolution tries each scope in that language then `en`, and the built-in falls back to English only when no catalog serves the tag. Owner and moderator hold notices are rendered per recipient. The machine-parsed `confirm TOKEN` subject is literal in every catalog. Runners (`policy_facts`, `digests`) and the browser membership view resolve preferences against the site default instead of a hardcoded `en`. | **Bounded local acceptance verified.** `crates/i18n/tests/catalog.rs` (8) covers supported tags, negotiation, `choose`, argument substitution, fallback without panics, full `vi` coverage of every `en` id, the literal confirmation subject and the translated receipt action word; `crates/mail/tests/templates.rs` (6, one new) covers Vietnamese built-ins, regional-tag selection, English fallback for an unshipped language, and that every `vi` body keeps every `$placeholder` its `en` body uses; `crates/db/tests/notice_language.rs` (4) covers welcome in the list language, a member preference beating the list language, two owners receiving the same hold in different languages, and unsupported list/site languages falling to the site default and then English, plus `postgres_notice_language_contract` (owner-per-language scenario on an isolated schema, wired into `scripts/test-postgres.sh`); `crates/db/tests/templates.rs` asserts `builtin:en`/`builtin:vi` sources. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 119 binaries, 760 passed, 0 failed, 51 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Limits and deviations:** only `en` and `vi` ship (Mailman's 40+ `.po` languages are Phase 6); the Vietnamese bodies are original translations, not Mailman's; language is negotiated per catalog, not per template, so an operator template stored for `en` still wins over the `vi` built-in at its scope; no browser or REST surface lists the supported languages yet; the SPA (Phase 4) will consume the same catalogs. Not Phase 3 completion. |
| P2-LIST-SETTINGS | Mailman's remaining list configuration groups as persisted, validated, audited settings (migration `0031`): *Alter Messages* — `filter_content`, `filter_types`, `pass_types`, `filter_extensions`, `pass_extensions`, `collapse_alternatives`, `convert_html_to_plaintext`, `filter_action` (`discard`/`reject`/`forward`/`preserve`), `include_rfc2369_headers`, `allow_list_posts`, `reply_goes_to_list` (`no_munging`/`point_to_list`/`explicit_header`/`explicit_header_only`), `reply_to_address`, `first_strip_reply_to`, `personalize` (`none`/`individual`/`full`), `include_sender_header`; *Member Policy* — `subscription_policy` and `unsubscription_policy` (`open`/`confirm`/`moderate`/`confirm_then_moderate`), `member_roster_visibility` (`public`/`members`/`moderators`); DMARC — `dmarc_addresses`, `dmarc_moderation_notice`, `dmarc_wrapped_message_text`; Bounces — `forward_unrecognized_bounces_to` (`discard`/`site_owner`/`administrators`). Defaults are Mailman's `BasicOperation` style. Every value is validated before the single-transaction write with its `list.config` audit event: enums by wire name, booleans strictly boolean in JSON, `reply_to_address` empty or a mailbox, MIME types `type` or `type/subtype` and extensions as printable ASCII tokens stored lowercase, `dmarc_addresses` exact addresses or compiling `^` regexes, free text at most 64 KiB. The list configuration resource exposes them on both prefixes for `GET`, `PATCH` and `PUT` (which resets omitted settings to defaults), including mailmanclient's form encoding: repeated keys become lists, one value is a one-element list, an empty value clears, and Python's `True`/`False` are accepted — this repeated-key support now also covers the existing address-list settings. | **Bounded local acceptance verified.** `crates/db/tests/alter_messages_settings.rs` (1 SQLite + 1 PostgreSQL contract) covers Mailman defaults, a full round trip with audit, clearing, and 18 rejected patches that leave the row unchanged; `crates/api/tests/rest/alter_messages.rs` (3) covers defaults, JSON round trip and `PUT` reset, mailmanclient form encoding (repeated keys, Python booleans, empty clears, single values) and atomic rejection on both prefixes and both methods; `crates/db/tests/schema_contract.rs` and `repositories.rs` chain the `alter-messages-schema.snapshot` fixture; the OpenAPI contract tests cover the new `ListConfigResponse`/`ListConfigInput` fields. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 120 binaries, 764 passed, 0 failed, 52 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Limits and deviations:** these are settings only — no handler reads them yet (`mime-delete` is P2-MIME-DELETE, `decorate`/`acknowledge` P2-HANDLERS-DECORATE, `Reply-To`/`Sender`/RFC 2369 gating P2-COOK-HEADERS, personalization P2-PERSONALIZE-VERP, subscription policies P3-SUBSCRIPTION-POLICY, unrecognized-bounce forwarding P3-BOUNCE-RUNNER, DMARC wrapping Phase 6); the browser settings form does not expose them (Phase 4 web UI, SSR per ADR-0002/0004); Mailman does not validate MIME tokens or `reply_to_address` at all, so listmngr is stricter; `explicit_header` policies with an empty `reply_to_address` are accepted as Mailman core does (Postorius adds that check in its form). Not Phase 2 completion. |
| P2-MIME-DELETE | Mailman's `mime-delete` content filter as the first handler of `default-posting-pipeline`, over the stored bytes (`listmngr_mail::mime_delete`): the outer type then file extension are checked with Mailman's four reasons, subparts are pruned recursively by `filter_types`/`pass_types` (`type` or `type/subtype`) and `filter_extensions`/`pass_extensions` (case-insensitive on the attachment file name), a multipart emptied by filtering is dropped whole, `collapse_alternatives` replaces every `multipart/alternative` with its first alternative (an outer alternative keeps the message headers and takes the part's content headers, as `reset_payload` does), `convert_html_to_plaintext` turns every `text/html` leaf into `text/plain; charset=utf-8` through a built-in HTML-to-text projection (block structure, `- ` list items, `text <href>` links, entities, no scripts/styles), and `X-Content-Filtered-By: listmngr/mime-delete <version>` marks a changed message. Kept parts are byte-identical; only the multipart framing is regenerated. When nothing deliverable remains the pipeline ends with the list's `filter_action`, applied by the `in` runner in one lease-fenced transaction (`ModerationRepo::refuse`): `discard` acks silently, `reject` acks and sends `list:user:notice:rejected` with the reason to a safe author address, `forward` acks and sends the moderators (the owners when there are none) a `multipart/mixed` notice with the unfiltered original attached as `message/rfc822`, and `preserve` shunts the job with the reason when `[mailman] filtered_messages_are_preservable = true` (else it is a discard, as in Mailman). Each leaves a `post.discard`/`post.reject`/`post.forward`/`post.preserve` audit event. The chain's own `reject` and `discard` outcomes now take the same path, so a rejected post finally tells its author why. | **Bounded local acceptance verified.** `crates/mail/tests/mime_delete.rs` (12) covers the off switch, outer-type and extension reasons, byte-identical survivors, main-type matches, `pass_types`, extensions, nested pruning, both collapse shapes, HTML conversion (nested and single-part), determinism/idempotence, unparsable input and hostile HTML (multibyte entities, quotes in attributes, unterminated tags, CDATA, comments); `crates/runners/tests/content_filter.rs` (7) covers filtered fan-out for every consumer, all four `filter_action`s including the moderators-then-owners roster and the preservable switch, and chain reject/discard notices and audit; `crates/pipeline/tests/handlers.rs` and `crates/api/tests/rest.rs` pin the handler order. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 122 binaries, 783 passed, 0 failed, 52 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Limits and deviations:** attached `message/rfc822` parts are leaves (Mailman recurses into them); HTML conversion is a built-in projection, not `lynx`, so output differs from Mailman's; preambles/epilogues of rewritten multiparts are dropped; a `forward` with no moderators goes to the owners rather than nowhere; the forward notice text is a catalog string, not a template; digests are not re-filtered (the digest runner consumes already-filtered copies); a settings change between admission and a consumer's cook can still refuse at cook time, which shunts that copy. Not Phase 2 completion. |
| P2-HANDLERS-DECORATE | Four more Mailman handlers. `decorate` (`listmngr_mail::decorate`) adds the `list:member:regular:header`/`footer` templates, resolved list → domain → site → built-in in the list's language and expanded with the list placeholders, to the subscriber copy only — applied at delivery by the out runner after `to-outgoing`, as Mailman's `DecoratingDelivery` does, so archive and digest copies stay undecorated; a single `text/plain` body is decoded and concatenated then re-encoded as UTF-8 (7bit or quoted-printable) with its `format`/`delsp` parameters kept, a `multipart/mixed` gets inline `text/plain` parts spliced first and last with every existing part byte-identical, anything else (alternatives, HTML, undecodable charsets) is wrapped in a new `multipart/mixed` keeping the message headers outside and the content headers inside; Mailman's default footer ships as the built-in. `tagger` (`listmngr_mail::topics` + `listmngr_pipeline::topics`) matches each topic's multi-line pattern (lines are case-insensitive alternatives) against every `Subject:`/`Keywords:` header and the leading `Subject:`/`Keywords:` pseudo-header lines of the body up to `topics_bodylines_limit` (negative all, zero none, the first ordinary line stops the scan) and adds `X-Topics: a, b`; settings `topics_enabled`, `topics_bodylines_limit` and `topics` (`{name, pattern, description}`, migration `0032`) are validated (unique names, compiling patterns, bounded text) and exposed on the config resource as a listmngr extension. `after-delivery` bumps `post_id` and stamps `last_post_at` in the transaction that acknowledges the inbound job (`MailQueueRepo::complete_accepted`). `acknowledge` sends `list:user:notice:post` from the bounces address, in the poster's language, when the poster is a member whose resolved `acknowledge_posts` preference is on. The posting pipeline is now Mailman's order minus `validate-authenticity`, `avoid-duplicates`, `to-usenet`, `arc-sign`. | **Bounded local acceptance verified.** `crates/mail/tests/decorate.rs` (6) covers no-op, plain-text concatenation with RFC 3676 parameters and header/footer separators, charset re-encoding, `multipart/mixed` splicing, wrapping of alternatives/HTML with content headers moved inside, and undecodable charsets; `crates/mail/src/encoding.rs` unit tests (2) cover 7bit/quoted-printable selection and soft breaks; `crates/pipeline/src/topics.rs` unit tests (2) and `crates/mail/tests/tagger.rs` (3) cover alternatives, case-insensitivity, header and pseudo-header sources, the body-line limit and stop rule, and every consumer seeing the tag; `crates/db/tests/topics_settings.rs` (1) and `crates/api/tests/rest/alter_messages.rs` (+1) cover defaults, round trip, audit, ten rejected patches and form/JSON/PUT behaviour; `crates/runners/tests/post_effects.rs` (3) covers the post counter, the acknowledgement in Vietnamese from the bounces address, and delivery-only decoration with an operator header and the built-in footer; transport-level runner fixtures store an empty footer so their byte-identity assertions keep meaning. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 126 binaries, 801 passed, 0 failed, 52 ignored (one earlier run tripped the 3 s FIFO timeout of `crates/cli/tests/queue.rs` under full parallel load; it passes alone and on the recorded run); `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Limits and deviations:** decoration is not personalized yet (`$user_*` placeholders stay literal until P2-PERSONALIZE-VERP) and uses the list language, not each member's; `X-Topics` joins names with `, ` rather than Mailman's folded lines; Mailman keeps topics out of its REST API, so `topics*` on the config resource are an extension (JSON only for `topics`); the acknowledgement template no longer carries the `$listinfo_uri`/`$user_options_uri` lines (no web URLs in the mail layer yet); `post_id` is bumped on acceptance as Mailman does, not on delivery. Not Phase 2 completion. |
| P2-COOK-HEADERS | `cook-headers` and `rfc-2369` completed to Mailman's set. `rfc-2369` (`listmngr_mail::list_headers`) emits `List-Id` (`description <list_id>`, quoted or RFC 2047 encoded as `formataddr` would, else `<list_id>`), `List-Help` (`<mailto:-request?subject=help>`), `List-Unsubscribe`, `List-Subscribe`, `List-Post` (`NO` when `allow_list_posts` is off), `List-Owner` (RFC 2369, which Mailman omits), and — when the list archives and `site.base_url` is configured — `List-Archive` (`{base}/archives/list/{list_id}/`) and `Archived-At` (`{base}/archives/list/{list_id}/message/{hash}/`, HyperKitty's URL shapes); `include_rfc2369_headers = false` drops the whole set, inbound `List-*`/`Archived-At` are replaced. `cook-headers` rewrites `Sender` to the bounces address when `include_sender_header` (else the inbound `Sender` is kept), applies the `Reply-To` policy (`listmngr_mail::reply_to`: inbound addresses kept unless `first_strip_reply_to`, `point_to_list` adds the posting address, `explicit_header` adds `reply_to_address`, `explicit_header_only` keeps only `reply_to_address`, deduplicated case-insensitively, display names re-emitted as phrases with RFC 2047 encoding; anonymous lists point at themselves first), and adds `X-Mailman-Version: listmngr <version>`, `Message-ID-Hash` and `X-Message-ID-Hash` (base32 SHA-1 of the `Message-ID`, the archive permalink key). `Database` carries `site.base_url` (`with_base_url`) so the out runner and the archive renderer cook with `cook_for_site`; every consumer (out, archive, digest) sees the same headers. | **Bounded local acceptance verified.** `crates/mail/tests/cook_headers.rs` (8) covers the full set with archive URLs and the exact permalink hash, `List-Id` quoting/encoding, the `archive_policy = never` and no-base-URL cases, identical headers on every consumer, both RFC 2369 switches, replacement of inbound `List-*`/`Archived-At`/`X-Mailman-Version`, `Sender` rewriting and its switch, all nine `Reply-To` policy/strip combinations, deduplication and RFC 2047 names, and anonymous lists; `crates/runners/tests/post_effects.rs` (+1) covers the delivered copy with and without a configured base URL; `crates/mail/tests/cook_characterization.rs` keeps the A3 capture and lists the six headers added since it. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 127 binaries, 810 passed, 0 failed, 52 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Limits and deviations:** `List-Unsubscribe-Post` (RFC 8058) waits for the HTTPS one-click endpoint (B5); `List-Owner` is a superset of Mailman's output; digests still use the older `listmngr_pipeline::list_headers` builder (no `List-Help`/`List-Owner`/archive URLs) until the digest work package (P3-DIGEST-SETTINGS); the archive URL shapes are committed here ahead of the SSR archive (Phase 5), which must serve them; Mailman's `Cc` handling for `point_to_list` is not reproduced; the `X-Mailman-Version` value names listmngr rather than a Mailman version. Not Phase 2 completion. |
| P2-ONE-CLICK-UNSUBSCRIBE | RFC 8058 one-click unsubscribe. Personalized lists (`personalize` ≠ `none`, as Mailman requires, since the URI must name the recipient) are delivered one SMTP transaction per recipient; each copy carries `List-Unsubscribe: <https://{base}/unsubscribe/{list_id}?token=…>, <mailto:…-leave@…>` and `List-Unsubscribe-Post: List-Unsubscribe=One-Click`, signed per copy, when `site.base_url` is configured and the recipient is a member. The token (`listmngr_core::one_click`) is an HMAC-SHA-256 over the list, the membership id and an expiry (90 days) under a site key generated once into `site_secrets` (migration `0033`; delete the row to rotate) — it never names the address, and the request logger records only the path. `POST /unsubscribe/{list_id}?token=…` with the body `List-Unsubscribe=One-Click` (form-encoded, nothing else) removes the membership at once with the list's goodbye notice and a `member.unsubscribe.one_click` audit event in one transaction, rate-limited per client IP; `GET` renders a zero-JS confirmation page (strict CSP, no session, no cookie) whose form posts the same pair; invalid, foreign-list, expired, non-member and already-redeemed tokens are not found and change nothing. Unpersonalized lists keep the single shared transaction and the mailto-only `List-Unsubscribe`. | **Bounded local acceptance verified.** `crates/core/src/one_click.rs` unit tests (2) cover round trip, list binding, key binding, expiry, tampering and the URL shape; `crates/api/tests/one_click_unsubscribe.rs` (4) covers the RFC `POST` with goodbye and audit, reuse, the `GET` page and its form, six rejected requests (wrong body, missing/garbage/foreign/expired token, unknown list), member-only links, canonical addresses and single key generation; `crates/mail/tests/cook_headers.rs` (+1) covers the header splice and the RFC 2369 switch; `crates/runners/src/outbound_tests.rs` (+2) drive the out runner against the SMTP sink: one transaction and a distinct, redeemable link per recipient for a personalized list, one shared transaction with the mailto-only header otherwise; the schema snapshot corpus pins `site_secrets`. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 128 binaries, 819 passed, 0 failed, 52 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Limits and deviations:** one-click redemption ignores `unsubscription_policy` (the RFC forbids further interaction; Postorius routes such requests through moderation when the policy asks); the per-recipient copy differs only by these two headers until the personalize work package (P2-PERSONALIZE-VERP) adds VERP, `$user_*` placeholders and `To:` rewriting; per-recipient copies re-sign DKIM once per recipient; the token TTL is fixed at 90 days; the confirmation page is English only. Not Phase 2 completion. |
| P2-PERSONALIZE-VERP | Mailman's `personalize` and VERP. `personalize = individual` delivers one signed copy per recipient with the decoration templates expanded per member (`$user_email`, `$user_address`, `$user_delivered_to`, `$user_name`, `$user_language`, `$member`, on top of the list placeholders; `listmngr_mail::personalize`), plus the one-click pair; `full` also rewrites `To:` to the recipient (`Name <address>`, RFC 2047 encoded). The member profile (`DeliveryRepo::recipient`) carries the address as written, the membership's or address's display name and the negotiated language. VERP (`listmngr_core::verp`): `[mta] verp_format` (`{bounces}+{local}={domain}`) and `verp_delimiter` are validated at load; `verp_personalized_deliveries` gives personalized copies a per-recipient envelope sender `list-bounces+local=domain@host`, and `verp_delivery_interval = N` delivers every Nth post (`post_id % N == 0`) of any list one recipient per transaction with VERP senders and identical bytes. Intake recognizes VERP bounce addresses (`decode`: bounces part up to the first delimiter, recipient local part up to the last `=`), queues them on `bounces` with `verp_recipient` in the context for the bounce runner, and `aliases regen` adds one `list-bounces\\+[^@=]+=[^@=]+@host` pattern per list to the recipient and transport maps. | **Bounded local acceptance verified.** `crates/core/src/verp.rs` unit tests (2) cover encode/decode round trip, `=` in a local part, malformed inputs and configuration validation; `crates/mail/tests/personalize.rs` (2) covers the `$user_*` placeholders and the `To:` rewrite with encoded names and header safety; `crates/runners/src/outbound_tests.rs` (+2) drive the out runner against the SMTP sink: `full` with `verp_personalized_deliveries` (per-recipient `MAIL FROM`, `To:`, expanded footer, one-click pair) and `verp_delivery_interval` splitting an ordinary post without personalizing it and leaving the next post shared; `crates/runners/tests/bounce_inbox.rs` covers VERP bounce intake (recipient canonicalized, non-VERP plus addresses still refused); `crates/cli/tests/aliases.rs` pins the map rows. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 129 binaries, 825 passed, 0 failed, 52 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Limits and deviations:** `$user_optionsurl` stays literal (no member options URL yet); `$user_language` is a language tag, not Mailman's description; `verp_confirmations`/`verp_probes` wait for the workflow and bounce work packages (P3-SUBSCRIPTION-POLICY / P3-BOUNCE-RUNNER); the Postfix snippet keeps `recipient_delimiter` empty so the generated patterns see the full address; the VERP format uses `{name}` placeholders where Mailman writes `${name}`; a `verp_delivery_interval` VERP delivery still shares one DKIM signature per copy (signed per transaction). Not Phase 2 completion. |
| P2-DELIVERY-POLICY | Outbound transaction sizing and retry policy (`listmngr_runners::delivery_policy`). Shared deliveries are cut into transactions of at most `[mta] max_recipients_per_transaction` recipients (Mailman's `max_recipients`, default 500, replacing the previous one-or-all switch) with recipients of the same domain adjacent (a stable sort by domain, as Mailman's `chunkify` intends), while `smtp_single_recipient`, personalized copies and VERP keep one recipient per transaction; every outcome is written back to its own recipient regardless of chunk order. Transient failures — relay unreachable, greeting/TLS failure, 4xx recipients, dependency lookups, and `in` processing errors — now back off exponentially from `[mta] retry_initial_secs` (10 s) doubling per attempt to `retry_max_secs` (1 h) with ±20% jitter, instead of the fixed 10 s/5 s; both bounds are validated at load. `listmngr queue stats` prints depth per queue and state, the shunted total and the age of the oldest ready job (metadata only). Mailman's `virgin` and `bad` queues are represented by the existing `virgin` pipeline (generated mail enqueues straight to `out`) and the `shunt` queue (`queue unshunt` replays with a fresh budget), so no new runner was added. | **Bounded local acceptance verified.** `crates/runners/src/delivery_policy.rs` unit tests (2) cover doubling, the cap, the jitter bounds and the one-second floor, and domain grouping with full coverage of the roster at sizes 0/1/2/500; `crates/runners/src/outbound_tests.rs` (+2) drive the out runner against the SMTP sink: three recipients over two domains at a limit of two arrive as `[a, c]` then `[b]` with one shared signed copy and every recipient marked sent, and a refused relay leaves the job ready with a first delay of 10 s ±20% and a second of 20 s ±20% on the next attempt; `crates/runners/src/signing_failure_tests.rs` asserts the jittered floor; `crates/cli/tests/queue.rs` (+1) covers `queue stats` on an empty and a populated queue without exporting message content. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 129 binaries, 830 passed, 0 failed, 52 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Limits and deviations:** the attempt budget (`max_attempts`, 8 for `out`) is unchanged, so the longest retry horizon is about 21 minutes rather than Mailman's `delivery_retry_period` of days — a per-queue budget is left for the metrics/operations work; the retry delay is per job, not per recipient; `queue stats` is a CLI/JSON surface, the HTTP status probes gain it with B10 metrics. Not Phase 2 completion. |
| P2-VALIDATE-AUTHENTICITY | Mailman's `validate-authenticity` and a real `dmarc-mitigation` rule. With `[mta] authenticity_checks = true` the `in` runner verifies DKIM, SPF (client IP and HELO read from the topmost `Received:` header the operator's MTA wrote, `MAIL FROM` from the stored envelope) and DMARC (RFC 9989 tree walk) through `mail-auth` against the system resolver (`listmngr_mail::authenticity::Verifier`), records the RFC 8601 `Authentication-Results` value and the From domain's policy on the stored message context at acceptance, and the pure `validate-authenticity` handler — first in the pipeline — writes the header on every copy after removing inbound fields that claim the same authserv-id. The chain gains Mailman's first link: the `dmarc-mitigation` rule tags a post when the list mitigates and the policy is `reject`/`quarantine` (or `dmarc_mitigate_unconditionally`, or the From address is in `dmarc_addresses`), and hits — jumping to the new `dmarc-mitigation` terminal chain, which rejects or discards with `dmarc_moderation_notice` (default: Mailman's wording) — only for the `reject` and `discard` actions, now accepted values of `dmarc_mitigate_action` (migration `0034` recreates the two columns without the `munge_from`-requires-unconditional rule, values preserved). The `dmarc` handler munges when the post was tagged or the list is unconditional; with checks off, only unconditional lists and `dmarc_addresses` mitigate. | **Bounded local acceptance verified.** `crates/mail/tests/authenticity.rs` (3) drive `mail-auth` with a seeded TXT cache and an owned RSA key: a signed post passes DKIM, SPF and DMARC with the exact header fields, `reject` and `quarantine` are restrictive and `none` is not, a tampered body reports the body-hash failure without a DMARC pass, no client skips SPF, an absent record yields `dmarc=none`, and unparsable input yields no header; `crates/mail/src/authenticity.rs` unit test covers `Received:` parsing (IPv4, IPv6 literal, missing); `crates/pipeline/tests/rules.rs` (+1) covers tagging by policy/unconditional/address list and the reject/discard jump with the default and custom notice; `crates/runners/tests/authenticity.rs` (4) run the `in` runner with a seeded verifier: conditional munging with the header on the delivered copy, an unmunged delivery for a domain without a policy, `reject`/`discard` with audit and the author's notice, and the checks-off behaviour; `crates/db/tests/dmarc.rs` and the REST/OpenAPI tests cover the widened action set and conditional settings; the schema snapshot corpus is unchanged by the column rebuild. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 131 binaries, 839 passed, 0 failed, 52 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Limits and deviations:** checks are off by default and use the system resolver only (no DoH, no cache — every post queries DNS); only the first `Received:` hop is trusted for SPF, so a message injected without one gets no SPF result; `wrap_message` and ARC sealing wait for Phase 6; the archive's read-time rendering omits `Authentication-Results` because it has only the bytes; Mailman's `$listowner` in the default notice is not expanded. Not Phase 2 completion. |
| P2-MTA-INTEGRATION | Mailman's `IMailTransportAgentAliases` running for real. `[mta] incoming = none|postfix|exim` with `map_directory`, `lmtp_map_target` (IP literal or DNS name), `transport_file_type = regex|hash`, `postmap_command`, `map_permissions = owner|group|world` and `map_generations_kept`; `listmngr_mail::mta::MapWriter` renders Postfix `regexp` maps (unchanged rows), Mailman's `postfix_domains`/`postfix_lmtp` hash files compiled by `postmap` inside the staging directory, or Exim `exim_domains`/`exim_recipients` lsearch files, publishes an immutable `generation-<uuid7>`, swaps a relative `current` symlink and prunes old generations. Maps are published at `serve` startup (failure aborts startup), after REST and CLI list creation/removal (failure logged, list change stands) and by `aliases regen` (`--output`, `--lmtp-target HOST:PORT`, `--mta`; Postfix when `incoming = none`). `COMMAND_SUFFIXES` moved to the mail crate (re-exported by runners). Compose gains a Postfix front MTA (`deploy/postfix/Dockerfile`: digest-pinned Alpine 3.22, `postfix=3.10.13-r0`, `postfix` user in a GID-1000 `listmngr` group; `main.cf` relay-domain configuration on `/var/lib/listmngr/mta/current/`; entrypoint waits for the first generation and reloads on `current` changes) on a static-address network, listmngr with the mail role on, LMTP unpublished, maps on a shared volume owned by UID 1000 (the scratch image now chowns `/var/lib/listmngr`), and outbound through Postfix as `plaintext_trusted_relay`; `deploy/exim/listmngr.conf` is a real router/transport set. | **Bounded local acceptance verified.** `crates/mail/tests/mta_maps.rs` (7) cover the three formats, `LmtpTarget` parsing, generation/symlink/permission/prune behaviour, a stand-in `postmap` and its failure leaving the published generation selected, and `[mta]` validation; `crates/cli/tests/aliases.rs` (+2, 7 running) cover the Exim flag/config path and regeneration after `lists create/remove` including the warn-and-continue path; the ignored `POSTMAP_BIN` tests (2) pass with the system `postmap` (`/usr/sbin/postmap`, regexp lookup matrix unchanged, hash compile + `hash:` lookup matrix); `crates/api/tests/rest/mta_maps.rs` (2) cover REST create/delete regeneration and the `none` default; `crates/cli/tests/mailpath_e2e.rs` asserts startup publication of the process under test. `scripts/check-mta-configs.sh` PASS: the Postfix image accepts `alpha@` and `alpha-bounces+a=b@`, refuses `nobody@` and `alpha+x@` with 550 and refuses relaying from outside `mynetworks` with 554; the Exim routers (`exim=4.98.2-r0` in Alpine) route the same three and fail the three others plus a foreign domain with `exim -bt`. `docker compose --env-file … -f deploy/docker-compose.yml config` OK; `docker compose -p listmngr-b9 --env-file <disposable copy of .env.example> -f deploy/docker-compose.yml build` then `up -d --wait` (all three services healthy, listmngr logged `MTA maps published`), `exec listmngr /listmngr domains add` / `lists create` (logged `MTA maps regenerated`, Postfix logged `refreshing the Postfix mail system` within 5 s and `postmap -q dev@lists.example.invalid regexp:…/current/transport.regexp` answered `lmtp:[172.28.0.10]:8024`), `members add` ×2, then a Python `smtplib` session to `127.0.0.1:2525`: the post was accepted, `nobody@lists.example.invalid` refused with 550, Postfix logged `lmtp … status=sent (250 2.1.5 accepted for delivery)`, listmngr's outbound copy arrived back at Postfix from `dev-bounces@lists.example.invalid` for both members (bounced by DNS as expected for `.invalid`), and the resulting DSN was handed to `dev-bounces@` over LMTP; `down -v` afterwards. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 132 binaries, 850 passed, 0 failed, 53 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Limits and deviations:** the Compose run proves SMTP→LMTP→outbound→bounce intake on this workstation, not deliverability from a real host (DNS, DKIM keys, port 25 exposure and `POSTFIX_RELAYHOST` are the operator's); Postfix reads regexp tables at daemon start, so a host installation must `postfix reload` after a generation switch (the Compose entrypoint does); hash mode leaves VERP to `recipient_delimiter = +`, which also admits `list-bounces+anything`; the Exim files carry no LMTP target (it is in the transport); Alpine Postfix has no `hash` type; map publication is not part of the list transaction; no automatic regeneration on domain changes (lists carry the domains). Not Phase 2 completion. |
| P2-METRICS | `/metrics` reports the mail path in Prometheus text: `listmngr_lmtp_recipients_total{result}`, `listmngr_posts_total{disposition}`, `listmngr_delivery_recipients_total{result}`, `listmngr_smtp_transactions_total{result}`, the histograms `listmngr_smtp_transaction_seconds` and `listmngr_delivery_latency_seconds` (LMTP acceptance → relay acceptance), and the gauges `listmngr_queue_jobs{queue,state}`, `listmngr_queue_shunted_jobs`, `listmngr_queue_oldest_ready_age_seconds` from `MailQueueRepo::stats`, cached five seconds. `listmngr_core::metrics` is a dependency-free registry (`LabeledCounter`, `Histogram`, `global()`); the LMTP handler, `process_one` and the out runner record outcomes where they become final. | **Bounded local acceptance verified.** `crates/core/tests/metrics.rs` (3) cover counter rendering with declared-only values, cumulative histogram buckets/sum/count with clamping, the full registry exposition and the global instance; `crates/runners/src/metrics_tests.rs` (2) drive `deliver_one` against a sink and against a relay that closes before greeting, asserting transaction/recipient/latency counts move (lower bounds, the registry is process-wide); `crates/api/tests/phase0_security.rs` (metrics test extended) enqueues two `in` jobs and shunts one, parses the scrape with `prometheus_parse`, checks the gauge values, every counter family, the histogram type, and that the gauges are cached. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 133 binaries, 855 passed, 0 failed, 53 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Limits and deviations:** counters and histograms are per process and reset on restart (multi-replica deployments need per-instance scraping); no HTTP request metrics, digest/archive/bounce runner metrics or per-list labels; `/metrics` remains unauthenticated as before; the latency histogram measures the first relay acceptance of a delivery, retries of remaining recipients are not attributed separately. Not Phase 2 completion. |
| P2-LMTP-PARAMETERS | The LMTP listener honours the ESMTP parameters it announces and refuses the rest. `MAIL FROM` accepts `SIZE=` (RFC 1870; over `mta.max_message_bytes` → `552 5.3.4` before `DATA`, at the limit → accepted) and `BODY=7BIT|8BITMIME` (RFC 6152, case-insensitive, any order); an unusable value is `501`; `BODY=BINARYMIME` (no CHUNKING), DSN's `RET`, a bare keyword and every other unannounced parameter are `555 5.5.4`, as is any `RCPT TO` parameter (`NOTIFY`, `ORCPT`) — checked before the transaction opens, so a refusal leaves no half-built transaction. Outgoing mail declares `BODY=8BITMIME` when the payload has 8-bit octets and the relay announced the extension (read from `EHLO`, or from `auth::Capabilities` on the authenticated path); a relay without it never receives the message, every recipient resolving as a transient failure before `MAIL FROM`. | **Bounded local acceptance verified.** `crates/mail/tests/lmtp.rs` (+3) cover the size refusal and that the session continues afterwards, the whole parameter matrix above with its three reply classes, and an 8-bit body stored byte-for-byte after a `BODY=8BITMIME SIZE=` sender; `crates/mail/tests/smtp.rs` (+3) cover the declaration on an 8-bit body, its absence on an ASCII body, and the fail-closed transient status with no `MAIL FROM` sent to a relay that lacks 8BITMIME. The in-crate SMTP sink fixtures now announce 8BITMIME. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 133 binaries, 861 passed, 0 failed, 53 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Limits and deviations:** a declared `SIZE` is only an early gate — the `DATA` cap still decides, and a client that under-declares is refused there with `552`; no 8-bit downgrade (quoted-printable re-encoding) for a relay without 8BITMIME, so such a delivery retries until the attempt budget shunts it; `SMTPUTF8`, `CHUNKING`/`BDAT`, DSN and `AUTH` remain unannounced and unimplemented on the LMTP side; the declared body type is not recorded on the stored message (the outbound side re-inspects the bytes). Not Phase 2 completion. |
| P3-SUBSCRIPTION-POLICY | Mailman's four subscription policies decide what a public request becomes, and a moderator queue stands behind the two that hold. Migration `0035` adds `subscription_workflows.state` (`pending_confirmation`/`pending_moderation`/`closed`, existing consumed rows closed) with a `(list_id,state,created_at)` index; the expiry sweep skips `pending_moderation`. `subscription_policy` (join) and `unsubscription_policy` (leave) drive `WorkflowRepo::request`: `open` changes the roster in the request's own transaction with the list's welcome/goodbye notices and no token, `confirm` is unchanged, `moderate` parks the request with no mail to the requester, and `confirm_then_moderate` spends the token to prove the address and then parks it. `WorkflowRepo::pending(list)` and `decide(id, accept|reject|discard|defer, audit_context)` are the moderator's side, each committing the membership change, its notices and the audit event (`subscription.accept/reject/discard/defer`) together and refusing a request that is not waiting; `listmngr requests ls|accept|reject|discard|defer` exposes them. The per-address hourly cooldown now applies only to policies that produce a mail or a queue row, so an `open` member can leave right after joining. | **Bounded local acceptance verified.** `crates/db/tests/subscription_requests.rs` (5) cover open join and leave (roster changed, no token row, welcome then goodbye, `subscription.open`), a moderated join (nothing subscribed, no notice, one pending row, accept subscribes with the welcome notice, a second decision fails), `confirm_then_moderate` (confirmation mail, nothing pending until the token is spent, the spent token cannot be replayed, reject leaves no member), discard and defer on a moderated leave, and a pending request surviving an expiry sweep; `crates/cli/tests/requests.rs` drives the real binary end to end (listing, accept, reject, the roster afterwards, and exit code 7 on a decided request); the schema snapshot corpus gains `subscription-state-schema.snapshot` in both contract tests. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 135 binaries, 867 passed, 0 failed, 53 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Limits and deviations:** no notice yet tells the requester that a moderator is deciding, or that a request was rejected, and moderators are not notified that one arrived (P3 notices); REST `/lists/{id}/requests` and invitations with `pre_verified`/`pre_confirmed`/`pre_approved` are the next work packages, so the queue is CLI-only; decisions are audited as the system actor from the CLI (no operator identity yet); a rejected request keeps its row until expiry, so its address stays in the hourly cooldown; `moderate` does not deduplicate a request an existing member makes. Not Phase 3 completion. |
| P3-SUBSCRIPTION-REQUESTS-REST | Mailman's `/lists/{id}/requests` on `/3.1` and `/api/v1` (scope `moderation`): `GET` lists undecided requests oldest first in Mailman's shape (`email`, `list_id`, `token` = request id, `token_owner`, `type`, `request_date`, `self_link`, `http_etag`) with `token_owner` and `request_type` filters and the usual paging; `GET …/count`; `GET …/{token}` for one request, only through its own list; `POST …/{token}` with `action=accept|reject|discard|defer` and an optional `reason`, by form or JSON, `204` on success, `400` on an unknown action, `404` once decided. `WorkflowRepo` gains `TokenOwner`, `RequestFilter`, `get`, and `decide` now acts on subscriber-owned requests too (accept applies the change and spends the token) and records the reason; `listmngr requests ls` shows moderator-owned requests by default and every undecided one with `--all`. | **Bounded local acceptance verified.** `crates/api/tests/rest/requests.rs` (2) seed a moderated join and a token-pending leave and check the listing shape and order, both filters, the count, one request on both flavours, the foreign-list and unauthenticated refusals, then defer/unknown/accept by form with the count after each, the roster after acceptance, a JSON reject with its reason in the audit event, and `404` for decided requests on `GET` and `POST`; `crates/api/tests/openapi.rs` covers the four operations' schemas and request media; the C1a suites pass unchanged under the widened repository API. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 135 binaries, 869 passed, 0 failed, 53 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Limits and deviations:** the collection `http_etag` is the shared `phase1` placeholder while entry etags are real; Mailman's `display_name` is not carried (the workflow stores none); no `reason` reaches the requester (P3 notices); the paging is offset-based over the full in-memory result like the other list collections; `mailmanclient`'s `requests` helper is not exercised by `scripts/test-mailmanclient.py`. Not Phase 3 completion. |
| P3-ADMIN-SUBSCRIBE | `POST /members` is Mailman's registrar for `role=member`: `WorkflowRepo::subscribe` reads `subscription_policy`, decides whether the address still owes a confirmation (`!pre_verified`, or a confirming policy without `pre_confirmed`) and whether a moderator still owes a decision (a moderating policy without `pre_approved`), then either subscribes in the transaction (`201` with the member) or stores the request in the state that says whose move it is (`202` with `{token, token_owner, http_etag}`, the token being the REST handle `/requests/{token}`, never the mailed secret). `invitation=true` always asks the address, using Mailman's `list:user:action:invite` template, and counts as the approval. Migration `0036` adds `display_name` (carried onto the member row on completion, surfaced in the moderator queue and in `/requests` entries) and `pre_approved`, so an approval given up front survives the confirmation the subscriber still owes. Role records (owner, moderator, nonmember) stay immediate on both flavours, carry no flags and refuse `invitation`; banned addresses are refused. | **Bounded local acceptance verified.** `crates/db/tests/subscription_requests.rs` (+2) cover the three outcomes under `confirm_then_moderate` (held for the subscriber, held for the moderator, subscribed with the display name on the member row), that the REST handle differs from the mailed secret and that confirming hands the request to the moderator queue with the operator's display name, that a pre-approved confirmation skips the moderator, and that an invitation mails the invite template and subscribes on acceptance; `crates/api/tests/rest/subscribe.rs` (5) cover `201` when everything is pre-supplied, `202`+`subscriber` with one confirmation notice and a moderator accepting it straight away, `202`+`moderator` with no mail and the request visible in `count`, the invitation (template, `subscription.invite` audit) and the role-record and banned-address rules; `crates/api/tests/rest/bounce_notice.rs` updates the administrative-role contract. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 135 binaries, 876 passed, 0 failed, 53 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Limits and deviations:** `/api/v1` role records no longer require the workflow flags (they never were subscriptions) — a deliberate change from the Phase 1 contract; verification and confirmation share one token rather than Mailman's two steps, so `pre_verified` alone still mails a confirmation on a confirming list; an unsubscribe request cannot be created administratively (the public leave path and `DELETE /members/{id}` remain); no notice tells an invited address that its invitation was superseded, and declining is still just ignoring the mail; `mailmanclient`'s `subscribe()` token handling is not exercised by `scripts/test-mailmanclient.py`. Not Phase 3 completion. |
| P3-EMAIL-COMMANDS | Mailman's `echo` and `end`/`stop` at `list-request@`. `EmailCommand::Echo(String)` carries the rest of the line, bounded to 200 characters and refused when it holds control or bidirectional characters or an argument-less verb is given one; `EmailCommand::End` is the halt, accepted with no notice, no workflow row and no audit event, the job finishing rather than retrying. `help_owned` generalises into `bot_reply(BotReply)`, so `echo` answers through the same notice path, the same per-list-and-address hourly row in `email_help_requests` and the same site-wide `subscription_rate` window as `help`; the new `list:user:notice:echo` template and `notice-echo-subject` exist in both catalogs, and both help texts now list the new verbs. | **Bounded local acceptance verified.** `crates/mail/tests/commands.rs` (+1) covers `echo` with and without text, case and internal spacing, the 200-character limit either side, control and bidirectional rejection, `end`/`stop` with and without arguments, and a body whose first line halts; `crates/runners/tests/subscription_commands.rs` (+1) drives the LMTP handler and the `in` runner: an echo produces exactly one reply carrying its own text with the request address in `Reply-To`, no workflow row and a `subscription.echo` audit event, a following `help` from the same mailbox is refused by the shared hourly budget, and `end` leaves no queue job, no workflow and a finished command job; the template and catalog completeness tests cover the new entries. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 135 binaries, 878 passed, 0 failed, 53 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Limits and deviations:** still one command per message (subject, else the first nonblank text/plain line), so `end` halts a message that in Mailman would have run the commands above it, and there is no combined results reply; `echo` shares `help`'s cooldown row, so either verb consumes the hour for the other; no `who`, `lists`, `set` or moderator commands. Not Phase 3 completion. |
| P3-AUTORESPONDER | Mailman's Automatic Responses. Migration `0037` adds `autorespond_owner|postings|requests` (`none|respond|respond_and_discard`), the three `autoresponse_*_text` bodies (up to 64 KiB, placeholder-expanded, empty = built-in template) and `autoresponse_grace_period` (0..3650 days) to `mailing_lists`, plus `autoresponse_records(list_id,email,kind,responded_at)`; the settings round-trip on both config prefixes as JSON and forms and reset on `PUT`. `AutoresponseRepo::respond` answers a writer at most once per address, kind and grace period (0 answers always), never a null, malformed or list-owned writer, composing in the writer's language with `Auto-Submitted: auto-replied` and auditing `list.autoresponse`, and always returns the configured action. The `in` runner consults it before owner forwarding, the command bot or the posting chain, skips automatic mail (`allows_reply`), and on `respond_and_discard` refuses the original as a `replybot` discard. | **Bounded local acceptance verified.** `crates/db/tests/autoresponder.rs` (5): nothing sent under `none`; owner replies with the expanded configured text and `auto-replied`, suppressed inside the grace period per writer, sent to another writer and again after the period; `respond_and_discard` keeps discarding while quiet, with the built-in body and one audit event; a zero grace period answers every message with kinds counted separately and the table pruned; null, malformed and list-owned writers are never answered. `crates/runners/tests/autoresponder.rs` (3) drive the LMTP handler and the `in` runner: owner mail answered and still forwarded, then not answered again, and answered-then-discarded with `list.autoresponse` + `post.discard`; a `help` answered and still run, a `join` swallowed with no workflow; a post answered and delivered, and swallowed with `post.discard` and nothing archived. `crates/api/tests/rest/autoresponder.rs` covers defaults, JSON and form writes, the four validation refusals and the `PUT` reset on both prefixes; the schema snapshot corpus gains `autoresponder-schema.snapshot`. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 137 binaries, 887 passed, 0 failed, 53 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Limits and deviations:** the reply is composed and committed in its own transaction before the original is forwarded, run or admitted, so a later failure of that step retries the original without a second reply (the grace period sees the record) — but a zero grace period would answer again on retry; Mailman's `X-Ack`/`Precedence` extras are not added; the posting-address reply is sent before the chain decides, so a post that is then held or rejected was still answered. Not Phase 3 completion. |
| P3-BOUNCE-RUNNER | The `bounces` queue has a consumer. `BounceProcessingRepo::process` (fenced by the lease clock, one transaction with the acknowledgement) names the member a report concerns by VERP (decoded at intake), by a delivery-status report whose `Original-Envelope-Id` — now kept by `dsn::parse_report` — matches a stored `dsn_issuances` row and verifies under the issuing key (the report's own claims are then ignored), or by the report's `Final-Recipient`s with `Action: failed`; each member of a list with `process_bounces` gets a `bounce_events` row (migration `0038` widens `source` to `verp|dsn|dsn_envid`, default kept) and is scored through the shared `smtp_bounces::score` (day rule, stale reset, threshold, disable, owner notices); delays are recognized but inert; a report naming nobody is forwarded per `forward_unrecognized_bounces_to` as a sanitized null-sender owner delivery to the administrators or the site owner, or dropped; the job is audited `bounce.process`. `runners::bounces` claims under lease renewal, feeds `listmngr_bounces_total{recognized|unrecognized|failed|scored}` and retries with the delivery backoff; `serve_mail_role` spawns it. | **Bounded local acceptance verified.** `crates/db/tests/bounce_processing.rs` (6): VERP scores, dedupes within the day and disables at the threshold with `bounce.process` and `bounce.disable` audited; a DSN scores a failed member, a delay changes nothing, a stranger leaves no event; `administrators`/`site_owner`/`discard` forwarding of an unattributable report; a list without `process_bounces` records nothing; a disabled member is left alone; a verified ENVID names the recipient the report tried to forge while a tampered one falls back to the claims. `crates/runners/tests/bounce_runner.rs` (2) go through the LMTP handler and the runner: a VERP report disables the member with the owner's notice as the only outgoing mail and a `verp` event, and an out-of-office at the bare address is forwarded to the owner with `bounce.forward` then `bounce.process`. `crates/mail/tests/dsn.rs` (+1) covers the envelope id (present, absent, oversized). The CLI inbox suites pass unchanged. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 139 binaries, 896 passed, 0 failed, 53 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Limits and deviations:** no heuristic detectors yet — a non-DSN, non-VERP bounce (Exchange, Yahoo, qmail prose) is unrecognized and forwarded (the flufl.bounce port is the next work package); no probe messages, and disabling is immediate at the threshold rather than after a probe; the `queue acknowledge-bounce` command now only applies to reports the runner has not consumed; a bounce for a deleted list is acknowledged and dropped; the DSN's `Status`/`Diagnostic-Code` are not stored on the event. Not Phase 3 completion. |
| P3-BOUNCE-DETECTORS | Heuristic bounce detection after `flufl.bounce`. `listmngr_mail::bounce::detect` reads a standards report exactly (failed → addresses, delays only → temporary) and otherwise hands the decoded `text/plain` parts (64 KiB) to detectors for Postfix (including its delay warning), qmail, Exim, Sendmail, Yahoo, Exchange (both the classic NDR and the modern one) and a generic permanent-failure phrase matcher, then a delay/warning matcher; addresses are lower-cased, capped at 100, never `mailer-daemon`/`postmaster`. `bounce_processing` uses it when a message is not a DSN: failures are scored as `heuristic` events (migration `0038` admits the source), a warning is recognized and inert, and only a message nothing reads is forwarded as unrecognized. | **Bounded local acceptance verified.** `crates/mail/tests/bounce_detect.rs` (3) run the corpus in `crates/mail/tests/fixtures/bounces/` (13 synthetic messages: DSN failure and delay, Postfix text and delay, qmail, Exim, Sendmail, Yahoo, two Exchange shapes, generic user-unknown, generic delay, out-of-office) asserting both the addresses and the detector for each, refusing a fixture without an expectation; the address rules (case, daemon exclusion, the hundred cap) and the empty/binary/oversized inputs. `crates/db/tests/bounce_processing.rs` (+1) scores a qmail prose bounce as `heuristic` and treats a prose delay as inert. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 140 binaries, 900 passed, 0 failed, 53 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Limits and deviations:** the corpus is synthetic (written to each family's documented shape), not `flufl.bounce`'s captured data, and covers seven families where `flufl.bounce` has some twenty (AOL, GroupWise, LLNL, Netscape, SMTP32, Sina, Yale and the rest are left to the generic matcher); the generic matcher can attribute a quoted address near a failure phrase, which only membership stops; HTML-only bounces are not read; the migration `0038` CHECK was widened in place because it is unreleased on this branch. Not Phase 3 completion. |
| P3-BOUNCE-PROBES | Mailman's probe step. With `[mailman] bounce_probes` (default on; `serve` applies it through `Database::with_bounce_probes`, a bare `Database` keeps the immediate disable) a member at the threshold is sent `list:user:notice:probe` from a one-time sender `list-bounces+probe=TOKEN@host` (`verp::encode` with `PROBE_LOCAL`, 40 lower-case hex characters, SHA-256 stored in `bounce_probes` — migration `0039`, cascading from the member — with `bounce_probe_lifetime_secs`), the score is reset and `bounce.probe` audited; `workflow_notices.mail_from` carries the sender and the out runner uses it as the envelope. A bounce to that address is stored as `probe_token` by the intake and `disable_after_probe` disables the member through the shared `disable_member` (owner notice, `bounce.disable`), spending every probe for them; an unknown, replayed, expired or foreign-list token is inert and never forwarded. | **Bounded local acceptance verified.** `crates/db/tests/bounce_probes.rs` (2): the threshold probes instead of disabling (notice to the member with the VERP sender, 40-hex token, hashed row, `bounce.probe` without `bounce.disable`, score reset), a second bounce probes again, the probe's bounce disables with the owner notice and spends every probe, a replay is inert and not forwarded; an expired probe is inert; without the configuration the threshold disables at once. `crates/runners/tests/bounce_runner.rs` (+1) goes through the LMTP handler and the runner: the probe is queued with its sender, the MTA's bounce to that address is stored as `probe_token` and disables the member. `crates/runners/src/probe_tests.rs` (1) delivers the probe through `deliver_one` to a sink and captures `MAIL FROM:<test-bounces+probe=…>` with the probe body and `From:`. The existing immediate-disable suites pass unchanged. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 141 binaries, 904 passed, 0 failed, 53 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Limits and deviations:** Mailman has no switch (it always probes) — `bounce_probes = false` is a listmngr addition that keeps the previous behaviour; the probe's bounce is trusted on its token alone, as in Mailman (no DSN parsing of the probe bounce); expired probe rows are removed only when their token arrives or the member goes; the member's own "delivery disabled" warning still comes from the maintenance sweep, not from the disable itself. Not Phase 3 completion. |
| P3-DIGEST-SETTINGS | Mailman's Digest settings and RFC 1153 issues. Migration `0040` adds `digests_enabled`, `digest_size_threshold` (KiB, 0 never), `digest_send_periodic` and `digest_volume_frequency` (`yearly|monthly|quarterly|weekly|daily`) to `mailing_lists`, round-tripping on both config prefixes (JSON and forms) with validation and `PUT` reset. `DigestRepo::flush` takes its triggers from them (plus the thousand-post cap and `force`), rolls the volume and restarts numbering when the calendar period of `digest_last_sent_at` changed under the frequency (audited `digest.bump`), resolves `list:member:digest:masthead|header|footer` for the list's language with `$volume`/`$issue` and hands them to the renderer on `DigestIssue`. `listmngr_mail::digest::build` writes the plain-text issue in RFC 1153's shape as Mailman does (masthead, `Today's Topics:` with authors, the rule, `Message: N` blocks behind thirty hyphens, `Subject: Digest Footer`, `End of … Digest, Vol X, Issue Y` and its underline) and the MIME issue with the templates as text parts around the `multipart/digest`; `to-digest` pushes no effect when `digests_enabled` is off. | **Bounded local acceptance verified.** `crates/db/tests/digests.rs` (+2): a 4 KiB threshold holds a small post and releases a 4 KiB one, a zero threshold never sends for size, the periodic switch holds a day-old megabyte until turned back on; monthly rollover from January to February (volume 2, issue 1), yearly keeping March in the volume, the header template expanded with volume and issue, the built-in masthead, and the list row's counters afterwards. `crates/mail/tests/digest.rs` (+2) pin the RFC 1153 plain-text layout with the three templates and the MIME issue's parts (four texts with templates, two without). `crates/pipeline/tests/handlers.rs` (+1): no digest effect when digests are off, the handler still runs. `crates/api/tests/rest/digest_settings.rs` (1): defaults, a form write of all four, two refusals, the `PUT` reset. The existing digest suites pass under the new defaults (30 KiB threshold, periodic on, monthly). Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 141 binaries, 910 passed, 0 failed, 53 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Limits and deviations:** `summary_digests` stays Mailman's MIME alias; the periodic trigger is \"a day since the oldest pending post\" rather than Mailman's cron-driven `mailman digests --periodic`; the plain-text issue drops non-text bodies with a placeholder line rather than converting them; `digest_header_uri`/`digest_footer_uri` are the template scopes, not separate URI settings; rollover happens at the next flush after the period changes, so an idle list rolls when it next publishes. Not Phase 3 completion. |
| P3-TASK-RUNNER | Mailman's task runner and `notify`. `TaskRepo::sweep(now, retention)` runs eight steps as bounded batches (100 rows, at most 100 batches per step per sweep), each batch in its own transaction with a `task.sweep` audit row naming the step and count: expired confirmation workflows outside `pending_moderation`, bounce probes past their life, help cooldown rows older than an hour, autoresponse records outside the list's `autoresponse_grace_period`; `done` queue jobs acknowledged more than `mailman.finished_job_retention_secs` (default 7 days) ago that no unexpired `dsn_issuances` row references, with their issuances (recipient snapshots, notice provenance, digest and owner delivery rows cascade); disposed held messages and their `moderation_log` after the same retention; messages older than the cutoff that no job, held row or issuance references, with their delivery bindings and the blob when it was the last message on it; and bounce scores whose `last_bounce_received` is older than the list's `bounce_info_stale_after`, reset fenced on that value and audited `bounce.stale_reset`. Shunted jobs and pending held posts keep their message. `listmngr_runners::tasks::run` sleeps `[mailman] run_tasks_every_secs` (default 3600, validated 60..604800; retention validated 3600..31536000) between sweeps, stops on shutdown, and is spawned by `serve_mail_role` beside the digest and archive runners. `TaskRepo::notify_list` counts held posts and `pending_moderation` (un)subscriptions and sends `list:admin:notice:pending` to every owner and moderator in their language with `$count` and a `$data` block of sections (fifty entries each, then `notify-more`), audited `list.notify` in the same transaction. CLI `tasks run` sweeps once and prints the JSON summary; `notify [--list ID]... [--dry-run]` is Mailman's daily reminder. | **Bounded local acceptance verified.** `crates/db/tests/tasks.rs` (4 SQLite + 1 PostgreSQL contract): day-by-day expiry of a 24 h token, an hour-old help cooldown, a 7-day probe and a 90-day-grace autoresponse record (each due only on its own day, a moderator-pending request untouched, every changing sweep audited); a finished job past retention goes with its message and blob while a young one, a shunted job and a pending held post stay; a stale score is reset only past `bounce_info_stale_after` and an idle sweep reports no change; `notify` sends one notice per owner naming the two waiting requests, omits empty sections, sends nothing when nothing waits, and repeats on the next run; `postgres_task_sweep_and_notify_contract` runs every scenario on isolated schemas (wired into `scripts/test-postgres.sh`). `crates/cli/tests/tasks.rs` (2): `tasks run` reports the expired workflow in JSON and leaves the young job; `notify` reminds the owner and `--dry-run` only reports. `crates/runners/src/tasks_tests.rs` (1): the runner sweeps on its interval and stops on shutdown. `crates/i18n/tests/catalog.rs` covers the new `notify-*` ids in both catalogs. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 143 binaries, 917 passed, 0 failed, 54 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Limits and deviations:** Mailman's task runner also evicts cached HTTP resources and archives orphan messages — listmngr has neither; `notify` is a CLI/cron entry point, not scheduled by the sweep, and has no `admin_notify_mchanges` counterpart yet; batch size and cap are constants, not configuration; a sweep interrupted mid-step loses only the batch in flight but repeats nothing else until the next period; the disabled-member warning and removal still belong to the bounce maintenance sweep (`P3-BOUNCE-MAINTENANCE`). Not Phase 3 completion. |
| P2-HEADER-MATCHES-REST | Mailman's `header-matches` resource on both prefixes: `GET`/`POST`/`DELETE /lists/{id}/header-matches`, `GET`/`PATCH`/`PUT`/`DELETE /lists/{id}/header-matches/{position}` and `POST /lists/{id}/header-matches/find`. Rows are numbered from zero; `POST` appends and answers `201` with the row's `Location`; `PATCH` changes only the named fields and `position` moves the row, shifting the rows between; `PUT` needs `header` and `pattern` and clears the optional fields it leaves out; deleting a row renumbers the rest; a non-digit position is `404` like Mailman's `{position:int}` route. Header names are lower-cased; `action` (Mailman) and `chain` (typed) are both accepted and must agree; the compatibility entry omits absent `action`/`tag` and carries `http_etag`, the typed entry uses `chain` with explicit `null`s. A repeated header (case-insensitive) and pattern is `400` as Mailman's "Pattern already exists"; `defer` names no chain and is refused; `find` filters by any of `header`, `tag`, `action` and returns the matching rows at their real positions. `HeaderMatchRepo` gained `get`, `append`, `update(position, HeaderMatchPatch)`, `remove` and `clear`, all through one locked edit that re-reads the set inside the transaction, validates every row, rewrites gap-free positions and records one `list.header_matches` audit event with the change kind, position and count; `replace` now goes the same way. Ban routes moved into `bans::routes()` alongside. | **Bounded local acceptance verified.** `crates/db/tests/header_matches.rs` (1 SQLite + 1 PostgreSQL contract): appends number from zero, `get` past the end is not found, a patch moves the last row first and keeps its edits, a position past the end / a case-insensitive duplicate / a bad pattern are refused and leave the set untouched, clearing a field differs from keeping it, removal shifts and clear empties, and the audit trail reads `append,append,append,update@2,update@0,remove@1,clear` with counts; `postgres_header_matches_contract` runs the same on an isolated schema (wired into `scripts/test-postgres.sh`). `crates/api/tests/rest/header_matches.rs` (6): Mailman-shaped entries and refusals (duplicate, `defer`, unknown action, bad regex, empty header, missing list), `PATCH` move and `PUT` reset with the four verbs on `2`, `-1`, `+1`, `first` all `404`, `find` by each field and combination with real positions, typed-prefix `chain`/`null` and both spellings, deletion renumbering, and scope/list-boundary denial on every route. `crates/api/tests/openapi.rs` covers the new module's routes and request media. Real client: `tests/compat/mailmanclient_phase1.py` now adds two rules through `mailmanclient==3.3.5`'s `HeaderMatches.add`, reads `action`/`tag`/`position` back, is refused a duplicate with `400`, moves and patches a rule with `save()`, finds by tag and by an action with no match, deletes by index and clears; `scripts/test-mailmanclient.py` PASS (Phase 1 flow and held flow) against the built binary on SQLite. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 144 binaries, 924 passed, 0 failed, 55 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS (37 tests) on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Harness repair:** `scripts/mailmanclient_held.py` had counted every sink delivery since before the hold notices (`P2-TEMPLATES`) existed and failed on the poster's "awaits moderator approval" notice; it now switches `respond_to_post_requests` and `admin_immed_notify` off on its list through the client. **Limits and deviations:** whether Mailman answers an empty `find` with `404` or an empty collection was not checked against a live Mailman — listmngr returns the empty collection, which mailmanclient's `find` treats the same; `tag` is free text with no `find`-by-prefix; the browser UI does not expose the rows; the site-wide `[antispam] header_checks` stay configuration-only. Not Phase 2 completion. |
| P3-SITE-BANS-REST | Mailman's site-wide `/bans`: `GET`/`POST /bans` and `GET`/`DELETE /bans/{email}` on both prefixes with the list resource's `email` input (exact mailbox or `^` regex), canonical identity, `Location`, pagination and entry shape. `BanRepo::site_get|site_list|site_count|site_create|site_delete` work the `list_id IS NULL` rows; because both engines treat `NULL` as distinct under the unique index, `site_create` selects under the site-wide reservation and returns `Conflict` (`409`) before inserting. Writes commit with `ban.create`/`ban.delete` on target `site`/`bans`. A site ban matches on every list through the unchanged `is_banned`, is not one of any list's bans, and a list ban is not a site ban. The routes are server administration: `lists:read` for reads, `lists:write` for writes, and the token must be bound to no list or domain (`authorize_site`, like domain creation). | **Bounded local acceptance verified.** `crates/db/tests/bans.rs` (+1 SQLite, +1 PostgreSQL contract): canonical creation, the repository-level duplicate refusal, count/list/pagination/get/negative pagination, a site ban banning on two lists while absent from both lists' bans, a list ban absent from the site's, deletion ending the match, and the audit trail (`site`/`bans` then `list`); `postgres_site_bans_contract` covers the `NULL`-distinct index, the duplicate check and the `IS NULL` filters on an isolated schema (wired into `scripts/test-postgres.sh`). `crates/api/tests/rest/site_bans.rs` (2): creation on both prefixes with `Location` and body, `409` on a repeat, three `400`s, ordered collections and totals per prefix, case-insensitive fetch, `404`s, site and list bans kept apart in both directions, deletion then `404`; a list-bound token holding the scopes is `403` on every route, an unbound `lists:read` token may read but not write, `system:read` is `403`, no token `401`. `crates/api/tests/openapi.rs` covers the new routes. Real client: `tests/compat/mailmanclient_phase1.py` adds a site ban through `mailmanclient==3.3.5`'s `client.bans.add`, checks `list_id` is absent, `in`, `find_by_email`, that the address is not in the list's bans, that `subscribe` of the banned address is refused with `400`, and `remove`; `scripts/test-mailmanclient.py` PASS. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 144 binaries, 927 passed, 0 failed, 56 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS (38 tests) on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Limits and deviations:** Mailman's ban entries carry `list_id` when set; listmngr's list-scoped entries still omit it (pre-existing); site bans have no CLI or browser surface; the site-wide reservation serializes ban writes with subscription workflows, as list bans already did. Not Phase 3 completion. |
| P3-ADMIN-NOTIFY-MCHANGES | Mailman's `admin_notify_mchanges` (migration `0041`, default false, on the list configuration resource under both prefixes with `PATCH`/`PUT` reset, form booleans and the single-attribute route). When on, every insert or delete of a `member` role — REST, CLI, mass operations, confirmed and moderated workflows, bounce removal, one-click unsubscribe — enqueues `list:admin:notice:subscribe` ("*display name* subscription notification") or `list:admin:notice:unsubscribe` ("… unsubscription notification") to every owner and moderator in the membership's own transaction, each in the recipient's language with `$member` set to the address, from the list's `-owner` address. The welcome keeps its own switch and ban suppression; the goodbye keeps its switch; the administrator notice depends on neither. Owner and moderator role changes are not membership changes. | **Bounded local acceptance verified.** `crates/db/tests/admin_notify_mchanges.rs` (3): off by default nothing reaches the administrators on subscribe or delete; on, a subscription yields the welcome plus one notice per owner and moderator with Mailman's subject and body, a removal yields one per administrator, and adding/removing a moderator yields nothing; with welcome and goodbye off the notices still go, and a Vietnamese owner receives the `vi` subject and body. `crates/api/tests/rest/admin_notify_mchanges.rs` (1): default false, a `True` form patch, the attribute route, a non-boolean refused, JSON false, on both prefixes. `crates/db/tests/fixtures/admin-notify-mchanges-schema.snapshot` is chained into the schema and repository contracts (also run on PostgreSQL by the gate); `crates/i18n/tests/catalog.rs` covers the two new subject ids in both catalogs. Real client: `tests/compat/mailmanclient_phase1.py` reads the default and round-trips the setting through `mailmanclient==3.3.5`'s `settings.save()`; `scripts/test-mailmanclient.py` PASS. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 145 binaries, 931 passed, 0 failed, 56 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS (38 tests) on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Limits and deviations:** Mailman raises the notice from its subscription and unsubscription services, so an address removed by list deletion is silent there as here; a member deleted together with their list (list removal) is not announced; the notice carries the address as posted, not the display name; the browser settings form does not expose the switch. Not Phase 3 completion. |
| P3-DIGEST-REST | Mailman's `/lists/{id}/digest` on both prefixes. `GET` returns `volume`, `next_digest_number` and `self_link` (plus `http_etag` on the compatibility prefix). `POST` accepts any of `bump`, `send` and `periodic` (JSON or form, Python `True`/`False` spellings) with unknown fields refused: `bump` advances the volume and restarts numbering through `DigestRepo::bump_with_context` audited `digest.bump` with the caller's token; `send` forces one issue through `DigestRepo::live().flush(..., true, render)`; `periodic` flushes only when the list's size or daily trigger is due; applied in that order and answered `202` with `{published, bumped}`. The digest renderer moved from `listmngr_runners::digests` to `listmngr_db::digests::render` (re-exported by the runner) so REST needs no runner dependency. Reads need `lists:read`, verbs `lists:write`, within the token's list bounds. | **Bounded local acceptance verified.** `crates/api/tests/rest/digest.rs` (2): counters at volume 1 / issue 1 on both prefixes with the prefix's link and etag rule; a form `bump=True` → 202, volume 2, issue 1; a send with nothing collected publishes 0; after one post is collected the way the runner does it, `periodic=True` publishes 0 (under the threshold and younger than a day) and a JSON forced send publishes 1, advances the issue number to 2 and leaves the issue `Dev Digest, Vol 2, Issue 1` in the outgoing queue for its one digest recipient; `send=maybe` and an unknown field are `400`, a missing list `404`; a list-bound token is `403` on the other list on both verbs and prefixes, `lists:read` may read but not post, no token is `401`. `crates/api/tests/openapi.rs` covers the new module's routes and request media; the existing digest suites (`crates/db/tests/digests.rs`, `crates/runners`, `crates/mail/tests/digest.rs`) pass unchanged through the moved renderer. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 145 binaries, 933 passed, 0 failed, 56 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS (38 tests) on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards; `scripts/test-mailmanclient.py` PASS (unchanged flows; `mailmanclient==3.3.5` has no digest verbs). **Limits and deviations:** Mailman answers `202` with no body — listmngr adds the JSON count; `periodic` here evaluates the list's own triggers at request time rather than scheduling anything; the CLI keeps its own `digests send|periodic|bump`; no browser surface. Not Phase 3 completion. |
| P2-QUEUES-REST | Mailman's `/queues` on both prefixes. `GET /queues` lists the twelve runner queues in Mailman's order (`Queue::ALL`), each with `name`, `directory` (`queue_jobs/<name>` — jobs are rows, not files), `files` (ids of the `ready`/`leased` jobs, oldest first, at most 1000 via `MailQueueRepo::pending_ids`), `count` (`pending_count`) and `self_link`; `GET /queues/{name}` is one queue; `GET /queues/{name}/{id}` is one job's metadata (state, attempts, `run_after`, last error, never the raw message) and is not found when the job belongs to another queue. `POST /queues/{name}` is mailmanclient's `Queue.inject(list_id, text)`: `in` only, the list's fqdn spelling accepted on the compatibility prefix, `From` mailbox as envelope sender, `Message-ID` required, CRLF-normalised text under the 10 MiB intake bound, the CLI's exact routing context and five attempts, `201` with the job's location. Reads need `system:read` on an unbound token; injection needs `lists:write` within the token's list bounds. | **Bounded local acceptance verified.** `crates/api/tests/rest/queues.rs` (3): the twelve names in order with empty `files` and Mailman links; a form injection lands one job in `in` with a followable location, the queue's `files`/`count`, the job's metadata without raw text, `404` for the job under another queue, unknown queue, non-uuid and unknown ids, and the typed prefix's links; injection refusals — `out` (400), unknown queue (404), missing list (404), no `From`, no `Message-ID`, `From` not a mailbox, missing `text` (400) — and the compatibility fqdn spelling; a list-bound token is `403` on every read and may inject into its own list but not the other, `system:read` reads, no token is `401`. `crates/api/tests/openapi.rs` covers the module's routes and request media. Real client: `tests/compat/mailmanclient_phase1.py` reads `client.queues` (the twelve names) and `Queue.files` through `mailmanclient==3.3.5`; injection is left to the REST suite because the gate's live mail role would carry an injected post to the held harness's sink; `scripts/test-mailmanclient.py` PASS. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 145 binaries, 936 passed, 0 failed, 56 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS (38 tests) on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Limits and deviations:** Mailman's `files` are switchboard file names and `directory` a path — listmngr reports job ids and the table; Mailman lets `inject` target any switchboard, listmngr only `in`; the per-job route is a listmngr addition so the `Location` is followable; `count` is listmngr's; no `DELETE` of queued jobs (the CLI's `queue` verbs remain the operator surface). Not Phase 2 completion. |
| P2-HELD-FORWARD | Mailman's `forward` on a held-message decision. `POST /lists/{id}/held/{held_id}` (both prefixes, JSON or form) takes `forward` (Python `True`/`False` spellings accepted) and `forward_to`; with `forward=True` a copy of the held post, wrapped as `message/rfc822` under "Forward of moderated message" (`notice-forward-subject`, `forward-moderated-body` in the recipient's language), goes from the list's `-bounces` address to that one mailbox in the same transaction as the decision — `defer` keeps the post held, the other actions apply as before. `ModerationRepo::review_forwarding` canonicalises and validates the address before any write (a `safe_mailbox` that parses as an `Address`, not one of the list's own addresses), so a bad address refuses the whole decision; `forward` without `forward_to` is `400`; `forward=False` ignores `forward_to`. Migration `0042` adds `moderation_log.forward_to`; the audit diff carries the address. The browser review path is unchanged (`None`). | **Bounded local acceptance verified.** `crates/db/tests/held_forward.rs` (3): with `defer` and with `discard` exactly one outgoing message to the canonical address from `dev-bounces@…` with the subject, the `message/rfc822` part, the original subject and body, the log row's `forward_to` and the audit diff, the post still held after `defer` and disposed after `discard`; a non-mailbox, the list's posting and owner addresses, and an empty string are refused with nothing decided, sent or logged; a review without forward logs no address and sends nothing. `crates/api/tests/held.rs` (+1): a form without `forward_to`, an empty and a non-mailbox JSON `forward_to`, and the list's own address are `400` with nothing queued; a form `forward=True&forward_to=Reviewer%40Example.NET` with `defer` is `204`, queues one forward for `reviewer@example.net` carrying the wrapped post, leaves the post held and logs the address; `forward=false` with an address forwards nothing. `crates/db/tests/fixtures/moderation-forward-schema.snapshot` is chained into the schema and repository contracts (also run on PostgreSQL by the gate); `crates/i18n/tests/catalog.rs` covers the two new ids in both catalogs. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 146 binaries, 940 passed, 0 failed, 56 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS (38 tests) on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards; `scripts/test-mailmanclient.py` PASS (unchanged flows; `mailmanclient==3.3.5`'s `moderate_message` has no forward argument). **Limits and deviations:** Mailman accepts one address and so does listmngr (no list of recipients); Mailman sends the forward with an empty body — listmngr adds one explanatory line; the forward goes through the outgoing queue like every notice, so `204` means queued, not delivered; the browser held review does not offer forwarding until `P4-HELD-QUEUE`. Not Phase 2 completion. |
| P2-E2E-ACCEPTANCE | The plan's Phase 2 acceptance matrix on the real binary. `crates/cli/tests/mailpath_e2e.rs` spawns `listmngr serve` on a disposable SQLite database with a bound LMTP socket, a real multi-connection SMTP sink and, per test, an RSA key generated by the system OpenSSL and configured as `[[mta.dkim_signing]]` through `LISTMNGR_CONFIG`; the fixture gained `child_env`, `start_with(dkim, extra)` and typed-API helpers for list configuration, inline templates and bans. DKIM is verified in-process by `listmngr_mail::authenticity::Verifier` over a seeded `TxtCache` carrying the fixture's `e2e._domainkey` record — the inbound `validate-authenticity` verifier — asserting `dkim=pass` in a real `Authentication-Results` value. | **Bounded local acceptance verified.** `crates/cli/tests/mailpath_e2e.rs` (9, 5 new): the delivered copy carries `Subject: [Dev] hello list`, `List-Id`, `List-Post`, `List-Unsubscribe` (mailto), `List-Archive` from `site.base_url`, `Precedence: list`, the body and the list's `list:member:regular:footer` template, and a `DKIM-Signature` for `d=e2e.example.invalid; s=e2e` that verifies `dkim=pass`; a banned nonmember gets `Request to mailing list "Dev" rejected` naming the ban and the member nothing, held count 0; a 3 KiB post over `max_message_size = 1` is held with the reason and never delivered; `personalize = full` with `LISTMNGR__MTA__VERP_PERSONALIZED_DELIVERIES=true` yields exactly three single-recipient deliveries with `dev-bounces+local=domain@e2e.example.invalid` envelopes, a personal `To`, `List-Unsubscribe-Post: List-Unsubscribe=One-Click` and an `https://lists.e2e.invalid/…` one-click URL ahead of the mailto; a post injected by `queue inject` while the server is killed is delivered exactly once after start; the pre-existing tests (member/disabled delivery, nonmember hold, scoped moderation with accept-once, SIGKILL while the relay holds DATA → exactly one delivery after restart) pass unchanged. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 146 binaries, 945 passed, 0 failed, 56 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS (38 tests) on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards; `scripts/test-mailmanclient.py` PASS. `Cargo.lock` gained the `listmngr` dev-dependencies `base64` and `mail-auth` (already locked versions). **Limits and deviations:** the harness runs on SQLite, not PostgreSQL (the plan's "pg + smtp sink"); the kill point is between the pipeline and the relay's DATA answer, not inside the `in` stage, whose lease expiry and re-claim are covered by `P2-QUEUE`/`P2-LEASE-HEARTBEAT` contracts; two runtime semantics were asserted as configured rather than changed — VERP on personalized copies follows Mailman's `verp_personalized_deliveries` (default off) and the RFC 8058 pair is emitted on per-recipient copies only; the DKIM key is RSA-2048 (no Ed25519 dual signing); no live MTA. Not full Phase 2 closure in the ledger's original denominator, but the plan's acceptance list is now exercised end to end. |
| P3-DUAL-BACKEND-CI | Both backends in CI. The ordinary workspace suite is the SQLite suite; `scripts/test-postgres.sh` stays the chosen PostgreSQL contract set (38); new `scripts/test-postgres-all.sh` runs every `#[ignore]`d PostgreSQL test in the workspace (`cargo test --locked --workspace --all-targets -- --ignored` minus the three browser/`postmap` fixtures) on the same disposable server, and CI runs it after the contract set. `listmngr_db::test_support::IsolatedSchema::create(prefix)` is the one way a test gets a schema: it reads `TEST_POSTGRES_URL`, refuses a non-PostgreSQL URL, creates `prefix_<uuid7>` and returns a `search_path` URL; `drop()` removes it. The fourteen tests that each demanded a fresh empty database through `WEBUI_*_POSTGRES_URL` / `ARCHIVE_THREAD_POSTGRES_URL` now use it and carry the uniform ignore reason; `CLAUDE.md` names the helper and the second gate. | **Bounded local acceptance verified.** `TEST_POSTGRES_URL=… scripts/test-postgres-all.sh` PASS: 53 PostgreSQL tests across `listmngr-db` (lib and integration), `listmngr-api` (`postgres_auth`, `held`, `webui` and its modules), `listmngr-runners` and `listmngr-archive`, run in parallel on one disposable PostgreSQL 14.24 database, with zero schemas left behind afterwards (`pg_namespace` checked before the database and role were dropped); `scripts/test-postgres.sh` PASS (38) on the same server. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `python3 -m unittest scripts/tests` OK; `python3 scripts/check-production-crates.py` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 146 binaries, 945 passed, 0 failed, 56 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `scripts/test-mailmanclient.py` PASS. **Limits and deviations:** the SQLite and PostgreSQL populations are not a symmetric matrix — the SQLite suite is the full workspace and the PostgreSQL population is the tests written as contracts (53), so a repository behaviour without a PostgreSQL contract is still SQLite-only evidence; the e2e harness and the mailmanclient gate run on SQLite; the hosted CI run itself (GitHub Actions on `postgres:17`) is not claimed here, only its wiring; a test that panics before `drop()` leaks its schema until the disposable server is discarded. |
| P2-PIPELINE-HANDLERS | Mailman-named handler pipeline: `Handler` trait, `HandlerRegistry`, named `Pipeline`s, and a `Target` that runs the list's pipeline either to plan fan-out (`member-recipients`, `to-archive`, `to-digest`, `to-outgoing` effects consumed by the `in` runner) or to take a consumer's copy at its own fan-out handler (`Out` after `dmarc`, `Archive`/`Digest` before it). Shipped `default-posting-pipeline`: `member-recipients → cleanse → cleanse-dkim → cook-headers → subject-prefix → rfc-2369 → to-archive → to-digest → dmarc → to-outgoing`; `virgin`; declared `default-owner-pipeline`. Byte handlers (`cleanse` incl. `Approved:` body line, `cleanse-dkim`, `cook-headers` incl. anonymous identity/`Precedence`/`X-BeenThere`, `subject-prefix`, `rfc-2369`, `dmarc`) live in `listmngr_mail::handlers`; `cook_post`/`cook_individual_post` are thin wrappers over `cook_for`. New list setting `posting_pipeline` (migration `0029`) validated against the registry and restricted to executable pipelines that resolve recipients and deliver. `/api/v1/system/pipelines` projects the live registry with handler order and executability. | **Bounded local acceptance verified.** Characterization first: `crates/mail/tests/cook_characterization.rs` holds 60 captured cases (4 list settings × 5 message shapes × 3 consumers) to the pre-pipeline output as a sorted header multiset plus exact body; `crates/pipeline/tests/handlers.rs` (9) covers per-target snapshots, effect order, `archive_policy=never`, original-bytes access, missing snapshot, unknown pipeline/handler, handler refusal and late-failure isolation; REST tests cover the registry projection and `posting_pipeline` validation (unknown, non-string, `virgin`, declared owner pipeline all 400). Existing runner, archive, digest, DKIM, DMARC and e2e tests pass unchanged except `tests/munge.rs`, which asserted byte-identity with an empty-additions call and now asserts identity preservation. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 113 binaries, 725 passed, 0 failed, 50 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Deliberate byte-level differences:** headers are emitted in handler order (`Precedence`/`X-BeenThere` before `List-*`, munged `From`/`Reply-To` after them) and the archive copy now carries the full `X-BeenThere` loop history like delivery and digest copies. **Limits:** the pipeline still runs lazily per consumer over one immutable stored message rather than once in a `pipeline` queue, so handlers must stay deterministic; `default-owner-pipeline` is declared but owner mail still uses its own admission path; the digest issue itself is still assembled with `cook_headers` outside the pipeline; only built-in pipelines exist (no per-list custom handler lists); `validate-authenticity`, `mime-delete`, `tagger`, `avoid-duplicates`, `after-delivery`, `acknowledge`, `to-usenet`, `arc-sign` are not implemented. Not Phase 2 completion. |
| P2-CHAIN-RULES | The built-in posting chain now follows Mailman 3's link order and semantics: `no-senders`, `approved`, `emergency`, `loop`, `banned-address`, `member-moderation`, `nonmember-moderation`, then deferred `administrivia`, `implicit-dest`, `max-recipients`, `max-size`, `no-subject`, `suspicious-header`, `any → moderation`, a detour through the list's `header-match` rows, and `accept`. Rules share Mailman's `msgdata` (`EvalState`); several deferred hits produce one hold listing every reason in chain order; an explicit member/nonmember `accept` bypasses the deferred checks; the `moderation` chain holds when no rule recorded an action. New list settings `administrivia`, `require_explicit_destination`, `acceptable_aliases`, `accept/hold/reject/discard_these_nonmembers` (JSON arrays of exact addresses or `^`-regexes) and write-only `moderator_password` (Argon2id) on both REST prefixes; new site config `[antispam] header_checks`/`jump_chain`; per-list `header_matches` rows evaluated with per-row chain/tag; `nonmember` role rows honored; an `Approved:` header or first unencoded body line is verified by the `in` runner and stripped by `cook_post` before delivery, archive and digest. Migration `0028_moderation_rules.sql`. | **Bounded local acceptance verified.** Behavior change from `P2-CHAIN-ENGINE` is deliberate parity: `crates/pipeline/tests/policy_characterization.rs` now transcribes the Mailman contract and holds the engine to it over 134 400 core contexts; `crates/pipeline/tests/rules.rs` (35) covers each rule, alias/regex matching, header-rule order/tag/site default/invalid pattern/unknown chain fail-closed; `crates/pipeline/tests/chain.rs` (19) covers `EvalState`, reason joining, moderation default hold; `crates/mail/tests/facts.rs` (7) and `tests/cook.rs` (+6) cover header unfolding, body preview, key extraction and byte-exact stripping, including refusing encoded parts; `crates/db/tests/moderation_rules.rs` (6 SQLite + 1 PostgreSQL, wired into `scripts/test-postgres.sh`) covers defaults, persistence, validation without audit rows, Argon2 hashing with a redacted audit diff, and atomic `header_matches` replacement; `crates/api/tests/rest.rs` covers JSON/form round-trips, PUT reset and the never-projected password on both prefixes; `crates/runners/tests/moderation_rules.rs` (8) drives the real `in` processor for header rules, site checks, correct/wrong/absent keys, legacy lists, role rows, multi-reason holds and owner/emergency precedence. Fixtures that posted without `From`/`To` or as owners were corrected to realistic posts. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 111 binaries, 712 passed, 0 failed, 50 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` advisories/bans/licenses/sources ok; `cargo audit --ignore RUSTSEC-2023-0071` clean; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS on an owned disposable PostgreSQL 14.24 role and database, dropped afterwards. **Limits and deliberate deviations:** `no-senders` is stricter than Mailman (a null reverse path is discarded even with a `From:`, and a non-null envelope with no `From`/`Sender`/`Reply-To` is discarded); owners and moderators post as explicitly accepted members, which under Mailman ordering bypasses size, recipient, subject, destination and header-rule checks but not emergency or bans; member/nonmember hold reasons keep the pre-existing text `moderation policy` rather than Mailman's wording until templates land; an `Approved:` key in a base64/quoted-printable body part is neither honored nor stripped (use the header form), and `text/html` alternatives are not stripped; `dmarc-mitigation` and `news-moderation` remain declared, unlinked chains; header-match rows have no REST or browser surface yet (repository only); no `X-Mailman-Rule-Hits`/`Misses` header is emitted. This is not Phase 2 completion. |
| P2-CHAIN-ENGINE | Mailman-named chain/rule engine with `jump`/`detour`/`defer`/`stop`/`run` links, terminal and moderation chains, a bounded hop limit, and a hit/miss trace. `decide_posting` now runs `default-posting-chain` and fails closed to a hold on any chain misconfiguration. `/api/v1/system/chains` projects the live registry instead of a hand-kept name list. | **Bounded local acceptance verified.** Characterization first: `crates/pipeline/tests/policy_characterization.rs` asserts the engine equals the pre-engine early-return ladder over all 22 400 reachable `PostingContext` values, that every input reaches a disposition, and that no non-accept reason is empty. `crates/pipeline/tests/chain.rs` (16 tests) covers each link action, detour return-versus-terminate, cycle bounding, unknown chain/rule, and terminal reason propagation. Commands run from the repository root: `scripts/check-phase0-artifacts.sh` OK; `cargo fmt --all --check` OK; `cargo build --locked --workspace` OK; `cargo test --locked --workspace --all-targets --no-fail-fast` 107 binaries, 645 passed, 0 failed, 49 ignored; `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` clean; `cargo deny check` advisories/bans/licenses/sources ok; `cargo audit --ignore RUSTSEC-2023-0071` clean over 408 dependencies; `TEST_POSTGRES_URL=… scripts/test-postgres.sh` PASS on an owned disposable PostgreSQL 14.24 role and database, both dropped afterwards. **Limits at the time (superseded by `P2-CHAIN-RULES` above):** the rule set was unchanged from the pre-engine subset and `header-match`/`dmarc-mitigation` were declared as empty chains. No `X-Mailman-Rule-Hits`/`X-Mailman-Rule-Misses` header is emitted yet: the trace exists on `decide_posting_traced` but the `in` runner has no durable channel to carry it to the `out` runner, so emission belongs with the `cook-headers` completion. This is not Phase 2 completion. |

## Aggregate final-audit increments — bounded acceptance verified

| Acceptance ID | Behavior | Evidence/status |
| --- | --- | --- |
| P2-DIGEST-DELIVERY-AUTHORITY | Digest collection retains the persisted renewed lease deadline before ACK and fences commit after ACK audit; post/queue rollback and empty-recipient controls. | **Bounded local acceptance verified.** Frozen `target/aggregate-authority-gates-20260910-045720/`: 25/25 gates, workspace615/0/48, mandatory PostgreSQL31/0/0, 390 stable source/harness paths. Eight observed digest audit waits; exact ignored test registered and executed. Fresh-build native API/LMTP collection, CLI publication, three digest modes, restart and independent SMTP body readback pass on SQLite/PostgreSQL. Earlier existing-binary preflight and worker ad-hoc reports are not the final-source evidence. Source-bound review found no new P1/P2; other-table count-only digest assertions remain a coverage limit. See [DELIVERY_AUTHORITY.md](DELIVERY_AUTHORITY.md). |
| P2-WORKFLOW-DELIVERY-AUTHORITY | Common leased workflow completion fences the final ACK audit, including child/no-child paths and confirmation-driven membership changes. | **Bounded local acceptance verified in the same frozen run.** 24 observed PostgreSQL audit waits, SQLite controls, all-column relevant-state rollback, exact-expiry/valid/renewed controls and successful retries. Static review source hashes matched; execution belongs to parent gates. Unleased APIs and incoming DSN authority unchanged. |
| P2-MODERATION-HOLD-DELIVERY-AUTHORITY | Hold retains the locked deadline before ACK and fences after held insertion and the later moderation.hold audit. | **Bounded local acceptance verified in the same frozen run.** Eight PostgreSQL cases independently block queue.ack or moderation.hold audit; full relevant-state rollback and valid/renewed/retry controls. Static review matched final source. Unleased moderation review and SMTP exactly-once semantics are not part of this repair. |

## Outbound DSN producer prerequisite (not carrier completion)

| Acceptance ID | Behavior | Evidence/status |
| --- | --- | --- |
| P2-DSN-PRODUCER-PREREQUISITE | Default-off `mta.smtp_single_recipient`; one recipient per non-null SMTP session, unchanged null-sender batching, durable ordered outcomes and conservative cancellation quarantine. | **Bounded local acceptance verified.** Frozen `target/single-recipient-gates-20260910-015656/`: 23/23 gates, workspace604/0/43, mandatory PostgreSQL26/0/0; 375 paths stable. Six focused tests and source-bound static review with no introduced P1/P2. Actual API→LMTP→SMTP default/enabled/disabled modes across process restarts on each DB, exact grouping/bytes, mixed RCPT550 durable outcomes and no old-message replay. Historical first composed run failed; retained, not counted as acceptance. No HMAC, durable issuance ledger, incoming token route or DSN scorer. Commands/logs and exact residual scope: [DSN_PRODUCER_PREREQUISITE.md](DSN_PRODUCER_PREREQUISITE.md). |

## Shared queue delivery authority

| Acceptance ID | Behavior | Evidence/status |
| --- | --- | --- |
| P2-DELIVERY-AUTHORITY | Final-audit pre-commit expiry fencing for shared begin-delivery, claim, heartbeat, ack/retry/shunt and child handoff; rollback and renewed-authority controls. | **Bounded local acceptance verified.** Frozen `target/delivery-authority-gates-20260910-024940/`: 23/23 gates, workspace611/0/44, mandatory PostgreSQL27/0/0, 378 stable paths. Parent focused PostgreSQL covers 20 actual audit waits; both-backend native API→LMTP→SMTP/restart regressions pass. Source review found no introduced P1/P2; parent subsequently strengthened only PG fixture waiter identity and reran tests. Commands and unmodified aggregate-helper exclusions: [DELIVERY_AUTHORITY.md](DELIVERY_AUTHORITY.md). No DSN ledger/HMAC/intake. |

## Archive final-ACK-audit increment

Archive completion is now separately fenced after the free ACK helper's audit.
`target/archive-final-audit/` retains SQLite and actual PostgreSQL audit-wait
behavioral RED→GREEN. Final focused tests pass: archive DB 1/0/1, shared queue
10/0/2, sibling clock 1/0/1; archive/runner regressions 8/0/1 and 2/0/0;
owned PostgreSQL archive test 1/0/0 with eight observed waits. Counts use
passed/failed/ignored. Valid, exact-expiry, stale-renewal and `never` controls
cover queue/index/audit rollback and existing-reply rethreading. Fmt and strict
DB all-target/all-feature Clippy pass. The maintained PostgreSQL gate invokes
this test and has passed on the final frozen candidate.
Digests, workflows and moderation remain outside this repair; DSN authority and
full Mailman replacement remain open. Historical acceptance totals above are not
totals for these new source bytes.

| Acceptance ID | Behavior | Evidence/status |
| --- | --- | --- |
| P2-ARCHIVE-AUTHORITY | Archive final ACK-audit lease fencing, atomic indexing/rethread/ACK rollback, persisted renewed authority and public/never controls. | **Bounded local acceptance verified.** Frozen `target/archive-authority-gates-20260910-034840/`: 25/25 gates, workspace612/0/45, mandatory PostgreSQL28/0/0 including the exact new ignored test; 382 stable source/harness paths. Native API→LMTP→SMTP/restart public→never→public cases preserve archive bodies on both engines. Limited static review found no introduced P1/P2; six reviewed hashes match. Full workspace build/tests/strict Clippy, artifacts, deny/audit with the unchanged documented exception, browser/client regressions pass. Owned PostgreSQL fixtures removed. See [DELIVERY_AUTHORITY.md](DELIVERY_AUTHORITY.md); other aggregate producers, DSN authority and whole replacement remain open. |

## Read-only incoming DSN inspection

| Acceptance ID | Behavior | Evidence/status |
| --- | --- | --- |
| P2-DSN-INSPECTION | Opt-in `queue show <id> --dsn`; bounded MIME/RFC3464 recipient claims marked untrusted, no diagnostics or business writes; raw/default export preserved. | **Bounded local acceptance verified.** Corrected frozen `target/dsn-inspection-gates-20260909-171235/`: 20/20, workspace598/0/43, mandatory PostgreSQL26/0/0; 372 paths stable. Eight parser tests, two native CLI SQLite tests plus three inbox regressions; actual false-boundary and padding RED→GREEN repairs; source-bound static rereview no remaining P1/P2. Exact inclusive limits, whole-input rejection, returned-original isolation, matching member/preferences and audit/job/raw unchanged. No HMAC correlation, token intake, DSN scoring or full replacement claim. [DSN_INSPECTION.md](DSN_INSPECTION.md). |

| Acceptance ID | Behavior | Evidence/status |
| --- | --- | --- |
| P2-UNICODE-SUBJECT-PREFIX | RFC2047 encoding and decoded exact-prefix deduplication for non-ASCII configured prefixes; bounded UTF-8 words, unchanged MIME/body and ASCII path. | **Bounded local acceptance verified.** Corrected frozen `target/unicode-prefix-gates-20260909-153157/`: 18/18, workspace588/0/43, PostgreSQL26/0/0; 367 hashes stable. Six serializer regressions plus reconciled shared DB consumer oracle retain storage/rollback checks. Seven native browser→LMTP→SMTP cases per engine independently decode Vietnamese/emoji, require ASCII headers, exact payload/multiplicity, restart and sibling controls. Earlier raw-UTF8-oracle failure is retained, not counted as acceptance. No general SMTPUTF8/full replacement claim. [UNICODE_SUBJECT_PREFIX.md](UNICODE_SUBJECT_PREFIX.md). |
| P4-WEB-SUBJECT-PREFIX | Owner escaped labelled prefix input; optional string patch preserves omission, explicit empty clears, verbatim whitespace/Unicode; shared CR/LF validator and atomic owner/audit guards. | **Bounded local acceptance verified.** `target/web-prefix-gates-20260909-143205/`: 17/17 gates; workspace582/0/43; mandatory PostgreSQL26/0/0; 362 frozen hashes stable. Sequential POST and label RED/GREEN; isolated schema/router authority, validation and audit rollback tests. Parent native browser→LMTP→SMTP: seven ASCII-prefix cases per database, exact bytes/multiplicity, restart and sibling isolation. [WEB_SUBJECT_PREFIX.md](WEB_SUBJECT_PREFIX.md) separates worker gates, parent runtime receipts and post-gate docs. Unicode storage is not SMTPUTF8 interoperability; no RFC2047, historical repair, new revocation race or full replacement claim. |
| P1-SUBJECT-PREFIX-VALIDATION | Reject CR/LF subject prefixes at the common config transaction; preserve valid prefix bytes, multiline prose and whole-patch/audit atomicity. | **Bounded local acceptance verified.** `target/subject-prefix-gates-20260909-135939/`: 17/17 gates, workspace579/0/42, PostgreSQL25/0/0; 359 frozen source/harness hashes stable. Historical producer/consumer RED→GREEN; supplementary both-flavor JSON/form PUT/PATCH regressions. No prefix-specific SMTP tracer, historical-data repair or complete replacement claim. [SUBJECT_PREFIX_VALIDATION.md](SUBJECT_PREFIX_VALIDATION.md). |
| P4-WEB-EMERGENCY | Owner emergency moderation select; optional strict boolean preserving omission, live authority and atomic audit; hold eligible new posts, not a transport shutdown. | **Bounded local acceptance verified.** `target/web-emergency-gates-20260909-133035/`: 17/17 gates, workspace575/0/41, PostgreSQL24/0/0, six browser→LMTP→held/SMTP cases per engine including sibling survival, retained holds and enabled/disabled restart. 356 frozen paths stable; POST and UI RED→GREEN. Earlier 429 fixture failure fixed with dedicated schema/router, not limiter relaxation. [WEB_EMERGENCY.md](WEB_EMERGENCY.md); no complete replacement claim. |
| P4-WEB-NOTICES | Owner web welcome/goodbye switches, optional strict booleans, unchanged live authority and atomic audit; no notices on configuration edits. | **Bounded local acceptance verified.** `target/web-notices-gates-20260909-121615/`: 17/17 gates; workspace574/0/40; mandatory PostgreSQL23/0/0; six browser→membership API→private SMTP cases per engine including sibling and enabled/disabled restart. 354 frozen source/harness paths stable; three sequential behavioral RED→GREENs and focused static review no P1/P2. [WEB_NOTICE_CONTROLS.md](WEB_NOTICE_CONTROLS.md) records exact commands and limitations; no whole Mailman-replacement claim. |
| P4-LIST-COPY | Effective list-copy suppression for original To/Cc on regular/transactional-held/digest paths; canonical mailbox extraction; labelled optional strict web preference with existing guards and atomic audit. | **Bounded local acceptance verified.** Completed frozen `target/list-copy-parent-gates-20260909-100711/`: 63/63 gates; workspace568/0/39; mandatory PostgreSQL22/0/0; actual browser→SMTP/restart on both engines. 349 frozen paths stable and matched before subsequent posting-limits work; bounded static review no P1/P2. Sequential RED→GREEN: `target/list-copy-handoff.md`. Not authentication, prior-delivery proof, exhaustive contention or whole replacement. |
| P4-WEB-POSTING-LIMITS | Owner browser configuration of original post size (KiB) and To/Cc hold threshold; optional integer fields preserve omission, common validator and transaction/audit/authority. | **Bounded local acceptance verified.** `target/web-posting-limits-gates-20260909-113407/`: 17/17 gates; workspace571/0/40; mandatory PostgreSQL23/0/0; 12 native browser→LMTP→held/SMTP cases per engine with restart and sibling controls; 352 source/harness paths stable. Two sequential RED→GREENs, supplementary security/rollback controls and bounded independent static review no P1/P2. Exact commands/limits: [WEB_POSTING_LIMITS.md](WEB_POSTING_LIMITS.md). RUSTSEC-2023-0071 exception retained. No full P4, live-MTA/cutover or whole replacement claim. |


## Own-post reception preference — bounded local acceptance verified

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P4-WEB-OWN-POSTINGS | Labelled account true/false select with effective selection; strict optional POST field; legacy omission preserves current stored override and old audit shape; explicit bool atomically audited with existing live authority and delivery restrictions; unrelated/shared preferences preserved. | **Bounded local acceptance verified.** Frozen `target/web-own-postings-parent-gates-20260909-090105/`: **58/58 gates; workspace561/0/37; mandatory PostgreSQL20/0/0**, including registered PostgreSQL router/rollback controls. Actual Chromium→SMTP on both engines: suppression/restoration, peer/sibling survival, restart in both states, omission, malformed/duplicate/foreign/revoked/CSRF/Origin denials and exact attributed audits. 339/339 source/harness paths stable; independent bounded review no causal P1/P2; receipt `target/web-own-postings-final-receipt.json`. Actual router POST422→303 and missing-select RED→GREEN logs in worker handoff; security controls first ran GREEN, not retrospective security TDD. Documented RUSTSEC-2023-0071 exception remains. No new contention/late-expiry matrix, authenticated sender or whole-P4/replacement claim. |

## Verified-session bounce recovery — bounded local acceptance verified

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P4-BOUNCE-WEB-RECOVERY | Existing verified session, current owned subscriber and direct by_bounces only; read-only confirmation GET and CSRF/configured-Origin POST; live authority after writer waits and writes; reset status/score/receipts/warnings with one attributed audit, retaining mode/history and generic-preference restrictions. | **Verified, bounded local acceptance:** frozen `target/web-bounce-recovery-parent-gates-20260909-072736/`, 54/54 gates; workspace558/0/36, mandatory PostgreSQL19/0/0. Actual Chromium confirmation, negative authorization controls, scheduler/restart and exact fresh SMTP recovery with healthy/still-disabled survivors on both engines. SQLite observed-BUSY eight-case authority/expiry matrix; backend rollback and enabled-maintenance exclusion. Build/fmt/strict Clippy/client/browser/DKIM/TLS/AUTH/security PASS with `RUSTSEC-2023-0071` exception; 334/334 frozen files stable; bounded independent review no P1/P2. Receipt `target/web-bounce-recovery-final-receipt.json`; post-run acceptance prose checked separately. No fresh mailbox proof, token/probe/email recovery, PostgreSQL recovery-contention matrix, audit/COMMIT-expiry or same-identity multi-role/list execution claim. Published warning jobs are not recalled. Not full P4 or whole replacement. |

Historical no-web-recovery statements below describe earlier checkpoints; only
this existing-verified-session confirmation is newly accepted.

## Opt-in automatic maintenance — bounded local acceptance verified

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P3-BOUNCE-SCHEDULER | Default-off explicit mail-role child; validated interval60 seconds (1..86400), batch100 (1..1000), enable requires mail role; one SQL-bounded real-clock page per completion-based delay; first-delay/no-overlap/no-catch-up; failed/non-due cursor progress, empty wrap, deferred page-error retry; redacted aggregate logging; owned shutdown/cancellation and unchanged per-list gating/transactions. | **Verified, bounded local acceptance:** frozen `target/bounce-scheduler-parent-gates-20260909-063657/`, 50/50 gates; workspace554/0/35, mandatory PostgreSQL18/0/0; six actual client→LMTP→SMTP→serve→restart cases per engine without CLI sweeps or SQL mutations. Default-off, batch-one progress, fast/zero removal, positive-day grace, paused/restored members, permanent warning rejection before removal, exact durable SMTP bytes, audits, healthy delivery and graceful restart passed. Build/fmt/strict Clippy/browser/client/DKIM/TLS/AUTH/security PASS with `RUSTSEC-2023-0071` exception. Source/harness stable; bounded independent review no P1/P2. Receipt `target/bounce-scheduler-final-receipt.json`; sequential RED/GREEN `target/bounce-scheduler-handoff.md`, parent old-binary timeout `target/bounce-scheduler-parent-baseline.log`. Acceptance prose checked separately. Held-pool cancellation is not direct held-SQL-transaction/earlier-commit cancellation proof. No schema/clock override, DSN/VERP detector, reenable, leadership, cutover or whole-replacement claim. |

This supersedes historical manual-only/no-scheduler claims only when explicitly
enabled; legacy/default-off behavior and prior acceptance receipts stay intact.

## Explicit maintenance — bounded local acceptance verified

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P3-BOUNCE-MAINTENANCE | Explicit bounded `bounce sweep` CLI; additive default3 warning maximum/default7 whole-day interval/default-true removal notice; default-off processing; current own/effective bounce status and preference/config locking; first warning immediately, inclusive later interval, final full-interval wait, count0 removal; one action per member per invocation; private publication and authoritative removal/goodbye/admin notice/audit atomicity; keyset progress and private nonzero failure summary. | **Verified, bounded local acceptance:** frozen `target/bounce-maintenance-parent-gates-20260909-054944/`, 46/46 gates; workspace 546/0/35; mandatory PostgreSQL 18/0/0 including owned-schema atomicity/concurrent winner/reenable. Seven actual client/LMTP/SMTP/CLI/restart cases on each engine, with warning rejection before deletion and goodbye/admin rejection afterward, actual attempts, audit counts and unchanged bounce events. Build/fmt/strict Clippy, browser/client/DKIM/TLS/AUTH/security PASS with documented advisory exception. Stable source/harness; bounded independent review no P1/P2. Receipt `target/bounce-maintenance-final-receipt.json`; sequential RED/GREEN provenance `target/bounce-maintenance-handoff.md`. Post-run acceptance prose checked separately. No whole replacement claim. |

Warnings reuse original subscriber spelling and real owner Reply-To with a fixed
safe body; subject `Membership disabled warning`, bounded 4096-byte MIME, no
nonfunctional recovery tokens/URLs. Removal preserves other roles/identities/lists,
uses `DELETE RETURNING`, optional goodbye and independent owner/moderator notice
`Member removed by bounces`. `bounce.warning`, `bounce.remove` and enabled
`bounce.removal_notice` (including recipient count zero) commit with all effects.
Warning counts are publication counts, not delivery guarantees. Repeated positive
interval sweeps have one eligible winner; zero interval intentionally permits a
new action on each subsequent serialized invocation. An unsafe admin roster rolls
back one member operation; admin fanout inherits its existing lack of aggregate
quota and published snapshots are not revoked. CLI cursor advances over non-due
and failed candidates; later cycles restart without `--after`.

The explicit CLI does not install cron or start the opt-in scheduler. Incoming
DSN/VERP/probes, localized templates, web/email token recovery and real-MTA cutover
remain outside this slice. Historical exclusions of all warning/removal support
below belong to earlier increments and are superseded only by this explicit
operator-maintenance acceptance, not by unattended lifecycle parity. Live-clock
due-crossing and held-list-config race tests are not claimed by this receipt.

## Previous increment notice — bounded local acceptance verified

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P3-BOUNCE-INCREMENT-NOTICE | Default-false canonical/legacy/config scalar; fresh eligible UTC-day direct RCPT increment notice to deduplicated owners+moderators, original transport spelling, pre-reset score; independent threshold disable notice; bounded private MIME, same fenced transaction, zero-admin audit and authoritative no-loop provenance. | **Verified, bounded local acceptance:** frozen `target/bounce-increment-parent-gates-20260908-203556/`, 42/42 gates; workspace 531/0/34; PostgreSQL 17/0/0; nine actual client/LMTP/SMTP/restart cases on both engines. Build/fmt/strict Clippy, browser/client/DKIM/TLS/AUTH/security PASS with documented advisory exception. Stable source/harness; bounded independent review no P1/P2. Receipt `target/bounce-increment-final-receipt.json`. Sequential config/publication RED→GREEN provenance: `target/bounce-increment-handoff.md`. Acceptance wording checked separately after frozen run. |

Migration 0024 is additive; both schema corpus consumers include its snapshot.
`process_bounces` remains false. The new scalar supports native/compat JSON/form,
Python booleans, PATCH preserve and PUT false reset, attribute reads and generated
schema. Mailman 3.3.10's below-threshold OR no-probes condition means this bounded
direct-RCPT/no-VERP implementation also sends at threshold, with pre-reset score.
Fresh stale observations qualify even for 1→1/decreasing scores; same-day receipt
refresh, replay, old timestamps, disabled effective status and config-only changes
do not. Nonmember/non-RCPT/internal notices are excluded. Existing disable behavior
and body remain unchanged. `bounce.increment_notice` counts zero-admin rosters;
false produces no notice/audit. Unsafe rosters abort transactionally; already
materialized snapshots are not revoked and no aggregate roster quota is added.
The fixed private MIME subject is `Member bounce score increased`, bounded at
4096 bytes/admin without raw posts/diagnostics/secrets, using existing provenance,
null sender and no-loop consumer. Warnings, removal, probes and incoming DSN trust
remain unimplemented scope; earlier accepted counts below are not new acceptance.


## Owner disable notice — bounded local acceptance verified

`P3-BOUNCE-DISABLE-NOTICE` adds `bounce_notify_owner_on_disable` with canonical
**true** default (including legacy JSON and migration 0023); `process_bounces`
remains **false**. Native/compat JSON and form config, Python `True`/`False`,
PATCH omission preservation, PUT omission reset, attribute reads and OpenAPI
are covered by focused tests. This does not introduce warning/probe/recovery,
incoming DSN/VERP, removal, templates, authentication or migration/cutover parity.

Only a winning automatic disable publishes fixed, bounded private MIME naming
the member and list. Within the existing fenced completion transaction, one
roster SELECT snapshots current owners plus moderators, deduplicated by canonical
address identity. Each receives a separate persisted job with `workflow_notices`
provenance; ordinary members and other-list admins are excluded. Subsequent roster
changes do not revoke materialized recipients. There is no local `-owner` relay
hop. Empty rosters still disable and record `bounce.disable_notice` with
`recipient_count: 0`, creating no job. False configuration creates no notice.
Blob/message/job/provenance, score reset, disable, event, audits and delivery
outcome commit or roll back together. Replay/config edits do not notify.
Generated notices use null reverse paths and existing no-bounce-recursion
provenance; their SMTP failure neither re-enables nor scores recipients. No
subscriber post, raw diagnostic or secret is included; MIME is capped at 4096
bytes per admin. Unsafe recipient rosters fail transactionally, not by loopback.

Parent integration repaired actual mailmanclient `add_owner`/`add_moderator`
HTTP400 via compat-only administrative role admission, without implicit address
verification or relaxing native/subscriber/invitation/authentication guards.
`cargo test --locked -p listmngr-api --test rest bounce_notice` passes two tests;
the added role regression first failed 400 vs201. Strict workspace Clippy passed.
`target/bounce-notice-parent-probe.py` passed all six SQLite process cases,
including durable notices blocked until restart, null reverse paths, deduplicated
admins, private MIME, healthy survivors and notice-failure event isolation.
Logs: `target/bounce-notice-parent-{role-red,role-green,process,clippy}.log`.

Frozen final command `python3 -u target/bounce-notice-parent-gates.py` completed
all 38 gates PASS in `target/bounce-notice-parent-gates-20260908-195604/`:
workspace 524/0/33, separate PostgreSQL 16/0/0 (including new transactional notice
regression), SQLite+PostgreSQL actual client/LMTP/SMTP/restart, strict Clippy,
build/fmt, browser/client/DKIM/TLS/AUTH, deny and audit with
`RUSTSEC-2023-0071` explicitly ignored. Frozen source/harness hashes stayed stable.
Independent static review: no concrete P1/P2, limited to the stated source scope.
Final receipt `target/bounce-notice-final-receipt.json` distinguishes this frozen
run from subsequent acceptance-documentation edits and their separate checks.
Prior counts/no-notice claims below describe earlier increments, not this scope.

| ID | Contract | Acceptance |
|---|---|---|
| P3-BOUNCE-DISABLE-NOTICE | Default-true private owner/moderator disable notification, atomic and replay-safe; zero-admin audit; null-envelope/no recursive scoring; compat administrative role creation without implicit address verification. | **Verified bounded local acceptance:** frozen run `target/bounce-notice-parent-gates-20260908-195604/`, 38/38 gates; workspace 524/0/33, PG16/0/0, both-backend real-client/restart and no-loop transport; review and limitations above. |


## Current threshold suspension increment — bounded local acceptance verified

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P3-BOUNCE-DISABLE | Default-off via `process_bounces`; numeric `bounce_score_threshold` default5, >0 and ≤1e6; fresh eligible UTC-day RCPT event reaching threshold resets score0 and sets Member `by_bounces` within fenced event/audit/outcome transaction. No retroactive configuration action or same-day trigger. | **Bounded local acceptance PASS.** All 34 frozen-candidate gates passed: workspace **518/0/32**, mandatory PostgreSQL **15/0/0**, including two preference-race barrier regressions. Final real client→LMTP→SMTP→restart probes on both backends cover default-off, threshold1 and fractional1.5, healthy-survivor DATA and disabled-recipient exclusion on the next post. No warnings, notices, re-enable/removal, DSN/VERP, universal concurrency safety or cutover claim. |

Parent receipts: `target/bounce-disable-parent-stale-red.log`,
`target/bounce-disable-parent-db-green.log`, `target/bounce-disable-parent-process.log`
and `target/bounce-disable-parent-pg-preflight.log`. PostgreSQL process evidence
binds the prebuilt binary hash in that log, not subsequent source repairs.
Those logs are historical integration evidence. Final source/harness-stable run:
`target/bounce-disable-parent-gates-20260908-164331/{results,final,source-before,source-after}.json`.
Executed commands include `cargo test --locked --workspace --all-targets`,
`scripts/test-postgres.sh` on an owned disposable PostgreSQL cluster, strict
workspace Clippy, build/fmt, deny/audit (RUSTSEC-2023-0071 exception), actual
mailmanclient, Chromium, TLS/AUTH/DKIM and both backend process probes.
Both concurrent preference-write findings have bounded PostgreSQL RED→GREEN
evidence in `target/bounce-disable-race-handoff.md`; parent lint cleanup passed.
Final closure receipt: `target/bounce-disable-final-receipt.json`. The prior
512/0/30 workspace and 13/0/0 PostgreSQL results below are historical scoring
acceptance, not acceptance of this candidate.

## Previous direct SMTP bounce score increment — bounded local acceptance verified

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P3-DIRECT-BOUNCE-SCORE | Default-off `process_bounces`; new direct permanent RCPT events update subscriber score, receipt timestamp and processed marker in the fenced event/audit transaction. Daily deduplication, lazy stale reset, role/list exclusions, immutable member score reads, native integer-day and Mailman whole-day config. | **Bounded local acceptance PASS.** Workspace **512/0/30**, mandatory PostgreSQL **13/0/0**, independent real client/SMTP/restart probes on both backends and all 30 frozen-candidate gates PASS. No incoming DSN authentication, VERP, automatic disable/warning/probe, historical replay or production concurrency/cutover claim. |

Parent behavioral RED→GREEN logs: `target/bounce-score-parent-canonical-{red,green}.log`
(canonical field), `target/bounce-score-parent-duration-{red,green}.log` (Mailman
whole-day representation), `target/bounce-score-parent-received-red.log` and
`target/bounce-score-parent-db-green.log` (same-day receipt refresh and exact stale
boundary), and `target/bounce-score-parent-bool-{red,green}.log` (real client form
booleans). Import/format/Clippy or fixture failures are not behavioral RED.
Passing focused commands include `cargo test --locked -p listmngr-api --test rest
bounce_score`, `cargo test --locked -p listmngr-db --test smtp_bounces --test
bounce_score --test schema_contract`, strict scoped Clippy and workspace build.
`target/replacement-verifier-venv/bin/python target/bounce-score-parent-probe.py`
passed (`target/bounce-score-parent-client-settings-green.log`): actual
mailmanclient settings, LMTP, real relay refusals at separate SMTP stages,
SQLite/API readback, multiple same-day events and process restart. The probe only
reads SQL; deterministic clock/rollback tests own separate disposable fixtures.
Earlier failed harness attempts used bracketed Message-ID queries and lacked
RSET handling; those fixture defects were fixed, not claimed as product failures.

Final run: `target/bounce-score-parent-gates-20260908-112513/`. `final.json` reports
`pass: true` and `source_stable: true`; `results.json` records every command and
zero exit for all 30 gates. Workspace build/test, fmt, strict all-target/all-feature
Clippy, deny, fresh-source audit (explicit `RUSTSEC-2023-0071` exception), PostgreSQL,
real TLS/AUTH, mailmanclient, Chromium and independent DKIM passed.
`postgres-bounce-score-path.log` and `bounce-score-process.log` prove real client
settings, stage differential SMTP behavior and durable score/API readback after
process restart. Deterministic stale-clock and sabotage tests remain distinct
from a real PostgreSQL contention proof. Read-only review found no concrete P1/P2
in its inspected scope; it was not an independent runtime certification.
Final acceptance docs were updated only after these receipts; no source/harness
changes are included in that post-run documentation update.

## Current goodbye increment — bounded local acceptance verified

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P3-GOODBYE | Default-off persisted goodbye setting across JSON/form/OpenAPI; actual Member removal atomically publishes a bounded private notice with audit, without duplicate/no-op/ineligible-role mail; direct/mass/sync/confirmation/browser/list teardown paths; original mailbox spelling, provenance and durable outgoing delivery. | **Bounded local acceptance PASS.** Focused configuration, mass, direct SMTP, legacy JSON and list-teardown RED→GREEN; supplementary rollback/role/confirmation/browser controls. Parent fixed a real mailmanclient unsubscribe 405 under a separate retained regression. Listening HTTP/client→spool→process restart→SMTP tracers PASS on SQLite and owned PostgreSQL. Final workspace **505/0/30**, PostgreSQL **13/0/0**, security/client/browser/independent SMTP controls PASS below. No custom templates/localization/per-request goodbye override, unsubscribe-policy parity, new contention proof or production cutover claim. |

Final complete rerun: `target/goodbye-parent-gates-20260908-102457/`, with
`final.json` reporting `pass: true`, identical `source-before.json`/`source-after.json`,
and each command/exit in `results.json`. Exact gates include
`scripts/check-phase0-artifacts.sh`, `cargo fmt --all --check`,
`cargo build --locked --workspace`, `cargo test --locked --workspace --all-targets`,
`cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`,
`cargo deny check`, fresh official RustSec-source `cargo audit --db
target/goodbye-parent-fresh-advisories --no-fetch --ignore RUSTSEC-2023-0071`,
mandatory `scripts/test-postgres.sh` on an owned loopback cluster, and pinned
mailmanclient/Chromium acceptance. SMTP AUTH/password-file/STARTTLS matrices also
passed. Independent DKIM verified 7 production SMTP profiles with 8 negative
controls each, using fresh captures from this workspace run.

Both goodbye process tracers prove default-off, two independently enabled lists,
native UUID deletion and actual mailmanclient address deletion, no extra notice on
replay, committed queue persistence across process restart, exact stored SMTP bytes,
original private recipient spelling, final sent status and no successful-path bounce.
Fixtures and PostgreSQL were stopped and removed. This is not a new PostgreSQL
contention/rollback test or production MTA deployment test. Final documentation-only
acceptance edits are fenced by `target/goodbye-final-receipt.json`; no source or
harness change is covered by that documentation-only exception.

Evidence: `target/goodbye-{config,mass,direct-smtp,teardown}-{red,green}.log`,
`target/goodbye-db-green.log`, `target/goodbye-browser-green.log`,
`target/goodbye-parent-alias-{red,green}.log`,
`target/goodbye-parent-process-green.log`. Alias first had a missing test import;
the retained behavioral RED is HTTP 405, not that compilation error. A subsequent
test fixture used compatibility `entries` instead of native `items`; correcting
that fixture is not a product RED. Parent strict Clippy/build passed after bounded
helper extraction and test lint cleanup. The independent probe keeps a stopped
mail role while committing notices, uses read-only DB inspection, restarts the
actual service, and asserts exact persisted SMTP bytes/private recipients and sent
state. PostgreSQL uses its own database in a verified owned loopback cluster.

First full run `target/goodbye-parent-gates-20260908-100733/` was **not accepted**:
the second schema composer omitted the additive goodbye snapshot, failing SQLite
and PostgreSQL; subsequent DKIM verification correctly rejected stale captures
after the workspace stopped before runners. The PostgreSQL process fixture also
needed explicit UTF-8 client encoding (its SQL_ASCII cluster returned text as
bytes). Those are now repaired without weakening schema or capture assertions.
Cargo audit's internal database fetch timed out; a fresh external Git clone of
the official RustSec repository followed by `cargo audit --no-fetch` passed with
the documented `RUSTSEC-2023-0071` exception. The complete rerun above then passed.

The new address alias explicitly supports only authenticated administrative
unsubscribe with both pre-confirmed/pre-approved flags true. Public confirmation
remains on the existing leave workflow. Full Mailman replacement stays open.

## Current SMTP AUTH increment — bounded local acceptance verified

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P2-SMTP-AUTH | Optional bounded AUTH PLAIN only over REQUIRED verified TLS; exact post-TLS capabilities, bounded command/empty334 continuation, exact235 after credential transmission before shared envelope/DATA; redacted credentials/config/errors; failed AUTH retries pending without mailbox bounce. | **Bounded local acceptance PASS.** Independent OpenSSL real-process AUTH and password-file startup matrices plus parent full gates below. Runner tests additionally cover long credentials and hostile replies. No PostgreSQL AUTH-specific tracer, deployed third-party relay certification or full Mailman replacement readiness claim. |

Parent evidence: `target/smtp-auth-parent-gates-20260908-090707/`, including
`results.json`, `final.json` and identical source-before/source-after manifests.
The foreground coordinator hit its 600-second limit during audit collection;
after confirming no audit descendant remained, a fresh audit and the remaining
gates ran sequentially on the unchanged source/harness snapshot. This is not a
claim of one uninterrupted run. Documentation updates follow acceptance.

- Locked workspace tests: **495 passed / 0 failed / 30 ignored**, 85 summaries.
- Explicit disposable PostgreSQL gate: **13 passed / 0 failed / 0 ignored**;
  PostgreSQL HTTP/config/LMTP/SMTP recipient tracer PASS, fixture stopped/removed.
- Independent Python/OpenSSL `serve → LMTP → SMTP` AUTH matrix: **10 scenarios
  PASS** (initial response, empty334, no-auth regression, 535, 454, absent AUTH,
  fake mechanism, pre-TLS-only advertisement, AUTH timeout, wrong TLS identity).
  Exact envelope, full body and retained From/To/Message-ID/Subject/List-Id are
  asserted; auth negatives verify no envelope, no bounce, pending recipient and
  released attempt/lease with future retry. Service logs do not expose the raw
  or base64 secret, including the hostile AUTH reply echo.
- Real CLI secret-file matrix: **7 cases PASS**: private file accepted; FIFO,
  public-readable/empty/missing file, directory and conflicting input rejected
  without blocking or credential output. Owned fixture identities removed.
- Mailmanclient, explicit Chromium acceptance, independent DKIM seven SMTP
  profiles with eight negative controls each, locked build, strict workspace
  Clippy, fmt/artifact/diff, deny and fresh audit PASS. Existing
  `RUSTSEC-2023-0071` audit waiver retained.

Parent RFC4954 repair: the admitted long credentials previously exceeded the
SMTP AUTH command limit (`smtp-auth-parent-long-red.log`, real wrong command).
The runner boundary matrix now passes with inline short credentials and bare
AUTH/empty334 for longer credentials, up to 255 bytes each. A separate mail
unit control rejects235 before any credential response is sent. These add one
runner test and one mail unit test to the worker's focused checkpoint below.

Sequential behavioral RED→GREEN receipts are in `target/smtp-auth-handoff.md`:
initial runner sent MAIL where AUTH was required; empty334 closed instead of
continuing; plaintext credentials were admitted; hostile post-AUTH reply leaked
credentials; symlink password file was admitted. Each failed before its fix.
The 11-case negative matrix and additional config/file bounds are supplemental
controls, not separate observed REDs. Exact synthetic credential comparisons do
not print credential bytes even on assertion failure.

Worker focused checkpoint before the parent long-command repair (all commands from canonical root):
- `cargo test --locked -p listmngr-runners --lib smtp_auth`: **7 passed**.
- `cargo test --locked -p listmngr-runners --lib outbound::`: **44 passed, 1 PostgreSQL-only ignored**, including AUTH, STARTTLS/no-auth, DKIM/signing-failure and durability regressions.
- `cargo test --locked -p listmngr-mail --test smtp`: **20 passed**.
- `cargo test --locked -p listmngr-core config`: **3 passed** (other filtered targets ran zero tests, not extra passes).
- `cargo clippy --locked -p listmngr-core -p listmngr-mail -p listmngr-runners --all-targets --all-features -- -D warnings`: PASS.
- `cargo fmt --all --check` and `git diff --check`: PASS.
Logs: `target/smtp-auth-{final-focused,runner-regressions,smtp-regressions,core-regressions,clippy,fmt,diff-check}.log`.
The direct hostile AUTH535 error Display/Debug assertion is additional supplemental
coverage; the post-AUTH550 redaction assertion retains its observed RED.
New sources: `crates/mail/src/smtp/auth.rs` and runner `smtp_auth*_tests.rs`.
The pre-AUTH STARTTLS/486/13 evidence below remains historical for this change;
the parent gates above cover the final AUTH candidate instead.

## Prior STARTTLS and outgoing dependency recovery — bounded local acceptance

Acceptance combines `target/starttls-parent-gates-20260908-080601/` with
`target/starttls-parent-gates-20260908-082506/`; the first directory's
`combined-acceptance.json` binds their results to unchanged product source.
The original `final.json` remains FAIL: a 600-second orchestration limit
interrupted collection during PostgreSQL, and the resumed recipient tracer
rejected its fixture's new TCP URL before touching application data. The
ownership preflight was repaired to verify the repository-owned running PG
data directory, PID and loopback port, and explicit migration was added.
PostgreSQL and its recipient tracer then passed together on a fresh fixture.
Only the two recorded harness files changed during that repair, not product
source. Fixtures were stopped and removed. Documentation changes follow these
gates; this is combined evidence, not one uninterrupted all-green execution.

- `cargo test --locked --workspace --all-targets`: **486 passed / 0 failed /
  30 ignored**, 85 summaries; ignored tests are not passes.
- `scripts/test-postgres.sh`: **13 passed / 0 failed / 0 ignored**, explicitly
  on a disposable loopback PostgreSQL instance, with no SQLite fallback.
- Fresh `serve → LMTP → SMTP/OpenSSL` required-TLS tracer: TLS 1.3 exact delivery
  PASS; wrong hostname, untrusted CA and missing extension PASS with no plaintext
  envelope, no successful invalid-certificate handshake, no bounce, and exact
  ready-with-backoff/pending/no-attempt-token retry state.
- PostgreSQL HTTP/config/LMTP/held/SMTP recipient tracer, mailmanclient and
  explicit Chromium browser acceptance PASS.
- Seven fresh SMTP DKIM profiles pass the independent dkimpy verifier and eight
  negative controls per profile.
- Locked workspace build, format, strict all-target/all-feature workspace
  Clippy, artifact and diff checks, deny and fresh RustSec audit PASS. The
  documented `RUSTSEC-2023-0071` SQLx/MySQL waiver remains; not zero-exception audit.

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P2-STARTTLS | Required verified outbound TLS with CA/identity validation, bounded upgrade, post-TLS EHLO and no downgrade, using shared delivery/DKIM semantics. | **Bounded local acceptance PASS.** Behavioral plaintext-envelope RED → verified-TLS runner GREEN; independent OpenSSL real-process matrix and combined gates above PASS. Rejected upgrade, timeout and exact capability-token controls are runner tests. No AUTH, opportunistic/implicit TLS, incoming TLS or live MTA cutover acceptance. |

### Outgoing dependency recovery regressions

`cargo test --locked -p listmngr-runners signing_failure_tests --lib` passes both
regressions after sequential behavioral REDs: a closed-pool signing lookup lost
its dependency classification, and the local disposition shunted instead of
retrying. Controls retain invalid-input quarantine, pending recipient bytes,
no active attempt token, backoff and no SMTP/bounce event. The same classifier
also covers the initial durable-message fetch. The combined candidate gates above
cover this source; older acceptance below remains historical.

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P2-LOCAL-DEPENDENCY-RETRY | Local DB lookup failures retry before SMTP; invalid authority/signing quarantines without unsigned fallback. | Two focused regressions PASS; 49 runner library tests PASS with one PostgreSQL-only test ignored in that invocation. Closed-pool classification and SQLite disposition are separate tests, not an injected live outage through the whole service. |

## Prior recipient-limit and DKIM local acceptance

The historical DKIM retry referenced below is no longer running and its named
gate files are absent in the current `target`. It is not passing evidence.
Fresh parent gates now pass for this candidate; historical totals below
must not be treated as current verification. The frozen run is retained at
`target/replacement-gates-20260908-064843/{results,final}.json` with per-command
logs and identical source-before/source-after SHA-256 manifests. Only evidence
documentation was edited after this run.

- `cargo test --locked --workspace --all-targets`: **480 passed / 0 failed /
  30 ignored**, 85 summaries. Ignored environment-specific tests are not counted
  as passes.
- `scripts/test-postgres.sh`: **13 passed / 0 failed / 0 ignored** on an owned
  Unix-socket-only PostgreSQL fixture, subsequently stopped and removed.
- Separate real PostgreSQL → HTTP config → LMTP → SMTP/held tracer: below-limit
  delivery, equality/above-limit hold, zero-disabled delivery and original
  recipient spelling PASS. This is not a full PostgreSQL malformed-header or
  contention matrix.
- Build, format, strict all-target/all-feature workspace Clippy, artifact/diff,
  unchanged-policy `cargo deny check` and fresh official RustSec HTTPS audit
  PASS. The existing inactive SQLx/MySQL `RUSTSEC-2023-0071` waiver is unchanged.
- `scripts/test-mailmanclient.py` with mailmanclient 3.3.5 PASS (CRUD plus live
  LMTP held-message and SMTP disposition/notice flows).
- Explicit `chromium_browser_acceptance -- --ignored --exact --nocapture` PASS
  with Chromium 140.0.7339.16, zero console/page errors and verified DB effects.
- Seven fresh real TCP SMTP profiles pass dkimpy 1.1.8 with eight rejection
  controls each: body, From, Subject, extra From, domain, selector, missing key,
  and valid unrelated RSA key. RSA minimum 2048, full body and oversigned From
  are asserted. Ordinary mail includes the trailing-whitespace regression.

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P2-RECIPIENT-LIMIT | Persisted default-zero integer config on both REST prefixes, JSON/form, PATCH preserve/PUT reset; parsed repeated/group To/Cc addresses; nonzero `count >= limit` or malformed input holds without outgoing/archive/digest children. | **Bounded local acceptance PASS.** Parent boundary RED observed actual `(held=0,outgoing=1)` at equality; repaired runner matrix has 3 passing tests, with DB/REST/OpenAPI tests and the complete frozen gates above. Real PostgreSQL HTTP/LMTP/SMTP path PASS. No complete message-acceptance parity claimed. |
| P3-DKIM-BODY | RFC relaxed-header/simple-body signing with unchanged MIME bytes. | **Bounded local acceptance PASS.** `cargo test --locked -p listmngr-mail --test dkim_body`: wrong body hash for trailing whitespace-only line RED → 1 passing test with eight independent RFC expectations. Initial folded-base64 assertion was a harness correction, not product RED. Fresh SMTP captures and full gates above PASS; production DNS/MTA acceptance remains open. |

## Historical outbound DKIM checkpoint — superseded by the current acceptance above

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P3-DKIM | Opt-in per-stored-list-domain RSA-SHA256 after final preparation and before SMTP; unchanged stored raw/envelope/recipient authority; redacted startup key errors and local signing failure without mailbox bounce events. Unix regular-file/owner-only/bounded reads reject FIFO paths without blocking. | **Parent acceptance pending.** FIFO product RED (exit 101) → GREEN, affected-package 153/0/1, strict affected Clippy PASS. Seven actual TCP SMTP profiles independently PASS dkimpy 1.1.8 valid + seven negative controls each (`target/dkim-parent-capture-matrix-1/results.json`). Static review PASS; parent verified all 20 reported source hashes. Full attempt 1 passed build/Clippy/fresh deny/audit/PostgreSQL, but workspace tests timed out at 900s (69/0/3 partial, 17 summaries) and capture freshness failed: overall FAIL, not accepted. Resource-bounded attempt 2 is running (`target/dkim-parent-gates-2-driver.log`). Full final acceptance and fresh final-run captures remain outstanding. |

The worker explicitly released an unfinished candidate with a stale `handoff.md`;
the parent corrected that handoff and generated its own delta inventory. Parent ownership
now includes the FIFO repair and fixture-permission correction. The latter is a
fixture setup correction, not a new product RED: safe production key permissions
were retained. The first broader parent run timed out; its partial results are
preserved separately and do not establish full acceptance. A later combined mail
test/Clippy run completed mail tests but timed out during Clippy; lint is retried
separately. Final affected-package rerun passed: `cargo test --locked -p
listmngr-mail -p listmngr-runners --all-targets` = **153 passed / 0 failed /
1 ignored**, 26 summaries (`parent-focused-3.log`); strict all-target/all-feature
Clippy passed (`parent-clippy-5.log`), as did fmt, artifact and diff checks.
Receipt: `target/dkim-evidence/parent-focused-receipt.json`. Welcome465 remains
the last accepted whole-workspace checkpoint.
Independent cross-profile evidence now passes in
`target/dkim-parent-capture-matrix-1/results.json`: seven actual TCP SMTP fixture
captures, each with a valid signature and seven rejection controls using dkimpy
1.1.8. Profiles are ordinary, owner, private rejection notice, digest regular,
plaintext, MIME and summary. The owner fixture includes a non-UTF-8 body; private
and owner fixtures retain null-envelope/privacy assertions. Only public keys and
synthetic messages are exported. Frozen full gates and source review remain open.
No incoming SPF/DKIM/DMARC, ARC, live DNS publication, production MTA cutover or
complete Mailman replacement is claimed.

## Bounded configurable welcome notice

Parent review corrected the worker's unintended administrative admission change:
bans suppress optional welcomes without denying previously supported direct/bulk
administrative insertions. Public/email join ban enforcement remains unchanged.
`parent-ban-red.log` records the default-off regression; `parent-ban-green.log`
records the corrected direct/bulk, enabled/disabled differential and workflow tests.
Worker handoff remains historical evidence. Parent acceptance passed on attempt 2.
Attempt 1 failed because a historical SQLite fixture stopped at migration 17
before calling current repository projections. The repaired fixture retains its
historical bounce row and metadata assertions and applies every later migration.
Both attempts are retained under `target/welcome-parent-gates-{1,2}/`.

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P3-WELCOME | Default-false authenticated JSON/form list setting and OpenAPI; eligible new Member direct/bulk/confirmed insertion atomically publishes one private bounded welcome with membership/audit. Default/off, other roles, existing/no-op, pending-only, rejected operations, banned recipients and replay do not publish welcomes. Administrative admission and unconditional confirmation receipts remain unchanged. | **Parent accepted:** workspace 465 passed / 0 failed / 30 ignored (81 summaries), all nine gates and 28 required tests PASS. `cargo test --locked --workspace --all-targets`, build, fmt, strict workspace Clippy, artifact/diff, fresh official HTTPS audit and unchanged-policy deny passed. PostgreSQL 14.24 canonical gate: 13/0/0; separate serial real CLI default/enable/private raw/owner/disable tracer PASS with settings seeded through owned fixture SQL. Both owned clusters stopped/removed and ports closed. Real router subscription → reopened SQLite → TCP SMTP sink is included in workspace tests; it is not listening HTTP, PostgreSQL contention or live MTA/cutover acceptance. Frozen source verified; final 20-file delta and evidence: `target/welcome-evidence/final.json`. |

Behavioral RED logs: `red.log` (missing default field), `red-enqueue.log`
(zero durable notices instead of one), `red-api.log` (form enablement 400 instead
of 200), all exit 101. Subsequent differential and audit/provenance/recipient
sabotage cases first ran GREEN and are supplementary regressions, not additional
historical TDD claims. The first SMTP test attempt failed at an incorrect fixture
directory, not product behavior; the corrected owned fixture passed.
The slice bounds whole English MIME output to 4096 bytes; no arbitrary template,
display/description content, requester mailbox or token is copied. Same-transaction
rollback is not exactly-once SMTP, and SQLite/router/sink success is not PostgreSQL
contention, live HTTP listener/MTA deployment or production replacement acceptance.
No custom template engine, per-request welcome override, localization, goodbye,
invitation/admin notices or full Mailman welcome parity is claimed.

## Bounded operator bounce acknowledgement

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P3-BOUNCE-ACK | Real `queue acknowledge-bounce JOB_ID --reason STRING` atomically marks only stored bounces/ready jobs done and audits job identity plus bounded reason. Retains raw/message/job; repeated acknowledgement, wrong queue, leases, shunts and missing jobs reject. Optional validated `queue ls --state` filters before LIMIT 1000 without changing default listing. | Final workspace 457 passed / 0 failed / 30 ignored (79 summaries); all 20 required regressions, locked build, fmt, strict workspace Clippy, artifact/diff and fresh official HTTPS audit PASS. Online deny retry PASS with unchanged policy and per-process Git-config isolation after default SSH fetch failure. Disposable PostgreSQL 14.24 canonical gate 13/0 and separate real CLI acknowledgement/backlog/raw/replay/audit tracer PASS. Evidence: `target/bounce-ack-evidence/final.json`. |

`target/bounce-ack-evidence/01-cli-red.log` (exit 101) proves missing CLI acknowledgement;
`02-cli-green.log` (0) passes real intake→reopened CLI→audited done with retained raw.
`03-filter-red.log` (101) proves missing state option; `04-filter-green.log` (0) passes.
Supplementary owning-DB audit sabotage/retry and rejection/boundary tests passed;
these are supplementary regressions, not additional behavioral RED claims.
Final focused logs: `09-db-final.log`, `10-cli-final.log`, `11-clippy.log`,
`12-fmt.log`, each exit 0. Earlier fmt (07, exit 1) and Clippy (08, exit 101)
were formatting/test-style failures, corrected before final gates.

Final parent evidence: `target/bounce-ack-parent-gates-1/` (source fingerprints
unchanged), `deny-retry.json`/`deny-retry.log` under the acknowledgement evidence
directory, and `target/bounce-ack-postgres-{1,2,3}/`. The first parent driver
failed overall: default deny fetch timed out over SSH, and the supplementary PG
tracer omitted required `--display-name` after the canonical PG gate had passed.
The next tracer attempt incorrectly expected exit 1 instead of the documented
redacted CLI-CONFLICT exit 6; neither harness failure is product RED evidence.
The corrected tracer passed on a fresh cluster without source changes. Deny used
a new advisory directory, online fetch and child-only Git-config isolation; no
policy check, TLS validation or global Git file was changed. Audit and deny used
fresh official RustSec revision `faedffd5118c1835e13cca3babb6059afb1eb8d0`.
Canonical PostgreSQL gate: `scripts/test-postgres.sh`, 13 named tests passed on
14.24; this does not turn the workspace's 30 ignored cases into 30 passes.
All owned clusters were stopped/deleted and their ports verified closed.
The parent SQLite claim race uses a file fixture, two prewarmed connections and
a barrier, not the worker's original single-connection pool. PostgreSQL CLI
coverage is serial and uses explicitly synthetic queue data, not an LMTP/MTA
delivery fixture or PostgreSQL acknowledgement-contention proof.

Only trusted local operator review is acknowledged: no SMTP, member writes,
trusted bounce_events, DSN/VERP parser/scoring, automatic consumer, forwarding,
quota, deletion or retention cleanup. Reason is trimmed, nonempty, ≤2048 UTF-8
bytes without control characters; operators must omit secrets/body content.
All raw remains sensitive retained storage. PostgreSQL contention/live MTA and
full Mailman bounce scoring/probes/warnings/disablement/removal remain open.
Prior intake-only acceptance below is historical, not certification of this delta.

## Durable incoming bounce inbox — bounded increment

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P3-BOUNCE-INBOX | Bare list-bounces LMTP intake commits exact raw/list context/job/audit before 250 into the separate untrusted inbox; null sender and absent Message-ID are supported, without posting, trusted failure events or member actions. Existing CLI reads the retained inbox. | Final parent driver PASS: workspace 452 passed / 0 failed / 30 ignored (78 summaries); build/fmt/strict workspace Clippy/deny/fresh official HTTPS advisory audit/artifact/diff and explicit real Postfix lookup PASS. All 15 required regression markers passed; Rust/schema hashes unchanged. |

Final logs and commands: `target/bounce-inbox-parent-gates-1/results.json`,
`03.log` (workspace), `06.log` (fresh official advisory checkout
`faedffd5118c1835e13cca3babb6059afb1eb8d0`, audit with the documented
`RUSTSEC-2023-0071` ignore), `08.log` (actual `postmap`: 1 passed), and
`required-tests.json`. Independent read-only review found no concrete regression
in the 12-file baseline-relative increment; it was static review, not another
test run. The ignored workspace checks do not certify PostgreSQL or external
services. No MTA daemon, live mail, production migration, commit or push occurred.

PLAN §3 routes `list-bounces` to the bounce queue; §4.7's authenticated VERP,
DSN detectors, processing, scoring, disable/probe/warning/removal and notices
remain open. This increment deliberately adds no automatic consumer, forwarding,
quota or retention cleanup. The subsequent bounded acknowledgement increment below
adds explicit operator bookkeeping only. Plus/token-bearing recipients still
fail closed; ordinary list posts still require Message-ID. Raw reports and their
CLI exports are sensitive storage, not authenticated delivery-failure proof.

Evidence: `target/bounce-inbox-evidence/01-red.log` shows actual LMTP RCPT 550;
`03-green.log` passes the wire-to-durable-inbox tracer. `06-missing-id-red.log`
shows [550,550] instead of [250,550] for a missing-ID bounce plus ordinary post;
`08-missing-id-green.log` passes intake through real CLI list/show/raw readback.
The intervening wrong state-name assertion, wrong package name, fixture type
inference and unavailable UUID-v4 constructor were setup/build failures, not
additional behavioral RED evidence. Audit rollback, cross-list identity,
exact-list precedence, plus rejection and malformed-header cases are supplementary
regressions. All fixtures are disposable; PostgreSQL/live MTA are not certified.

## Typed SMTP failure-stage/code — bounded increment

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P3-SMTP-FAILURE-METADATA | Real SMTP remote permanent replies retain typed EHLO/MAIL FROM/RCPT/DATA-start/DATA-final stage and numeric code through outgoing consumer, atomic durable event and existing authenticated metadata API. Historical/legacy/local metadata is null; RCPT outcomes survive later DATA results. | Final parent workspace: 447 passed / 0 failed / 30 ignored (76 summaries); build, fmt, strict workspace Clippy, deny, fresh HTTPS RustSec audit, artifact and diff checks PASS. Rust/schema fingerprints unchanged during gates. No live PostgreSQL/MTA certification. |

PLAN anchors: §3 outgoing delivery → permanent event and §4.7 bounce processing.
This does not implement enhanced-status parsing, DSN/VERP, scoring, disabling,
new endpoints or UI; even a RCPT 550 policy refusal is not an invalid-mailbox
verdict. Greeting non-220 stays transient. No diagnostics are projected.

Evidence under `target/smtp-stage-evidence/` (raw logs plus individual `.exit`):
- `red-runner.log` → `green-runner.log`: real TCP RCPT 550 consumer read-back
  failed on missing/null stage, then passed with stage/code in the durable DTO.
- `red-stages-behavior.log` → `green-stages.log`: four command-stage tracers
  failed on null metadata, then passed (554); DATA cases retain a prior RCPT
  550 despite identical policy diagnostic text. Initial `red-stages.log` also
  contains a fixture recipient-order error for DATA, not behavioral RED.
- `green-mail.log`: 20 existing duplex SMTP regressions pass with the new remote
  status representation; local validation, retry and ambiguity coverage retained.
- `green-db-2.log`: six DB tests, including genuine pre-0017 schema upgrade with
  an old event remaining null, rollback/lease/provenance/uniqueness and reopen.
  Initial `green-db.log` exposed a positional nine-column fixture INSERT after
  the additive migration; the fixture now names its columns (not a behavior RED).
- `green-api.log`, `green-openapi.log`: authenticated scope/pagination/privacy,
  typed stage/code and explicit legacy null fields; closed OpenAPI stage enum.
- `schema-contract.log`, `schema-repositories.log`: both complete SQLite semantic
  corpus composers include a separate additive metadata snapshot.
- `clippy.log`: `cargo clippy --locked -p listmngr-core -p listmngr-mail -p
  listmngr-db -p listmngr-runners -p listmngr-api --all-targets --all-features --
  -D warnings` passes. Final formatting and rerun results are in `handoff.md`.

Final parent run: `python3 target/smtp-stage-parent-gates.py`, evidence directory
`target/smtp-stage-parent-gates-1/`. Driver exited 0 with `FINAL PASS`; every
command in `results.json` exited 0 and all nine required regression markers
passed in the same workspace run. `source-before.json` and `source-after.json`
match across Rust, manifests/lockfile, SQL migrations and schema snapshots.
The fresh official HTTPS RustSec clone was
`5a0ebedfe8bdd2e295b171f4162f8c977bcad9a5`; audit used that new database with
`--no-fetch --ignore RUSTSEC-2023-0071`, not a reused cached-only substitute.
The 30 ignored tests do not establish PostgreSQL runtime acceptance. No live
MTA, live/dev data changes, commits or pushes were part of this increment.

The original event-slice parent acceptance below is historical baseline evidence,
not final acceptance of this new stage/code delta. Supplemental API/schema/legacy
checks first ran GREEN; they are not separate retrospective TDD cycles.

## Direct SMTP bounce events — bounded increment

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P3-SMTP-BOUNCE-EVENT | Permanent outcomes of reserved ordinary-list SMTP attempts atomically create one list-owned event per job/original recipient, with audit and fenced recipient/queue transition; authenticated paginated metadata reads on both REST prefixes. | Parent final workspace: 442 passed / 0 failed / 30 ignored (76 summaries); build, fmt, strict workspace Clippy, deny, fresh HTTPS RustSec audit, artifact and diff checks PASS; Rust/schema fingerprints unchanged during gates. No scoring/disable/removal, incoming DSN/VERP, digest classification, owner notifications or PostgreSQL/MTA certification. |

PLAN anchors: outgoing permanent failures in §3 and bounce events in §4.7.
This adds unprocessed direct-transport observations, not the complete bounce
runner. Generic permanent SMTP outcomes do not establish an invalid mailbox.
Producer-owned workflow/owner/digest deliveries are excluded; no raw SMTP detail,
body or token-bearing context is exposed. Metadata outlives spool records and
cascades with list deletion; independent TTL processing is not implemented.

Historical focused evidence under `target/smtp-bounce-evidence/`:
- `01-red.log`: missing-schema failure (`no such table: bounce_events`), not a
  behavioral event-count assertion. `02-green.log`: persistence test PASS.
- `03-clock-red.log` → `04-clock-green.log`: behavioral late-audit expiry RED
  followed by rollback GREEN; `07-api-red.log` → `08-api-green.log`: HTTP 404
  followed by the scoped, paginated metadata response.
- `09-db-final.log`: five DB tests PASS, including reservation/provenance
  negatives, audit sabotage/retry, reopen persistence and list deletion rollback
  with a surviving other-list event. `05-smtp-green.log`: real TCP SMTP 550
  through the outgoing consumer; `10-openapi.log`: typed schema/route test PASS.
- `13-schema.log`: both SQLite semantic corpus composers PASS without replacing
  historical snapshots; `14-clippy.log`: affected DB/API/runners strict Clippy PASS.
  Earlier lint failures are retained. Supplemental controls first ran GREEN;
  they are not retrospective historical TDD claims.

After explicit writer release, the parent reviewed the baseline-relative 16-file
source delta (`parent-review.patch`), strengthened late-expiry rollback assertions
for queue state, recipient attempt token and audit, and added these documents.
Final parent evidence is in `target/smtp-bounce-parent-gates-1/`: `results.json`
records zero exits for every command below, `03.log` contains 442/0/30 over 76
summaries, `required-tests.json` confirms the new DB/expiry/API/SMTP tests actually
ran, and `source-before.json` equals `source-after.json` for Cargo/Rust/schema
inputs. The driver itself exited 0 with `FINAL PASS`.

```sh
scripts/check-phase0-artifacts.sh
cargo fmt --all --check
cargo build --locked --workspace
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo deny check
git clone --depth 1 https://github.com:443/RustSec/advisory-db.git target/smtp-bounce-parent-gates-1/advisory-db
cargo audit --db target/smtp-bounce-parent-gates-1/advisory-db --no-fetch --ignore RUSTSEC-2023-0071
git diff --check
```

The audit database was freshly cloned in this gate attempt, not reused from an
earlier run: official RustSec revision `5a0ebedfe8bdd2e295b171f4162f8c977bcad9a5`
is retained in `06.log`. No TLS, Git configuration or dependency policy changes.
Parent final changes also fix module ordering caught by full `cargo fmt`; a
private lease-token access in the supplemental test was corrected to a stored
reservation snapshot before the passing focused run, not counted as behavioral
RED. `parent-final.patch` and `parent-final-manifest.json` retain the full
baseline-relative source/document delta. Existing disposable fixtures only;
no live/dev data, MTA activation, commit or push is part of this increment.

## HTTP confirmation completion receipt — bounded increment

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P3-HTTP-CONFIRM-RECEIPT | Public REST and browser token confirmations now atomically publish the same private fixed-English join/leave receipt as email confirmation, addressed only to the workflow's original mailbox. Response formats unchanged; replay/error publishes none. | Behavioral HTTP-router RED: only 1 notice instead of challenge + receipt (2). Parent final workspace: 434 passed / 0 failed / 30 ignored (75 summaries); build, fmt, strict workspace Clippy, deny, artifact and diff checks PASS. Fresh HTTPS-cloned RustSec audit PASS after built-in fetch errors; details below. PostgreSQL/MTA and full P3 remain uncertified. |

PLAN anchors: §4.2 subscription/unsubscription and §4.9 user notices; this is a
bounded completion notice, not configurable welcome/goodbye parity. The preceding
email-only increment's statement that HTTP generated no receipt is historical
and is superseded here. Administrative CRUD and authenticated direct removal do
not consume workflow tokens and remain outside this behavior.

Evidence in `target/http-receipt-evidence/` (ignored local artifacts):

- `red.log`: `cargo test --locked -p listmngr-api --test workflows public_confirmation_publishes_private_join_and_leave_receipts_once -- --exact`, exit 101 at the expected notice-count assertion.
- `green.log`: intermediate fixture SQL typo (`queue_recipients` does not exist),
  not a second behavioral RED. Corrected to existing `delivery_recipients.email`.
- `green-fixed.log`: same focused command PASS. Later rollback, replay,
  original-case recipient, concurrent-consumer and SMTP assertions are additional
  controls, not retrospective test-first RED claims.
- `db-tests.log`: `cargo test --locked -p listmngr-db --test workflows --test subscription_commands --test email_command_clock`, exit 0.
- `api-tests.log`: `cargo test --locked -p listmngr-api --test workflows`, exit 0.
- `runner-tests.log`: `cargo test --locked -p listmngr-runners --lib workflow_tests`, exit 0.
- `clippy-final-bytes.log`: `cargo clippy --locked -p listmngr-db -p listmngr-api -p listmngr-runners --all-targets --all-features -- -D warnings`, exit 0. The initial `clippy.log` rejected a test's boolean-to-integer `if`; repaired without lint exemptions.
- `affected-tests.log`: first three-crate suite attempt timed out at 300 seconds;
  this partial execution is not PASS. The bounded crate-suite rerun
  `cargo test --locked -p listmngr-db -p listmngr-api -p listmngr-runners --all-targets`
  exited 0: **286 passed / 0 failed / 28 ignored**, 45 summaries
  (`affected-tests-final.log`, `test-totals.json`). These are worker-scope results;
  final parent workspace evidence follows below.
- `fmt.log` and `diff-check.log`: affected Rust files' `rustfmt --edition 2024
  --check` and `git diff --check` passed. `baseline.json`, `final.json`,
  `changed-paths.txt` and `increment.patch` retain the exact eight-file increment
  against the dirty dispatch baseline; cumulative pre-existing work is preserved.

Parent final-candidate evidence in `target/http-receipt-evidence/parent-gates-1/`:

- After explicit writer release, parent reviewed the eight-file baseline-relative
  delta and strengthened the HTTP fixture with different recipient local parts
  and domains. The exact focused HTTP regression passed; this extra control is
  not a new historical RED claim.
- `results.json` and `00.log` through `07.log` retain the full commands and exit
  statuses. `cargo test --locked --workspace --all-targets` exited 0 with
  **434 passed / 0 failed / 30 ignored**, 75 summaries. Build, format, strict
  workspace Clippy, deny, artifact and diff checks exited 0.
- The built-in HTTPS advisory fetch failed twice with a server IO error. Those
  failures remain recorded; the first gate driver therefore exited 1, not PASS.
  An alternative fresh `git clone --depth 1
  https://github.com:443/RustSec/advisory-db.git` succeeded into the previously
  absent `advisory-db-cli` directory, at revision
  `5a0ebedfe8bdd2e295b171f4162f8c977bcad9a5`. `cargo audit --db
  target/http-receipt-evidence/parent-gates-1/advisory-db-cli --no-fetch
  --ignore RUSTSEC-2023-0071` then exited 0, loading 1239 advisories and scanning
  345 dependencies. This uses newly fetched official data, not a pre-existing
  cache; no Git configuration, TLS checks or dependency policy were changed.
- `source-before.json` and `source-after.json` match for Rust, manifests, lockfile,
  SQL and schema snapshots throughout the parent gate run. The ledger update is
  documentation-only; worker manifests remain historical, and the final parent
  delta is retained separately.

Unleased rollback controls cover actual join and member leave, receipt-provenance
and confirmation-audit failure, unchanged membership/preferences/addresses/token/
spool/recipients/audit, and a successful retry. The reopened file-backed SQLite
concurrent-consumer control now requires exactly one receipt. The existing SMTP
fixture delivers both unleased receipts with a null sender and exact mixed-case
recipient, without token leak or post cooking. HTTP router publication and
repository-to-SMTP consumption are separate evidence boundaries, not a new live
HTTP-to-SMTP service or Chromium run. Email receipt/lease regressions remain GREEN.
Templates/settings/localization, moderation/invitations, owner membership notices,
bounce processing, PostgreSQL runtime and full Mailman replacement remain open.

## Email confirmation completion receipt — preceding increment

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P3-EMAIL-CONFIRM-RECEIPT | Successful leased email join/leave confirmation atomically publishes a private completion receipt to the token-stored original mailbox with membership/token/audit/ACK. Replay/error paths publish none. HTTP confirmations unchanged; no configurable welcome/goodbye claim. | Behavioral RED: challenge-only outbox count 1 instead of required 2. Focused DB subscription commands 7 PASS, runner workflow fixtures 5 PASS. Final workspace **432 passed / 0 failed / 30 ignored**; build/fmt/Clippy/deny/fresh HTTPS audit/artifact/diff PASS; Rust/schema fingerprints stable. Read-only three-file increment review found no concrete regressions. PostgreSQL/live MTA, templates, approval/invitation and broader PLAN P3 remain open. |

Commands: `cargo test --locked -p listmngr-db --test subscription_commands`,
`cargo test --locked -p listmngr-runners --lib workflow_tests`, and
`cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`.
The new receipt producer has a test-first RED→GREEN; subsequent transactional
sabotage and SMTP consumer controls first ran GREEN and are not retrospective REDs.
Existing TCP SMTP fixtures require exact stored mixed-case RCPT TO, null sender,
action-specific completion, no token/request-body leak, and no owner/archive/digest
fanout even with anonymous/munge settings enabled. Private receipts record state
at confirmation time, not guaranteed current state when SMTP eventually delivers.

Final commands all exited 0: `scripts/check-phase0-artifacts.sh`,
`cargo fmt --all --check`, `cargo build --locked --workspace`,
`cargo test --locked --workspace --all-targets` (75 summaries),
`cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`,
`cargo deny check`, `git diff --check`, and
`cargo audit --url https://github.com:443/RustSec/advisory-db.git --db <repository>/target/confirm-receipt-evidence/gates-1/advisory-db --ignore RUSTSEC-2023-0071`.
Exact command/log records and before/after source manifests are in
`target/confirm-receipt-evidence/gates-1/`; baseline-relative changes and the
original RED log are under `target/confirm-receipt-evidence/`. No additional
advisory ignore, Git configuration change, live migration, or MTA activation.

## Unconditional DMARC From rewriting — bounded increment

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P2-DMARC-MUNGE | Opt-in `munge_from,true`; default `no_mitigation,false` and inactive `no_mitigation,true`. Reject conditional pair/unsupported actions, including PATCH transitions. Transactional settings/audit, both REST prefixes JSON/form/PATCH/PUT/OpenAPI. Individual-only From rewriting with MIME-safe attribution, reply fallback, anonymity precedence and immutable body; archive/digest authors are not munged. | Final workspace **429 passed / 0 failed / 30 ignored**; build/fmt/Clippy/deny/fresh HTTPS audit/artifact/diff PASS, Rust/schema fingerprints stable. API/rendering REDs and parent archive/digest scope REDs observed; fixtures are not live PostgreSQL/MTA certification. Conditional DNS/PSL, other actions, full PLAN §4.1/§4.10/P2/P6, DKIM/ARC and remote deliverability remain open. |

`target/munge-evidence/` holds the writer's dispatch-era baselines and logs, not
final candidate acceptance. Its archive munging and plaintext attribution
experiments were superseded during parent review: DMARC belongs only on individual
deliveries. The parent observed these behavioral REDs before separating renderers:

- `cargo test --locked -p listmngr-db --lib dmarc_delivery_policy` — archive From
  was `test@example.invalid` instead of `author@elsewhere.invalid`.
- `cargo test --locked -p listmngr-runners --test digests munge_only` — nested
  digest From was `test@example.invalid` instead of `author@private.invalid`.

Initial cold-build timeouts were harness outcomes, not behavioral RED. The later
plaintext check was corrected to its existing body-only presentation; decoded
MIME/summary article assertions still require the original author. Tests also
cover ordinary SMTP enabled/default behavior, invalid-author shunting before
connection, API defaults/transitions, DB audit rollback and private-notice bypass.

Final parent commands: `scripts/check-phase0-artifacts.sh`, `cargo fmt --all --check`,
`cargo build --locked --workspace`, `cargo test --locked --workspace --all-targets`
(75 summaries), `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`,
`cargo deny check`, `git diff --check`, and fresh online
`cargo audit --url https://github.com:443/RustSec/advisory-db.git --db <fresh-evidence-directory>/advisory-db --ignore RUSTSEC-2023-0071`.
All exited 0. Exact commands/log locations and source manifests are recorded in
`target/munge-evidence/final-gates.json`; raw evidence directory:
`/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-munge-gates-rwfiug_8`.
The first workspace attempt found a second schema-snapshot composer missing the
additive fragment; both consumers were reconciled without weakening assertions.
The final run above supersedes that partial failed run. No additional advisory
ignore, Git configuration change, PostgreSQL execution or live MTA activation.

Supported mailbox syntax and irreversible anonymous publication/digest snapshot
boundaries are documented in README/ARCHITECTURE. Valid Reply-To targets are
normalized without display names; unsupported/ambiguous Reply-To falls back to
the validated author. Missing/multiple/unsafe author fails closed, not unmitigated
fallback. Anonymous policy wins and private owner/workflow notices remain separate.

## Replyable email help — current increment

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P3-HELP-REPLY | Help replies target the list request bot rather than administrative forwarding; help explains subject replacement and a separate owner contact. | New tracer failed on missing Reply-To, then passed after rendering change. Actual SMTP sink receives help and subsequent join confirmation at the original-case envelope mailbox with null sender, no reflected private input, no post cooking, no owner/digest/archive effects, and no immediate membership. |

Normative anchors: PLAN §4.6 email help/request bot and distinct owner route;
§4.4 generated notices. No schema, command enum, owner roster or policy changes.
Existing one-hour help cooldown, global notice rate, provenance and lease fencing
remain covered by the affected crate suites.

Exact commands (all final commands below exit 0):

- `cargo test --locked -p listmngr-runners --lib help_reply_reaches_command_bot_and_sends_confirmation_not_owner_mail -- --nocapture`
  — 1 passed (`green.log`; final refactored test also passes in the suite).
- `cargo test --locked -p listmngr-mail -p listmngr-db -p listmngr-runners --all-targets`
  — **253 passed / 0 failed / 14 ignored**, `focused-tests.log`.
- `cargo clippy --locked -p listmngr-db -p listmngr-runners --all-targets --all-features -- -D warnings`
  — `clippy.log`.
- `cargo fmt --all --check` — `fmt.log`.
- `git diff --check` — `diff-check.log` (incremental untracked source delta also exported separately).

Evidence directory: `target/help-reply-evidence/` (ignored build artifacts,
local evidence rather than committed fixtures). `red-behavior.log` records the
expected missing-Reply-To failure, exit 101. Initial `red.log` was an invalid
fixture failure: a claim used the time from before live-clock publication and
saw no due output; fixed before the behavioral RED. `clippy-first.log` records
a 121-line test exceeding the 100-line lint; extracted a fixture delivery helper,
without lint exceptions, and reran tests and Clippy successfully. Baseline hashes,
source snapshots, `increment.patch` and `changed-paths.txt` preserve the exact
increment independently of the cumulative dirty worktree.

Remaining gaps: unchanged help subject is still unsupported (instructions say to
replace it); no topic-specific help, echo/end/stop, multi-command scripts,
autoresponder settings, localized/templates parity, or new owner-chain behavior.
Standalone live email/MTA and PostgreSQL acceptance were not run or authorized.
Parent final candidate gates passed: `scripts/check-phase0-artifacts.sh`,
`cargo fmt --all --check`, `cargo build --locked --workspace`,
`cargo test --locked --workspace --all-targets` (**418 passed / 0 failed / 30 ignored**, 73 summaries),
`cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`,
`cargo deny check`, and `git diff --check`.
Online audit passed with a fresh official advisory database:
`cargo audit --url https://github.com:443/RustSec/advisory-db.git --db /var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-command-gates-ouw86si5/advisory-db --ignore RUSTSEC-2023-0071`.
No Git configuration or advisory exceptions were changed.
Logs, exact exit codes and before/after fingerprints are in
`/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-command-gates-ouw86si5/`.
Rust sources, manifests, lockfile and schema inputs remained byte-identical
through these gates. PostgreSQL and live MTA acceptance remain unrun;
this is bounded usable help progress, not full Mailman replacement.

## Individual ban resource GET — current increment

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P3-BAN-RESOURCE-GET | Authenticated list-local resource GET follows emitted links on both prefixes; canonical exact identity, verbatim regex identifiers, scope enforcement and 404 after deletion. | Behavioral RED: returned 405 instead of 200. Focused REST bans 5 PASS and OpenAPI 5 PASS. Final workspace 417 passed / 0 failed / 30 ignored (73 summaries); build/fmt/Clippy/deny/online audit/artifact/diff PASS, Rust/schema fingerprints unchanged. No live Mailman-client or PostgreSQL certification. |

Focused commands: `cargo test --locked -p listmngr-api --test rest bans::` and
`cargo test --locked -p listmngr-api --test openapi`. The new resource reads stored
policy only: a mailbox matched by a stored regex does not become a resource.
Final logs and exact commands are in
`/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-ban-resource-gates-3rphrlej/results.json`:
`scripts/check-phase0-artifacts.sh`, `cargo fmt --all --check`,
`cargo build --locked --workspace`, `cargo test --locked --workspace --all-targets`,
`cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`,
`cargo deny check`, `git diff --check`, and
`cargo audit --url https://github.com:443/RustSec/advisory-db.git --db /var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-ban-resource-gates-3rphrlej/advisory-db --ignore RUSTSEC-2023-0071`
all exited 0. Audit fetched into a fresh task-owned database over official HTTPS,
without changing Git configuration or replacing the online gate with cached data.
Earlier candidate totals below are historical, not this candidate's acceptance.

## Subscription ban admission

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P3-SUBSCRIPTION-BAN-ADMISSION | Public/email join honors list and existing global bans at request and confirmation; denied requests publish nothing, denied confirmations roll back consumption, leave remains available. List-ban writers share the workflow reservation. | Final workspace 416 passed / 0 failed / 30 ignored (73 summaries); build/fmt/Clippy/deny/artifact/diff PASS. Default audit fetch failed on SSH port 22; a fresh official HTTPS audit PASS is recorded below. Rust/schema fingerprints unchanged. No PostgreSQL contention, live transport/client, global administration or privileged member CRUD/import enforcement claim. |

Behavioral RED observed separately for confirmation accepting a pre-ban token
and request publishing a banned address's challenge. Focused commands:
- `cargo test --locked -p listmngr-db --test workflows bans::`: 5 passed,
  covering exact/regex differential inputs, no cooldown on suppression, unban
  retry, list/global scope, leave and reservation-failure rollback.
- `cargo test --locked -p listmngr-db --test subscription_commands`: 5 passed,
  including suppressed join ACK and newly banned confirmation rollback/retry.
- `cargo test --locked -p listmngr-api --test workflows`: 3 passed, including
  indistinguishable request responses and rechecking issued tokens.
- `cargo test --locked -p listmngr-runners --lib exact_bans_use_mailbox_identity_but_regex_keeps_original_sender`: 1 passed with the shared matcher.

Final gate logs: `listmngr-subscription-ban-gates-2e_ri1lb`. Commands
`scripts/check-phase0-artifacts.sh`, `cargo fmt --all --check`,
`cargo build --locked --workspace`,
`cargo test --locked --workspace --all-targets`,
`cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`,
`cargo deny check` and `git diff --check` exited 0. The driver exited 1 because
`cargo audit --ignore RUSTSEC-2023-0071` could not fetch via the configured SSH
rewrite (GitHub port 22 timeout), not because it reported a vulnerability.

Recovery command executed with a fresh, task-owned database, unchanged Git
configuration, normal TLS validation and the same documented advisory exception:
`cargo audit --url https://github.com:443/RustSec/advisory-db.git --db /var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-audit-https-pm0r021u/db --ignore RUSTSEC-2023-0071`.
It exited 0 after fetching 1239 advisories and updating the crates.io index;
this is fresh online evidence, not a cached/no-fetch substitution. Its log and
exact argv are in `listmngr-audit-https-pm0r021u/{audit.log,result.json}`.
Read-only review of the changed DB/consumer/test paths reported no findings;
it did not execute PostgreSQL or certify concurrent behavior there.

Earlier candidate results below are historical, not this candidate's totals.

## Moderator rejection notices — earlier increment

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P3-MODERATOR-REJECTION-NOTICE | Explicit moderator reject atomically publishes one guarded, private author notice; discard is silent; original posting never fans out. Both review and legacy repository paths share the producer. | Final workspace 409 passed / 0 failed / 30 ignored across 73 summaries; build/fmt/Clippy/online deny/audit/artifact/diff PASS. Rust/schema fingerprints unchanged during gates. No PostgreSQL/MTA cutover or sender-authentication certification. |

Focused commands executed successfully:
- `cargo test --locked -p listmngr-db --test moderation_notices`: 5 passed,
  including audit-trigger sabotage/retry, UTF-8 truncation, suppression and both
  review/legacy entry points.
- `cargo test --locked -p listmngr-api --test held`: 13 passed, 1 PostgreSQL test
  ignored; the final workspace also includes the latest context assertion.
- `cargo test --locked -p listmngr-runners --lib workflow_tests::`: 3 passed,
  including real generated notice delivery and forged-context negative control.

Final candidate commands (`listmngr-rejection-notice-gates-2hkp5ikj` logs):
`scripts/check-phase0-artifacts.sh`, `cargo fmt --all --check`,
`cargo build --locked --workspace`,
`cargo test --locked --workspace --all-targets`,
`cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`,
`cargo deny check`, `cargo audit --ignore RUSTSEC-2023-0071`, and
`git diff --check` all exited 0. Cached advisory checks also passed but are not
used in place of those online commands. PostgreSQL ignored tests were not run.
The maintained real-client helper now expects one author notice and silent
discard; only its Python syntax was checked in this increment, not its live flow.

The REST contract covers `/api/v1` and `/3.1`, JSON and form, conflict on replay,
job-bound provenance, original-byte preservation and no post fanout. The previous
reject/discard test expected zero outgoing jobs; it now distinguishes the new
author notice from forbidden original-post delivery and keeps discard silent.
The runner fixture exercises the existing SMTP client against a test-owned
loopback sink: null envelope sender, exact original envelope recipient (not From
or Reply-To), decoded Unicode moderator comment, no list cooking/private headers,
and a durable Done outcome. This does not certify an external MTA.

Notices render at most 4096 UTF-8 comment bytes plus a truncation marker; the
full reason remains in moderation/audit. Syntax, loop and envelope/metadata
consistency checks suppress unsafe replies, but do not authenticate senders.
Use discard for spam or suspected forged senders. Hold notices, automatic
policy-rejection notices, templates/localization and complete anti-backscatter
protection remain open. Earlier increment counts below are historical baselines.

## List-scoped posting bans — preceding increment

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P3-LIST-POSTING-BANS | List-local REST create/list/delete with exact identity or bounded Rust regex, list/token scope, pagination, atomic audit, and posting no-fanout enforcement. | Final workspace 402 passed / 0 failed / 30 ignored across 72 summaries; build/fmt/Clippy/artifact/diff PASS. Online deny and audit PASS on rerun. Source fingerprints unchanged during gates. Experimental SQLite scope, not global/subscription bans, PostgreSQL/MTA acceptance or full Mailman compatibility. |

Observed behavioral REDs:
- `cargo test --locked -p listmngr-api --test rest bans::list_bans_roundtrip_json_form_and_pagination_on_both_prefixes -- --exact`: 404 instead of 201 before the routes; PASS after implementation.
- `cargo test --locked -p listmngr-runners --lib policy_facts::tests::exact_bans_use_mailbox_identity_but_regex_keeps_original_sender -- --exact`: canonical exact ban did not match `AUTHOR@EXAMPLE.NET.`; PASS after shared mailbox identity comparison. Case-sensitive regex positive/negative controls remain.

Additional focused commands already passed:
- `cargo test --locked -p listmngr-api --test rest bans::`
- `cargo test --locked -p listmngr-runners --test bans`
- `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`

The final command initially found redundant `pub(crate)` visibility in the new
private API module; visibility was corrected without relaxing lint policy.
The first full workspace run then failed the existing OpenAPI gates: the new
responses were untyped and lacked shared error/rate-limit schemas. These now
use the project's typed item/page and shared error contracts. The source-based
validator also assumed all handlers lived in `lib.rs`; it now preserves
`bans::handler` qualification and resolves the explicitly registered module,
failing on unknown modules rather than skipping operations. The qualified-handler
regression plus all existing structural and generated-input OpenAPI checks pass:
`cargo test --locked -p listmngr-api --test openapi` (5 passed).
No live/dev data, MTA activation or service launch is part of this increment.

Final candidate commands all exited 0 (local evidence directory
`listmngr-ban-gates-4mug2y18`, with `results.json`, per-command logs and
`source-before.json` / `source-after.json`):
- `scripts/check-phase0-artifacts.sh`
- `cargo fmt --all --check`
- `cargo build --locked --workspace`
- `cargo test --locked --workspace --all-targets` (402 / 0 / 30 ignored)
- `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`
- `cargo deny check`
- `cargo audit --ignore RUSTSEC-2023-0071` (normal online refresh, existing documented exception unchanged)
- `git diff --check`

Supplemental frozen deny and no-fetch/offline audit also passed, but do not
replace the online results above. The earlier ban gate's 45-second deny timeout
was not a passing check; the final normal command completed successfully. This
supersedes the prior increment's current online-refresh blocker, not its
historical failure record. DB bans tests (6) cover canonical/regex identity,
validation before writer acquisition, pagination/isolation, create/delete audit
rollback and successful retry with user/token/IP attribution. These overlap
the full workspace count and are not added to it.

## Per-list message size — prior increment

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P2-MESSAGE-SIZE | Durable `max_message_size` in KiB (headers + body), zero disables only the per-list check; oversized otherwise-eligible posts hold without fanout, exact boundary passes. REST JSON/form config, PATCH preservation and PUT reset on both prefixes. | RED→GREEN commands below; final workspace 389 passed / 0 failed / 30 ignored. Build/fmt/Clippy PASS. Online advisory refresh BLOCKED by GitHub connection timeouts; cached checks PASS separately. Experimental SQLite-backed slice, not full message-acceptance, PostgreSQL or live MTA certification. |

Observed test-first failures and subsequent focused passes:

- `cargo test --locked -p listmngr-db --test message_size`: initially rejected
  the unknown setting; then passed persistence, range/type validation, default
  zero and audit rollback/retry assertions.
- `cargo test --locked -p listmngr-runners --test message_size`: initially
  produced `(held=0, out=1)` for a 1025-byte post at a 1-KiB limit; then passed
  hold/no-child/raw-retention and exact-boundary/disabled/larger-limit controls.
  This drives the real in processor from a disposable queue, not live LMTP.
- `cargo test --locked -p listmngr-api --test rest message_size_config_roundtrips_and_put_resets_on_both_prefixes -- --exact`:
  initially PUT retained 7 instead of resetting to 0; after that repair, the
  form-number case returned 400. Both passed after explicit numeric form
  conversion, keeping JSON type validation strict.
- `cargo test --locked -p listmngr-db --test schema_contract`: 2 passed,
  PostgreSQL scenario ignored. Both complete schema consumers retain historical
  snapshots and add `message-size-schema.snapshot`.

The per-list range is 0..2147483647 KiB, sampled when the in runner evaluates
policy, not a concurrent-config-change fence. Null sender, bans and loops keep
their existing dispositions. No administrative/command behavior, site LMTP
cap, streaming/memory bound or automatic rejection notice is added. A test
helper exceeded the strict Clippy line limit and was split without relaxing
the lint; subsequent full-workspace Clippy passed. Earlier increment evidence
below describes earlier source, not this candidate.

Final candidate run: `listmngr-message-size-gates-nmu7cyfl` (2026-09-07).
The run's `results.json` records each exit and log path separately. All three
new behavior tests ran and passed in the workspace suite (70 target summaries).
Before/after hashes of Rust source, manifests, lockfile, migrations and schema
fixtures matched. The overall run exited **1**, not PASS, because online
security database fetches failed:

| Command | Result |
|---|---|
| `scripts/check-phase0-artifacts.sh` | PASS |
| `cargo fmt --all --check` | PASS |
| `cargo build --locked --workspace` | PASS |
| `cargo test --locked --workspace --all-targets` | PASS: 389 passed, 0 failed, 30 ignored |
| `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` | PASS |
| `cargo deny check` | BLOCKED / exit 1: RustSec fetch attempted GitHub SSH port 22, timed out |
| `cargo audit --ignore RUSTSEC-2023-0071` | BLOCKED / exit 1: advisory fetch timed out on GitHub SSH port 22 |
| `git diff --check` | PASS |

A bounded public HTTPS `git ls-remote` probe with process-local global/system
Git configuration disabled also timed out (30 seconds); no user Git settings
were modified. The following supplemental checks passed **using cached data**,
not a fresh online advisory refresh:

- `cargo deny --frozen check`: exit 0; advisories/bans/licenses/sources OK,
  with duplicate-dependency warnings under the unchanged policy.
- `CARGO_NET_OFFLINE=true cargo audit --no-fetch --ignore RUSTSEC-2023-0071`:
  exit 0; loaded 1239 cached advisories and scanned 345 crate dependencies.

Online dependency gates need rerunning when GitHub access is restored.
PostgreSQL and ignored live scenarios were not executed. Final documentation
edits are followed by artifact/diff checks; no migration was run on live/dev data.

## Experimental owner forwarding — prior increment

| ID | Contract | Evidence / remaining boundary |
|---|---|---|
| P2-OWNER-FORWARD | `list-owner@` intake → durable outgoing snapshot of owners + moderators, deduplicated by address, retaining transport spelling; exact-list precedence; no ordinary-member/archive/digest fanout. | `cargo test --locked -p listmngr-runners --test owner_plan_contract --test owner`: 7 passed. Final local workspace gates PASS below. Not full configurable owner-chain policy. |
| P2-OWNER-SAFETY | Reject unsafe/null senders, malformed/automatic/list traffic and own-list loop destinations; normalize trailing-dot domain identity but reject SMTP whitespace; shunt missing/unsafe roster. | Independent trailing-dot and SMTP-whitespace regressions each failed before repair, then passed. Moderator recipient assertion failed against Owner-only selection, then passed with the PLAN roster. |
| P2-OWNER-ATOMIC | Owner/moderator snapshot, outgoing child, job-bound provenance, audit and input ACK commit together; post-write live expiry and audit errors roll back. Outgoing context/header flags cannot authorize administrative cooking. | Three SQLite owner repository tests and the outgoing preparation/provenance test PASS in the full workspace run. No fresh PostgreSQL execution or actual SMTP/MTA delivery is inferred. |

Forwarded bytes preserve author/reply/thread/MIME presentation and body octets,
strip private/transport/authentication controls, add `Auto-Submitted`, and use
a null envelope sender. The retained intake remains unchanged. `aliases regen`
now emits `-owner`; earlier map evidence rejecting that suffix describes the
pre-owner candidate. No standalone service, live/dev-data migration or MTA
activation was performed. Bounce/VERP/DSN, notices, authentication/DMARC,
PostgreSQL and full Mailman replacement remain open.

Final local commands (2026-09-07; run `listmngr-owner-gates-jbd4k_q_`):

| Command | Result |
|---|---|
| `scripts/check-phase0-artifacts.sh` | PASS |
| `cargo fmt --all --check` | PASS |
| `cargo build --locked --workspace` | PASS |
| `cargo test --locked --workspace --all-targets` | PASS: 386 passed, 0 failed, 30 ignored across 68 target summaries |
| `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` | PASS |
| `cargo deny check` | PASS |
| `cargo audit --ignore RUSTSEC-2023-0071` | PASS with the existing documented exception |
| `POSTMAP_BIN=/usr/sbin/postmap cargo test --locked -p listmngr --test aliases real_postfix_lookup_agrees_with_runtime_recipient_validation -- --exact --ignored` | PASS: 1 passed, 0 ignored; actual lookup, not delivery |
| `git diff --check` | PASS |

Each command exit is recorded separately in the run's `results.json`; the outer
process also exited 0. Code/manifests/migrations stayed unchanged during the run.
The ignored backend/live scenarios are not counted as executed. Final edits
after this run only record documentation evidence, followed by artifact/diff
checks; no source change is covered by an older passing result.

## Heartbeat verification repair — prior increment

| ID | Contract | Evidence |
|---|---|---|
| P2-LEASE-HEARTBEAT | Production-owned renewal scheduler, unchanged live repository adapter, TTL/3 cadence and timeout, initial validation and cancellation. Deterministic cadence/expiry checks plus real SQLite wrapper and pool-contention controls. | `cargo test --locked -p listmngr-runners --lib heartbeat::`: 8 passed. Final local workspace gates PASS as detailed below; full-product/backend obligations remain open. |

The earlier workspace failure remains historical evidence. The old success test
required SQLite background-thread work to complete under a 50ms wall-clock
timeout throughout a 350ms real sleep. A controlled sole-connection hold now
demonstrates that the actual wrapper legitimately returns `LeaseLost` while the
lease remains valid; after releasing that connection the same lease renews.
This establishes the fixture's scheduling sensitivity, not the exact reason for
the uninstrumented historical failure.

The replacement separates paused-time scheduler assertions from explicit-time
SQLite expiry assertions. A real wrapper positive control reads the persisted
extended deadline from inside work; negatives refuse work on acknowledged jobs
and blocked renewal. The scheduler refactor first produced an interface RED
(missing `renewing`), then passed. Additional controls are supplemental coverage.
A deliberate cadence mutation from TTL/3 to TTL caused the focused scheduler
test to fail with actual 150ms versus expected 50ms; the mutation was restored.
This is sensitivity evidence, not retroactive behavioral TDD for a runtime bug.

No production TTL/timeout was widened, no lease predicate bypassed, and no
development or live data was changed. Fresh PostgreSQL and full Mailman product
acceptance remain open.

Final commands after restoring the cadence mutation:

| Command | Result |
|---|---|
| `scripts/check-phase0-artifacts.sh` | PASS |
| `cargo fmt --all --check` | PASS |
| `cargo build --locked --workspace` | PASS |
| `cargo test --locked --workspace --all-targets` | PASS: 375 passed, 0 failed, 30 ignored across 65 target summaries |
| `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` | PASS |
| `cargo deny check` | PASS |
| `cargo audit --ignore RUSTSEC-2023-0071` | PASS with existing documented exception |
| `POSTMAP_BIN=/usr/sbin/postmap cargo test --locked -p listmngr --test aliases real_postfix_lookup_agrees_with_runtime_recipient_validation -- --exact --ignored` | PASS: 1 passed, 0 ignored |
| `git diff --check` | PASS |

Source/Cargo fingerprints remained unchanged throughout the final run.
Independent read-only review of the two heartbeat files found no blocking
refactor issue. This closes the previously observed local workspace-gate failure
for this candidate, not all future scheduling failures. Ignored tests are not
passing backend evidence; PostgreSQL, live MTA cutover and full replacement are
still not certified.

## Explicit Postfix map generation — previous increment

| ID | Implemented and exercised | Boundary/status |
|---|---|---|
| P2-MTA-MAPS | `aliases regen --output DIRECTORY [--lmtp-target IP:PORT]`: one DB identity snapshot, hidden lists, exact supported command/recipient maps, collision deduplication, deterministic bytes, private staged publication to fresh generations, previous-generation retention, deletion reflected on regeneration, no audit mutation, DB/I/O failure controls. | SQLite CLI PASS; no automatic activation/refresh, daemon cutover, Postfix multi-process atomicity or whole Phase 2 acceptance. |
| P2-MTA-MAP-TARGET | Reject wildcard/multicast/zero-port/scoped IPv6 destinations before filesystem publication; explicit IPv6 destination overrides wildcard listener without config mutation. | CLI PASS; no IPv6 connectivity or target reachability claim. |
| P2-MTA-MAP-LOOKUP | Actual installed Postfix `postmap` 3.2.2 loads generated regexp files through a fresh fixture config. Positive/negative address matrix compared with production LMTP recipient handler, including hidden/colliding lists, uppercase, dot escaping, unsupported owner/bounce/plus suffixes and unrelated domains. | PASS for real map lookup; no Postfix daemon or mail delivery. The opt-in test was explicitly executed, not merely compiled. |

Observed vertical evidence:

- `cargo test --locked -p listmngr --test aliases regen_publishes_exact_supported_recipients_in_a_fresh_generation -- --exact`:
  initial exit **101** because real CLI returned **CLI-USAGE/2**, then GREEN.
  A macOS `/var` versus `/private/var` fixture-path assertion was normalized;
  that intermediate mismatch was test infrastructure, not product RED.
- `cargo test --locked -p listmngr --test aliases map_targets_reject_non_destinations_before_creating_output -- --exact`:
  behavioral RED (wildcard destination incorrectly published, exit 0), then GREEN.
- `cargo test --locked -p listmngr --test aliases explicit_ipv6_target_overrides_wildcard_listener_without_changing_config -- --exact`:
  RED (unknown override argument), then GREEN.
- `cargo test --locked -p listmngr --test aliases`: **5 passed, 1 opt-in ignored**.
- `POSTMAP_BIN=/usr/sbin/postmap cargo test --locked -p listmngr --test aliases real_postfix_lookup_agrees_with_runtime_recipient_validation -- --exact --ignored`:
  **1 passed, 0 ignored**. Initial fixture config failed Postfix's account-alias
  validation; setting fixture-only `mail_owner=_postfix` and
  `setgid_group=_postdrop` repaired it without editing system configuration.
- `cargo test --locked -p listmngr-runners --test subscription_commands --test inbound_safety`:
  **4 + 5 passed** after sharing the suffix table.
- `cargo clippy --locked -p listmngr -p listmngr-runners --all-targets --all-features -- -D warnings`:
  passed after correcting test string-building and documentation lint errors.

Final gate execution for this increment:

| Command | Observed result |
|---|---|
| `scripts/check-phase0-artifacts.sh` | PASS |
| `cargo fmt --all --check` | PASS |
| `cargo build --locked --workspace` | PASS |
| `cargo test --locked --workspace --all-targets` | **FAIL**, exit 101: `heartbeat::tests::short_lease_renews_during_slow_work_and_blocks_second_claimant`, `crates/runners/src/heartbeat.rs:116`; runners unit target 19 passed, 1 failed, 1 ignored. Later targets are not certified. |
| `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` | PASS |
| `cargo deny check` | PASS |
| `cargo audit --ignore RUSTSEC-2023-0071` | PASS with existing documented exception |
| Explicit real `postmap` test above | PASS again after workspace test failure |
| `git diff --check` | PASS |

The source/Cargo fingerprint was unchanged throughout these gates. The focused
diagnostic rerun `cargo test --locked -p listmngr-runners --lib heartbeat::tests::
-- --nocapture` passed both tests. This does **not** repair or supersede the
workspace failure. The affected fixture uses a 150ms wall-clock lease, a 350ms
sleep and a 50ms renewal timeout; scheduler/DB timing is a hypothesis, not a
confirmed root cause. No heartbeat behavior or assertion was weakened in this
increment. Full workspace acceptance remains blocked.

Independent read-only map review found no P0/P1 blocker in generation-only scope
and identified an ancestor-directory crash-durability limitation. The runbook
now explicitly states that durable output-root provisioning is a prerequisite
for the crash-durability claim; no power-loss acceptance is asserted.

Fresh PostgreSQL,
standalone live-email and actual MTA cutover are **not** certified by this
increment; prior denied harnesses were not retried or replaced. Bounce processing
and owner delivery were absent at that checkpoint; the owner increment above
supersedes that part. Bounce processing and full Mailman replacement remain open.

The map protocol/runbook is in `POSTFIX_MAPS.md`. Deployment examples remain
commented and now correctly use `relay_recipient_maps` for relay domains rather
than the earlier `local_recipient_maps` example. No service activation, live/dev
data mutation, commit or push was performed.

## Owner browser list settings — current increment

| ID | Implemented and exercised | Boundary/status |
|---|---|---|
| WEB-LIST-SETTINGS | Owner-visible GET/POST linked from `/web/admin`; all six requested fields, escaped text, selected options and persistent readback. Existing list validator/update/audit transaction reused. Unrelated settings committed after form rendering survive save. | SQLite HTTP and Chromium PASS; no fields omitted. Same-field concurrent edits remain last successful writer wins. Not full Postorius parity. |
| WEB-LIST-SETTINGS-AUTH | Live verified list owner/server owner; foreign-list and moderator-only denial; bad/missing/null/foreign Origin, CSRF, unknown/duplicate/missing/invalid fields rejected without settings/audit effects. Password generation and verification revocation denied; audit insert failure rolls settings back. | SQLite HTTP PASS. Controls added after the first vertical GREEN are regression controls, not additional historical RED claims. |
| WEB-LIST-SETTINGS-LOCK | Shared 12-case corpus: valid list owner, valid server owner, session deletion, credential generation, expiry, unverify, unlink, role removal, downgrade to moderator, as-user mismatch, server-owner revocation, audit failure. Actual backend lock-conflict notification precedes revocation commit; settings, concurrent unrelated prefix and attributed audit independently checked. | SQLite multi-connection file-backed execution PASS. PostgreSQL implementation compiles but live execution BLOCKED by permission denial. No PostgreSQL PASS inferred. |
| WEB-LIST-SETTINGS-PG | Shared HTTP matrix and shared causal lock corpus with explicit fresh disposable URL requirements. | BLOCKED: PostgreSQL gate launch denied; no retry or equivalent workaround attempted. Full backend acceptance remains open. |

Exact evidence for this source increment:

- RED: `cargo test --locked -p listmngr-api --test webui list_settings::owner_list_settings -- --exact`
  exited **101**, one test failed because GET returned **404 instead of 200**,
  before settings routes/repository vertical were implemented. Same command then
  exited **0** (one passed); expanded controls also passed.
- `cargo test --locked -p listmngr-db --lib web_sessions::settings_tests::sqlite_settings_authority_barrier -- --exact`
  exited **0**, one test covering all 12 cases.
- Focused strict Clippy initially exited **101** on two test-helper cognitive
  complexity warnings. Helpers were split without lint exemptions; rerun
  `cargo clippy --locked -p listmngr-api -p listmngr-db --all-targets --all-features -- -D warnings`
  exited **0**.
- `python3 /tmp/listmngr-composed-browser-parent.py` exited **0**, exact real
  Chromium test executed. Six native form fields saved and reloaded, hostile
  text remained literal, no script element; inspected `14-list-settings.png`.
  Prior composed search/member-policy/preferences/archive/confirmation-bridge/
  leave/password paths remained green. Source fingerprint unchanged and token
  bridge removed. Evidence: `listmngr-composed-browser-parent-77stoj29`.
- `python3 /tmp/listmngr-composed-final-gates.py` exited **0**; every command
  below exited **0** on final Rust/Python bytes:
  `scripts/check-phase0-artifacts.sh`; `cargo fmt --all --check`;
  `cargo build --locked --workspace`; `cargo test --locked --workspace --all-targets`;
  `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`;
  `cargo deny check`; `cargo audit --ignore RUSTSEC-2023-0071`; `git diff --check`.
  Evidence: `listmngr-composed-final-gates-9d8jigcg`. The workspace log explicitly
  shows both new SQLite tests passing and both new PostgreSQL tests ignored.
- PostgreSQL invocation adapting the existing owned-fixture wrapper was
  permission-denied before execution (tool blocked, not a test failure).
  Pending exact tests: API `list_settings::postgres_owner_list_settings`
  (`WEBUI_SETTINGS_POSTGRES_URL`) and DB-lib
  `web_sessions::settings_tests::postgres_settings_authority_barrier`
  (`WEBUI_SETTINGS_BARRIER_POSTGRES_URL`), both requiring a **new empty owned
  disposable database** and `--exact --ignored`.

Evidence directories are under
`/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/`. No production/development DB,
SMTP delivery, external email, standalone email-only join/leave/restart, commit,
push or unrelated worktree cleanup was performed. Browser confirmation here
remains the existing isolated durable DB bridge, not SMTP proof.

## Owner member search — current increment

| ID | Implemented and exercised | Boundary |
|---|---|---|
| WEB-MEMBER-SEARCH | Literal email substring search before LIMIT; 25 nonmatching rows precede 25 matching rows. The same SQLite/PG HTTP corpus checks uppercase input, literal `%`/`_`/`!`/`+`, generated next/previous links, disjoint 20/5 pages, generated hidden selection fields and policy-save redirect/readback. Initial query rejection was behavioral RED 400→200. | Email search only, no name search, bulk operations, index or throughput claim. |
| WEB-MEMBER-SEARCH-INPUT | No-match/XSS-shaped input, foreign-list denial, overlong query and invalid return page (no policy mutation). NUL query separately reproduced RED 200→400; control characters now rejected on both backends. | Existing authority and atomic-audit behavior retained, not a new lock-contention proof. |

Final candidate commands all passed:

- `python3 /tmp/listmngr-search-postgres.py`: actual exact
  `member_search::postgres_literal_member_search`, owned database removed with
  absence verified. Evidence `listmngr-member-search-pg-y40z9oki`.
- `python3 /tmp/listmngr-composed-final-gates.py`: artifacts/fmt/locked workspace
  build/tests/strict Clippy/deny/audit with documented ignore/diff.
  Evidence `listmngr-composed-final-gates-8abpcgn0`.
- `python3 /tmp/listmngr-composed-postgres-full.py`: existing 13 exact live tests,
  owned database removed. Evidence `listmngr-composed-pg-full-jaxc341w`.
- `python3 /tmp/listmngr-composed-browser-parent.py`: real search form, uppercase
  query preserved across policy save/reload, no-match feedback and clear-search
  recovery, plus previous composed workflows. Screenshot `13-member-policy.png`
  inspected. Source fingerprint unchanged; confirmation bridge removed.
  Evidence `listmngr-composed-browser-parent-h8vtqzk1`.

These evidence directories are under
`/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/`. No live/development data,
denied standalone email workflow, commits or pushes were used.

## Owner member administration — current increment

| ID | Implemented and exercised | Boundary |
|---|---|---|
| WEB-MEMBER-ADMIN | Owner-only list index and member roster, hidden-list access, account navigation, per-member posting-policy form; all enum values and clearing override round-trip to DB with attributed audit. HTTP 404→200 behavioral RED→GREEN; malformed fixture import was fixed before that RED. | Not member search, bulk operations, role editing or full Postorius parity. |
| WEB-MEMBER-ADMIN-AUTH | Bad CSRF, foreign-list and administrative-role IDs, invalid action and owner→moderator revocation denied. SQLite actual writer contention covers valid control, session/password/verification/link/server-owner revocation and audit failure. Shared SQLite/PG HTTP matrix proves audit rollback and successful retry. | No new PostgreSQL causal contention or end-to-end SMTP-policy proof. |
| WEB-MEMBER-ADMIN-PAGES | Unauthorized lists sort before the owned list; permission filtering still returns it. Member pages contain 20/5 disjoint identities, escaped list text, and reject out-of-range offsets. | SQLite pagination corpus; PostgreSQL matrix is a narrower query/update test. |

Final source candidate verification:

- `python3 /tmp/listmngr-admin-postgres.py`: exact ignored
  `member_admin::postgres_owner_member_policy` actually ran and passed; disposable
  database removal and absence verified. Evidence `listmngr-member-admin-pg-0iv3n8d8`.
- `python3 /tmp/listmngr-composed-final-gates.py`: artifacts/fmt/locked workspace
  build/tests/strict Clippy/deny/audit (documented RUSTSEC-2023-0071 ignore)/diff all
  passed. Evidence `listmngr-composed-final-gates-pqrlte7r`.
- `python3 /tmp/listmngr-composed-postgres-full.py`: existing 13 exact live tests
  passed; owned DB removed. Evidence `listmngr-composed-pg-full-vde6w7fr`.
- `python3 /tmp/listmngr-composed-browser-parent.py`: actual native form saves hold,
  reloads its persisted selection, restores default and reloads again; existing
  account/archive/download/leave/password/confirmation-bridge scenarios pass.
  An earlier exact-label timeout exposed ambiguous wrapping label text; explicit
  label-for/select-id markup fixed it without weakening the browser selector.
  Evidence `listmngr-composed-browser-parent-n_0elw0o`, inspected screenshot
  `13-member-policy.png`; source fingerprint unchanged and token bridge removed.

Evidence directories above are under
`/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/`. No production/development data,
denied live email harness, commits or pushes were used.

## Authenticated member departure — current increment

| ID | Implemented and exercised | Boundary |
|---|---|---|
| WEB-MEMBER-LEAVE | Account discovery, hidden-list confirmation GET, exact address/list identity, protected POST, own membership/preferences removal and attributed audit. HTTP RED 404→GREEN; missing confirmation address separately RED→GREEN. Foreign/administrative-role IDs, bad CSRF and replay denied; account/address/other memberships/owner role preserved. | Authenticated member subscription only; anonymous requests still use email proof; queued mail not cancelled. |
| WEB-MEMBER-LEAVE-ATOMIC | Shared SQLite/PostgreSQL HTTP matrix injects audit failure, verifies original member and preference survival, then removes sabotage and succeeds with exactly one deletion audit. SQLite actual-writer contention matrix covers valid control, session/password/verification/link/AsUser revocation. | Not a dedicated PostgreSQL causal contention matrix or new transport-delivery proof. |

Final candidate gates:

- `python3 /tmp/listmngr-leave-postgres.py`: exact
  `postgres_browser_member_leave` passed; owned database removed and absence
  verified. `/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-member-leave-pg-_s_uz817/`.
- `python3 /tmp/listmngr-composed-final-gates.py`: artifacts/fmt/locked workspace
  build/tests/strict Clippy/deny/audit with documented ignore/diff all passed.
  Includes `member_departure_rechecks_authority_after_real_writer_contention`.
  `/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-final-gates-zwqncxhb/`.
- `python3 /tmp/listmngr-composed-postgres-full.py`: existing 13 exact live tests
  passed, owned database removed. `/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-pg-full-euxb8z97/`.
- `python3 /tmp/listmngr-composed-browser-parent.py`: real private-list leave form
  submission, private membership disappears while public membership survives,
  and the still-authenticated browser immediately loses private archive access.
  Prior forms/password/archive/download scenarios pass; fingerprint unchanged,
  confirmation bridge removed. Screenshot `12-leave-confirmation.png` inspected.
  `/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-browser-parent-ro0na4m0/`.

No development/live data, denied standalone email workflow, commit or push used.

## Browser password change — current increment

| ID | Implemented and exercised | Boundary |
|---|---|---|
| WEB-PASSWORD-CHANGE | Account form, current-password proof, confirmation/strength/length checks, session revocation and attributed audit. HTTP RED 404→GREEN; wrong current password, CSRF, foreign Origin, anonymous access and invalid inputs denied without mutation. Other users' sessions survive. | Authenticated change only: not signup, email reset, 2FA or API-token revocation. |
| WEB-PASSWORD-LOCK | SQLite and PostgreSQL actual-DML contention matrices: valid counterpart, same-version hash change, session deletion/expiry, verification withdrawal; audit sabotage preserves credential/session state and permits retry after removal. | Final post-write expiry check implemented; this matrix tests expiry mutation before acquisition, not a separate wall-clock expiry across a later audit wait. |

Final verification:

- `python3 /tmp/listmngr-password-postgres-db.py`: exact
  `web_sessions::password::tests::password_change_postgres_lock_matrix` passed;
  `/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-password-pg-khmijkrm/`.
- `python3 /tmp/listmngr-password-postgres-api.py`: exact
  `postgres_browser_password_change` passed;
  `/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-password-pg-cobbt2vl/`.
  Both used new owned databases, removed afterward with absence verified.
- `python3 /tmp/listmngr-composed-final-gates.py`: artifacts/fmt/locked workspace
  build/tests/strict Clippy/deny/audit with documented ignore/diff all passed;
  `/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-final-gates-wkn2n0xt/`.
  Earlier test fixture column error and test-only Clippy failures were corrected;
  the final passing run supersedes them.
- `python3 /tmp/listmngr-composed-postgres-full.py`: existing 13 exact live tests
  passed, owned database removed;
  `/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-pg-full-dy7tofsx/`.
- `python3 /tmp/listmngr-composed-browser-parent.py`: actual account form submission,
  password change, cookie removal, new-password login and logout passed alongside
  prior browser scenarios; fingerprint unchanged and confirmation bridge removed;
  `/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-browser-parent-xlb2u6v6/`.

This remains experimental product development, not production Mailman replacement
acceptance. No live/development data or denied email-only workflow was used.

## Tolerant archive threading metadata — current increment

| ID | Implemented and exercised | Boundary |
|---|---|---|
| ARCHIVE-THREAD-HINTS | `cargo test --locked -p listmngr-archive --test archive malformed_thread_metadata_does_not_reject_valid_archive_messages -- --exact`: RED with `Validation("invalid Message-ID")`, then GREEN. Skips invalid References entries, falls back to In-Reply-To or standalone identity; verifies stored thread membership and queue state done. | Message's own ID validation unchanged; no subject heuristics, historical reindex or automatic shunt retry. |
| ARCHIVE-LATE-REFERENCES-CONTROL | `references_only_replies_join_the_root_even_when_parent_arrives_late`: references-only grandchild/child/root indexed in reverse order form one thread. | This was already GREEN before production changes: regression evidence, not a newly implemented late-parent fix. |

Final candidate: `python3 /tmp/listmngr-thread-postgres.py` ran the same malformed
hint and late-parent processing/readback matrices on fresh PostgreSQL, exact
`postgres_archive_thread_metadata_matrix` passed without ignore; owned database
removed and absence verified. Evidence:
`/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-thread-pg-pskfv1fd/`.

`python3 /tmp/listmngr-composed-final-gates.py` passed artifacts/fmt/locked workspace
build/tests/strict Clippy/deny/audit with documented ignore/diff. Evidence:
`/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-final-gates-q3o8cozd/`.
`python3 /tmp/listmngr-composed-postgres-full.py` passed the existing 13 exact live
tests, removed its owned database. Evidence:
`/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-pg-full-1gopjpup/`.
This increment exercises the archive processor and database, not new Chromium or
live SMTP/LMTP delivery; no development/live data, commit or push was used.

## Attachment text-byte fidelity — current increment

| ID | Implemented and exercised | Boundary |
|---|---|---|
| ARCHIVE-ATTACHMENT-CHARSET | `cargo test --locked -p listmngr-mail --test attachments text_attachment_download_preserves_charset_octets_and_line_endings -- --exact`: observed behavioral RED (Windows-1252 octets expanded to UTF-8), then GREEN. Text/plain, text/html, text/csv with quoted-printable/base64/8bit preserve distinct payloads, CRLF/LF and non-UTF-8 bytes. Download now transfer-decodes the selected raw MIME body rather than its display string. | Supersedes earlier text-normalization download limitation. Does not preserve original response filename or establish whole-message/whole-archive backup fidelity. |

Final candidate verification:

- `python3 /tmp/listmngr-composed-final-gates.py`: required artifacts/fmt/locked
  workspace build/tests/strict Clippy/deny/audit (documented advisory ignore)/diff
  passed. `/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-final-gates-9_m7ipbx/`.
- `python3 /tmp/listmngr-attachments-postgres.py`: attachment HTTP matrix including
  exact charset payload bytes passed on fresh PostgreSQL; database deletion and
  absence verified. `/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-attachment-pg-mtdjepce/`.
- `python3 /tmp/listmngr-composed-postgres-full.py`: existing 13 exact live tests
  passed, owned database removed. `/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-pg-full-jihzpkhx/`.
- `python3 /tmp/listmngr-composed-browser-parent.py`: actual private CSV attachment
  download compares saved Windows-1252 bytes, including NUL/non-UTF-8, and retains
  safe filename plus post-logout denial. Existing browser forms/archive/mbox pass;
  source fingerprint unchanged and confirmation bridge removed.
  `/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-browser-parent-inbhfokv/`.

## Individual archive attachments — current increment

This supersedes the earlier “attachment UI unavailable” limitation for bounded
individual downloads, not full HyperKitty parity or archival byte preservation.

| ID | Implemented and exercised | Boundary |
|---|---|---|
| WEB-ARCHIVE-ATTACHMENTS | Generated per-message/index links; exact binary bytes, distinct HTML payload forced to download, escaped names, invalid/conflicting selections, current private membership and never-policy guards. `cargo test --locked -p listmngr-api --test webui archive_attachment_download_uses_current_policy_and_exact_selection -- --exact`: observed RED 400 vs 200, then GREEN. | Native links; no inline rendering, original filename preservation, malware scanning or new aggregate-memory cap. |
| ARCHIVE-ATTACHMENT-MIME | `cargo test --locked -p listmngr-mail --test attachments`: 2 passed. Base64/quoted-printable differential bytes, absent index, empty/oversized input and 64/65 attachment boundary; corrupt transfer-encoding regression observed RED before rejecting parser recovery flags. | Text parts may normalize to UTF-8; limits apply after repository publication reading, not before every allocation. |

Final candidate gates all passed:

- `python3 /tmp/listmngr-composed-final-gates.py`: artifacts, fmt, locked workspace
  build/tests, strict Clippy, deny, audit with documented `RUSTSEC-2023-0071`
  ignore, diff. Evidence: `/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-final-gates-265f1h7q/`.
- `python3 /tmp/listmngr-attachments-postgres.py`: same HTTP attachment matrix,
  exact `postgres_archive_attachment_matrix`, 1 passed / 0 ignored; fresh owned
  database removed and absence verified. Evidence: `/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-attachment-pg-4p74np50/`.
- `python3 /tmp/listmngr-composed-postgres-full.py`: existing 13 exact live tests
  passed, owned database removed. Evidence: `/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-pg-full-ancaj4eq/`.
- `python3 /tmp/listmngr-composed-browser-parent.py`: real Chromium clicked the
  private attachment link, saved `private-attachment.bin`, compared exact bytes
  and suggested safe filename, then verified post-logout denial. Existing forms,
  public/private archives and mbox checks also passed; multipart mbox body is
  independently parsed by Python mailbox. Screenshot inspected, source fingerprint
  unchanged, confirmation bridge removed. Evidence: `/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-browser-parent-x78rcy2q/`.

No standalone email-only workflow, live/development data mutation, commit or push.

## Private browser archive — current increment

This section supersedes earlier public-only browser limitations below, not the
remaining whole-product, attachment, administration or deployment obligations.

| ID | Implemented and exercised | Boundary |
|---|---|---|
| WEB-ARCHIVE-PRIVATE | Account discovery link; page/search/thread/permalink/mbox guarded by current session, credential version and verified owned membership. Server-owner-only outsiders denied. `cargo test --locked -p listmngr-api --test webui private_archive_browser_requires_current_verified_membership_and_session -- --exact`: behavioral RED (403 instead of 200), then GREEN; missing account link also observed RED before its addition. | Membership-based browser authority, not a synthesized admin token; coarse lock throughput remains open. |
| WEB-ARCHIVE-PRIVATE-LOCK | `cargo test --locked -p listmngr-db --lib private_archive_read_rechecks_authority_after_real_writer_contention`: valid-reader control plus session, credential, address verification/link, membership deletion, expiry and AsUser identity changes after actual SQLITE_BUSY observation. Workspace run below passed this test. | SQLite causal lock matrix, not PostgreSQL private-archive contention proof. |

Final-byte execution:

- `python3 /tmp/listmngr-composed-final-gates.py`: artifacts/fmt/locked workspace
  build/tests/strict Clippy/deny/audit with documented ignore/diff all passed.
  `/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-final-gates-ihlbxw9j/`.
- `python3 /tmp/listmngr-composed-postgres-full.py`: existing 13 exact live tests
  passed; owned database removed and absence verified.
  `/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-pg-full-sigpxd90/`.
- `python3 /tmp/listmngr-private-archive-postgres.py`: new exact opt-in
  `postgres_private_archive_browser_matrix`, 1 passed, 0 ignored. Same six HTTP
  projections and revocation controls on a fresh PostgreSQL database, then verified
  database removal. This test is separate from the existing PostgreSQL script.
  `/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-private-archive-pg-v7xow2jy/`.
- `python3 /tmp/listmngr-composed-browser-parent.py`: actual account → unadvertised
  private archive → permalink → mbox download, independently parsed private body,
  and post-logout denial without body disclosure. Public archive and prior forms
  also pass. Screenshot `10-private-archive.png` inspected; source fingerprint
  unchanged and confirmation bridge removed.
  `/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-browser-parent-3b2qu__y/`.

No live/development database or denied standalone email-only workflow was used.

## Browser mbox download — current increment

| ID | Implemented and exercised | Boundary |
|---|---|---|
| WEB-ARCHIVE-MBOX | “Download this selection (mbox)” preserves page/search/thread/permalink, returns an attachment of at most 20 cooked messages, and retains private/never policy rejection. `cargo test --locked -p listmngr-api --test webui archive_browser_mbox_exports_only_selected_page_thread_or_message -- --exact` passed after observed behavioral RED (400 instead of 200). | SQLite HTTP matrix; not full-archive export completeness, original spool bytes, private browser authorization or a new aggregate-memory cap. |

The regression follows generated HTML download links, checks differential
cardinalities (20/5/2/1/0), and corrupts only the look-ahead fixture row to prove
export does not decode that unselected message. Actual Chromium downloaded
`archive-selection.mbox`; Python `mailbox` independently parsed its exact single
message, cooked subject and body. Browser evidence and the real downloaded file:
`/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-browser-parent-3jvtzl0i/`.

Final `python3 /tmp/listmngr-composed-final-gates.py` passed artifacts, fmt, locked
workspace build/tests, strict Clippy, deny, audit with the documented ignore and
diff check. Logs:
`/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-final-gates-m9ayv0u2/`.
`python3 /tmp/listmngr-composed-postgres-full.py` ran the existing 13 exact live
tests and verified owned database removal. Logs:
`/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-pg-full-428xrdq3/`.
That PostgreSQL gate does not imply a new PostgreSQL browser-download matrix.
The final Chromium wrapper also passed with unchanged source fingerprint and
removed confirmation bridge. Live email-only permission remains denied.

## Archive message permalinks — current increment

| ID | Implemented and exercised | Boundary |
|---|---|---|
| WEB-ARCHIVE-MESSAGE | Exact list/hash lookup via `?message={hash}`, escaped cooked text, stable link/reload, missing-message 404, input bounds, conflicting-filter rejection, and current private/never policy denial. `cargo test --locked -p listmngr-api --test webui archive_permalink_selects_exact_message_beyond_first_page_with_current_policy -- --exact`: behavioral RED (400 instead of 200) before implementation, then GREEN. | Public browser only; native URL, not HyperKitty URL parity. |

The shared HTTP matrix places 25 corrupt unrelated archive rows before the target
and requires successful exact read without decoding them, plus cross-list denial.
`python3 /tmp/listmngr-permalink-postgres.py` ran the exact ignored
`postgres_archive_permalink_matrix` on a newly created PostgreSQL database:
1 passed, 0 ignored; database removed and absence verified. Log:
`/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-permalink-pg-m0y7gyf5/gate.log`.
This new opt-in test is separate from the existing PostgreSQL gate.

Final required workspace gates passed via
`python3 /tmp/listmngr-composed-final-gates.py` (artifacts, fmt, locked build/tests,
strict Clippy, deny, audit with the documented ignore, diff):
`/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-final-gates-easwlr7v/`.
The existing PostgreSQL script was also rerun: 13 exact tests passed, owned DB
cleanup verified, log directory
`/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-pg-full-ccjz9xaj/`.

`python3 /tmp/listmngr-composed-browser-parent.py` passed the real Chromium test
with a stable source fingerprint: click “Permanent link”, assert its exact URL,
reload, assert the correct single message and no executable script node. Evidence:
`/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-browser-parent-wjos45sh/`.
This does not change the denied live-email-only boundary or whole-product status.

## Public archive browser — current implementation evidence

WEB-ARCHIVE now supplies actual public archive browsing rather than another
baseline-only checkpoint. `cargo test --locked -p listmngr-api --test webui
public_archive` passed both new regressions after observed HTTP 404→200 and
missing-link→visible-link RED/GREEN cycles. Private/never policy changes, search,
thread filtering, escaping and page boundaries are asserted on isolated SQLite.

Final-source commands and evidence (all exited zero):

- `python3 /tmp/listmngr-composed-final-gates.py`: artifacts, fmt, locked workspace
  build/all-target tests, strict all-feature Clippy, deny, audit with the documented
  advisory ignore, and diff check. Logs:
  `/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-final-gates-czp2tbg6/`.
- `python3 /tmp/listmngr-composed-browser-parent.py`: exact opt-in Chromium test,
  including public archive discovery/search/thread and mobile screenshot
  `08-mobile-archive.png`, inspected visually. Source fingerprint unchanged;
  private confirmation bridge removed. Evidence:
  `/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-browser-parent-u19slmoi/`.
- `python3 /tmp/listmngr-composed-postgres-full.py`: existing PostgreSQL gate,
  13 exact tests executed (not skipped), fresh owned database removed and absence
  verified. Evidence:
  `/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-pg-full-l0j45d7w/`.

The new archive HTTP matrix itself is SQLite-backed; the PostgreSQL gate above
does not imply a new PostgreSQL archive-browser matrix. Full private archive UX,
attachment access and HyperKitty parity are still open. No live email-only test
was run, and no production/development database was used.

## Canonical UI/login + held-recipient convergence

The UI is now an **integrated candidate**, not an accepted whole Mailman product.
The exact UI seed-relative delta and held-recipient semantic delta were applied
in canonical, preserving newer email commands, notice provenance, archive cooking
and live lease/final-ACK paths. `ReviewAction::Accept { max_attempts }` carries
intent only. API review and browser review resolve the effective roster under
SQLite writer reservation / PostgreSQL DML-conflicting table locks, then commit
Out + Digest + policy-enabled Archive, disposition and contextual audit together.
The composed policy/audit regression checks all three children, including zero
immediate recipients and rollback. Original SMTP spelling and frontend CSRF /
exact Origin checks remain intact. Login verifies Argon2 outside writer locks,
then binds exact credentials/address/predecessor to atomic issuance.

Migrations 0000–0012 coexist without renumbering 0009. Historical Phase 1 and
Phase 2 corpora are unchanged; `phase4-composed-schema.snapshot` is the additive
workflow/digest/archive/email/session ledger used by schema/repository tests.

Evidence: `/tmp/listmngr-canonical-convergence/`. `before.json` snapshots the
pre-integration source; `final-tests.log` records workspace all-targets PASS:
341 passed, 18 opt-in ignored across 63 targets. Locked workspace build PASS
(`final-build.log`); focused DB (51), API (59), runner (29) and archive gates also
passed. These overlapping runs are not summed. Chromium self-service PASS with
installed Google Chrome (`browser-chrome.log`, screenshots in `browser/`);
initial bundled-browser attempt failed because its executable was missing, not
because a form assertion failed. The browser uses a disposable DB token bridge,
not standalone SMTP acceptance. Final verification logs use the `verified-`
prefix; they follow removal of one unused test import reported by full Clippy.
No deliberate negative controls or source conflict markers remain.

Parent verification after handback passed all required workspace commands:
`scripts/check-phase0-artifacts.sh`, `cargo fmt --all --check`,
`cargo build --locked --workspace`, `cargo test --locked --workspace --all-targets`,
`cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`,
`cargo deny check`, `cargo audit --ignore RUSTSEC-2023-0071`, and `git diff --check`.
Runner: `/tmp/listmngr-composed-final-gates.py`; logs:
`/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-final-gates-rbe3vj3n/`.

`python3 /tmp/listmngr-composed-postgres-full.py` executed the canonical
`scripts/test-postgres.sh` in a NEW owned database: all 13 exact live tests ran
with no ignored cases; migration/connectivity/CRUD passed. Logs:
`/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-pg-full-g9pwgu0j/`.
`python3 /tmp/listmngr-composed-postgres-parent.py` separately ran the four
PostgreSQL UI/login/session/revocation/authority tests, one new database per test.
All passed with source hashes unchanged; all probe databases were removed and
absence verified. Logs:
`/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-pg-parent-v4klxuww/`.
An earlier run passed its cases but was invalidated because `crates/db/Cargo.toml`
changed during execution; it is not substituted for the stable rerun.

Parent Chromium evidence:
`/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-browser-parent-3gwv_1tk/`.
The production router passed real browser actions and persisted-effect assertions,
with no page errors and a checked mobile screenshot. This was before the final
DB manifest adjustment. The final-manifest rerun then passed with unchanged
source/manifests and token cleanup; evidence:
`/var/folders/ws/hhlmghps1mjf1_lchf2qfh5h0000gn/T/listmngr-composed-browser-parent-vfheaz9g/`.
The test bridge token was removed; no SMTP confirmation evidence is implied.

Remaining gates/boundaries: PostgreSQL held-recipient selection contention,
compatibility/deployment/hosted CI acceptance.
Previous donor PostgreSQL logs below remain historical, not canonical evidence.
Coarse PostgreSQL locks, GET-authorization concurrency, private archive browsing
UX, complete administration and broader product/security parity remain open.
**Standalone email-only live acceptance remains BLOCKED: permission denied.**
No denied harness or equivalent live workflow was executed. No commit/push or
source-worktree cleanup was performed.

## Canonical email-command integration — bounded regression acceptance

The email candidate was snapshotted externally and integrated against exact
`/tmp/listmngr-commands-seed.json` file bytes, not its anomalous index. Canonical
pre-integration bytes, candidate delta and logs are retained under
`/tmp/listmngr-command-integration/`. No source worktree cleanup, commit, reset or
push was performed; the separate web UI candidate was not modified.

- **Integrated code:** shared core `EmailCommand`; plain-text-only MIME command
  parsing; durable suffix/request routing and worker dispatch; confirm-only
  join/leave and bounded help; migration 0012 and a separate composed schema
  corpus. Original mailbox spelling/0010, job-specific notice provenance/0011,
  live lease clocks, posting fanout, digest and cooked archive privacy remain.
- **Focused regression evidence:** `cargo test --locked -p listmngr-mail --test
  commands` passed (`mime.log`); DB command/schema and runner command/provenance/
  recipient/intake suites passed (`clock-green.log`, `runner-green.log`). The
  expanded `email_command_clock` suite passed (`clock-expanded-green.log`),
  covering expired pool authority, final post-business ACK, confirm join/leave
  rollback and throttled/missing-list ACKs with valid controls. Initial missing
  clock API was compilation-only RED; `clock-semantic-red.log` then demonstrated
  an incorrectly committed join after the injected deadline, repaired by the
  final-clock ACK. Additional confirmation/early-return cases are supplemental
  controls, not retrospective test-first evidence.
- **Composition adjustment:** the candidate's forged-notice test required
  shunting; canonical intentionally treats forged context as an ordinary cooked
  post. The test now asserts stripping Bcc, list headers and a non-null bounce
  sender; the existing real-sink provenance test remains. Outbound production
  code was not replaced by the stale candidate.
- **Standalone live email-only acceptance: BLOCKED (authorization denied).**
  `/tmp/listmngr-commands-evidence/live_email_commands.py` and equivalent
  standalone join/leave/restart flows were not run. Existing fixture SMTP/runner
  tests are not a substitute. Fresh permission is required. No full Mailman,
  browser UI or hosted CI acceptance is claimed.

All Cargo commands in this integration use
`CARGO_TARGET_DIR=<repository>/target`.
The final gate results below are recorded separately from earlier parent runs.

| Fresh canonical gate | Result | Log |
| --- | --- | --- |
| `cargo build --locked --workspace` | PASS, exit 0 | `/tmp/listmngr-command-integration/final-build.log` |
| `cargo test --locked --workspace --all-targets` | PASS, exit 0: 321 passed, 13 explicitly ignored across 62 targets | `/tmp/listmngr-command-integration/final-workspace.log` |
| `cargo test --locked --workspace --doc` | PASS, exit 0; eight crates, no runnable doctests | `/tmp/listmngr-command-integration/final-doctests.log` |
| `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` | PASS, exit 0 | `/tmp/listmngr-command-integration/final-clippy.log` |
| `cargo fmt --all --check` | PASS, exit 0 | `/tmp/listmngr-command-integration/final-fmt.log` |
| `python3 /tmp/listmngr-lease-gates/run_pg.py email-integration-final-postgres` | PASS, exit 0, maintained PostgreSQL gate including composed schema and existing queue/sibling clock matrices | `/tmp/listmngr-lease-gates/email-integration-final-postgres.log` |

The new command clock tests ran on SQLite with injected clocks. The PostgreSQL
gate covers the composed migration/schema and existing backend matrices, not a
new command-specific PostgreSQL contention matrix or standalone live email-only
acceptance. The historical heartbeat timing sensitivity remains recorded; this
fresh full workspace invocation passed without a retry.

## Focused mailbox-spelling repair (not composed closure)

| Acceptance behavior | Evidence | Boundary |
| --- | --- | --- |
| New confirmation preserves delivery spelling; existing account is not verified/relinked | Worktree `/tmp/listmngr-workflow-repair`: `CARGO_TARGET_DIR=/tmp/listmngr-workflows/target cargo test --locked -p listmngr-db --test workflows` — 9 pass; original-spelling test first failed behaviorally | SQLite focused suite; not PostgreSQL or subscription moderation parity |
| Regular SMTP recipient spelling and own-post identity comparison | Same worktree: `CARGO_TARGET_DIR=/tmp/listmngr-workflows/target cargo test --locked -p listmngr-runners --test recipient_spelling` — 2 pass after 2 behavioral failures; corresponding Clippy `-D warnings` pass | Digest snapshots need separate composed verification |
| Restarted real server consumes confirmation once and delivers to exact original SMTP recipient | `CARGO_TARGET_DIR=/tmp/listmngr-workflows/target cargo build --locked -p listmngr` then `/tmp/listmngr-final-edyu_th6/venv/bin/python /tmp/listmngr-final-edyu_th6/live_acceptance.py --repo /tmp/listmngr-workflow-repair --binary /tmp/listmngr-workflows/target/debug/listmngr` — exit 0 | Disposable SQLite/SMTP/LMTP; does not close archive export privacy or full product acceptance |

[PLAN.md](PLAN.md) is the normative product contract. This ledger separates the
current development checkpoint, unclosed acceptance, and historical verification.
Code presence and a passing neighboring test are not full parity or release evidence.

## Parent composed verification — 2026-09-06

| ID | Fresh evidence | Boundary |
| --- | --- | --- |
| REPLACEMENT-SCHEMA | `cargo test --locked -p listmngr-db --test repositories --test schema_contract`: 25 + 2 passed, two PostgreSQL cases ignored in this SQLite run; schema assertion first failed for missing implicit rowid uniqueness in the extractor. `cargo fmt --all --check` and DB all-targets/all-features Clippy pass. | Fixed extractor, not relaxed corpus or altered production constraint. |
| REPLACEMENT-COMPOSED | `cargo build --locked --workspace` passed; `cargo test --locked --workspace --all-targets` and `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` passed on rerun. | Initial full suite failed `heartbeat::tests::short_lease_renews_during_slow_work_and_blocks_second_claimant`; isolated retry and full rerun passed. Timing sensitivity remains open. Does not include separate UI/email-command/lease-fencing candidates. |
| REPLACEMENT-POSTGRES | `python3 /tmp/listmngr-final-repair/postgres_gate.py` passed against the disposable loopback PostgreSQL fixture, invoking `scripts/test-postgres.sh`. Separate exact ignored tests `postgres_isolated_digest_publish_rollback_and_single_winner` and `postgres_waiting_patch_does_not_overwrite_other_committed_fields` also passed; these are now included in the gate script. | Actual PostgreSQL, no SQLite fallback; not exhaustive new-feature PostgreSQL coverage. |
| REPLACEMENT-CLIENT | `/tmp/listmngr-final-edyu_th6/venv/bin/python scripts/test-mailmanclient.py` passed with mailmanclient 3.3.5 after fixing the replay probe's asynchronous child-job race. | Real isolated binary/HTTP/LMTP/SMTP; bounded Phase 1 and held behavior, not full Mailman parity. Fixture server stopped and temporary DB removed. |

## Subsequent parent lease-fencing verification

- The worker left an archive negative control in canonical. Parent restored
  `ack_leased_job(..., queue.time(now_ms))` before final verification. The isolated
  archive-only negative control was not executed; prior publication-wait RED
  evidence directly caught digest. Do not claim a separate archive mutation kill.
- `cargo test --locked -p listmngr-db --test sibling_lease_clock --test mail_queue_lock_clock`:
  four SQLite tests passed; two PostgreSQL tests explicitly ignored in that run.
- `CARGO_TARGET_DIR=<canonical>/target python3 /tmp/listmngr-lease-gates/run_pg.py parent-final-postgres`:
  full `scripts/test-postgres.sh` passed, including the final queue/sibling
  PostgreSQL clock matrix; `/tmp/listmngr-lease-gates/parent-final-postgres.log`.
- After applying remaining test formatting, `cargo build --locked --workspace`,
  `cargo test --locked --workspace --all-targets`,
  `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`
  and `cargo fmt --all --check` passed in one fail-propagating command chain.
  Workspace test log: `/tmp/listmngr-lease-gates/parent-final-workspace.log`.
- The old gate wrapper returned zero despite Clippy failure; it is not final
  acceptance evidence. UI/email-only worktrees and known heartbeat timing
  sensitivity remain separate. Earlier RED PostgreSQL fixture schemas have not
  been reported cleaned up; no development data was used.

## Historical checkpoint — migration 0004

### Ongoing replacement work (not whole-product acceptance)

The complete open scope is tracked in [MAILMAN_REPLACEMENT.md](MAILMAN_REPLACEMENT.md).
The checkpoint tables below describe the historical base; they do not certify
new source currently under integration.

| ID | Implemented behavior | Fresh focused evidence |
|---|---|---|
| **REPLACEMENT-SAFETY** | Typed transient intake, atomic batch rollback, private-header/anonymity and all-marker loop checking; scoped identity/relink and legacy admin bounds. | Parent: `cargo test --locked -p listmngr-runners --all-targets`, `cargo test --locked -p listmngr-db --test identity_safety`, `cargo test --locked -p listmngr-api --test rest` (31 REST tests pass). Focused integrated evidence, not whole-product acceptance. |
| **REPLACEMENT-POSTING** | Announcement/per-list defaults control actual accept/hold queues; config PATCH preserves unrelated concurrent writes. | `cargo test --locked -p listmngr-runners --test announcement` (2 pass); `cargo test --locked -p listmngr-db --test list_posting_settings`; isolated PG `... --test list_posting_settings postgres_waiting -- --ignored --nocapture` (1 pass after behavioral RED). Broader rule catalog remains open. |
| **REPLACEMENT-DIGEST-RENDER** | Decoded plain text, nested MIME messages, summary, stable issue bytes and invalid-mode rejection. | `cargo test --locked -p listmngr-mail --test digest` (3 pass). Renderer only; durable worker integration pending. |
| **P2-RECOVERY** | Recipient inspection and explicit audited sent/failed/retry resolution; retry requires duplicate-risk acknowledgement; known-sent outcomes preserved. | Parent: `cargo test --locked -p listmngr --test queue_recovery` (2 pass); `cargo test --locked -p listmngr-db --test queue_operations` (3 SQLite pass); isolated PostgreSQL `postgres_isolated_resolution_is_atomic_and_single_winner -- --ignored --exact` (1 pass, PostgreSQL 14.24). Full composed acceptance/review pending. |


The implemented mail slice is **opt-in plaintext trusted-relay LMTP → inbound
policy → held moderation or outbound queue → SMTP**, with exact-byte database
intake, durable recipient attempts, and conservative uncertainty quarantine.
Enablement requires both `mta.enabled` and
`mta.smtp_tls = "plaintext_trusted_relay"`. This is **not production-ready, full
Phase 2 acceptance, or a Mailman replacement**.

| Phase | Status | Boundary |
|---|---|---|
| 0 | Implemented baseline; enclosing operational gates open | Historical local container/unit checks; no hosted CI or booted target-host systemd acceptance claimed. |
| 1 | Implemented, with bounded local acceptance | CRUD, REST/CLI, preference layering and pinned-client subset; current SQLite/client gates pass, PostgreSQL acceptance predates 0004. |
| 2 | Implemented, with bounded local acceptance per row | Mail path end to end (`P2-*` rows above); open: `P2-E2E-ACCEPTANCE`, `P2-HELD-FORWARD`. |
| 3 | Implemented, with bounded local acceptance per row | Subscription workflows, email commands, bounces, digests, task runner, notices, i18n (`P3-*` rows above); open: `P3-DUAL-BACKEND-CI`, `P3-CLIENT-SUITE`, `P3-DIGEST-SNAPSHOT`, `P3-SUBSCRIPTION-E2E`. |
| 4 | Partial / interim rendering | String-built SSR slices (`P4-WEB-*`); the template shell, accounts, 2FA/OIDC and the admin screens are the `P4-*` packages in `docs/PLAN.md`. |
| 5 | Partial | Threading, Message-ID-Hash and a basic read-only archive; search, UI, interactions and import are the `P5-*` packages. |
| 6 | Not implemented | Advanced features and Mailman migration. |
| 7 | Not complete | Release hardening and 1.0. |

### Current verification results

These are recorded parent/worker results, not fresh executions by the documentation
writer. A command below is a reproducible entry point, not a claim that every gate
passed on the latest bytes.

| Gate | Latest evidence and qualification |
|---|---|
| Focused O2/O3 tests | PASS for SQLite real-TCP sink/audit rollback/reopen/reclaim, cancellation, mixed/reserved outcomes, failed reservation and final-250/no-QUIT regressions listed below. |
| `cargo test --locked --workspace --all-targets` | Parent reports PASS for current attempt candidate. External PostgreSQL tests are ignored in this suite. |
| `cargo build --locked --workspace` | Parent reports PASS for current attempt candidate. |
| `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings` | Parent reports PASS for current attempt candidate. |
| `scripts/test-postgres.sh` | **Current attempt gate timed out (300 seconds); no PASS.** The prior seven-contract PASS predates migration 0004 and does not verify the new attempt contract. |
| Real `mailmanclient==3.3.5`, Phase 1 + held | Parent reran the maintained uv command below on the migration-0004 candidate before commit: exit 0, both flows PASS; fixture server and database cleaned up. |
| fmt/artifacts/Python/actionlint/deny/audit | Parent reran the maintained commands below before commit: all exit 0. Audit retains `RUSTSEC-2023-0071`; deny retains duplicate-dependency warnings. |
| Container/systemd/hosted CI | Historical evidence only. No current image/runtime, hosted CI or target-host systemd acceptance claimed. |

## Current bounded implementation and acceptance IDs

“Implemented” below means the named subset exists, not that the enclosing PLAN
acceptance has passed. PostgreSQL evidence is historical unless explicitly stated.

| ID | Implemented subset | Repository evidence / focused command |
|---|---|---|
| **P2-STORE** | Exact-byte DB intake, distinct submission UUIDs, SHA-256 blob identity; bounded Message-ID parsing/archive hash; standalone immutable filesystem library. | `crates/db/src/mail_queue.rs`, `crates/mail/src/{metadata,store}.rs`; `cargo test --locked -p listmngr-db --test mail_queue`; `cargo test --locked -p listmngr-mail --all-targets` |
| **P2-QUEUE** | Atomic enqueue/audit, claim, retry/shunt, expired-lease recovery and token fencing, subject to O1. | `crates/db/tests/{mail_queue,mail_queue_security}.rs`; isolated queue contract in `scripts/test-postgres.sh` |
| **P2-CLI** | Validated inbound injection, bounded ls, metadata show and opt-in exact raw export. | `crates/cli/src/queue.rs`; `cargo test --locked -p listmngr --test queue`; CLI unshunt absent |
| **P2-RUNTIME** | Monotonic heartbeat, repository `unshunt`, `complete_with_children`, `pending_recipients`, `begin_delivery`, `finish_delivery`; opt-in supervision and real mail workers. | `crates/db/tests/mail_queue_runtime.rs`, `crates/runners/src/`, `crates/cli/tests/mailpath_e2e.rs`; workspace tests |
| **P2-MODERATION** | Transactional durable hold/review and out-job/recipient snapshot on accept; no notices/bounces. | `crates/db/src/moderation.rs`, `crates/db/tests/moderation.rs`, `crates/api/tests/held.rs` |
| **P2-POLICY** | Pure bounded posting decisions and enabled regular-recipient selection honoring own-post preference. Unsupported header-match handling fails closed to hold. | `crates/pipeline/src/policy.rs`; `cargo test --locked -p listmngr-pipeline --all-targets`; not full chains/DMARC |
| **P2-TRANSPORT** | LMTP session framing/deadlines/reply cardinality, bounded plaintext SMTP responses and per-recipient outcomes, injection-safe header cooking; wired to the binary's enabled mail role. | `crates/mail/src/{lmtp,smtp,cook}.rs`, `crates/mail/tests/`; library and real-process tests; R1 still open |

Migrations `0001_mail_queue.sql`, `0002_mail_policy.sql`,
`0003_delivery_ambiguous_status.sql`, and `0004_delivery_attempt_token.sql` are
additive. The original Phase 1 semantic corpus is preserved; separate snapshots
extend both corpus consumers (`repositories.rs` and `schema_contract.rs`).
Filesystem-store selection, filesystem/DB lifecycle and garbage collection remain
absent; DB intake does not establish selectable-store parity.

### Held moderation: H1–H4

| ID | Bounded result | Tests and evidence boundary |
|---|---|---|
| **H1** | Accept/defer comments persist on native/compat and JSON/form requests. | `accept_and_defer_preserve_comments_on_both_prefixes_and_encodings`: historical RED empty comment → GREEN. |
| **H2** | Pending-state fence, disposed defer returns 409 without stray writes. | `disposed_defer_conflicts_without_stray_writes`, `racing_accept_and_defer_have_a_serializable_result`; SQLite serialized/concurrent controls and historical isolated PostgreSQL held contract PASS. |
| **H3** | User/token/peer-IP audit context and comment are transactional with moderation effects. | `moderation_audit_preserves_edge_context` (historical RED → GREEN), `moderation_audit_failure_rolls_back_every_business_write` (SQLite sabotage/rollback). PostgreSQL moderation-audit sabotage and broader domain/mismatched-ID coverage remain open. |
| **H4** | Real pinned-client Phase 1 + held flow PASS on the current migration-0004 candidate, independently rerun by parent before commit. | `scripts/test-mailmanclient.py`, `scripts/mailmanclient_held.py`; real binary, disposable SQLite, LMTP nonmember submission and TCP SMTP sink, no seeded held/message/queue rows. |

H4 checks count/list/get, all tested held properties and raw preview, scope denial,
unsupported options, defer comment retention, accept/replay with one sink delivery,
and reject/discard without delivery. The `/3.1` client's `name@host` paths use
flavor-aware `parse_list_path`; native IDs and authorization are not relaxed.
Action/comment only is supported; forwarding fails closed. Moderation replay is
not exactly-once SMTP acceptance.

### O2/O3 durable attempts and restart evidence

`begin_delivery` commits ambiguous/in-flight reservations, owning attempt tokens,
and audit before SMTP commands (TCP connect may happen first). Only the owning
lease can resolve a reservation through `finish_delivery`. Known transient results
explicitly restore pending; omitted reserved results remain ambiguous, while
omitted unreserved results remain pending. Outcome/audit rollback, cancellation,
and reclaim do not erase durable uncertainty. Ambiguous recipients are excluded
from automatic retry, including after repository unshunt. They can remain on a
done job; done is not a delivery receipt. Even never-sent attempts may require
manual reconciliation; there is no resolution CLI/UI or exactly-once guarantee.

| ID / scenario | Bounded evidence |
|---|---|
| **O2/O3** final-250 and missing-final-reply with failed ACK audit | `outbound::durability_tests::{final250_audit_failure_restart_never_replays_data,missingfinalreply_audit_failure_restart_never_replays_data}`: SQLite RED second DATA → GREEN zero second DATA after pool reopen/new lease; inspectable ambiguity and original bytes retained. |
| **O2/O3** cancellation after DATA | `canceled_after_data_restart_never_replays`: actual DATA barrier, cancel before final reply, reopen/reclaim, no second DATA; additional passing coverage, not a separate historical RED claim. |
| **O2/O3** reservation rollback and mixed outcomes | `reservation_audit_failure_fences_smtp_and_rolls_back`, `reserved_mixed_results_retry_only_known_transient_and_fence_old_lease`: no SMTP command after failed begin audit; only known transient retries; omitted reserved recipient remains ambiguous; stale/unrelated tokens cannot resolve it. |
| **O2** unreserved mixed/missing control | `mixed_and_missing_outcomes_persist_and_retry_only_pending_recipients` in `crates/runners/src/outbound_tests.rs`; missing unreserved recipients stay pending, unlike reserved uncertainty. |
| **O3** final DATA publication | `crates/mail/tests/smtp_final_result.rs::final250_returns_without_waiting_for_quit`: historical RED → GREEN; known acceptance returns within the bounded test deadline without QUIT I/O. |
| **O2/O3 PostgreSQL** | `outbound::durability_tests::postgres_isolated_audit_failure_restart_never_replays_data` in `crates/runners/src/outbound_durability_tests.rs` covers both final replies with unique schemas, real TCP and PostgreSQL ACK-audit sabotage. Wired into the maintained PG gate; **current execution timed out, not passed**. |
| **P2-RUNTIME / restart-lock** | `transient_delivery_waits_for_sqlite_writer_before_reading_snapshot`: historical RED → GREEN after `BEGIN IMMEDIATE`, with a real second-connection writer lock and retry-state assertion. This fixes snapshot upgrade failure, not O1. |
| **P2-RUNTIME / restart** | `durable_intake_survives_a_real_process_restart` in `crates/cli/tests/mailpath_e2e.rs`; zero accepted DATA before restart and exact raw retention. Historical ten parallel-suite repetitions passed; current workspace suite includes the restart test. |

## Residual P1s and enclosing blockers

| ID / gate | Status | Unclosed obligation |
|---|---|---|
| **R1 — partial LMTP batch commit timeout** | **Superseded (2026-09-14)** | Multi-recipient intake commits as one batch and a DATA-time database wait answers every recipient without late intake: `crates/runners/tests/inbound_safety.rs` (`second_recipient_storage_failure_rolls_back_entire_batch`, `data_database_wait_timeout_replies_for_all_without_late_intake`), `crates/mail/src/lmtp.rs` (one transaction per batch, never per recipient). PASS in `cargo test --locked --workspace --all-targets --no-fail-fast` on 2026-09-14 (145 binaries, 936 passed, 0 failed, 56 ignored). |
| **O1 — lease clock after database lock waits** | **Superseded (2026-09-14)** | The lease clock is read after lock acquisition and fences every mutation: `crates/db/tests/mail_queue_lock_clock.rs` (`expiry_during_postgres_row_lock_wait_fences_all_mutations`, `postgres_final_audit_wait_fences_queue_authority`), `crates/db/tests/sibling_lease_clock.rs`, `crates/db/tests/email_command_clock.rs`, and the `*_final_audit` waits, all wired into `scripts/test-postgres.sh`. PASS on 2026-09-14 (`TEST_POSTGRES_URL=… scripts/test-postgres.sh`, 38 tests, disposable PostgreSQL 14.24). |
| **O2/O3 backend acceptance** | Closed by the current gate | The PostgreSQL gate has passed on every ledger row since `P2-TEMPLATES`; the historical attempt timeout is no longer the latest evidence. |
| Full Phase 2 and R/O review denominator | Open | Remaining: `P2-E2E-ACCEPTANCE` (headers/DKIM verify, ban DSN, max-size, `personalize=full` VERP, kill -9 mid-pipeline) and `P2-HELD-FORWARD`; see `docs/PLAN.md` Phase 2. |
| Later features | Partly implemented | Transport TLS/SMTP AUTH, DKIM, DMARC mitigation, bounces, digests, subscription workflows and notices have their own rows above. Open: DMARC `wrap_message`/ARC (Phase 6), archive search/UI (Phase 5), administration UI (Phase 4), migration (Phase 6). |
| Recovery and operations | Open | No automatic uncertainty resolution, store GC, full latest-candidate image/runtime or target-host systemd acceptance; no hosted CI PASS claimed. |

## Maintained verification commands

Run from the repository root. PostgreSQL commands require a deliberately disposable
backend supplied via `TEST_POSTGRES_URL`; the script migrates/writes fixtures and
must not target a development or production database. Credentials are not included
here. Historical temporary helpers/log files are not prerequisites or authoritative
commands.

```sh
cargo fmt --all --check
cargo build --locked --workspace
cargo test --locked --workspace --all-targets
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
scripts/check-phase0-artifacts.sh
python3 -m unittest discover -s scripts/tests -v
python3 scripts/check-production-crates.py
actionlint
cargo deny check
cargo audit --ignore RUSTSEC-2023-0071
# Install the pinned requirements in a venv, or use uv:
uv run --with-requirements tests/compat/requirements-mailmanclient.txt python scripts/test-mailmanclient.py
# Set TEST_POSTGRES_URL securely to a disposable backend first:
scripts/test-postgres.sh
```

The PG script explicitly invokes repeated migration/CRUD, exact semantic schema,
scoped API authorization, isolated queue, monotonic heartbeat, fenced delivery,
held review, and durable-attempt sabotage contracts; there is no SQLite fallback.
Default workspace success does not execute its ignored external-backend tests.
The real-client harness starts and cleans up its own loopback server, SQLite DB,
LMTP fixture and SMTP sink without reading development `.env` or credentials.
Local success never substitutes for a hosted CI result.

## Historical acceptance — not latest-candidate proof

### Phase 0 baseline (2026-09-05)

| ID | Contract | Repository evidence | Verification command | Historical checkpoint status |
|---|---|---|---|---|
| **P0-08** | Blocking CI runs fmt, locked build, clippy, tests, a PostgreSQL-backed gate, deny, audit, and cache. | `.github/workflows/ci.yml`, `scripts/test-postgres.sh`, `scripts/test-mailmanclient.py` | `actionlint`; `TEST_POSTGRES_URL=… scripts/test-postgres.sh`; Cargo/Python gates | local commands verified; hosted CI pending |
| **P0-09** | Compose credentials align without committed secrets; root context is filtered; application becomes healthy. | `.env.example`, `.dockerignore`, `deploy/docker-compose.yml`, `deploy/Dockerfile` | isolated Compose config + `up -d --build --wait`; HTTP probes; `docker inspect` (historical topology summarized below) | verified locally: real scratch image, healthy service, HTTP 200, UID 1000, read-only, dropped capabilities |
| **P0-10** | A runnable hardened systemd unit defines state/work directories and required protections. | `deploy/systemd/listmngr.service`, `deploy/README.md` | `systemd-analyze verify` on Linux with the real built executable mounted at `/usr/local/bin/listmngr` | unit validation verified on systemd 252; running service/syscall-filter validation still requires the target Linux host |
| **P0-11** | Contributor, architecture, security, parity, deployment, and MTA-boundary docs are actionable and avoid later-phase claims. | `README.md`, `CLAUDE.md`, `docs/*.md`, `security.txt`, `deploy/README.md` | `scripts/check-phase0-artifacts.sh` plus documentation review | contract verified; external review pending |
| **SEC-10** | Supply chain and deployments are least-privilege and reproducible: full AGPL text, locked/pinned inputs, deny/audit, static non-root image, read-only runtime, systemd hardening. | `LICENSE`, `Cargo.lock`, `deny.toml`, CI, Docker/Compose/systemd files | contract script, `cargo deny check`, `cargo audit`, image inspection and runtime probes | local supply-chain/container/unit checks verified; documented inactive RSA exception retained; target-host systemd runtime and hosted CI pending |


Historical local evidence includes blocking Rust/Python/artifact/actionlint gates,
deny/audit with the existing inactive `RUSTSEC-2023-0071` exception, a real static
musl/scratch Compose image (healthy, HTTP 200, UID/GID 1000, read-only root,
capabilities dropped), and Linux systemd 252 unit validation with the real binary.
Unit validation was not a booted service/syscall-filter test. Reproduction topology
and safe disposable-project handling are described in `../deploy/README.md`.

### Phase 1 baseline and CLI integration (2026-09-05)

| ID | Contract | Repository evidence | Verification command | Historical checkpoint status |
|---|---|---|---|---|
| **P1-DB** | Complete Phase 1 schema and portable Domain/User/Address/List/Member/Preferences/Token/Audit CRUD on SQLite and PostgreSQL. | `crates/db/migrations/0000_init.sql`, `crates/db/src/lib.rs`, `crates/db/tests/repositories.rs`, `crates/db/tests/schema_contract.rs` | `cargo test --locked -p listmngr-db --test repositories`; `TEST_POSTGRES_URL='<postgres-url>' scripts/test-postgres.sh`; `TEST_POSTGRES_URL='<postgres-url>' cargo test --locked -p listmngr-db --test schema_contract live_postgres_matches_the_exact_sqlite_semantic_corpus -- --ignored --exact` | verified |
| **P1-PREF** | Nullable system → user → address → member preference layers resolve by precedence for all Phase 1 fields. | `crates/core/tests/preferences_exhaustive.rs`, `crates/db/tests/preference_layering.rs` | `cargo test --locked -p listmngr-core --test preferences_exhaustive`; `cargo test --locked -p listmngr-db --test preference_layering` | verified |
| **P1-REST** | `/3.1` and `/api/v1` share CRUD handlers with distinct serializers; form compatibility, pagination, typed response ETags, and full list config are behavioral. | `crates/api/src/lib.rs`, `crates/api/tests/rest.rs`, `crates/api/tests/openapi.rs`, `tests/compat/fixtures/mailman-3.3/` | `cargo test --locked -p listmngr-api --all-targets` | verified |
| **P1-AUTH** | Bearer scopes and list/domain bounds fail closed; Basic is restricted to enabled `/3.1` compatibility calls from allowlisted socket peers; pre/post-auth rate limits apply. | `crates/api/src/lib.rs`, `crates/api/tests/rest.rs`, `crates/api/tests/phase0_security.rs`, `crates/api/tests/postgres_auth.rs` | `cargo test --locked -p listmngr-api --all-targets`; `TEST_POSTGRES_URL='<postgres-url>' scripts/test-postgres.sh` explicitly runs scoped-user API authorization on PostgreSQL | verified |
| **P1-CLI** | Phase 1 commands use shared repositories; passwords avoid argv; typed errors, HTTP status, normalized IDNA lookup, and one-time token issuance are covered. | `crates/cli/src/{main,errors,status}.rs`, `crates/cli/tests/{cli,contracts,security,status}.rs` | `cargo test --locked -p listmngr --all-targets`; maintained commands above and historical CLI evidence below | verified locally |
| **P1-COMPAT** | Real `mailmanclient==3.3.5` creates a domain and list, subscribes a member, round-trips config, reads the roster, and cleans up. | `tests/compat/mailmanclient_phase1.py`, `tests/compat/requirements-mailmanclient.txt`, `scripts/test-mailmanclient.py` | After a locked build: `uv run --with-requirements tests/compat/requirements-mailmanclient.txt python scripts/test-mailmanclient.py` (or install the same requirements in a venv and run `python3 scripts/test-mailmanclient.py`) | verified |
| **P1-AUDIT** | Persistent business writes and their audit entry commit atomically; owned preferences/tokens are cleaned up transactionally. | `crates/db/src/lib.rs`, `crates/db/tests/repositories.rs`, `crates/api/tests/rest.rs` | `cargo test --locked -p listmngr-db --test repositories`; `cargo test --locked -p listmngr-api --test rest` | verified |


Historical Phase 1 gates passed on SQLite and a disposable PostgreSQL 17 backend,
including scoped authorization and exact schema semantics, and against real
`mailmanclient==3.3.5`. CLI integration added hidden/stdin/FD passwords, redacted
errors, status/proxy/redirect and IPv4/IPv6 controls, IDNA lookup/deletion, token
hash/revocation/expiry checks, and retained atomic role-scoped synchronization.
Earlier suite-size counts are intentionally omitted: they are not current counts.
No historical image/client pass establishes acceptance of migration 0004.

### Protocol/runtime repairs (2026-09-06, pre-0004)

These review IDs and useful regression names are retained for traceability. They
record the earlier RED → GREEN library/runtime repairs, not blanket closure of
the current R/O review. R1 and O1 above are superseded by the named tests as of 2026-09-14.

| Finding | Root cause | Regression test (RED before fix, GREEN after) |
|---|---|---|
| Q1 | `heartbeat` overwrote `lease_until` unconditionally; a shorter-TTL or out-of-order renewal could shrink an already-extended deadline and let another worker reclaim the job early. Fixed with a portable `CASE WHEN $1>lease_until THEN $1 ELSE lease_until END` clamp (works identically on SQLite and PostgreSQL; no `GREATEST`/multi-arg `MAX`). | `heartbeat_deadline_is_monotonic_and_never_shrinks` (`crates/db/tests/mail_queue_runtime.rs`); `postgres_isolated_heartbeat_deadline_is_monotonic` (same file, live PostgreSQL via `scripts/test-postgres.sh`) |
| T1 | `smtp::read_response` sliced a `&str` reply at fixed byte offsets (`[..3]`, `[4..]`); a non-ASCII byte at that boundary (e.g. an emoji greeting) panicked. Fixed by parsing bytes throughout and lossy-decoding only the trailing text. | `non_ascii_greeting_bytes_never_panic_and_are_treated_as_malformed` (`crates/mail/tests/smtp.rs`) |
| T2 | SMTP response reads were unbounded (`String` `read_line`, unbounded continuation-line accumulation, per-line-restarted timeout). Fixed with a byte-capped line reader, a cap on continuation lines and total bytes, and a single deadline covering the whole response. | `unbounded_continuation_lines_do_not_hang_or_grow_forever` |
| T3 | LMTP `DATA` (including the oversized-message discard/drain path) had no deadline at all; SMTP writes/flushes were unbounded. Fixed with a deadline threaded through the whole `DATA` read loop (421 + close on expiry) and timeout-wrapped SMTP writes/flushes. | `data_phase_stall_after_start_times_out_and_closes_instead_of_hanging`, `oversized_drain_also_respects_the_data_deadline` (`crates/mail/tests/lmtp.rs`); `a_relay_that_stops_reading_mid_data_does_not_hang_the_write_forever` (`crates/mail/tests/smtp.rs`) |
| T4 | SMTP multiline parsing trusted the first line's code and never checked continuation lines matched it; the DATA-completion check accepted any `2xx` as `Sent`. Fixed: continuation-line code mismatch is a hard parse error, and only an exact `250` completes DATA as `Sent`. | `mismatched_continuation_code_is_never_reported_as_sent`, `only_exact_250_completes_data_as_sent` |
| T5 | Hostname/envelope-sender/recipient values were spliced into SMTP command lines, and header names/values/subject-prefix into cooked message headers, without validating for CR/LF/`:` injection. Fixed with pre-I/O validation (`is_safe_smtp_text`, `is_safe_value`, `is_safe_header_name`); an unsafe recipient is isolated to a `PermanentFailure` for that entry only, an unsafe hostname/envelope-sender/header/prefix is a hard `Err` before any I/O. | `hostname_and_envelope_sender_crlf_injection_is_rejected_before_any_io`, `envelope_sender_crlf_injection_is_rejected_before_any_command_is_sent`, `recipient_crlf_injection_is_isolated_to_that_recipient` (`crates/mail/tests/smtp.rs`); `rejects_a_subject_prefix_carrying_a_header_injection`, `rejects_addition_header_names_or_values_carrying_crlf` (`crates/mail/tests/cook.rs`) |
| T6 | A later recipient's I/O failure blanket-overwrote every recipient's outcome (including an already-known `550` for a different recipient) via an outer catch-all; DATA-start `554` was misclassified transient. Fixed by threading `Vec<Option<RecipientStatus>>` through the transaction so only still-`None` (pending) slots are ever resolved by a later failure, plus proper 4xx/5xx classification for the DATA-start reply, plus a new `Ambiguous` status for a connection lost strictly after the full message was written (distinct from an ordinary pre-send `TransientFailure`). | `a_permanent_rcpt_failure_survives_a_later_recipients_connection_loss`, `data_start_554_is_a_permanent_failure_not_transient`, `connection_loss_after_full_data_write_is_ambiguous_not_sent_or_plain_transient` |
| T7 | LMTP accepted `EHLO` (RFC 2033 §4.1 forbids it; only `LHLO` is valid) and never advertised/emitted `ENHANCEDSTATUSCODES`. Fixed: `EHLO`/`HELO` are rejected without a 250, and every substantive reply (except the `LHLO` capability lines and `354`, which have no RFC 3463 convention) carries a class-matching enhanced status code. | `ehlo_is_rejected_only_lhlo_is_accepted`, `enhanced_status_codes_are_advertised_and_present_on_replies` |
| T8 | If a handler returned more outcomes than accepted recipients, the transport sent one reply per outcome instead of one per recipient, desynchronizing pipelining. Fixed by iterating exactly `0..recipients.len()` and ignoring extra hook results. | `excess_hook_results_never_produce_more_replies_than_recipients`, `duplicate_rcpt_still_gets_one_reply_per_command` |
| E1 | The existing dot-unstuffing/byte-preservation test only asserted the reply code, never the exact bytes the handler received. Fixed by making the test handler capture deliveries (`Arc<Mutex<..>>`, inspectable after the session ends) and asserting the exact unstuffed bytes. | `dot_unstuffing_and_binary_bytes_are_preserved_exactly` (rewritten) |


Subsequent pre-0004 evidence included seven explicit PostgreSQL contracts
(CRUD/schema/auth/queue/heartbeat/fenced-delivery/held review) and the real pinned
Phase 1 + held client flow. Those passed against task-owned fixtures. Older
permission-denied/unrun notes described earlier attempts and are not the latest
PG result: the **current migration-0004 attempt gate timed out**. No historical
PASS is promoted to a new-candidate PASS.

## Version and license baseline

Workspace packages are `0.1.0`, unreleased development, not a completed phase or
release tag. The license is `AGPL-3.0-or-later`; `LICENSE` contains the full AGPLv3
text. `Cargo.toml` and `Cargo.lock` are executable version/dependency sources of
truth (ADR-0003).


## Current browser self-service slice (2026-09-06)

This additive section supersedes historical "no UI" statements only for the
bounded behaviors below. Full Phase 4 and the Mailman-replacement target remain
open. The donor was independently resumed from `/tmp/listmngr-webui-seed.json`.
It is now integrated into canonical with existing archive APIs/fanout. Public
archive browsing/search is covered by WEB-ARCHIVE below; private archive browser
access remains open. Historical donor evidence follows.

| ID | Implemented / independently exercised | Boundary |
|---|---|---|
| WEB-PUBLIC | Advertised-only escaped directory with real pagination; join/leave request forms; non-consuming confirmation GET and one-time confirmation POST. | SQLite HTTP tests cover join/leave/replay. Chromium submits request+confirm; token comes from disposable DB notice, not SMTP. |
| WEB-SESSION | Verified linked-address password login, token rotation/hash storage, CSRF/exact Origin, cookie flags, expiry, pool-wait expiry, password-change rejection, logout revocation and login-audit rollback. | Fixed 8h auth / 30m anonymous lifetime; five node-local login attempts/minute. No signup/reset UI. Password-to-session issuance now revalidates exact credentials/address and predecessor under DML-conflicting locks; see fresh login evidence below. |
| WEB-PREFERENCES | Own verified member delivery mode and self-pause forms persist; other-member/unverified actions fail. Account lists are SQL-paginated at 20 members/page with ownership filtered before LIMIT. | No arbitrary user/address preferences; moderator/bounce restrictions cannot be lifted. |
| WEB-INDEX-PAGING | Moderator list index filters verified role/server-owner authority in SQL before LIMIT 21; renders 20 rows/page with stable ordering and bounded page index. | SQLite HTTP regressions include unauthorized rows before the page boundary, verified-address revocation, and server-owner visibility. PostgreSQL smoke exercises both index queries, not the complete pagination matrix. |
| WEB-REVIEW | Verified list-bounded moderation, escaped SQL-bounded 64 KiB source, 20 records/page, persisted decisions/comments, actual outgoing job and exact eligible-recipient effects. | Reject/discard do not send notices. Canonical acceptance retains archive scheduling; no complete administrator console. |
| WEB-ARCHIVE | Public archive browser, escaped cooked plain text, substring search, thread links, 20-row pagination, current private/never policy rejection and public-list discovery link. `cargo test --locked -p listmngr-api --test webui public_archive` — 2 passed. Both route and discovery tests observed behavioral RED before implementation. Actual Chromium also exercised discovery/search/thread/mobile with no script execution or overflow. | Browser archive is public-only; private-session access, attachments, ranked full text and richer threading remain open. HTTP fixture uses SQLite; no new PostgreSQL UI claim. |
| WEB-BROWSER | Real Chromium, live ephemeral loopback server, responsive CSS, native login/preference/request/confirm/accept/logout forms, desktop/mobile screenshots, no console/page errors, DB effects checked after browser exits. | `chromium_browser_acceptance` is opt-in; not implied by default Cargo tests. Native browser exposed and regression-fixed `no-referrer` → `Origin: null` 403, which synthetic HTTP missed. |
| WEB-POSTGRES | Fresh isolated PostgreSQL 14.24: migrations, hashed sessions/login, directory, bounded binary preview, preference, discard, logout/revocation. | `postgres_browser_session_forms_and_bounded_preview` executed explicitly; not a whole workspace PostgreSQL gate or PostgreSQL browser render. |

### Fresh login issuance repair evidence

Source: `crates/db/src/web_sessions.rs`, `crates/db/src/web_login_tests.rs`,
`crates/db/src/web_sessions_tests.rs`, `crates/api/src/webui.rs`.
The deterministic password-reset interleaving first returned a session (expected
Authentication rejection: RED, with valid counterpart PASS). Exact verified hash
and version/address ownership are now checked after writer reservation, before
session insert, predecessor deletion and audit. Argon2 is outside writer locks.

Executed from `/tmp/listmngr-webui`:

```sh
CARGO_TARGET_DIR=/tmp/listmngr-webui/target cargo test -p listmngr-db --lib browser_login_issuance -- --nocapture
CARGO_TARGET_DIR=/tmp/listmngr-webui/target cargo clippy -p listmngr-db -p listmngr-api --all-targets -- -D warnings
WEBUI_LOGIN_POSTGRES_URL=postgres://webui_test@127.0.0.1:56439/webui_login_issuance_20260906 CARGO_TARGET_DIR=/tmp/listmngr-webui/target cargo test -p listmngr-db --lib browser_login_issuance_postgres_lock_matrix -- --ignored --nocapture
CARGO_TARGET_DIR=/tmp/listmngr-webui/target cargo test -p listmngr-api -p listmngr-db
```

Results: focused 3 passed / 1 opt-in ignored; explicit PostgreSQL 1 passed (10
cases); strict Clippy PASS; API/DB suites PASS, including existing browser 14
passed / 4 opt-in ignored and SQLite revocation matrix. PostgreSQL data directory
was verified as `/tmp/listmngr-webui-revocation-pg`; only the fresh named login DB
was created/dropped, with DROP confirmed. Both backend matrices cover valid
rotation, independent hash/version changes, unverify, unlink, address reassignment,
predecessor expiry/deletion/CSRF change, and login-audit rollback. Busy signals are
observations of real DML lock conflict, not timing sleeps. The initial reset test
uses the real password mutator; later matrices are supplemental GREEN coverage.
No negative control remains. No Chromium/email-only live run or full PostgreSQL
workspace acceptance is claimed here. The former held-recipient pre-transaction snapshot defect is repaired in the
canonical convergence below. GET authorization concurrency and lock scalability
remain open production boundaries.

Earlier independent commands and artifacts (outside source tree):

- Browser-write revocation follow-up: parent fixed a final test-helper Clippy
  needless borrow, then ran `cargo fmt --all --check`, API/DB all-targets tests
  (`/tmp/listmngr-webui-revocation-parent-tests.log`), and API/DB all-targets,
  all-features Clippy with `-D warnings`: PASS. Opt-in Chromium also passed;
  evidence directory `/tmp/listmngr-webui-revocation-parent-browser`.
- `postgres_browser_authority_matrix_at_lock_barrier --ignored --exact` passed
  against a newly created database in the verified worker-owned PostgreSQL
  cluster; `/tmp/listmngr-webui-revocation-parent-pg.log`. Parent database removed.
  This covers transaction-bound preference/moderation authority, not login
  races, GET authorization, concurrent recipient-preference snapshots, lock
  throughput, the complete PostgreSQL gate or integrated main acceptance.

- Parent pagination follow-up: both new HTTP regressions first failed with
  21 rows instead of 20. After SQL pagination, `cargo test --locked -p listmngr-api
  --all-targets` passed (`/tmp/listmngr-ui-pagination-api-tests.log`), as did
  `cargo clippy --locked -p listmngr-api --all-targets --all-features -- -D warnings`
  (`/tmp/listmngr-ui-pagination-clippy.log`) and `cargo fmt --all --check`.
- Fresh opt-in Chromium flow passed after the pagination implementation;
  screenshots: `/tmp/listmngr-webui-browser-pagination-evidence/`.
- `python3 /tmp/listmngr-ui-pagination-postgres.py` ran the explicit PostgreSQL
  form/index smoke in a newly initialized disposable cluster and stopped it:
  `/tmp/listmngr-ui-page-pg-z_7r7vj3/acceptance.log` (PASS).

- `cargo test --locked -p listmngr-api --test webui --test phase0_security`:
  `/tmp/listmngr-webui-resume-focused.log` (PASS; opt-in browser/PG tests separate).
- `cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`:
  `/tmp/listmngr-webui-resume-clippy.log` (PASS).
- Chromium reproduction is in README and `scripts/test-webui-browser.py`;
  `/tmp/listmngr-webui-browser-acceptance.log` and
  `/tmp/listmngr-webui-browser-evidence/01-directory.png` through
  `07-mobile-directory.png` are actual captured evidence.
- PostgreSQL probe: `/tmp/listmngr-webui-postgres-acceptance.log` (PASS).
- Historical resume REDs remain in `/tmp/listmngr-webui-resume-red.log` and
  `/tmp/listmngr-webui-browser-origin-red.log`; first workspace attempt exposed
  missing additive workflow/session schema corpora. Frozen prior snapshots were
  preserved and the additions were recorded separately in both consumers.

Remaining browser production gates: transaction-bound ownership/role/session
revocation under concurrent changes, cross-process login-abuse controls, complete
resource-specific authorization scaling, signup/password recovery, full administration,
accessibility audit, TLS reverse-proxy deployment and complete PostgreSQL/browser
matrix. No release-readiness or full-replacement claim follows from these probes.


## Verified-session recovery behavior and evidence provenance

Logged-in web confirmation now offers `GET /web/members/{id}/recover` and an
existing-CSRF/configured-Origin protected POST at the same URL. Only a live
session's verified, owned ordinary membership with **direct** `by_bounces`
preferences qualifies (as-user memberships must also match the session user).
The GET never mutates; the POST resets delivery status, bounce score and warning
cycle atomically with session-user-attributed `bounce.recover`. Other preferences
and historical bounce events are untouched. General preference editing still
rejects restricted reasons. The existing browser transaction barrier coordinates
with scorer/maintenance DML, with live-session revalidation after writes.

The user must verify their mailbox is working before restoring delivery. This
slice sends no challenge, probe or recovery email and provides no token-based
recovery. Earlier no-web-recovery evidence describes the previous implementation;
this narrowly authorized action does not establish full P4 or Mailman replacement.
The acceptance row above and `target/web-bounce-recovery-final-receipt.json`
record the frozen parent results. Initial DB/router RED/GREEN chronology is in
`target/web-bounce-recovery-handoff.md`; later security controls are supplemental,
not retrospective strict per-guard TDD. Coverage limits remain explicit above.

## P3-DSN-ISSUANCE — bounded outbound vertical

Durable ordinary-delivery correlation and actual RFC3461 ENVID carrier are
implemented, default off. Local producer→ledger→socket→pool-reopen tracer and
owned PostgreSQL ledger/relay/audit/retry/schema checks passed; final affected
gate receipts and source hashes are in `target/dsn-issuance/HANDOFF.json`.
This is not incoming DSN authentication/scoring or full Mailman parity.
See [DSN_ISSUANCE.md](DSN_ISSUANCE.md) for configuration, generation-path inventory,
RED provenance and remaining parent acceptance.
