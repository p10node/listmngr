//! The browser's list directory, list creation and list summary.
//!
//! The directory is a read for anyone; a signed-in reader's roles decorate
//! it and, on request, add their own unadvertised lists. Creating a list is
//! one transaction under live browser authority: the domain's owner or a
//! server owner, the list row with its first settings, its first owner and
//! every audit event commit together or not at all.
use crate::{AuditContext, Database, ListRepo, NewList, db_error, web_sessions::WebSession};
use listmngr_core::{
    Address, Domain, Error, ListId, MailingList, MemberId, MemberRole, PreferencesId, Result,
    UserId,
};
use sqlx::Row as _;

/// What the directory is asked for.
#[derive(Debug, Clone, Copy, Default)]
pub struct DirectoryFilter<'a> {
    /// Case-insensitive substring of the id, display name or description;
    /// empty matches every list.
    pub query: &'a str,
    /// Exact mail host, when set.
    pub domain: Option<&'a str>,
    /// Add the reader's own unadvertised lists (every list for a server
    /// owner). Ignored for a visitor.
    pub show_all: bool,
    /// Row offset.
    pub offset: i64,
}

/// One directory row, with the reader's standing on the list.
#[derive(Debug)]
pub struct DirectoryRow {
    pub id: String,
    pub name: String,
    pub description: String,
    pub advertised: bool,
    pub role: Option<MemberRole>,
}

/// What a reader is to a list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Standing {
    /// The reader's highest role through a verified address, if any.
    pub role: Option<MemberRole>,
    /// Whether the reader is a server owner with a verified address.
    pub server_owner: bool,
}

impl Standing {
    /// Whether the reader may open the list's administration.
    #[must_use]
    pub const fn administers(&self) -> bool {
        self.server_owner || matches!(self.role, Some(MemberRole::Owner))
    }
}

/// A list as the browser's create form describes it.
#[derive(Debug, Clone)]
pub struct BrowserNewList {
    pub list_id: ListId,
    pub display_name: String,
    pub style: String,
    /// The first owner's mailbox; created as an address when unknown.
    pub owner: String,
    pub advertised: bool,
    pub description: String,
}

const ROW: &str = "SELECT l.list_id, substr(l.display_name,1,512) AS display_name, substr(l.description,1,1024) AS description, l.advertised FROM mailing_lists l WHERE ";
const MATCHES: &str = "(l.list_id LIKE $1 ESCAPE '!' OR lower(l.display_name) LIKE $1 ESCAPE '!' OR lower(l.description) LIKE $1 ESCAPE '!') AND ($2='' OR l.mail_host=$2) ORDER BY l.list_id LIMIT 21 OFFSET $3";
const ROLE_ON: &str = "EXISTS (SELECT 1 FROM members m JOIN addresses a ON a.id=m.address_id WHERE m.list_id=l.list_id AND a.user_id=$5 AND a.verified_on IS NOT NULL AND (m.subscription_mode='as_address' OR m.user_id=$5) AND m.role<>'nonmember')";
const SERVER_OWNER: &str = "SELECT COUNT(*) FROM users u JOIN addresses a ON a.user_id=u.id WHERE u.id=$1 AND u.is_server_owner=1 AND a.verified_on IS NOT NULL";

fn escape_like(query: &str) -> String {
    format!(
        "%{}%",
        query
            .to_lowercase()
            .replace('!', "!!")
            .replace('%', "!%")
            .replace('_', "!_")
    )
}

const fn rank(role: MemberRole) -> u8 {
    match role {
        MemberRole::Owner => 3,
        MemberRole::Moderator => 2,
        MemberRole::Member => 1,
        MemberRole::Nonmember => 0,
    }
}

impl Database {
    /// Whether `user` is a server owner with a verified address.
    async fn verified_server_owner(&self, user: UserId) -> Result<bool> {
        let count: i64 = sqlx::query_scalar(SERVER_OWNER)
            .bind(user.to_string())
            .fetch_one(self.pool())
            .await
            .map_err(db_error)?;
        Ok(count > 0)
    }

    /// The reader's highest role on each list they belong to through a
    /// verified address, bounded.
    async fn reader_roles(&self, user: UserId) -> Result<Vec<(String, MemberRole)>> {
        let rows = sqlx::query("SELECT m.list_id, m.role FROM members m JOIN addresses a ON a.id=m.address_id WHERE a.user_id=$1 AND a.verified_on IS NOT NULL AND (m.subscription_mode='as_address' OR m.user_id=$1) AND m.role<>'nonmember' ORDER BY m.list_id LIMIT 5000")
            .bind(user.to_string())
            .fetch_all(self.pool())
            .await
            .map_err(db_error)?;
        let mut roles: Vec<(String, MemberRole)> = Vec::new();
        for row in rows {
            let list: String = row.try_get("list_id").map_err(db_error)?;
            let role: MemberRole = row
                .try_get::<String, _>("role")
                .map_err(db_error)?
                .parse()?;
            match roles.iter_mut().find(|(id, _)| *id == list) {
                Some(slot) if rank(role) > rank(slot.1) => slot.1 = role,
                Some(_) => {}
                None => roles.push((list, role)),
            }
        }
        Ok(roles)
    }

    /// A page of the directory (21 rows at most, the last one only signalling
    /// a next page). A visitor sees advertised lists; a signed-in reader also
    /// sees, with `show_all`, every list they have a role on, or every list
    /// when they are a server owner.
    /// # Errors
    /// Rejects an invalid offset and database failures.
    pub async fn browser_directory(
        &self,
        reader: Option<UserId>,
        filter: &DirectoryFilter<'_>,
    ) -> Result<Vec<DirectoryRow>> {
        crate::web_admin::valid_offset(filter.offset)?;
        let pattern = escape_like(filter.query.trim());
        let domain = filter.domain.unwrap_or_default().trim().to_lowercase();
        let (rows, roles) = match reader {
            Some(user) if filter.show_all => {
                let server_owner = self.verified_server_owner(user).await?;
                let sql = format!("{ROW}(l.advertised=1 OR $4<>0 OR {ROLE_ON}) AND {MATCHES}");
                let rows = sqlx::query(&sql)
                    .bind(&pattern)
                    .bind(&domain)
                    .bind(filter.offset)
                    .bind(i64::from(server_owner))
                    .bind(user.to_string())
                    .fetch_all(self.pool())
                    .await
                    .map_err(db_error)?;
                (rows, self.reader_roles(user).await?)
            }
            Some(user) => {
                let rows = sqlx::query(&format!("{ROW}l.advertised=1 AND {MATCHES}"))
                    .bind(&pattern)
                    .bind(&domain)
                    .bind(filter.offset)
                    .fetch_all(self.pool())
                    .await
                    .map_err(db_error)?;
                (rows, self.reader_roles(user).await?)
            }
            None => {
                let rows = sqlx::query(&format!("{ROW}l.advertised=1 AND {MATCHES}"))
                    .bind(&pattern)
                    .bind(&domain)
                    .bind(filter.offset)
                    .fetch_all(self.pool())
                    .await
                    .map_err(db_error)?;
                (rows, Vec::new())
            }
        };
        rows.iter()
            .map(|row| {
                let id: String = row.try_get("list_id").map_err(db_error)?;
                let role = roles
                    .iter()
                    .find(|(list, _)| *list == id)
                    .map(|(_, role)| *role);
                Ok(DirectoryRow {
                    name: row.try_get("display_name").map_err(db_error)?,
                    description: row.try_get("description").map_err(db_error)?,
                    advertised: row.try_get::<i64, _>("advertised").map_err(db_error)? != 0,
                    role,
                    id,
                })
            })
            .collect()
    }

    /// The reader's standing on one list.
    /// # Errors
    /// Database failures.
    pub async fn browser_standing(
        &self,
        reader: Option<UserId>,
        list: &ListId,
    ) -> Result<Standing> {
        let Some(user) = reader else {
            return Ok(Standing::default());
        };
        let role = self
            .reader_roles(user)
            .await?
            .into_iter()
            .find(|(id, _)| id == list.as_str())
            .map(|(_, role)| role);
        Ok(Standing {
            role,
            server_owner: self.verified_server_owner(user).await?,
        })
    }

    /// The list behind a summary page, with the reader's standing. An
    /// unadvertised list is a page only for a reader with a role on it or a
    /// server owner; to anyone else it does not exist.
    /// # Errors
    /// `NotFound` for a missing or hidden list; database failures.
    pub async fn browser_list_summary(
        &self,
        reader: Option<UserId>,
        list: &ListId,
    ) -> Result<(MailingList, Standing)> {
        let stored = self.lists().get(list).await?;
        let standing = self.browser_standing(reader, list).await?;
        if !stored.advertised && standing.role.is_none() && !standing.server_owner {
            return Err(Error::NotFound("list".into()));
        }
        Ok((stored, standing))
    }

    /// The domains this reader may create lists on, under live authority:
    /// every domain for a server owner, otherwise the domains they own.
    /// Empty when they may create none.
    /// # Errors
    /// Rejects a stale session and database failures.
    pub async fn browser_creatable_domains(&self, session: &WebSession) -> Result<Vec<Domain>> {
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_user_tx(&mut tx, session).await?;
        let server_owner: i64 = sqlx::query_scalar(SERVER_OWNER)
            .bind(user.to_string())
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
        let rows = if server_owner > 0 {
            sqlx::query("SELECT id,mail_host,description,alias_domain,created_at FROM domains ORDER BY mail_host")
                .fetch_all(&mut *tx)
                .await
        } else {
            sqlx::query("SELECT d.id,d.mail_host,d.description,d.alias_domain,d.created_at FROM domains d JOIN domain_owners o ON o.domain_id=d.id WHERE o.user_id=$1 AND EXISTS (SELECT 1 FROM addresses a WHERE a.user_id=$1 AND a.verified_on IS NOT NULL) ORDER BY d.mail_host")
                .bind(user.to_string())
                .fetch_all(&mut *tx)
                .await
        }
        .map_err(db_error)?;
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)?;
        rows.iter().map(crate::domain_from_row).collect()
    }

    /// Creates a list from the browser form in one transaction: the domain
    /// must exist (`NotFound`) and the reader must own it or the server
    /// (`Forbidden`); the id must be free (`Conflict`); the list row, its
    /// `advertised` and `description` through the ordinary settings
    /// validator, its first owner (the address created when unknown) and the
    /// `list.create`, `list.update` and `member.create` events commit
    /// together.
    /// # Errors
    /// As above, plus `Validation` for a bad owner mailbox and database or
    /// audit failures.
    pub async fn browser_create_list(
        &self,
        session: &WebSession,
        new: BrowserNewList,
    ) -> Result<MailingList> {
        let owner = Address::new(new.owner.trim(), String::new())?;
        let mut tx = self.browser_write_tx().await?;
        let user = Self::browser_user_tx(&mut tx, session).await?;
        let domain_exists: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM domains WHERE mail_host=$1")
                .bind(new.list_id.mail_host())
                .fetch_one(&mut *tx)
                .await
                .map_err(db_error)?;
        if domain_exists == 0 {
            return Err(Error::NotFound(new.list_id.mail_host().to_owned()));
        }
        let allowed: i64 = sqlx::query_scalar(&format!("SELECT ({SERVER_OWNER}) + (SELECT COUNT(*) FROM domain_owners o JOIN domains d ON d.id=o.domain_id WHERE o.user_id=$1 AND d.mail_host=$2 AND EXISTS (SELECT 1 FROM addresses a WHERE a.user_id=$1 AND a.verified_on IS NOT NULL))"))
            .bind(user.to_string())
            .bind(new.list_id.mail_host())
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
        if allowed == 0 {
            return Err(Error::Forbidden("browser domain owner authority".into()));
        }
        let taken: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM mailing_lists WHERE list_id=$1")
            .bind(new.list_id.as_str())
            .fetch_one(&mut *tx)
            .await
            .map_err(db_error)?;
        if taken > 0 {
            return Err(Error::Conflict(new.list_id.to_string()));
        }
        let context = AuditContext::new(Some(user), None, None);
        let list = ListRepo::create_tx(
            &mut tx,
            NewList {
                list_id: new.list_id,
                display_name: new.display_name,
                style: new.style,
            },
            &context,
        )
        .await?;
        let list = ListRepo::update_tx(
            &mut tx,
            &list.id,
            &serde_json::json!({"advertised": new.advertised, "description": new.description}),
            &context,
        )
        .await?;
        let existing: Option<(String, Option<String>)> =
            sqlx::query_as("SELECT id,user_id FROM addresses WHERE email=$1")
                .bind(&owner.email)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_error)?;
        let (address_id, owner_user) = if let Some(found) = existing {
            found
        } else {
            sqlx::query("INSERT INTO addresses(id,email,original_email,display_name,registered_on) VALUES($1,$2,$3,'',$4)")
                .bind(owner.id.to_string()).bind(&owner.email).bind(&owner.original_email).bind(owner.registered_on.to_rfc3339())
                .execute(&mut *tx).await.map_err(db_error)?;
            (owner.id.to_string(), None)
        };
        let member = MemberId::new();
        let preferences = PreferencesId::new();
        sqlx::query("INSERT INTO preferences(id) VALUES($1)")
            .bind(preferences.to_string())
            .execute(&mut *tx)
            .await
            .map_err(db_error)?;
        sqlx::query("INSERT INTO members(id,list_id,role,address_id,user_id,subscription_mode,display_name,preferences_id,created_at) VALUES($1,$2,'owner',$3,$4,'as_address','',$5,$6)")
            .bind(member.to_string()).bind(list.id.as_str()).bind(&address_id).bind(owner_user)
            .bind(preferences.to_string()).bind(chrono::Utc::now().to_rfc3339())
            .execute(&mut *tx).await.map_err(db_error)?;
        Self::record_tx_with_context(
            &mut tx,
            &context,
            "member.create",
            "member",
            &member.to_string(),
            serde_json::json!({"list_id": list.id, "role": "owner", "verified": false, "source": "browser-create"}),
        )
        .await?;
        Self::browser_user_tx(&mut tx, session).await?;
        tx.commit().await.map_err(db_error)?;
        Ok(list)
    }
}
