//! The secrets the database holds under the master key: what is sealed,
//! sealing what is not, and moving everything to a new key.
use crate::keyring::{MasterKey, TOTP_PURPOSE, is_sealed};
use crate::{AuditContext, Database, db_error};
use listmngr_core::{Error, Result};
use serde::Serialize;
use sqlx::Row;

/// How many TOTP secrets are sealed and how many are in the clear.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct SecretsState {
    pub totp_sealed: u64,
    pub totp_plain: u64,
}

/// Reads and rewrites of the sealed secrets.
#[derive(Debug)]
pub struct SecretsRepo<'a> {
    db: &'a Database,
}

impl Database {
    #[must_use]
    pub const fn secrets(&self) -> SecretsRepo<'_> {
        SecretsRepo { db: self }
    }
}

fn count(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

impl SecretsRepo<'_> {
    /// Sealed and plain TOTP rows.
    /// # Errors
    /// Database errors.
    pub async fn status(&self) -> Result<SecretsState> {
        let row = sqlx::query(
            "SELECT COALESCE(SUM(CASE WHEN secret LIKE 'v1:%' THEN 1 ELSE 0 END),0) AS sealed, COALESCE(SUM(CASE WHEN secret LIKE 'v1:%' THEN 0 ELSE 1 END),0) AS plain FROM user_totp",
        )
        .fetch_one(self.db.pool())
        .await
        .map_err(db_error)?;
        Ok(SecretsState {
            totp_sealed: count(row.try_get::<i64, _>("sealed").map_err(db_error)?),
            totp_plain: count(row.try_get::<i64, _>("plain").map_err(db_error)?),
        })
    }

    /// Seal every TOTP secret still in the clear under the site's master
    /// key, in one transaction with one `security.encrypt_secrets` audit
    /// event; returns how many rows changed.
    /// # Errors
    /// Validation when the site has no master key; database errors.
    pub async fn encrypt(&self, context: &AuditContext) -> Result<u64> {
        let key = self.master_key()?;
        let mut tx = self.db.write_tx().await?;
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT user_id, secret FROM user_totp WHERE secret NOT LIKE 'v1:%' ORDER BY user_id",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(db_error)?;
        for (user_id, secret) in &rows {
            let sealed = key.seal(TOTP_PURPOSE, user_id.as_bytes(), secret.as_bytes())?;
            sqlx::query("UPDATE user_totp SET secret=$1 WHERE user_id=$2")
                .bind(sealed)
                .bind(user_id)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
        }
        let sealed = u64::try_from(rows.len()).unwrap_or(u64::MAX);
        Database::record_tx_with_context(
            &mut tx,
            context,
            "security.encrypt_secrets",
            "site",
            "totp",
            serde_json::json!({ "sealed": sealed }),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(sealed)
    }

    /// Open every sealed TOTP secret with `previous` and seal it again under
    /// the site's current master key, in one transaction with one
    /// `security.rewrap_secrets` audit event; a row `previous` cannot open
    /// fails the whole operation. Returns how many rows changed.
    /// # Errors
    /// Validation when the site has no master key or a row does not open
    /// under `previous`; database errors.
    pub async fn rewrap(&self, previous: &MasterKey, context: &AuditContext) -> Result<u64> {
        let key = self.master_key()?;
        let mut tx = self.db.write_tx().await?;
        let rows: Vec<(String, String)> = sqlx::query_as(
            "SELECT user_id, secret FROM user_totp WHERE secret LIKE 'v1:%' ORDER BY user_id",
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(db_error)?;
        for (user_id, sealed) in &rows {
            debug_assert!(is_sealed(sealed));
            let plain = previous
                .open(TOTP_PURPOSE, user_id.as_bytes(), sealed)
                .map_err(|_| {
                    Error::Validation("a secret does not open under the previous key".into())
                })?;
            let resealed = key.seal(TOTP_PURPOSE, user_id.as_bytes(), &plain)?;
            sqlx::query("UPDATE user_totp SET secret=$1 WHERE user_id=$2")
                .bind(resealed)
                .bind(user_id)
                .execute(&mut *tx)
                .await
                .map_err(db_error)?;
        }
        let rewrapped = u64::try_from(rows.len()).unwrap_or(u64::MAX);
        Database::record_tx_with_context(
            &mut tx,
            context,
            "security.rewrap_secrets",
            "site",
            "totp",
            serde_json::json!({ "rewrapped": rewrapped }),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(rewrapped)
    }

    fn master_key(&self) -> Result<&MasterKey> {
        self.db
            .master_key()
            .ok_or_else(|| Error::Validation("security.master_key is not set".into()))
    }
}
