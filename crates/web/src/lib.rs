#![forbid(unsafe_code)]
//! Server-rendered browser presentation.
//!
//! Every browser page is an Askama template with compile-time auto-escaping;
//! handlers build view models and never concatenate markup (ADR-0004). The
//! shell — document language, navigation, skip link and footer — comes from the
//! shared Fluent catalog, so a page renders in the reader's language. Styles
//! and the vendored progressive-enhancement library are served from this
//! origin; no page loads a third-party asset.

mod pages;

/// Auto-escaping for every `.html` template, configured in `askama.toml`.
///
/// It escapes exactly the five characters that can leave an HTML text or
/// quoted-attribute context, writing the named references (`&amp;`, `&lt;`,
/// `&gt;`, `&quot;`) and `&#39;` for the apostrophe.
#[derive(Debug, Clone, Copy)]
pub struct HtmlEscaper;

impl askama::filters::Escaper for HtmlEscaper {
    fn write_escaped_str<W: core::fmt::Write>(
        &self,
        mut dest: W,
        string: &str,
    ) -> core::fmt::Result {
        let mut rest = string;
        while let Some(index) = rest.find(['&', '<', '>', '"', '\'']) {
            dest.write_str(&rest[..index])?;
            let character = rest[index..].chars().next().expect("a found character");
            dest.write_str(match character {
                '&' => "&amp;",
                '<' => "&lt;",
                '>' => "&gt;",
                '"' => "&quot;",
                _ => "&#39;",
            })?;
            rest = &rest[index + character.len_utf8()..];
        }
        dest.write_str(rest)
    }
}

pub use pages::{
    Account, AdminIndex, AdminRow, Archive, ArchiveAttachment, ArchiveMessage, CheckEmail,
    ConfirmForm, Confirmed, DeliveryRow, Directory, DirectoryEntry, ErrorPage, Held, HeldItem,
    Leave, ListPage, Login, MemberRow, Members, Moderation, ModerationRow, NumberField, Password,
    PasswordChanged, Preferences, Recover, SelectField, Settings, ShownSecret, Subscription,
    Unsubscribe, Unsubscribed, WebhookDeliveries, WebhookRow, Webhooks,
};
pub use pages::{
    AccountDeleted, AddressRow, Addresses, ApiDocs, ArchiveCompat, ArchiveCompatEntry,
    DeleteAccount, DocOperation, DocPath, ProfilePage, ResetConfirm, ResetDone, ResetRequest,
    SessionRow, Sessions, SignupPage, TokenIssued, TokenRow, Tokens, Verified, VerifyForm,
};
pub use pages::{
    ActionForm, Bans, DeleteList, DiffRow, Fact, GroupLink, HeaderRuleDraft, HeaderRuleRow,
    HeaderRules, SettingField, SettingsGroup, TemplateCatalogue, TemplateEditor, TemplateRow,
};
pub use pages::{
    AdminAddressRow, AdminUser, AdminUserRow, AdminUsers, DomainPage, DomainRow, Domains,
    MembershipRow, OwnerRow,
};
pub use pages::{
    AdminCategory, ArchiveAdmin, ArchiveLinks, ArchiveOverview, ArchiveSender, ArchiveThreads,
    HiddenRow, MonthRow, PosterRow, ThreadRow,
};
pub use pages::{AdminForms, ArchivePost, CategoryOption, TagLink, ThreadMetaView, VoteForm};
pub use pages::{AtomFeed, FeedEntry, RssFeed};
pub use pages::{AuditPage, AuditRow, QueueRow, ReattachForm, SystemPage};
pub use pages::{
    CreateList, Flag, MassSubscribe, MemberOptionsPage, RequestItem, Requests, Roster,
};
pub use pages::{
    LoginTotp, Oidc, PasskeyRow, Passkeys, ProviderLink, ProviderRow, TotpCodes, TotpPage,
};

/// The stylesheet, served from this origin with the design tokens the shell and
/// every page share. Both colour schemes come from the same token names.
pub const STYLESHEET: &str = include_str!("../assets/style.css");

/// htmx 2.0.10, vendored and served from this origin.
///
/// `assets/README.md` records the exact provenance, license and hash. No page
/// loads it until a work package needs progressive enhancement.
pub const HTMX: &str = include_str!("../assets/htmx.min.js");

/// The passkey ceremonies, first-party and served from this origin. Pages
/// that include a script carry a CSP that allows `'self'` scripts and fetches.
pub const PASSKEYS_SCRIPT: &str = include_str!("../assets/passkeys.js");

/// The held queue's keyboard shortcuts, first-party and served from this
/// origin; the queue works the same without it.
pub const MODERATION_SCRIPT: &str = include_str!("../assets/moderation.js");

/// SHA-384 of [`HTMX`], base64 as an `integrity` attribute spells it. A test
/// pins this so a swapped asset fails the build gates rather than reaching a
/// browser.
pub const HTMX_SHA384: &str = "H5SrcfygHmAuTDZphMHqBJLc3FhssKjG7w/CeCpFReSfwBWDTKpkzPP8c+cLsK+V";

/// The navigation entry a page belongs to, so the shell can mark it current.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Nav {
    /// The public list directory.
    Lists,
    /// The signed-in member's own subscriptions and account pages.
    Account,
    /// Moderator queues.
    Moderation,
    /// The login form.
    Login,
    /// A page that belongs to no navigation entry.
    None,
}

/// One rendered navigation link.
#[derive(Debug)]
pub struct NavItem {
    /// Target path, always same-origin.
    pub href: &'static str,
    /// Translated link text.
    pub label: String,
    /// Whether this is the page being read.
    pub current: bool,
}

/// One `<option>` of a select control.
#[derive(Debug)]
pub struct Choice {
    /// Submitted value.
    pub value: String,
    /// Translated label.
    pub label: String,
    /// Whether the control currently holds this value.
    pub selected: bool,
}

/// Translated options for `values`, each `(submitted value, message id)`.
#[must_use]
pub fn choices(language: &str, values: &[(&str, &str)], selected: Option<&str>) -> Vec<Choice> {
    values
        .iter()
        .map(|(value, id)| Choice {
            value: (*value).to_owned(),
            label: listmngr_i18n::message(language, id, &[]),
            selected: selected == Some(*value),
        })
        .collect()
}

/// Previous/next links for a bounded listing; absent links are not rendered.
#[derive(Debug, Default)]
pub struct Pagination {
    /// Path of the previous page, if there is one.
    pub previous: Option<String>,
    /// Path of the next page, if there is one.
    pub next: Option<String>,
}

impl Pagination {
    /// The usual numbered pagination over `path`, which carries no query string.
    #[must_use]
    pub fn numbered(path: &str, page: u32, more: bool, limit: u32) -> Self {
        Self::filtered(path, "", page, more, limit)
    }

    /// Numbered pagination over `path` that keeps `query` — the already
    /// encoded filters, without `page` — on every link.
    #[must_use]
    pub fn filtered(path: &str, query: &str, page: u32, more: bool, limit: u32) -> Self {
        let prefix = if query.is_empty() {
            format!("{path}?page=")
        } else {
            format!("{path}?{query}&page=")
        };
        Self {
            previous: page
                .checked_sub(1)
                .map(|previous| format!("{prefix}{previous}")),
            next: (more && page < limit).then(|| format!("{prefix}{}", page + 1)),
        }
    }
}

/// The document shell: language, title and navigation state.
#[derive(Debug, Clone)]
pub struct Shell {
    language: &'static str,
    title: String,
    active: Nav,
    script: Option<&'static str>,
    htmx: bool,
}

impl Shell {
    /// A shell whose title is the catalog message `title`.
    #[must_use]
    pub fn new(language: &str, title: &str, active: Nav) -> Self {
        let language = listmngr_i18n::negotiate(language);
        Self {
            language,
            title: listmngr_i18n::message(language, title, &[]),
            active,
            script: None,
            htmx: false,
        }
    }

    /// The page loads the first-party passkey script and needs a CSP that
    /// allows it; every form on the page still works without it.
    #[must_use]
    pub const fn with_scripts(mut self) -> Self {
        self.script = Some("/web/passkeys.js");
        self
    }

    /// The page loads one first-party script at `path`; every form on the
    /// page still works without it.
    #[must_use]
    pub const fn with_script(mut self, path: &'static str) -> Self {
        self.script = Some(path);
        self
    }

    /// Whether the page loads a first-party script.
    #[must_use]
    pub const fn scripts(&self) -> bool {
        self.script.is_some() || self.htmx
    }

    /// The first-party script the page loads, if any.
    #[must_use]
    pub const fn script(&self) -> Option<&'static str> {
        self.script
    }

    /// The page loads the vendored htmx for partial updates; every form and
    /// link on it works without it.
    #[must_use]
    pub const fn with_htmx(mut self) -> Self {
        self.htmx = true;
        self
    }

    /// Whether the page loads htmx.
    #[must_use]
    pub const fn htmx(&self) -> bool {
        self.htmx
    }

    /// A shell whose title is data, such as a list's display name.
    #[must_use]
    pub fn titled(language: &str, title: String, active: Nav) -> Self {
        Self {
            language: listmngr_i18n::negotiate(language),
            title,
            active,
            script: None,
            htmx: false,
        }
    }

    /// The negotiated document language, for `lang` and for page strings.
    #[must_use]
    pub const fn language(&self) -> &'static str {
        self.language
    }

    /// The page title, used for `<title>` and the first heading.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The catalog message `id` in the document language.
    #[must_use]
    pub fn t(&self, id: &str) -> String {
        listmngr_i18n::message(self.language, id, &[])
    }

    /// The catalog message `id` with arguments, in the document language.
    #[must_use]
    pub fn with(&self, id: &str, args: &[(&str, &str)]) -> String {
        listmngr_i18n::message(self.language, id, args)
    }

    /// The navigation, with the reader's current page marked.
    #[must_use]
    pub fn navigation(&self) -> Vec<NavItem> {
        [
            ("/web", "web-nav-lists", Nav::Lists),
            ("/web/account", "web-nav-account", Nav::Account),
            ("/web/moderation", "web-nav-moderation", Nav::Moderation),
            ("/web/login", "web-nav-login", Nav::Login),
        ]
        .into_iter()
        .map(|(href, id, nav)| NavItem {
            href,
            label: self.t(id),
            current: nav == self.active,
        })
        .collect()
    }
}
