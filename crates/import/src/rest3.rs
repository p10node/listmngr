//! The Mailman 3 core REST API as a read-only source: Basic
//! authentication, JSON in, nothing written back.
//!
//! The core answers every collection with an envelope (`entries`,
//! `start`, `total_size`) and every resource with its `http_etag`; this
//! client only reads, so a site can be imported from a running core
//! without changing it.
use crate::import3::Source;
use crate::{Error, Result};
use serde_json::Value as Json;
use std::time::Duration;

/// A Mailman 3 core's REST root, with the credentials from its
/// `[webservice]` section. Its `Debug` shows the root only: the
/// password never reaches a log.
pub struct Rest {
    client: reqwest::Client,
    root: String,
    user: String,
    password: String,
}

impl std::fmt::Debug for Rest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Rest")
            .field("root", &self.root)
            .field("user", &self.user)
            .finish_non_exhaustive()
    }
}

impl Rest {
    /// The client for `url` (the API root, e.g.
    /// `http://127.0.0.1:8001/3.1`).
    ///
    /// # Errors
    /// Returns `Rest` when the HTTP client cannot be built.
    pub fn new(url: &str, user: &str, password: &str) -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .user_agent(concat!("listmngr/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| Error::Rest(error.to_string()))?;
        Ok(Self {
            client,
            root: url.trim_end_matches('/').to_owned(),
            user: user.to_owned(),
            password: password.to_owned(),
        })
    }

    /// The API root this client reads, for messages. It never carries the
    /// credentials: they travel in the `Authorization` header.
    #[must_use]
    pub fn root(&self) -> &str {
        &self.root
    }

    fn url(&self, path: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            path.to_owned()
        } else {
            format!("{}/{}", self.root, path.trim_start_matches('/'))
        }
    }

    /// One resource, by its path under the API root or by an absolute URL
    /// the core itself gave.
    ///
    /// # Errors
    /// Returns `Rest` when the core cannot be reached, answers anything
    /// but `200`, or answers something that is not JSON. The message
    /// carries the path and the status, never the password.
    pub async fn get(&self, path: &str) -> Result<Json> {
        let url = self.url(path);
        let response = self
            .client
            .get(&url)
            .basic_auth(&self.user, Some(&self.password))
            .send()
            .await
            .map_err(|error| Error::Rest(format!("{path}: {}", scrub(&error.to_string()))))?;
        let status = response.status();
        if !status.is_success() {
            return Err(Error::Rest(format!("{path}: the core answered {status}")));
        }
        response
            .json()
            .await
            .map_err(|error| Error::Rest(format!("{path}: {}", scrub(&error.to_string()))))
    }
}

impl Rest {
    /// One resource that may not be there: `404` is `None`, every other
    /// refusal is an error.
    ///
    /// # Errors
    /// As `get`, except for a missing resource.
    pub async fn get_optional(&self, path: &str) -> Result<Option<Json>> {
        match self.get(path).await {
            Ok(body) => Ok(Some(body)),
            Err(Error::Rest(message)) if message.contains("404") => Ok(None),
            Err(error) => Err(error),
        }
    }
}

/// The credentials travel in the `Authorization` header, never in the
/// URL — but a URL with userinfo (`https://user:secret@host/`) would
/// reach a message through the client's own error text, so any such URL
/// is replaced before the message is used.
fn scrub(message: &str) -> String {
    message
        .split_whitespace()
        .map(|word| {
            let userinfo = word
                .split_once("://")
                .and_then(|(_, rest)| rest.split('/').next())
                .is_some_and(|host| host.contains('@'));
            if userinfo { "<url>" } else { word }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

impl Source for Rest {
    async fn get(&self, path: &str) -> Result<Json> {
        Self::get(self, path).await
    }

    async fn get_optional(&self, path: &str) -> Result<Option<Json>> {
        Self::get_optional(self, path).await
    }
}
