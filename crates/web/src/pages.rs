//! One view model per browser page. Handlers fill these in; the templates own
//! every byte of markup and escape every value.
use crate::{Choice, Pagination, Shell};
use askama::Template;

/// Public directory of advertised lists.
#[derive(Debug, Template)]
#[template(path = "directory.html")]
pub struct Directory {
    /// Document shell.
    pub shell: Shell,
    /// The lists on this page.
    pub entries: Vec<DirectoryEntry>,
    /// Directory paging, carrying the filters.
    pub pagination: Pagination,
    /// The search text as submitted.
    pub query: String,
    /// The domain filter: any domain first, then each mail host.
    pub domains: Vec<Choice>,
    /// Whether the reader asked for their own unadvertised lists too.
    pub show_all: bool,
    /// Whether the reader is signed in, so the scope control is offered.
    pub signed_in: bool,
    /// The create-list form, when this reader may create one.
    pub create_href: Option<String>,
}

/// One list in the directory.
#[derive(Debug)]
pub struct DirectoryEntry {
    /// Link to the list page.
    pub href: String,
    /// Display name.
    pub name: String,
    /// Fully qualified list id.
    pub id: String,
    /// Short description.
    pub description: String,
    /// The reader's translated role on the list, if any.
    pub badge: Option<String>,
    /// Whether the list is kept out of the public directory.
    pub unadvertised: bool,
}

/// The create-list form: its fields with their inline refusals.
#[derive(Debug, Template)]
#[template(path = "create_list.html")]
pub struct CreateList {
    /// Document shell.
    pub shell: Shell,
    /// Form action.
    pub action: String,
    /// Session CSRF token.
    pub csrf: String,
    /// The fields in order, each with its current value and refusal.
    pub fields: Vec<SettingField>,
    /// A refusal that belongs to no one field.
    pub error: Option<String>,
}

/// Password login form.
#[derive(Debug, Template)]
#[template(path = "login.html")]
pub struct Login {
    /// Document shell.
    pub shell: Shell,
    /// Session CSRF token.
    pub csrf: String,
    /// Whether self-service signup is offered.
    pub signup: bool,
    /// Identity providers, in configuration order.
    pub providers: Vec<ProviderLink>,
}

/// One identity provider the login page offers.
#[derive(Debug)]
pub struct ProviderLink {
    /// Where the sign-in starts.
    pub href: String,
    /// The provider's name as configured.
    pub display_name: String,
}

/// The signed-in member's own subscriptions.
#[derive(Debug, Template)]
#[template(path = "account.html")]
pub struct Account {
    /// Document shell.
    pub shell: Shell,
    /// Session CSRF token.
    pub csrf: String,
    /// The site requires a second factor this reader has not enrolled.
    pub second_factor_missing: bool,
    /// "Signed in as …" line.
    pub signed_in: String,
    /// Subscriptions on this page.
    pub subscriptions: Vec<Subscription>,
    /// Subscription paging.
    pub pagination: Pagination,
}

/// One of the reader's own subscriptions.
#[derive(Debug)]
pub struct Subscription {
    /// List the subscription belongs to.
    pub list_id: String,
    /// Resolved delivery mode and status sentence.
    pub delivery: String,
    /// Archive link when the list archives at all.
    pub archive_href: Option<String>,
    /// Self-service recovery link for a disabled subscription.
    pub recover_href: Option<String>,
    /// Whether delivery is disabled without a self-service path.
    pub restricted: bool,
    /// Editable preferences while delivery is not disabled.
    pub preferences: Option<Preferences>,
    /// Confirmation page for leaving this list.
    pub leave_href: String,
}

/// The member preference controls of one subscription.
#[derive(Debug)]
pub struct Preferences {
    /// Form target.
    pub action: String,
    /// Delivery mode options.
    pub modes: Vec<Choice>,
    /// Delivery status options.
    pub statuses: Vec<Choice>,
    /// Own-post options.
    pub own_postings: Vec<Choice>,
    /// Direct-copy options.
    pub list_copy: Vec<Choice>,
}

/// Lists the reader owns or administers.
#[derive(Debug, Template)]
#[template(path = "admin.html")]
pub struct AdminIndex {
    /// Document shell.
    pub shell: Shell,
    /// The create-list form, when this reader may create one.
    pub create_href: Option<String>,
    /// Administered lists on this page.
    pub rows: Vec<AdminRow>,
    /// Listing paging.
    pub pagination: Pagination,
}

/// One administered list.
#[derive(Debug)]
pub struct AdminRow {
    /// Fully qualified list id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Member roster link.
    pub members_href: String,
    /// Settings form link.
    pub settings_href: String,
}

/// The owner's roster of one role: search, the rows, bulk removal and the
/// links to the other rosters and tools.
#[derive(Debug, Template)]
#[template(path = "members.html")]
pub struct Members {
    /// Document shell, loading htmx.
    pub shell: Shell,
    /// Scope and effect sentence.
    pub intro: String,
    /// The roster fragment.
    pub roster: Roster,
    /// The role tabs.
    pub roles: Vec<GroupLink>,
    /// The role being shown, as its wire name.
    pub role: String,
    /// Search form target.
    pub search_action: String,
    /// Current search term.
    pub query: String,
    /// Link that clears the search.
    pub clear_href: String,
    /// Session CSRF token.
    pub csrf: String,
    /// Where mass subscription starts.
    pub subscribe_href: String,
    /// Where the CSV export is.
    pub export_href: String,
    /// Where bulk removal posts.
    pub remove_action: String,
    /// A notice after a redirect.
    pub notice: Option<String>,
    /// Link back to the administration index.
    pub admin_href: String,
}

/// The rows of a roster page and their paging: the part htmx swaps.
#[derive(Debug, Template)]
#[template(path = "members_roster.html")]
pub struct Roster {
    /// Document shell, for the strings.
    pub shell: Shell,
    /// Session CSRF token.
    pub csrf: String,
    /// Search term carried through a policy write.
    pub carried_query: String,
    /// Page number carried through a policy write.
    pub carried_page: u32,
    /// Role carried through a policy write.
    pub carried_role: String,
    /// Members on this page.
    pub rows: Vec<MemberRow>,
    /// Roster paging.
    pub pagination: Pagination,
}

/// One member of a roster.
#[derive(Debug)]
pub struct MemberRow {
    /// Member id, for the bulk selection.
    pub id: String,
    /// Member's address.
    pub email: String,
    /// Display name, possibly empty.
    pub display_name: String,
    /// The options page.
    pub href: String,
    /// Policy form target.
    pub action: String,
    /// Control id, unique per row.
    pub control: String,
    /// Posting policy options.
    pub choices: Vec<Choice>,
    /// Member-level delivery summary, empty when inherited.
    pub delivery: String,
    /// Bounce score, shown when above zero.
    pub bounce: Option<String>,
}

/// Owner-facing list settings.
#[derive(Debug, Template)]
#[template(path = "settings.html")]
pub struct Settings {
    /// Document shell.
    pub shell: Shell,
    /// Scope and effect sentence.
    pub intro: String,
    /// Form target.
    pub action: String,
    /// Session CSRF token.
    pub csrf: String,
    /// Current display name.
    pub display_name: String,
    /// Current description.
    pub description: String,
    /// Current subject prefix.
    pub subject_prefix: String,
    /// Select controls in display order.
    pub selects: Vec<SelectField>,
    /// Numeric limits in display order.
    pub numbers: Vec<NumberField>,
    /// Link back to the administration index.
    pub admin_href: String,
    /// Every settings group, for the navigation.
    pub groups: Vec<GroupLink>,
}

/// A labelled select control.
#[derive(Debug)]
pub struct SelectField {
    /// Form field name, also the control id.
    pub name: String,
    /// Translated label.
    pub label: String,
    /// Options.
    pub choices: Vec<Choice>,
}

/// A labelled bounded number control with help text.
#[derive(Debug)]
pub struct NumberField {
    /// Form field name, also the control id.
    pub name: String,
    /// Translated label.
    pub label: String,
    /// Current value.
    pub value: u32,
    /// Translated help text.
    pub help: String,
}

/// Authenticated password change.
#[derive(Debug, Template)]
#[template(path = "password.html")]
pub struct Password {
    /// Document shell.
    pub shell: Shell,
    /// Session CSRF token.
    pub csrf: String,
}

/// Confirmation that every session was signed out.
#[derive(Debug, Template)]
#[template(path = "password_changed.html")]
pub struct PasswordChanged {
    /// Document shell.
    pub shell: Shell,
}

/// Confirmation before leaving a list.
#[derive(Debug, Template)]
#[template(path = "leave.html")]
pub struct Leave {
    /// Document shell.
    pub shell: Shell,
    /// Session CSRF token.
    pub csrf: String,
    /// Form target.
    pub action: String,
    /// What exactly is being removed.
    pub prompt: String,
}

/// Confirmation before restoring one's own disabled delivery.
#[derive(Debug, Template)]
#[template(path = "recover.html")]
pub struct Recover {
    /// Document shell.
    pub shell: Shell,
    /// Session CSRF token.
    pub csrf: String,
    /// Form target.
    pub action: String,
    /// What exactly is being restored.
    pub prompt: String,
}

/// Public list page with the join/leave request form.
#[derive(Debug, Template)]
#[template(path = "list.html")]
pub struct ListPage {
    /// Document shell.
    pub shell: Shell,
    /// Short description.
    pub description: String,
    /// Long information text.
    pub info: String,
    /// The list's addresses and policies.
    pub facts: Vec<Fact>,
    /// The reader's translated role on the list, if any.
    pub badge: Option<String>,
    /// Whether the list is kept out of the public directory.
    pub unadvertised: bool,
    /// The settings link, for a reader who administers the list.
    pub settings_href: Option<String>,
    /// Public archive link, when the archive is public.
    pub archive_href: Option<String>,
    /// Request form target.
    pub action: String,
    /// Session CSRF token.
    pub csrf: String,
    /// Link to the token confirmation form.
    pub confirm_href: String,
}

/// Acknowledgement that confirmation instructions may be on their way.
#[derive(Debug, Template)]
#[template(path = "check_email.html")]
pub struct CheckEmail {
    /// Document shell.
    pub shell: Shell,
}

/// Token entry for a join or leave request.
#[derive(Debug, Template)]
#[template(path = "confirm.html")]
pub struct ConfirmForm {
    /// Document shell.
    pub shell: Shell,
    /// What confirming will do.
    pub intro: String,
    /// Form target.
    pub action: String,
    /// Session CSRF token.
    pub csrf: String,
    /// Token prefilled from the link, if any.
    pub token: String,
}

/// A completed subscription request.
#[derive(Debug, Template)]
#[template(path = "confirmed.html")]
pub struct Confirmed {
    /// Document shell.
    pub shell: Shell,
}

/// Lists the reader may moderate.
#[derive(Debug, Template)]
#[template(path = "moderation.html")]
pub struct Moderation {
    /// Document shell.
    pub shell: Shell,
    /// Moderated lists on this page.
    pub rows: Vec<ModerationRow>,
    /// Listing paging.
    pub pagination: Pagination,
}

/// One moderated list.
#[derive(Debug)]
pub struct ModerationRow {
    /// Held-message queue link.
    pub href: String,
    /// Translated link text.
    pub label: String,
    /// Subscription requests link.
    pub requests_href: String,
    /// Translated counts of what waits.
    pub waiting: String,
}

/// The held-message queue of one list.
#[derive(Debug, Template)]
#[template(path = "held.html")]
pub struct Held {
    /// Document shell, loading the shortcuts script.
    pub shell: Shell,
    /// Session CSRF token.
    pub csrf: String,
    /// Where bulk decisions post.
    pub bulk_action: String,
    /// Where the requests queue is.
    pub requests_href: String,
    /// Held messages on this page.
    pub items: Vec<HeldItem>,
    /// Available decisions.
    pub decisions: Vec<Choice>,
    /// The sender moderation choices, unselected.
    pub sender_actions: Vec<Choice>,
    /// A notice after a redirect.
    pub notice: Option<String>,
    /// A refusal that names no field.
    pub error: Option<String>,
    /// Queue paging.
    pub pagination: Pagination,
}

/// One held message awaiting a decision.
#[derive(Debug)]
pub struct HeldItem {
    /// Held id, for the bulk selection and the shortcuts.
    pub id: String,
    /// Subject as held.
    pub subject: String,
    /// Envelope sender.
    pub sender: String,
    /// Why the message was held.
    pub reason: String,
    /// When it was held, formatted.
    pub held_at: String,
    /// Decoded `From`, `To`, `Date`.
    pub from: String,
    pub to: String,
    pub date: String,
    /// The text body, decoded; empty when there is none.
    pub body: String,
    /// How many attachments the MIME structure names.
    pub attachments: usize,
    /// Bounded raw source.
    pub source: String,
    /// Decision form target.
    pub action: String,
    /// The submitted forward address to keep after a refusal.
    pub forward_to: String,
    /// Forward refusal, if any.
    pub forward_error: Option<String>,
    /// Where "moderate sender" posts.
    pub sender_action: String,
    /// The sender's current standing, translated.
    pub sender_standing: String,
    /// Sender moderation choices with the current one selected.
    pub sender_choices: Vec<Choice>,
    /// Where "ban sender" posts; `None` when already banned.
    pub ban_action: Option<String>,
    /// The header-rule shortcut.
    pub rule_href: String,
}

/// The subscription requests queue of one list.
#[derive(Debug, Template)]
#[template(path = "requests.html")]
pub struct Requests {
    /// Document shell.
    pub shell: Shell,
    /// Session CSRF token.
    pub csrf: String,
    /// Where the held queue is.
    pub held_href: String,
    /// The requests.
    pub items: Vec<RequestItem>,
    /// Decision choices.
    pub decisions: Vec<Choice>,
    /// A notice after a redirect.
    pub notice: Option<String>,
}

/// One undecided subscription request.
#[derive(Debug)]
pub struct RequestItem {
    /// The address as written.
    pub email: String,
    /// Display name, possibly empty.
    pub display_name: String,
    /// Translated: join or leave.
    pub action: String,
    /// Translated: who it waits for.
    pub waiting: String,
    /// When it was made, formatted.
    pub requested_at: String,
    /// Decision form target.
    pub decide_action: String,
}

/// Archive reading and search for one list.
#[derive(Debug, Template)]
#[template(path = "archive.html")]
pub struct Archive {
    /// Document shell.
    pub shell: Shell,
    /// Current search term.
    pub query: String,
    /// Current thread filter.
    pub thread: String,
    /// Link that clears search and thread.
    pub all_href: String,
    /// mbox export of this selection.
    pub download_href: String,
    /// Messages on this page.
    pub messages: Vec<ArchiveMessage>,
    /// Previous page link.
    pub previous: Option<String>,
    /// Next page link.
    pub next: Option<String>,
}

/// One archived message.
#[derive(Debug)]
pub struct ArchiveMessage {
    /// Subject.
    pub subject: String,
    /// Thread view link.
    pub thread_href: String,
    /// Stable permalink.
    pub permalink: String,
    /// Rendered body text.
    pub body: String,
    /// Downloadable attachments.
    pub attachments: Vec<ArchiveAttachment>,
    /// Whether the attachment list could not be produced.
    pub attachments_unavailable: bool,
}

/// One attachment download.
#[derive(Debug)]
pub struct ArchiveAttachment {
    /// Download link.
    pub href: String,
    /// Translated link text including the file name.
    pub label: String,
}

/// The one error page every failed browser request renders.
#[derive(Debug, Template)]
#[template(path = "error.html")]
pub struct ErrorPage {
    /// Document shell.
    pub shell: Shell,
}

/// RFC 8058 one-click confirmation page, reached from a mail client.
#[derive(Debug, Template)]
#[template(path = "unsubscribe.html")]
pub struct Unsubscribe {
    /// Document shell.
    pub shell: Shell,
    /// Which subscription the click would end.
    pub prompt: String,
    /// One-click form target, carrying the token.
    pub action: String,
}

/// Confirmation that the one-click unsubscribe completed.
#[derive(Debug, Template)]
#[template(path = "unsubscribed.html")]
pub struct Unsubscribed {
    /// Document shell.
    pub shell: Shell,
    /// What was removed.
    pub prompt: String,
}

/// The HyperKitty-compatible archive path, kept minimal until Phase 5 renders
/// the full archive UI.
#[derive(Debug, Template)]
#[template(path = "archive_compat.html")]
pub struct ArchiveCompat {
    /// Fully qualified list id.
    pub list: String,
    /// Messages, newest first.
    pub entries: Vec<ArchiveCompatEntry>,
}

/// One message on the compatibility archive path.
#[derive(Debug)]
pub struct ArchiveCompatEntry {
    /// Thread link.
    pub href: String,
    /// Subject.
    pub subject: String,
    /// Rendered body text.
    pub body: String,
}

/// This server's own API reference, rendered from its `OpenAPI` document so
/// the page needs no script and no third-party asset.
#[derive(Debug, Template)]
#[template(path = "api_docs.html")]
pub struct ApiDocs {
    /// API title from the document.
    pub title: String,
    /// API version from the document.
    pub version: String,
    /// What this page is and what it is not.
    pub intro: String,
    /// How a caller authenticates, in one sentence.
    pub authentication: String,
    /// Documented paths, in document order.
    pub paths: Vec<DocPath>,
    /// Closing note.
    pub footer: String,
}

/// One documented path and its operations.
#[derive(Debug)]
pub struct DocPath {
    /// Path template, such as `/api/v1/lists/{id}`.
    pub path: String,
    /// Operations on this path.
    pub operations: Vec<DocOperation>,
}

/// One documented operation.
#[derive(Debug)]
pub struct DocOperation {
    /// Upper-case HTTP method.
    pub method: String,
    /// Summary, empty when the document carries none.
    pub summary: String,
    /// Required scopes, already joined for reading.
    pub scopes: String,
    /// Parameters, already joined for reading.
    pub parameters: String,
    /// Response status codes, already joined for reading.
    pub responses: String,
}

/// The reader's own browser sessions.
#[derive(Debug, Template)]
#[template(path = "sessions.html")]
pub struct Sessions {
    /// Document shell.
    pub shell: Shell,
    /// Session CSRF token.
    pub csrf: String,
    /// Live sessions, newest first.
    pub sessions: Vec<SessionRow>,
    /// Whether any session other than this browser is listed.
    pub others: bool,
}

/// One listed browser session.
#[derive(Debug)]
pub struct SessionRow {
    /// Revocation form target.
    pub action: String,
    /// When it was issued, already formatted.
    pub created: String,
    /// When it expires, already formatted.
    pub expires: String,
    /// Whether this is the browser making the request.
    pub current: bool,
}

/// The reader's own profile form.
#[derive(Debug, Template)]
#[template(path = "profile.html")]
pub struct ProfilePage {
    /// Document shell.
    pub shell: Shell,
    /// Session CSRF token.
    pub csrf: String,
    /// Current display name.
    pub display_name: String,
    /// Interface languages, the current one selected.
    pub locales: Vec<Choice>,
    /// IANA time zones, the current one selected.
    pub timezones: Vec<Choice>,
}

/// Self-service account creation.
#[derive(Debug, Template)]
#[template(path = "signup.html")]
pub struct SignupPage {
    /// Document shell.
    pub shell: Shell,
    /// Session CSRF token.
    pub csrf: String,
}

/// Token entry for address verification.
#[derive(Debug, Template)]
#[template(path = "verify.html")]
pub struct VerifyForm {
    /// Document shell.
    pub shell: Shell,
    /// Session CSRF token.
    pub csrf: String,
    /// Token prefilled from the link, if any.
    pub token: String,
}

/// A verified address.
#[derive(Debug, Template)]
#[template(path = "verified.html")]
pub struct Verified {
    /// Document shell.
    pub shell: Shell,
}

/// Password reset request.
#[derive(Debug, Template)]
#[template(path = "reset.html")]
pub struct ResetRequest {
    /// Document shell.
    pub shell: Shell,
    /// Session CSRF token.
    pub csrf: String,
}

/// Token entry and new password.
#[derive(Debug, Template)]
#[template(path = "reset_confirm.html")]
pub struct ResetConfirm {
    /// Document shell.
    pub shell: Shell,
    /// Session CSRF token.
    pub csrf: String,
    /// Token prefilled from the link, if any.
    pub token: String,
}

/// A completed reset.
#[derive(Debug, Template)]
#[template(path = "reset_done.html")]
pub struct ResetDone {
    /// Document shell.
    pub shell: Shell,
}

/// The reader's own addresses.
#[derive(Debug, Template)]
#[template(path = "addresses.html")]
pub struct Addresses {
    /// Document shell.
    pub shell: Shell,
    /// Session CSRF token.
    pub csrf: String,
    /// Addresses, primary first.
    pub addresses: Vec<AddressRow>,
}

/// One listed address.
#[derive(Debug)]
pub struct AddressRow {
    /// Address id for the action forms.
    pub id: String,
    /// Normalized address.
    pub email: String,
    /// Whether it has been proven from its mailbox.
    pub verified: bool,
    /// Whether it is the account's primary address.
    pub primary: bool,
}

/// The reader's own API tokens and the form to mint one.
#[derive(Debug, Template)]
#[template(path = "tokens.html")]
pub struct Tokens {
    /// Document shell.
    pub shell: Shell,
    /// Session CSRF token.
    pub csrf: String,
    /// Tokens, newest first; never a secret.
    pub tokens: Vec<TokenRow>,
    /// Scopes the reader may choose.
    pub scopes: Vec<String>,
    /// Lists the reader may bind to, as `(id, name)`.
    pub lists: Vec<(String, String)>,
    /// Whether an unbound token may be minted.
    pub server_owner: bool,
}

/// One listed token.
#[derive(Debug)]
pub struct TokenRow {
    /// Token id for the revoke form.
    pub id: String,
    /// Name.
    pub name: String,
    /// Scopes, space separated.
    pub scopes: String,
    /// What it is bound to, already worded.
    pub bound: String,
    /// Creation time, already formatted.
    pub created: String,
    /// Expiry, already worded.
    pub expires: String,
    /// Last use, already worded.
    pub last_used: String,
    /// Whether it has been revoked.
    pub revoked: bool,
}

/// The one page that shows a freshly minted secret.
#[derive(Debug, Template)]
#[template(path = "token_issued.html")]
pub struct TokenIssued {
    /// Document shell.
    pub shell: Shell,
    /// The secret, shown once.
    pub token: String,
}

/// Confirmation before deleting the account.
#[derive(Debug, Template)]
#[template(path = "delete_account.html")]
pub struct DeleteAccount {
    /// Document shell.
    pub shell: Shell,
    /// Session CSRF token.
    pub csrf: String,
}

/// The account is gone.
#[derive(Debug, Template)]
#[template(path = "account_deleted.html")]
pub struct AccountDeleted {
    /// Document shell.
    pub shell: Shell,
}

/// Second-factor status and enrolment.
#[derive(Debug, Template)]
#[template(path = "totp.html")]
pub struct TotpPage {
    /// Document shell.
    pub shell: Shell,
    /// Session CSRF token.
    pub csrf: String,
    /// A confirmed second factor exists.
    pub enabled: bool,
    /// The site requires this reader to enrol.
    pub required: bool,
    /// Pending secret, base32.
    pub secret: String,
    /// Pending provisioning URI.
    pub uri: String,
    /// Pending provisioning QR code as inline SVG, generated server-side.
    pub qr: String,
}

/// Recovery codes, shown once.
#[derive(Debug, Template)]
#[template(path = "totp_codes.html")]
pub struct TotpCodes {
    /// Document shell.
    pub shell: Shell,
    /// The codes.
    pub codes: Vec<String>,
}

/// The second step of a login.
#[derive(Debug, Template)]
#[template(path = "login_totp.html")]
pub struct LoginTotp {
    /// Document shell.
    pub shell: Shell,
    /// Session CSRF token.
    pub csrf: String,
}

/// The reader's passkeys and the form that registers one.
#[derive(Debug, Template)]
#[template(path = "passkeys.html")]
pub struct Passkeys {
    /// Document shell, with the script.
    pub shell: Shell,
    /// Session CSRF token.
    pub csrf: String,
    /// Registered passkeys, oldest first.
    pub passkeys: Vec<PasskeyRow>,
}

/// One listed passkey.
#[derive(Debug)]
pub struct PasskeyRow {
    /// Row id for the removal form.
    pub id: String,
    /// Name.
    pub name: String,
    /// Registration time, already formatted.
    pub created: String,
    /// Last use, already worded.
    pub last_used: String,
}

/// The reader's identity-provider links.
#[derive(Debug, Template)]
#[template(path = "oidc.html")]
pub struct Oidc {
    /// Document shell.
    pub shell: Shell,
    /// Session CSRF token.
    pub csrf: String,
    /// Every configured provider, linked or not.
    pub providers: Vec<ProviderRow>,
    /// Whether unlinking asks for the password (the account has a usable one).
    pub needs_password: bool,
    /// Whether the one link is the only way into the account.
    pub last_way_in: bool,
}

/// One configured provider on the account page.
#[derive(Debug)]
pub struct ProviderRow {
    /// Configured name, part of the form paths.
    pub name: String,
    /// Display name.
    pub display_name: String,
    /// The email the provider asserted when linked.
    pub linked_email: Option<String>,
    /// Last sign-in through it, already formatted.
    pub last_used: Option<String>,
}

/// A link in the settings group navigation.
#[derive(Debug)]
pub struct GroupLink {
    /// Page path.
    pub href: String,
    /// Translated title.
    pub label: String,
    /// Whether this is the page being shown.
    pub current: bool,
}

/// One control of a settings group, rendered by `kind`.
#[derive(Debug)]
pub struct SettingField {
    /// Form field name, also the control id.
    pub name: String,
    /// Translated label.
    pub label: String,
    /// Translated help, empty when there is none.
    pub help: String,
    /// `text`, `textarea`, `lines`, `select`, `number` or `decimal`.
    pub kind: String,
    /// Current or submitted value, as text.
    pub value: String,
    /// Options for a `select`.
    pub choices: Vec<Choice>,
    /// Inline refusal, if the submitted value was not accepted.
    pub error: Option<String>,
}

/// One row of a preview: what a save would change.
#[derive(Debug)]
pub struct DiffRow {
    /// Translated label.
    pub label: String,
    /// Wire name.
    pub name: String,
    /// Value now.
    pub before: String,
    /// Value after the save.
    pub after: String,
}

/// A read-only fact about the list.
#[derive(Debug)]
pub struct Fact {
    /// Translated label.
    pub label: String,
    /// Value.
    pub value: String,
}

/// One settings group: its form, the read-only facts, an optional preview.
#[derive(Debug, Template)]
#[template(path = "settings_group.html")]
pub struct SettingsGroup {
    /// Document shell.
    pub shell: Shell,
    /// Translated group title.
    pub title: String,
    /// Translated introduction.
    pub intro: String,
    /// Every group, with the current one marked.
    pub groups: Vec<GroupLink>,
    /// Form action.
    pub action: String,
    /// Session CSRF token.
    pub csrf: String,
    /// Read-only facts shown before the form.
    pub facts: Vec<Fact>,
    /// The controls.
    pub fields: Vec<SettingField>,
    /// A notice, such as "Saved".
    pub notice: Option<String>,
    /// A refusal that names no single field.
    pub error: Option<String>,
    /// The preview rows; `Some(empty)` means nothing would change.
    pub preview: Option<Vec<DiffRow>>,
    /// Extra actions rendered after the form (digest, archivers).
    pub extras: Vec<ActionForm>,
    /// Where the list's administration index is.
    pub admin_href: String,
}

/// A one-button action or a small form rendered after a group.
#[derive(Debug)]
pub struct ActionForm {
    /// Form action.
    pub action: String,
    /// Translated button text.
    pub button: String,
    /// Translated explanation.
    pub help: String,
    /// Extra controls: `(name, label, choices)`; a select each.
    pub selects: Vec<SelectField>,
}

/// One header rule row on the page.
#[derive(Debug)]
pub struct HeaderRuleRow {
    /// Position, from zero.
    pub position: usize,
    /// Header name.
    pub header: String,
    /// Pattern.
    pub pattern: String,
    /// Action choices with the current one selected.
    pub actions: Vec<Choice>,
    /// Tag.
    pub tag: String,
    /// Whether the tested value matched this rule.
    pub matched: Option<bool>,
}

/// The add form of the header rules page.
#[derive(Debug, Default)]
pub struct HeaderRuleDraft {
    /// Header name.
    pub header: String,
    /// Pattern.
    pub pattern: String,
    /// Tag.
    pub tag: String,
}

/// The header rules page.
#[derive(Debug, Template)]
#[template(path = "header_rules.html")]
pub struct HeaderRules {
    /// Document shell.
    pub shell: Shell,
    /// Group navigation.
    pub groups: Vec<GroupLink>,
    /// Base path of the page.
    pub base: String,
    /// Session CSRF token.
    pub csrf: String,
    /// Rules in order.
    pub rules: Vec<HeaderRuleRow>,
    /// The add form's values (kept after a refusal).
    pub draft: HeaderRuleDraft,
    /// Draft pattern refusal.
    pub pattern_error: Option<String>,
    /// Draft header refusal.
    pub header_error: Option<String>,
    /// Action choices for the add form.
    pub actions: Vec<Choice>,
    /// The tested header and value, and the translated verdict.
    pub test: Option<(String, String, String)>,
    /// A notice after a redirect.
    pub notice: Option<String>,
    /// A refusal that names no field.
    pub error: Option<String>,
}

/// Bans of one list or of the site.
#[derive(Debug, Template)]
#[template(path = "bans.html")]
pub struct Bans {
    /// Document shell.
    pub shell: Shell,
    /// Group navigation (empty on the site page).
    pub groups: Vec<GroupLink>,
    /// Translated introduction.
    pub intro: String,
    /// Base path of the page.
    pub base: String,
    /// Session CSRF token.
    pub csrf: String,
    /// The bans on this page.
    pub bans: Vec<String>,
    /// How many there are in all.
    pub total: i64,
    /// The add form's value (kept after a refusal).
    pub draft: String,
    /// Draft refusal.
    pub error: Option<String>,
    /// A notice after a redirect.
    pub notice: Option<String>,
    /// Previous/next links.
    pub pagination: Pagination,
}

/// One template in the catalogue.
#[derive(Debug)]
pub struct TemplateRow {
    /// Name.
    pub name: String,
    /// Editor path.
    pub href: String,
    /// Languages the list stores its own body in.
    pub languages: Vec<String>,
}

/// The template catalogue of a list.
#[derive(Debug, Template)]
#[template(path = "templates.html")]
pub struct TemplateCatalogue {
    /// Document shell.
    pub shell: Shell,
    /// Group navigation.
    pub groups: Vec<GroupLink>,
    /// Every known template.
    pub rows: Vec<TemplateRow>,
    /// A notice after a redirect.
    pub notice: Option<String>,
}

/// The editor of one template.
#[derive(Debug, Template)]
#[template(path = "template_editor.html")]
pub struct TemplateEditor {
    /// Document shell.
    pub shell: Shell,
    /// Group navigation.
    pub groups: Vec<GroupLink>,
    /// Template name.
    pub name: String,
    /// Form action.
    pub action: String,
    /// Where the catalogue is.
    pub catalogue_href: String,
    /// Session CSRF token.
    pub csrf: String,
    /// Language choices.
    pub languages: Vec<Choice>,
    /// The body being edited.
    pub body: String,
    /// Whether the list stores its own body in this language.
    pub stored: bool,
    /// Where the effective body comes from.
    pub source: String,
    /// The effective body when nothing is stored.
    pub effective: String,
    /// The rendered preview, if asked for.
    pub preview: Option<String>,
    /// Placeholder names and sample values.
    pub placeholders: Vec<Fact>,
    /// A refusal.
    pub error: Option<String>,
}

/// The confirmation page that deletes a list.
#[derive(Debug, Template)]
#[template(path = "delete_list.html")]
pub struct DeleteList {
    /// Document shell.
    pub shell: Shell,
    /// Group navigation.
    pub groups: Vec<GroupLink>,
    /// The list id to type back.
    pub list_id: String,
    /// Translated consequences, one per line.
    pub consequences: Vec<String>,
    /// Form action.
    pub action: String,
    /// Session CSRF token.
    pub csrf: String,
    /// A refusal.
    pub error: Option<String>,
}

/// One member's options page.
#[derive(Debug, Template)]
#[template(path = "member_options.html")]
pub struct MemberOptionsPage {
    /// Document shell.
    pub shell: Shell,
    /// The member's address.
    pub email: String,
    /// Where the roster is.
    pub roster_href: String,
    /// Form action.
    pub action: String,
    /// Session CSRF token.
    pub csrf: String,
    /// Read-only facts (role, mode, since).
    pub facts: Vec<Fact>,
    /// The controls.
    pub fields: Vec<SettingField>,
    /// The effective preferences, resolved, as label/value pairs.
    pub effective: Vec<Fact>,
    /// Bounce score, last bounce, and whether delivery is off by bounces.
    pub bounce_score: String,
    pub last_bounce: String,
    pub bounced: bool,
    /// Where the bounce reset posts.
    pub bounce_action: String,
    /// Where removal posts.
    pub remove_action: String,
    /// A notice after a redirect.
    pub notice: Option<String>,
    /// A refusal that names no single field.
    pub error: Option<String>,
}

/// The mass subscription form and, after a submission, its outcomes.
#[derive(Debug, Template)]
#[template(path = "mass_subscribe.html")]
pub struct MassSubscribe {
    /// Document shell.
    pub shell: Shell,
    /// Where the roster is.
    pub roster_href: String,
    /// Form action.
    pub action: String,
    /// Session CSRF token.
    pub csrf: String,
    /// Role choices.
    pub roles: Vec<Choice>,
    /// The flags: `(name, label, help, checked)`.
    pub flags: Vec<Flag>,
    /// The textarea, kept after a submission.
    pub addresses: String,
    /// Outcomes of the last submission.
    pub outcomes: Vec<Fact>,
    /// A refusal.
    pub error: Option<String>,
}

/// One checkbox of a form.
#[derive(Debug)]
pub struct Flag {
    /// Field name.
    pub name: String,
    /// Translated label.
    pub label: String,
    /// Translated help.
    pub help: String,
    /// Whether it is checked.
    pub checked: bool,
}
