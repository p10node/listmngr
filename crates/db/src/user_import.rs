//! An account carried over from another system.
//!
//! Mailman 3 has a user behind every address; an import brings those
//! accounts across with their addresses, the one they prefer and their
//! display name — but never a password. The other system's hash is in
//! another scheme, so the account is created with a random password
//! nobody knows, marked unusable, and its owner takes it over through
//! the recovery flow, exactly as a just-in-time account does.
use crate::{AuditContext, Database, UserRepo, db_error, now};
use argon2::PasswordHasher;
use argon2::password_hash::{SaltString, rand_core::OsRng};
use listmngr_core::{Address, Error, PreferencesId, Result, User, UserId};
use rand::TryRngCore;
use sqlx::Row;

/// One address of an imported account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedAddress {
    pub email: String,
    pub display_name: String,
    /// Whether the other system had proof of the mailbox.
    pub verified: bool,
}

/// An account to carry over: its addresses, the one it prefers, and what
/// the other system knew about it. The password is deliberately absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedUser {
    pub display_name: String,
    pub is_server_owner: bool,
    pub locale: String,
    pub addresses: Vec<ImportedAddress>,
    /// The preferred address, which must be one of `addresses`; without
    /// one the first address is preferred, as this site always has one.
    pub preferred: Option<String>,
}

impl UserRepo<'_> {
    /// Create an imported account with its addresses in one transaction.
    ///
    /// An address nobody owns is adopted (its registration date and any
    /// verification kept, and verified when the other system had proof);
    /// an address that belongs to another account is a conflict and
    /// nothing is written. The account has no usable password.
    ///
    /// # Errors
    /// `Validation` without an address or with an address this site
    /// cannot parse, `Conflict` when an address belongs to somebody
    /// else, and the database's own errors.
    pub async fn create_imported_with_context(
        self,
        imported: ImportedUser,
        context: &AuditContext,
    ) -> Result<User> {
        if imported.addresses.is_empty() {
            return Err(Error::Validation("an account needs an address".into()));
        }
        let db = self.db;
        let planned = plan_addresses(db, &imported).await?;
        let preferred = preferred_address(&planned, imported.preferred.as_deref())?;
        let preferences = PreferencesId::new();
        let user = User {
            id: UserId::new(),
            display_name: imported.display_name,
            is_server_owner: imported.is_server_owner,
            locale: listmngr_i18n::negotiate(&imported.locale).to_owned(),
            timezone: "UTC".into(),
            preferred_address_id: Some(preferred),
            created_at: chrono::Utc::now(),
        };
        let mut random = [0_u8; 32];
        rand::rngs::OsRng
            .try_fill_bytes(&mut random)
            .map_err(db_error)?;
        let hash = self
            .password_hasher()?
            .hash_password(&random, &SaltString::generate(&mut OsRng))
            .map_err(db_error)?
            .to_string();
        let mut tx = db.write_tx().await?;
        sqlx::query("INSERT INTO preferences(id) VALUES($1)")
            .bind(preferences.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        sqlx::query("INSERT INTO users(id,display_name,is_server_owner,preferences_id,locale,timezone,preferred_address_id,created_at) VALUES($1,$2,$3,$4,$5,$6,NULL,$7)")
            .bind(user.id.to_string())
            .bind(&user.display_name)
            .bind(i64::from(user.is_server_owner))
            .bind(preferences.to_string())
            .bind(&user.locale)
            .bind(&user.timezone)
            .bind(user.created_at.to_rfc3339())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        sqlx::query("INSERT INTO user_credentials(user_id,password_hash,password_updated_at,usable) VALUES($1,$2,$3,0)")
            .bind(user.id.to_string())
            .bind(hash)
            .bind(now())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        write_addresses(&mut tx, user.id, &planned).await?;
        sqlx::query("UPDATE users SET preferred_address_id=$1 WHERE id=$2")
            .bind(preferred.to_string())
            .bind(user.id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        Database::record_tx_with_context(
            &mut tx,
            context,
            "user.import",
            "user",
            &user.id.to_string(),
            serde_json::json!({
                "addresses": planned
                    .iter()
                    .map(|(address, _, _)| address.email.clone())
                    .collect::<Vec<_>>(),
                "is_server_owner": user.is_server_owner,
                "usable_password": false,
            }),
        )
        .await?;
        tx.commit().await.map_err(db_error)?;
        Ok(user)
    }
}

/// Write the account's addresses inside its transaction: a bare one
/// adopted (its display name kept when the import has none), a new one
/// inserted, a mailbox the other system had proof of verified, and every
/// membership of the address given the account.
async fn write_addresses(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    user: UserId,
    planned: &[Planned],
) -> Result<()> {
    for (address, adopted, verified) in planned {
        if *adopted {
            sqlx::query("UPDATE addresses SET user_id=$1,display_name=CASE WHEN $2='' THEN display_name ELSE $2 END WHERE id=$3")
                .bind(user.to_string())
                .bind(&address.display_name)
                .bind(address.id.to_string())
                .execute(&mut **tx)
                .await
                .map_err(db_error)?;
        } else {
            sqlx::query("INSERT INTO addresses(id,email,original_email,display_name,user_id,registered_on) VALUES($1,$2,$3,$4,$5,$6)")
                .bind(address.id.to_string())
                .bind(&address.email)
                .bind(&address.original_email)
                .bind(&address.display_name)
                .bind(user.to_string())
                .bind(address.registered_on.to_rfc3339())
                .execute(&mut **tx)
                .await
                .map_err(db_error)?;
        }
        if *verified && address.verified_on.is_none() {
            sqlx::query("UPDATE addresses SET verified_on=$1 WHERE id=$2")
                .bind(now())
                .bind(address.id.to_string())
                .execute(&mut **tx)
                .await
                .map_err(db_error)?;
        }
        sqlx::query("UPDATE members SET user_id=$1 WHERE address_id=$2 AND user_id IS NULL")
            .bind(user.to_string())
            .bind(address.id.to_string())
            .execute(&mut **tx)
            .await
            .map_err(db_error)?;
    }
    Ok(())
}

/// Each address as it will be written: the row, whether an existing bare
/// one is being adopted, and whether the other system had proof of the
/// mailbox.
type Planned = (Address, bool, bool);

async fn plan_addresses(db: &Database, imported: &ImportedUser) -> Result<Vec<Planned>> {
    let mut planned = Vec::new();
    for entry in &imported.addresses {
        let mut address = Address::new(&entry.email, entry.display_name.clone())?;
        let adopted = match db.addresses().get(&address.email).await {
            Ok(existing) if existing.user_id.is_none() => {
                address.id = existing.id;
                address.registered_on = existing.registered_on;
                address.verified_on = existing.verified_on;
                true
            }
            Ok(_) => return Err(Error::Conflict("address belongs to another user".into())),
            Err(Error::NotFound(_)) => false,
            Err(error) => return Err(error),
        };
        planned.push((address, adopted, entry.verified));
    }
    Ok(planned)
}

/// The account's preferred address, which must be one of its own.
fn preferred_address(
    planned: &[Planned],
    preferred: Option<&str>,
) -> Result<listmngr_core::AddressId> {
    let Some(email) = preferred else {
        return Ok(planned[0].0.id);
    };
    let wanted = Address::new(email, String::new())?.email;
    planned
        .iter()
        .find(|(address, _, _)| address.email == wanted)
        .map(|(address, _, _)| address.id)
        .ok_or_else(|| {
            Error::Validation("the preferred address is not one of the account's".into())
        })
}

/// The addresses of an account, for the importer's reports.
impl Database {
    /// Whether `email` is already known and owned by an account.
    ///
    /// # Errors
    /// The database's own errors.
    pub async fn address_has_account(&self, email: &str) -> Result<bool> {
        let owned: Option<Option<String>> =
            sqlx::query("SELECT user_id FROM addresses WHERE email=$1")
                .bind(Address::new(email, String::new())?.email)
                .fetch_optional(&self.pool)
                .await
                .map_err(db_error)?
                .map(|row| row.try_get("user_id").ok().flatten());
        Ok(owned.flatten().is_some())
    }
}
