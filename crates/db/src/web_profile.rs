//! The reader's own profile: display name, interface language and time zone.
//!
//! One validator serves the browser form and the REST user patch, so neither
//! surface can store a name with control characters, a language no catalog
//! serves, or a zone the IANA database does not know.
use crate::web_sessions::WebSession;
use crate::{AuditContext, Database, db_error};
use listmngr_core::{Error, Result};
use sqlx::Row;

/// Longest display name accepted, in characters.
pub const DISPLAY_NAME_MAX: usize = 256;

/// What the reader may edit about themselves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// Name shown on pages and in notices.
    pub display_name: String,
    /// Interface language: a tag a shipped catalog serves.
    pub locale: String,
    /// IANA time zone name.
    pub timezone: String,
}

impl Profile {
    /// Reject anything a page or a notice could not render faithfully.
    /// # Errors
    /// Names that are empty, longer than [`DISPLAY_NAME_MAX`] characters or
    /// carrying control characters; languages without a catalog; time zones
    /// outside the IANA database.
    pub fn validate(&self) -> Result<()> {
        validate_display_name(&self.display_name)?;
        if !listmngr_i18n::is_supported(&self.locale) {
            return Err(Error::Validation("locale is not a shipped language".into()));
        }
        if !timezones().contains(&self.timezone.as_str()) {
            return Err(Error::Validation(
                "timezone is not an IANA zone name".into(),
            ));
        }
        Ok(())
    }
}

/// A name a page or a notice can show faithfully.
/// # Errors
/// Empty, longer than [`DISPLAY_NAME_MAX`] characters, or carrying control
/// characters.
pub fn validate_display_name(display_name: &str) -> Result<()> {
    if display_name.trim().is_empty() {
        return Err(Error::Validation("display_name must not be empty".into()));
    }
    if display_name.chars().count() > DISPLAY_NAME_MAX {
        return Err(Error::Validation(format!(
            "display_name exceeds {DISPLAY_NAME_MAX} characters"
        )));
    }
    if display_name.chars().any(char::is_control) {
        return Err(Error::Validation(
            "display_name must not contain control characters".into(),
        ));
    }
    Ok(())
}

/// Every IANA zone name the build knows, sorted, for validation and for a
/// form's options.
#[must_use]
pub fn timezones() -> &'static [&'static str] {
    static NAMES: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| {
        let mut names: Vec<&'static str> = chrono_tz::TZ_VARIANTS
            .iter()
            .map(|zone| zone.name())
            .collect();
        names.sort_unstable();
        names.dedup();
        names
    })
}

impl Database {
    /// The reader's own profile, under a live session.
    /// # Errors
    /// Rejects a stale or anonymous session; returns database errors.
    pub async fn browser_profile(&self, session: &WebSession) -> Result<Profile> {
        let live = self
            .web_session(&session.token, chrono::Utc::now().timestamp_millis())
            .await?;
        let user = live.user_id.ok_or(Error::Authentication)?;
        let row = sqlx::query("SELECT display_name,locale,timezone FROM users WHERE id=$1")
            .bind(user.to_string())
            .fetch_one(self.pool())
            .await
            .map_err(db_error)?;
        Ok(Profile {
            display_name: row.try_get("display_name").map_err(db_error)?,
            locale: row.try_get("locale").map_err(db_error)?,
            timezone: row.try_get("timezone").map_err(db_error)?,
        })
    }

    /// The interface language a signed-in reader chose, if any.
    /// # Errors
    /// Returns database errors; an anonymous session yields `None`.
    pub async fn browser_locale(&self, session: &WebSession) -> Result<Option<String>> {
        let Some(user) = session.user_id else {
            return Ok(None);
        };
        sqlx::query_scalar("SELECT locale FROM users WHERE id=$1")
            .bind(user.to_string())
            .fetch_optional(self.pool())
            .await
            .map_err(db_error)
    }

    /// Replace the reader's profile and audit it in the same transaction.
    /// # Errors
    /// Validation, a stale session or failed CSRF binding, or audit failure.
    pub async fn browser_update_profile(
        &self,
        session: &WebSession,
        profile: &Profile,
    ) -> Result<()> {
        profile.validate()?;
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_user_tx(&mut tx, session).await?;
        let changed =
            sqlx::query("UPDATE users SET display_name=$1,locale=$2,timezone=$3 WHERE id=$4")
                .bind(profile.display_name.trim())
                .bind(&profile.locale)
                .bind(&profile.timezone)
                .bind(user.to_string())
                .execute(&mut *tx)
                .await
                .map_err(db_error)?
                .rows_affected();
        if changed != 1 {
            return Err(Error::Authentication);
        }
        Self::record_tx_with_context(
            &mut tx,
            &AuditContext::new(Some(user), None, None),
            "user.profile",
            "user",
            &user.to_string(),
            serde_json::json!({
                "display_name": profile.display_name.trim(),
                "locale": profile.locale,
                "timezone": profile.timezone,
            }),
        )
        .await?;
        tx.commit().await.map_err(db_error)
    }
}
