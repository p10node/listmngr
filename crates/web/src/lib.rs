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
    ConfirmForm, Confirmed, Directory, DirectoryEntry, ErrorPage, Held, HeldItem, Leave, ListPage,
    Login, MemberRow, Members, Moderation, ModerationRow, NumberField, Password, PasswordChanged,
    Preferences, Recover, SelectField, Settings, Subscription, Unsubscribe, Unsubscribed,
};
pub use pages::{
    ApiDocs, ArchiveCompat, ArchiveCompatEntry, DocOperation, DocPath, ProfilePage, SessionRow,
    Sessions, SignupPage, Verified, VerifyForm,
};

/// The stylesheet, served from this origin with the design tokens the shell and
/// every page share. Both colour schemes come from the same token names.
pub const STYLESHEET: &str = include_str!("../assets/style.css");

/// htmx 2.0.10, vendored and served from this origin.
///
/// `assets/README.md` records the exact provenance, license and hash. No page
/// loads it until a work package needs progressive enhancement.
pub const HTMX: &str = include_str!("../assets/htmx.min.js");

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
        Self {
            previous: page
                .checked_sub(1)
                .map(|previous| format!("{path}?page={previous}")),
            next: (more && page < limit).then(|| format!("{path}?page={}", page + 1)),
        }
    }
}

/// The document shell: language, title and navigation state.
#[derive(Debug, Clone)]
pub struct Shell {
    language: &'static str,
    title: String,
    active: Nav,
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
        }
    }

    /// A shell whose title is data, such as a list's display name.
    #[must_use]
    pub fn titled(language: &str, title: String, active: Nav) -> Self {
        Self {
            language: listmngr_i18n::negotiate(language),
            title,
            active,
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
