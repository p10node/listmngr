//! The address a signed-in reader posts from on the web: one of their
//! verified addresses that holds a membership on the list, the primary
//! first, and not banned there.
use crate::{Database, web_sessions::WebSession};
use listmngr_core::{Error, ListId, MemberRole, Result, UserId};

/// Who a web post is from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Poster {
    pub user_id: UserId,
    /// The verified, subscribed address.
    pub email: String,
    /// The account's display name, possibly empty.
    pub display_name: String,
}

impl Database {
    /// The reader's posting address on `list`.
    /// # Errors
    /// `Authentication` for a stale or anonymous session; `Forbidden` when
    /// no verified address of theirs is subscribed (as a member, owner or
    /// moderator) or when that address is banned from the list.
    pub async fn browser_poster(&self, session: &WebSession, list: &ListId) -> Result<Poster> {
        let addresses = self.browser_addresses(session).await?;
        let live = self
            .web_session(&session.token, chrono::Utc::now().timestamp_millis())
            .await?;
        let user_id = live.user_id.ok_or(Error::Authentication)?;
        let mut chosen = None;
        for address in addresses.iter().filter(|address| address.verified) {
            let subscribed = self
                .members()
                .find(&address.email)
                .await?
                .into_iter()
                .any(|m| {
                    m.list_id == *list
                        && matches!(
                            m.role,
                            MemberRole::Member | MemberRole::Owner | MemberRole::Moderator
                        )
                });
            if subscribed {
                chosen = Some(address.email.clone());
                break;
            }
        }
        let email = chosen.ok_or_else(|| {
            Error::Forbidden("posting from the web needs a verified subscribed address".into())
        })?;
        if self.bans().is_banned(list, &email).await? {
            return Err(Error::Forbidden(
                "the address is banned from the list".into(),
            ));
        }
        let display_name = self.users().get(user_id).await?.display_name;
        Ok(Poster {
            user_id,
            email,
            display_name,
        })
    }
}
