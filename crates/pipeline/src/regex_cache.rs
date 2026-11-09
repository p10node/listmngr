//! A process-wide cache of the regular expressions list owners configure.
//!
//! Address lists, header rules, topics and bans hold owner-supplied
//! patterns; the single shared `in` worker evaluates them for every
//! message of every list, so compiling each pattern per evaluation made
//! the cost of a message proportional to the owner's configuration. A
//! compiled pattern (or its compile error) is kept by `(kind, pattern)`
//! and reused. The table is bounded: it is emptied when it reaches
//! [`CAPACITY`] entries, so a stream of distinct patterns cannot grow it
//! without limit.

use regex::Regex;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex};

/// The most compiled patterns kept before the table starts over.
pub const CAPACITY: usize = 20_000;

type Compiled = Result<Regex, regex::Error>;

static CACHE: LazyLock<Mutex<HashMap<(&'static str, String), Compiled>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static COMPILATIONS: AtomicUsize = AtomicUsize::new(0);

/// How many patterns have been compiled by [`get_or_compile`] so far.
#[must_use]
pub fn compilations() -> usize {
    COMPILATIONS.load(Ordering::Relaxed)
}

/// The pattern as `build` compiles it, built at most once per `kind` and
/// pattern while it stays cached. `kind` names the builder, since the same
/// text compiles differently for different callers.
///
/// # Errors
/// Returns the (cached) compile error of a pattern that does not build.
pub fn get_or_compile(
    kind: &'static str,
    pattern: &str,
    build: impl FnOnce(&str) -> Compiled,
) -> Compiled {
    let key = (kind, pattern.to_owned());
    if let Some(found) = CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
    {
        return found.clone();
    }
    COMPILATIONS.fetch_add(1, Ordering::Relaxed);
    let compiled = build(pattern);
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if cache.len() >= CAPACITY {
        cache.clear();
    }
    cache.insert(key, compiled.clone());
    compiled
}
