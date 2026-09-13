#![forbid(unsafe_code)]

//! The opt-in mail role: an LMTP acceptor plus `in`/`out` queue processors.
//!
//! Disabled unless `mta.enabled = true` (validated fail-closed by
//! `listmngr_core::Config::load`: enabling it requires explicit
//! `plaintext_trusted_relay` or verified `required` STARTTLS). Uses the
//! hardened `listmngr_mail` LMTP/SMTP protocol and `listmngr_db`
//! queue/moderation primitives; see `docs/FEATURE_PARITY.md` for their
//! individual acceptance evidence. This crate only wires them into a running
//! process — it does not reimplement or relax any of that hardening.

#[cfg(test)]
mod lifecycle_tests;

mod archive;
pub mod bounce_maintenance;
mod bounces;
pub mod delivery_policy;
pub mod digests;
mod heartbeat;
mod inbound;
mod outbound;
mod policy_facts;
mod processor;
mod visible_recipients;

pub use bounces::run as run_bounce_processor;
pub use inbound::{COMMAND_SUFFIXES, InboundHandler};
pub use outbound::run as run_out_processor;
pub use outbound::{PrepareError, prepare_individual};
pub use policy_facts::resolve_recipients;
pub use processor::run as run_in_processor;

use listmngr_core::Config;
use listmngr_db::Database;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::{Semaphore, watch};

/// Runtime configuration for the mail role, derived from validated
/// `listmngr_core::Config`.
#[derive(Debug, Clone)]
pub struct MailRoleConfig {
    pub bounce_maintenance_enabled: bool,
    pub bounce_maintenance_interval: Duration,
    pub bounce_maintenance_batch_size: u32,
    pub dkim: listmngr_mail::dkim::SigningKeys,
    pub local_hostname: String,
    pub lmtp_listen: SocketAddr,
    pub smtp_relay: SocketAddr,
    /// Separate SMTP sessions per recipient for non-null list envelopes only.
    pub smtp_single_recipient: bool,
    /// `[mta] max_recipients_per_transaction`: recipients per SMTP transaction
    /// when a delivery is shared (Mailman's `max_recipients`).
    pub max_recipients_per_transaction: usize,
    /// `[mta] retry_initial_secs` / `retry_max_secs` as a backoff policy.
    pub backoff: delivery_policy::Backoff,
    /// `[mta] authenticity_checks`: the SPF/DKIM/DMARC verifier, when on.
    pub authenticity: Option<Arc<listmngr_mail::authenticity::Verifier>>,
    /// `[mta] verp_format` and `verp_delimiter`, validated at load.
    pub verp_format: String,
    pub verp_delimiter: String,
    /// `[mta] verp_personalized_deliveries`.
    pub verp_personalized_deliveries: bool,
    /// `[mta] verp_delivery_interval`; zero never.
    pub verp_delivery_interval: u32,
    pub dsn_issuer: Option<listmngr_core::dsn_issuance::Issuer>,
    pub smtp_tls: listmngr_mail::smtp::TransportSecurity,
    pub max_recipients: usize,
    pub max_message_bytes: usize,
    pub command_timeout: Duration,
    pub max_concurrent_sessions: usize,
    pub in_max_attempts: i64,
    pub out_max_attempts: i64,
    /// Durable queue lease TTLs, in milliseconds. Lease renewal (see
    /// `crate::heartbeat`) is derived from these, never from an unrelated
    /// per-operation timeout such as `command_timeout`: overridable so tests
    /// can exercise expiry/renewal without real-time sleeps.
    pub in_lease_ms: i64,
    pub out_lease_ms: i64,
    /// Bound on how long shutdown waits for in-flight LMTP sessions to
    /// finish on their own before abandoning them. Overridable so tests can
    /// exercise a stalled-session shutdown without a real long wait.
    pub session_drain_timeout: Duration,
}

impl MailRoleConfig {
    /// # Errors
    /// Returns a validation error for an unparsable listen/relay address.
    pub fn from_core(config: &Config) -> Result<Self, listmngr_core::Error> {
        let lmtp_listen = config
            .mta
            .lmtp_listen
            .parse()
            .map_err(|_| listmngr_core::Error::Validation("invalid mta.lmtp_listen".into()))?;
        let smtp_relay = config
            .mta
            .smtp_relay
            .parse()
            .map_err(|_| listmngr_core::Error::Validation("invalid mta.smtp_relay".into()))?;
        Ok(Self {
            bounce_maintenance_enabled: config.mta.bounce_maintenance_enabled,
            bounce_maintenance_interval: Duration::from_secs(u64::from(
                config.mta.bounce_maintenance_interval_secs,
            )),
            bounce_maintenance_batch_size: config.mta.bounce_maintenance_batch_size,
            dkim: listmngr_mail::dkim::SigningKeys::load(&config.mta.dkim_signing)?,
            local_hostname: config.mta.local_hostname.clone(),
            lmtp_listen,
            smtp_relay,
            smtp_single_recipient: config.mta.smtp_single_recipient,
            max_recipients_per_transaction: config.mta.max_recipients_per_transaction as usize,
            backoff: delivery_policy::Backoff {
                initial_ms: i64::from(config.mta.retry_initial_secs) * 1000,
                max_ms: i64::from(config.mta.retry_max_secs) * 1000,
            },
            authenticity: if config.mta.authenticity_checks {
                Some(Arc::new(
                    listmngr_mail::authenticity::Verifier::system(&config.mta.local_hostname)
                        .map_err(|error| {
                            listmngr_core::Error::Validation(format!(
                                "mta.authenticity_checks: resolver unavailable: {error}"
                            ))
                        })?,
                ))
            } else {
                None
            },
            verp_format: config.mta.verp_format.clone(),
            verp_delimiter: config.mta.verp_delimiter.clone(),
            verp_personalized_deliveries: config.mta.verp_personalized_deliveries,
            verp_delivery_interval: config.mta.verp_delivery_interval,
            dsn_issuer: listmngr_core::dsn_issuance::Issuer::load(&config.mta)?,
            smtp_tls: listmngr_mail::smtp::TransportSecurity::from_mta(&config.mta)?,
            max_recipients: config.mta.max_recipients as usize,
            max_message_bytes: config.mta.max_message_bytes as usize,
            command_timeout: Duration::from_secs(u64::from(config.mta.command_timeout_secs)),
            max_concurrent_sessions: 50,
            in_max_attempts: 5,
            out_max_attempts: 8,
            in_lease_ms: 30_000,
            out_lease_ms: 20_000,
            session_drain_timeout: Duration::from_secs(5),
        })
    }

    fn handler(&self, db: Database) -> InboundHandler {
        InboundHandler {
            db,
            local_hostname: self.local_hostname.clone(),
            max_message_bytes: self.max_message_bytes,
            max_recipients: self.max_recipients,
            command_timeout: self.command_timeout,
            in_max_attempts: self.in_max_attempts,
            verp_delimiter: self.verp_delimiter.clone(),
        }
    }
}

/// Accept LMTP connections on `listener` with bounded concurrency until
/// `shutdown` is signalled. Each connection gets its own durable-intake
/// handler over a real socket (`listmngr_mail::lmtp::serve_session`).
async fn run_acceptor(
    listener: TcpListener,
    db: Database,
    role: MailRoleConfig,
    mut shutdown: watch::Receiver<bool>,
) -> std::io::Result<()> {
    let permits = Arc::new(Semaphore::new(role.max_concurrent_sessions));
    let mut sessions = tokio::task::JoinSet::new();
    loop {
        if *shutdown.borrow() {
            break;
        }
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            _ = shutdown.changed() => break,
            _ = sessions.join_next(), if !sessions.is_empty() => continue,
        };
        let (stream, peer) = accepted?;
        let Ok(permit) = Arc::clone(&permits).try_acquire_owned() else {
            tracing::warn!(%peer, "LMTP: connection rejected, session limit reached");
            drop(stream);
            continue;
        };
        let mut handler = role.handler(db.clone());
        sessions.spawn(async move {
            let _permit = permit;
            if let Err(error) = listmngr_mail::lmtp::serve_session(stream, &mut handler).await {
                tracing::debug!(%peer, %error, "LMTP: session ended with a transport error");
            }
        });
    }
    // No detached sessions: allow a bounded drain, then cancel and reap all.
    if tokio::time::timeout(role.session_drain_timeout, async {
        while sessions.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        sessions.abort_all();
        while sessions.join_next().await.is_some() {}
    }
    Ok(())
}

/// Run the acceptor and both queue processors on an already-bound LMTP
/// listener until `shutdown` is signalled.
///
/// Binding is the caller's responsibility (see [`bind_lmtp`]) so a bind
/// failure surfaces immediately as a startup error, not silently inside a
/// background task.
///
/// Shutdown drains owned work for a bounded interval, then cancels it.
/// # Errors
/// Returns an error if an enabled role fails or exits before shutdown.
pub async fn serve_mail_role(
    db: Database,
    config: Config,
    role: MailRoleConfig,
    listener: TcpListener,
    shutdown: watch::Receiver<bool>,
) -> std::io::Result<()> {
    let mut tasks = tokio::task::JoinSet::new();
    let drain = role.session_drain_timeout;
    if role.bounce_maintenance_enabled {
        let maintenance = bounce_maintenance::run(
            db.clone(),
            role.bounce_maintenance_interval,
            role.bounce_maintenance_batch_size,
            shutdown.clone(),
        );
        tasks.spawn(async move {
            maintenance.await;
            Ok(())
        });
    }
    let acceptor = run_acceptor(listener, db.clone(), role.clone(), shutdown.clone());
    tasks.spawn(acceptor);
    // The `in` processor's future is large (chain, pipeline and templated
    // notices all inline); keep it on the heap rather than in this frame.
    let site_owner = config.site.site_owner.clone();
    let inbound = Box::pin(processor::run(
        db.clone(),
        config,
        role.clone(),
        "in-0".into(),
        shutdown.clone(),
    ));
    tasks.spawn(async move {
        inbound.await;
        Ok(())
    });
    let bounces = bounces::run(
        db.clone(),
        role.clone(),
        site_owner,
        "bounces-0".into(),
        shutdown.clone(),
    );
    tasks.spawn(async move {
        Box::pin(bounces).await;
        Ok(())
    });
    let digest = digests::run(db.clone(), shutdown.clone());
    tasks.spawn(async move {
        digest.await;
        Ok(())
    });
    let archive = archive::run(db.clone(), shutdown.clone());
    tasks.spawn(async move {
        archive.await;
        Ok(())
    });
    let outbound = outbound::run(db, role, "out-0".into(), shutdown.clone());
    tasks.spawn(async move {
        Box::pin(outbound).await;
        Ok(())
    });
    supervise_tasks(tasks, shutdown, drain).await
}

async fn supervise_tasks(
    mut tasks: tokio::task::JoinSet<std::io::Result<()>>,
    mut shutdown: watch::Receiver<bool>,
    drain: Duration,
) -> std::io::Result<()> {
    let result = if *shutdown.borrow() {
        Ok(())
    } else {
        tokio::select! {
            _ = shutdown.changed() => Ok(()),
            result = tasks.join_next() => match result {
                Some(Ok(Err(error))) => Err(error),
                Some(Err(error)) => Err(std::io::Error::other(error)),
                _ if *shutdown.borrow() => Ok(()),
                _ => Err(std::io::Error::other("enabled mail role exited unexpectedly")),
            }
        }
    };
    if result.is_err() {
        tasks.abort_all();
    }
    let drained = tokio::time::timeout(drain, async {
        while let Some(result) = tasks.join_next().await {
            result.map_err(std::io::Error::other)??;
        }
        Ok::<(), std::io::Error>(())
    })
    .await;
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    result?;
    if let Ok(result) = drained {
        result?;
    }
    Ok(())
}

/// Bind the LMTP listen address. Split from [`serve_mail_role`] so a caller
/// can fail startup immediately on a bind error, rather than only noticing
/// once a background task exits.
/// # Errors
/// Returns an I/O error if the address cannot be bound.
pub async fn bind_lmtp(role: &MailRoleConfig) -> std::io::Result<TcpListener> {
    let listener = TcpListener::bind(role.lmtp_listen).await?;
    tracing::info!(address = %role.lmtp_listen, "LMTP listener bound");
    Ok(listener)
}
