//! What the out runner needs to know about one recipient of a personalized
//! delivery: their membership, how they wrote their address, their display
//! name and their negotiated language.
use crate::{Database, db_error};
use listmngr_core::{Error, MailingList, MemberId, Result};
use listmngr_mail::personalize::Recipient;
use sqlx::Row;

/// One member as a delivery recipient.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryRecipient {
    pub member_id: MemberId,
    pub profile: Recipient,
}

#[derive(Debug, Clone, Copy)]
pub struct DeliveryRepo<'a> {
    db: &'a Database,
}

impl Database {
    #[must_use]
    pub const fn delivery(&self) -> DeliveryRepo<'_> {
        DeliveryRepo { db: self }
    }
}

impl DeliveryRepo<'_> {
    /// The recipient profile for `email`'s membership of `list`, or `None`
    /// when they are not a member. The display name is the membership's,
    /// else the address's; the language is negotiated like every notice.
    /// # Errors
    /// Returns a database error.
    pub async fn recipient(
        &self,
        list: &MailingList,
        email: &str,
    ) -> Result<Option<DeliveryRecipient>> {
        let Ok(address) = listmngr_core::Address::new(email, String::new()) else {
            return Ok(None);
        };
        let row = sqlx::query(
            "SELECT m.id, m.display_name AS member_name, a.original_email, a.display_name AS address_name, COALESCE(pm.preferred_language, pa.preferred_language, pu.preferred_language) AS language FROM members m JOIN addresses a ON a.id=m.address_id LEFT JOIN preferences pm ON pm.id=m.preferences_id LEFT JOIN preferences pa ON pa.id=a.preferences_id LEFT JOIN users u ON u.id=m.user_id LEFT JOIN preferences pu ON pu.id=u.preferences_id WHERE m.list_id=$1 AND a.email=$2 AND m.role='member' LIMIT 1",
        )
        .bind(list.id.as_str())
        .bind(&address.email)
        .fetch_optional(self.db.pool())
        .await
        .map_err(db_error)?;
        let Some(row) = row else {
            return Ok(None);
        };
        let member_id: MemberId = row
            .try_get::<String, _>("id")
            .map_err(db_error)?
            .parse()
            .map_err(|_| Error::Validation("corrupt member id".into()))?;
        let member_name: String = row.try_get("member_name").map_err(db_error)?;
        let address_name: String = row.try_get("address_name").map_err(db_error)?;
        let delivered_to: String = row.try_get("original_email").map_err(db_error)?;
        let preference: Option<String> = row.try_get("language").map_err(db_error)?;
        let language = listmngr_i18n::choose_notice(
            [
                preference.as_deref().unwrap_or(""),
                list.preferred_language.as_str(),
                self.db.default_language(),
            ]
            .into_iter()
            .filter(|tag| !tag.is_empty()),
        );
        Ok(Some(DeliveryRecipient {
            member_id,
            profile: Recipient {
                email: address.email,
                delivered_to,
                display_name: if member_name.trim().is_empty() {
                    address_name
                } else {
                    member_name
                },
                language: language.to_owned(),
            },
        }))
    }
}
