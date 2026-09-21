//! A Mailman 2.1 `config.pck`: the list's `MailList.__dict__` pickled
//! (protocol 0–2, Python 2 `str` as bytes), read into plain values.
use crate::pickle::{self, Item};
use crate::{Error, Result};
use std::collections::BTreeMap;

/// A value of the pickled dictionary, as Mailman 3's importer sees it.
///
/// Bytes are decoded as ASCII then UTF-8 (`bytes_to_str`), tuples read as
/// lists, class instances and classes (which the importer ignores) as
/// `None`.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    List(Vec<Value>),
    Dict(BTreeMap<String, Value>),
}

impl Value {
    fn from_pickled(value: Item) -> Self {
        match value {
            Item::None | Item::Global(_) | Item::Instance(_) => Self::None,
            Item::Bool(flag) => Self::Bool(flag),
            Item::Int(number) => Self::Int(number),
            Item::Float(number) => Self::Float(number),
            Item::Bytes(bytes) => Self::Text(bytes_to_str(&bytes)),
            Item::Text(text) => Self::Text(text),
            Item::List(items) | Item::Tuple(items) => {
                Self::List(items.into_iter().map(Self::from_pickled).collect())
            }
            Item::Dict(entries) => Self::Dict(
                entries
                    .into_iter()
                    .filter_map(|(key, value)| Some((key_text(key)?, Self::from_pickled(value))))
                    .collect(),
            ),
        }
    }

    /// The text of a text value.
    #[must_use]
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }

    /// A number as Python's `int()`/`bool` would give it (a bool is 0/1).
    #[must_use]
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(number) => Some(*number),
            Self::Bool(flag) => Some(i64::from(*flag)),
            #[allow(clippy::cast_possible_truncation)]
            Self::Float(number) if number.is_finite() => Some(number.trunc() as i64),
            _ => None,
        }
    }

    /// Python truth of a value.
    #[must_use]
    pub fn truthy(&self) -> bool {
        match self {
            Self::None => false,
            Self::Bool(flag) => *flag,
            Self::Int(number) => *number != 0,
            Self::Float(number) => *number != 0.0,
            Self::Text(text) => !text.is_empty(),
            Self::List(items) => !items.is_empty(),
            Self::Dict(entries) => !entries.is_empty(),
        }
    }
}

/// Mailman's `bytes_to_str`: ASCII, then UTF-8, then ASCII with
/// replacement.
fn bytes_to_str(bytes: &[u8]) -> String {
    if bytes.is_ascii() {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    String::from_utf8(bytes.to_vec()).unwrap_or_else(|_| {
        bytes
            .iter()
            .map(|b| if b.is_ascii() { *b as char } else { '\u{fffd}' })
            .collect()
    })
}

/// A dictionary key as text; keys that are not strings or integers are
/// dropped with their entries.
fn key_text(key: Item) -> Option<String> {
    match key {
        Item::Bytes(bytes) => Some(bytes_to_str(&bytes)),
        Item::Text(text) => Some(text),
        Item::Int(number) => Some(number.to_string()),
        _ => None,
    }
}

/// The dictionary a `config.pck` holds.
#[derive(Debug, Clone, PartialEq)]
pub struct Config21 {
    entries: BTreeMap<String, Value>,
}

impl Config21 {
    /// Read a `config.pck`.
    /// # Errors
    /// Returns `Pickle` when the bytes are not a pickle of a dictionary.
    pub fn from_pickle(bytes: &[u8]) -> Result<Self> {
        match Value::from_pickled(pickle::read(bytes)?) {
            Value::Dict(entries) => Ok(Self { entries }),
            other => Err(Error::Pickle(format!(
                "expected the list's dictionary, found {}",
                match other {
                    Value::List(_) => "a list",
                    Value::Text(_) => "a string",
                    Value::None => "nothing",
                    _ => "a scalar",
                }
            ))),
        }
    }

    /// The value under `key`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.entries.get(key)
    }

    /// Whether `key` is present at all.
    #[must_use]
    pub fn has(&self, key: &str) -> bool {
        self.entries.contains_key(key)
    }

    /// Every key, sorted.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }

    #[must_use]
    pub fn text(&self, key: &str) -> Option<String> {
        self.get(key)?.as_text().map(str::to_owned)
    }

    #[must_use]
    pub fn int(&self, key: &str) -> Option<i64> {
        self.get(key)?.as_int()
    }

    #[must_use]
    pub fn float(&self, key: &str) -> Option<f64> {
        match self.get(key)? {
            Value::Float(number) => Some(*number),
            // 2.1 keeps small integers here (seconds, counts).
            #[allow(clippy::cast_precision_loss)]
            Value::Int(number) => Some(*number as f64),
            Value::Bool(flag) => Some(f64::from(u8::from(*flag))),
            _ => None,
        }
    }

    /// Python truth of `key`, `None` when absent.
    #[must_use]
    pub fn bool(&self, key: &str) -> Option<bool> {
        self.get(key).map(Value::truthy)
    }

    /// The text items of a list value; other items are dropped.
    #[must_use]
    pub fn text_list(&self, key: &str) -> Vec<String> {
        match self.get(key) {
            Some(Value::List(items)) => items
                .iter()
                .filter_map(|item| item.as_text().map(str::to_owned))
                .collect(),
            _ => Vec::new(),
        }
    }

    /// A dictionary value's entries.
    #[must_use]
    pub fn dict(&self, key: &str) -> BTreeMap<String, Value> {
        match self.get(key) {
            Some(Value::Dict(entries)) => entries.clone(),
            _ => BTreeMap::new(),
        }
    }

    /// A dictionary value's text entries.
    #[must_use]
    pub fn text_dict(&self, key: &str) -> BTreeMap<String, String> {
        self.dict(key)
            .into_iter()
            .filter_map(|(key, value)| value.as_text().map(|text| (key, text.to_owned())))
            .collect()
    }

    /// The list value's items.
    #[must_use]
    pub fn list(&self, key: &str) -> Vec<Value> {
        match self.get(key) {
            Some(Value::List(items)) => items.clone(),
            _ => Vec::new(),
        }
    }
}
