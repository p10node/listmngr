//! One disposable `PostgreSQL` schema per test.
//!
//! Tests take their schema from the operator-supplied `TEST_POSTGRES_URL`,
//! so every backend test runs against the same server without a fresh
//! database each. Production code never calls this; it lives in the library
//! so integration tests of every crate can share it.

use crate::db_error;
use listmngr_core::{Error, Result};

/// The environment variable naming the disposable `PostgreSQL` server.
pub const POSTGRES_URL_ENV: &str = "TEST_POSTGRES_URL";

/// A schema created for one test; `url` connects with it as the search path.
#[derive(Debug)]
pub struct IsolatedSchema {
    base: String,
    name: String,
    /// Connection URL whose `search_path` is this schema alone.
    pub url: String,
}

impl IsolatedSchema {
    /// Create `prefix_<unique>` on `TEST_POSTGRES_URL`.
    ///
    /// # Errors
    /// Returns configuration errors when the variable is unset or not a
    /// `PostgreSQL` URL, and database errors when the schema cannot be created.
    pub async fn create(prefix: &str) -> Result<Self> {
        let base = std::env::var(POSTGRES_URL_ENV)
            .map_err(|_| Error::Validation(format!("{POSTGRES_URL_ENV} is not set")))?;
        if !(base.starts_with("postgres://") || base.starts_with("postgresql://")) {
            return Err(Error::Validation(format!(
                "{POSTGRES_URL_ENV} must be a PostgreSQL URL"
            )));
        }
        let name = format!("{prefix}_{}", uuid::Uuid::now_v7().simple());
        let admin = admin_pool(&base).await?;
        sqlx::query(&format!("CREATE SCHEMA {name}"))
            .execute(&admin)
            .await
            .map_err(db_error)?;
        admin.close().await;
        let url = format!(
            "{base}{}options=-csearch_path%3D{name}",
            if base.contains('?') { '&' } else { '?' }
        );
        Ok(Self { base, name, url })
    }

    /// The schema's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Drop the schema and everything in it. Close the test's pool first.
    ///
    /// # Errors
    /// Returns a database error when the drop fails.
    pub async fn drop(self) -> Result<()> {
        let admin = admin_pool(&self.base).await?;
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.name))
            .execute(&admin)
            .await
            .map_err(db_error)?;
        admin.close().await;
        Ok(())
    }
}

async fn admin_pool(base: &str) -> Result<sqlx::AnyPool> {
    crate::install_drivers();
    sqlx::any::AnyPoolOptions::new()
        .max_connections(1)
        .connect(base)
        .await
        .map_err(db_error)
}
