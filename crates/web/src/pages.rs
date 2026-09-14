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
    /// Advertised lists on this page.
    pub entries: Vec<DirectoryEntry>,
    /// Directory paging.
    pub pagination: Pagination,
}

/// One advertised list in the directory.
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
}

/// Password login form.
#[derive(Debug, Template)]
#[template(path = "login.html")]
pub struct Login {
    /// Document shell.
    pub shell: Shell,
    /// Session CSRF token.
    pub csrf: String,
}

/// The signed-in member's own subscriptions.
#[derive(Debug, Template)]
#[template(path = "account.html")]
pub struct Account {
    /// Document shell.
    pub shell: Shell,
    /// Session CSRF token.
    pub csrf: String,
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

/// Bounded member roster with per-member posting policy.
#[derive(Debug, Template)]
#[template(path = "members.html")]
pub struct Members {
    /// Document shell.
    pub shell: Shell,
    /// Scope and effect sentence.
    pub intro: String,
    /// Search form target.
    pub search_action: String,
    /// Current search term.
    pub query: String,
    /// Link that clears the search.
    pub clear_href: String,
    /// Session CSRF token.
    pub csrf: String,
    /// Search term carried through a policy write.
    pub carried_query: String,
    /// Page number carried through a policy write.
    pub carried_page: u32,
    /// Members on this page.
    pub rows: Vec<MemberRow>,
    /// Roster paging.
    pub pagination: Pagination,
    /// Link back to the administration index.
    pub admin_href: String,
}

/// One member of a roster.
#[derive(Debug)]
pub struct MemberRow {
    /// Member's address.
    pub email: String,
    /// Policy form target.
    pub action: String,
    /// Control id, unique per row.
    pub control: String,
    /// Posting policy options.
    pub choices: Vec<Choice>,
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
}

/// The held-message queue of one list.
#[derive(Debug, Template)]
#[template(path = "held.html")]
pub struct Held {
    /// Document shell.
    pub shell: Shell,
    /// Session CSRF token.
    pub csrf: String,
    /// Held messages on this page.
    pub items: Vec<HeldItem>,
    /// Available decisions.
    pub decisions: Vec<Choice>,
    /// Queue paging.
    pub pagination: Pagination,
}

/// One held message awaiting a decision.
#[derive(Debug)]
pub struct HeldItem {
    /// Subject as held.
    pub subject: String,
    /// Envelope sender.
    pub sender: String,
    /// Why the message was held.
    pub reason: String,
    /// Bounded raw source.
    pub source: String,
    /// Decision form target.
    pub action: String,
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
