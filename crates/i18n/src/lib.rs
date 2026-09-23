#![forbid(unsafe_code)]

//! Runtime strings in the operator's and members' languages.
//!
//! Catalogs are Fluent files embedded at build time, one per supported
//! language; English is the source of truth and the fallback for every other
//! language, per message. Template *bodies* are a separate catalog in
//! `listmngr-mail`; this crate holds the strings code generates itself
//! (notice subjects, fixed phrases).
//!
//! The interface speaks the [`SUPPORTED`] languages. Notices speak those
//! and every language Mailman's translators brought ([`notice_languages`]):
//! their subjects come from Mailman's `.po` catalogs where the English is
//! the same, and everything else falls back to English per message.
use fluent_bundle::{FluentArgs, FluentResource, FluentValue, bundle::FluentBundle};
use std::sync::OnceLock;
use unic_langid::LanguageIdentifier;

/// Supported catalog languages, English first.
pub const SUPPORTED: &[&str] = &["en", "vi"];

const DEFAULT: &str = "en";

const CATALOGS: &[(&str, &str)] = &[
    ("en", include_str!("../locales/en.ftl")),
    ("vi", include_str!("../locales/vi.ftl")),
];

#[path = "mailman.rs"]
mod mailman;

/// A bundle that never carries thread-local state, so it can live in a
/// `OnceLock` and be read from any runner.
type Bundle = FluentBundle<FluentResource, intl_memoizer::concurrent::IntlLangMemoizer>;

struct Catalog {
    language: &'static str,
    bundle: Bundle,
}

fn catalogs() -> &'static [Catalog] {
    static CATALOG: OnceLock<Vec<Catalog>> = OnceLock::new();
    CATALOG.get_or_init(|| {
        CATALOGS
            .iter()
            .chain(mailman::CATALOGS)
            .map(|(language, source)| {
                let id: LanguageIdentifier = language.parse().expect("catalog language tag");
                let resource =
                    FluentResource::try_new((*source).to_owned()).unwrap_or_else(|(_, errors)| {
                        panic!("{language}.ftl does not parse: {errors:?}")
                    });
                let mut bundle: Bundle = FluentBundle::new_concurrent(vec![id]);
                // Bidi isolation marks would leak U+2068/U+2069 into subjects.
                bundle.set_use_isolating(false);
                bundle
                    .add_resource(resource)
                    .unwrap_or_else(|errors| panic!("{language}.ftl has duplicates: {errors:?}"));
                // Every catalog names the notice languages the same way.
                let names = FluentResource::try_new(mailman::NAMES.to_owned()).unwrap_or_else(
                    |(_, errors)| panic!("languages.ftl does not parse: {errors:?}"),
                );
                bundle.add_resource_overriding(names);
                Catalog { language, bundle }
            })
            .collect()
    })
}

#[must_use]
pub fn is_supported(language: &str) -> bool {
    SUPPORTED.contains(&language)
}

/// The shipped catalog that serves `requested`, if any.
///
/// An exact tag wins, else the same primary subtag (`vi-VN` → `vi`). `None`
/// means no catalog ships for that language family, so a caller may try its
/// next preference.
#[must_use]
pub fn supported_match(requested: &str) -> Option<&'static str> {
    closest(SUPPORTED.iter().copied(), requested)
}

/// The notice language that serves `requested`, if any: the interface's
/// languages and Mailman's, matched as [`supported_match`] matches.
#[must_use]
pub fn notice_match(requested: &str) -> Option<&'static str> {
    closest(notice_languages(), requested)
}

/// Every language a notice can be written in, English first.
pub fn notice_languages() -> impl Iterator<Item = &'static str> {
    SUPPORTED
        .iter()
        .copied()
        .chain(mailman::LANGUAGES.iter().map(|(code, _)| *code))
}

/// Every notice language as a picker option, English first: the code and
/// the Fluent id of its name.
pub const NOTICE_LANGUAGE_OPTIONS: &[(&str, &str)] = mailman::OPTIONS;

/// The notice language closest to `requested`, falling back to English.
#[must_use]
pub fn negotiate_notice(requested: &str) -> &'static str {
    notice_match(requested).unwrap_or(DEFAULT)
}

/// [`choose`] over the notice languages: the first preference a notice can
/// be written in, else English.
#[must_use]
pub fn choose_notice<'a>(preferences: impl IntoIterator<Item = &'a str>) -> &'static str {
    preferences
        .into_iter()
        .find_map(notice_match)
        .unwrap_or(DEFAULT)
}

fn closest(
    candidates: impl Iterator<Item = &'static str>,
    requested: &str,
) -> Option<&'static str> {
    let wanted = requested.trim().parse::<LanguageIdentifier>().ok()?;
    let mut family = None;
    for candidate in candidates {
        let Ok(tag) = candidate.parse::<LanguageIdentifier>() else {
            continue;
        };
        if tag.matches(&wanted, false, false) {
            return Some(candidate);
        }
        if family.is_none() && tag.language == wanted.language {
            family = Some(candidate);
        }
    }
    family
}

/// The supported language closest to `requested`, falling back to English.
#[must_use]
pub fn negotiate(requested: &str) -> &'static str {
    supported_match(requested).unwrap_or(DEFAULT)
}

/// The first preference that a shipped catalog serves, else English. Use
/// this with `[member preference, list language, site default]`.
#[must_use]
pub fn choose<'a>(preferences: impl IntoIterator<Item = &'a str>) -> &'static str {
    preferences
        .into_iter()
        .find_map(supported_match)
        .unwrap_or(DEFAULT)
}

fn lookup(language: &str, id: &str, args: &FluentArgs<'_>) -> Option<String> {
    let catalog = catalogs()
        .iter()
        .find(|catalog| catalog.language == language)?;
    let message = catalog.bundle.get_message(id)?;
    let pattern = message.value()?;
    let mut errors = Vec::new();
    let text = catalog
        .bundle
        .format_pattern(pattern, Some(args), &mut errors)
        .into_owned();
    errors.is_empty().then_some(text)
}

/// The message `id` in `language` (any notice language), falling back to
/// English and finally to the id itself, so a missing translation never
/// produces an empty string.
#[must_use]
pub fn message(language: &str, id: &str, args: &[(&str, &str)]) -> String {
    let mut fluent_args = FluentArgs::new();
    for (name, value) in args {
        fluent_args.set(*name, FluentValue::from(*value));
    }
    let language = negotiate_notice(language);
    lookup(language, id, &fluent_args)
        .or_else(|| lookup(DEFAULT, id, &fluent_args))
        .unwrap_or_else(|| id.to_owned())
}

/// Every message id the English catalog defines, for completeness checks.
#[must_use]
pub fn message_ids() -> Vec<&'static str> {
    CATALOGS
        .iter()
        .find(|(language, _)| *language == DEFAULT)
        .map(|(_, source)| {
            source
                .lines()
                .filter(|line| !line.starts_with('#') && line.contains('='))
                .filter_map(|line| line.split('=').next())
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .collect()
        })
        .unwrap_or_default()
}
