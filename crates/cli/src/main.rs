#![forbid(unsafe_code)]

mod aliases;
mod archive;
mod bounce;
mod digests;
mod doctor;
mod errors;
mod import21;
mod import3;
mod nntp;
mod notify;
mod plugins;
mod queue;
mod requests;
mod status;
mod tasks;
mod tls;
mod webhooks;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use listmngr_core::{Address, Config, ListId, MemberRole, SubscriptionMode, TokenId, UserId};
use listmngr_db::{Database, NewList, NewMember, NewUser};
use std::path::{Path, PathBuf};

#[derive(Debug, Parser)]
#[command(name = "listmngr", version, about = "Mailing-list manager")]
struct Cli {
    #[arg(long, env = "LISTMNGR_CONFIG")]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Debug, Subcommand)]
enum Command {
    /// The archive's search index: rebuild it from every archived post.
    Archive {
        #[command(subcommand)]
        command: archive::Command,
    },
    /// Explicit bounce warning/removal maintenance; does not start a scheduler.
    Bounce {
        #[command(subcommand)]
        command: bounce::Command,
    },
    /// Generate a new immutable Postfix map generation; never reloads the MTA.
    Aliases {
        #[command(subcommand)]
        command: aliases::Command,
    },
    Version,
    Conf {
        #[arg(long)]
        key: Option<String>,
    },
    Info,
    Status,
    /// Read-only DB/schema, relay and mail-domain DNS diagnostics as JSON.
    Doctor(doctor::Options),
    Migrate,
    Serve,
    Domains {
        #[command(subcommand)]
        command: DomainCommand,
    },
    Lists {
        #[command(subcommand)]
        command: ListCommand,
    },
    Members {
        #[command(subcommand)]
        command: MemberCommand,
    },
    User {
        #[command(subcommand)]
        command: UserCommand,
    },
    Token {
        #[command(subcommand)]
        command: TokenCommand,
    },
    /// Publish collected digests or advance the volume.
    Digests {
        #[command(subcommand)]
        command: digests::Command,
    },
    /// Durable queue operations; does not start delivery workers.
    Queue {
        #[command(subcommand)]
        command: queue::Command,
    },
    /// Subscription requests waiting for a moderator.
    Requests {
        #[command(subcommand)]
        command: requests::Command,
    },
    /// The periodic task sweep, run once by hand.
    Tasks {
        #[command(subcommand)]
        command: tasks::Command,
    },
    /// Remind owners and moderators of held messages and requests.
    Notify(notify::Options),
    /// The news gateway: poll the newsgroups of gatewayed lists once.
    Nntp {
        #[command(subcommand)]
        command: nntp::Command,
    },
    /// What this build's plugins add: rules, links, handlers, pipelines,
    /// archivers.
    Plugins,
    /// The site's webhooks: where its audit events are posted.
    Webhooks {
        #[command(subcommand)]
        command: webhooks::Command,
    },
    /// Import a Mailman 3 site over its REST API: its domains, lists and
    /// their settings, rosters with each member's own preferences, bans
    /// and header matches; and, with `--hyperkitty`, the votes, tags,
    /// categories and favourites of its archive. What is here already is
    /// left alone.
    Import3(import3::Options),
    /// Import a Mailman 2.1 list's `config.pck` into an existing list:
    /// its settings, bans, header filter rules, decorations and rosters.
    Import21 {
        /// The list to import into, e.g. `dev.example.invalid`.
        list_id: String,
        /// The 2.1 list's `lists/<name>/config.pck`.
        path: std::path::PathBuf,
        /// Print the plan as JSON and change nothing.
        #[arg(long)]
        dry_run: bool,
    },
}
#[derive(Debug, Subcommand)]
enum DomainCommand {
    Add {
        host: String,
        #[arg(long, default_value = "")]
        description: String,
    },
    Rm {
        host: String,
    },
    Ls,
}
#[derive(Debug, Subcommand)]
enum ListCommand {
    Create {
        list_id: ListId,
        #[arg(long)]
        display_name: String,
        #[arg(long, default_value = "legacy-default")]
        style: String,
    },
    Remove {
        list_id: ListId,
    },
    Ls,
}
#[derive(Debug, Subcommand)]
enum MemberCommand {
    Add(MemberArgs),
    Del {
        list_id: ListId,
        email: String,
    },
    Ls {
        list_id: ListId,
        #[arg(long, default_value = "member")]
        role: MemberRole,
    },
    Find {
        email: String,
    },
    Sync {
        list_id: ListId,
        file: PathBuf,
        #[arg(long, default_value = "member")]
        role: MemberRole,
        #[arg(long, default_value = "as_address")]
        mode: SubscriptionMode,
        #[arg(long)]
        dry_run: bool,
    },
}
#[derive(Debug, Args)]
struct MemberArgs {
    list_id: ListId,
    email: String,
    #[arg(long, default_value = "member")]
    role: MemberRole,
    #[arg(long, default_value = "as_address")]
    mode: SubscriptionMode,
    #[arg(long, default_value = "")]
    display_name: String,
}
#[derive(Debug, Subcommand)]
enum UserCommand {
    Create {
        email: String,
        #[arg(long)]
        display_name: String,
        #[command(flatten)]
        password: PasswordInput,
        #[arg(long)]
        server_owner: bool,
    },
    Passwd {
        id: UserId,
        #[command(flatten)]
        password: PasswordInput,
    },
    /// Everything stored about an account, as JSON; never a secret.
    Export { id: UserId },
    /// Erase an account: memberships, addresses, tokens, credential,
    /// sessions and domain ownerships go with it, audited.
    Erase { id: UserId },
}
#[derive(Debug, Args)]
struct PasswordInput {
    #[arg(long)]
    #[cfg_attr(unix, arg(conflicts_with = "password_fd"))]
    password_stdin: bool,
    #[cfg(unix)]
    #[arg(long, value_name = "FD", conflicts_with = "password_stdin")]
    password_fd: Option<u32>,
}

fn read_password(input: &PasswordInput) -> Result<String> {
    use std::io::Read;
    let reader: Option<Box<dyn Read>> = if input.password_stdin {
        Some(Box::new(std::io::stdin()))
    } else {
        #[cfg(unix)]
        {
            input
                .password_fd
                .map(|fd| {
                    std::fs::File::open(format!("/dev/fd/{fd}"))
                        .map(|file| Box::new(file) as Box<dyn Read>)
                })
                .transpose()?
        }
        #[cfg(not(unix))]
        {
            None
        }
    };
    let mut value = if let Some(reader) = reader {
        let mut value = String::new();
        // One extra byte beyond a maximum password plus optional CRLF detects
        // oversize input without accepting a truncated prefix or waiting for EOF.
        reader.take(1027).read_to_string(&mut value)?;
        value
    } else {
        rpassword::prompt_password("Password: ")?
    };
    if value.ends_with('\n') {
        value.pop();
        if value.ends_with('\r') {
            value.pop();
        }
    }
    if value.len() > 1024 {
        bail!(listmngr_core::Error::Validation(
            "password exceeds 1024 bytes".into()
        ));
    }
    Ok(value)
}

#[derive(Debug, Subcommand)]
enum TokenCommand {
    Create {
        user_id: UserId,
        name: String,
        #[arg(long, value_delimiter = ',', default_value = "system:read")]
        scopes: Vec<String>,
    },
    Revoke {
        id: TokenId,
    },
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    // The command future carries whole list rows; keep it off the stack.
    if let Err(error) = Box::pin(run()).await {
        let error_id = uuid::Uuid::now_v7();
        let (code, category, message) = errors::classify(&error);
        tracing::error!(%error_id, category, "CLI command failed");
        eprintln!("error[{category}]: {message}; correlation={error_id}");
        std::process::exit(code.into());
    }
}

async fn run() -> Result<()> {
    // The build's plugins, before any registry is built from them.
    let _ = listmngr_plugins::install();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) if error.use_stderr() => {
            eprintln!("error[CLI-USAGE]: invalid command line");
            std::process::exit(2);
        }
        Err(error) => {
            let _ = error.print();
            std::process::exit(0);
        }
    };
    let config = Config::load(cli.config.as_deref())?;
    match cli.command {
        Command::Version => println!("listmngr {}", env!("CARGO_PKG_VERSION")),
        Command::Conf { key } => print_config(&config, key.as_deref())?,
        Command::Info => println!(
            "listmngr {}\ndatabase: {}\nweb: {}\napi: {}",
            env!("CARGO_PKG_VERSION"),
            config.database_backend(),
            config.web.listen,
            config.api.listen
        ),
        Command::Status => status::check(&config).await?,
        Command::Plugins => plugins::run(),
        Command::Doctor(options) => doctor::run(&config, options).await?,
        command => Box::pin(run_database(command, config)).await?,
    }
    Ok(())
}
fn print_config(config: &Config, key: Option<&str>) -> Result<()> {
    let value = config.redacted_json();
    if let Some(key) = key {
        let mut selected = &value;
        for part in key.split('.') {
            selected = selected.get(part).ok_or_else(|| {
                listmngr_core::Error::Validation("unknown configuration key".into())
            })?;
        }
        match selected {
            serde_json::Value::String(v) => println!("{v}"),
            other => println!("{other}"),
        }
    } else {
        println!("{}", serde_json::to_string_pretty(&value)?);
    }
    Ok(())
}
/// Waits for `SIGTERM` (or Ctrl+C) and then signals graceful shutdown: axum
/// stops accepting new connections and the mail role (if any) stops
/// accepting new work and claiming new queue jobs.
async fn shutdown_signal(tx: tokio::sync::watch::Sender<bool>) {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        else {
            return;
        };
        signal.recv().await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {}
        () = terminate => {}
    }
    tracing::info!("shutdown signal received");
    let _ = tx.send(true);
}
async fn run_database(command: Command, config: Config) -> Result<()> {
    let db = Database::connect_with_security(
        &config.database.url,
        config.database.max_connections,
        &config.security,
    )
    .await?
    .with_default_language(&config.site.default_language)
    .with_base_url(&config.site.base_url)
    .with_webhooks(config.webhooks.signing_key(), config.webhooks.allow_http)
    .with_mail_archive_address(&config.archive.archivers.mail_archive_address)
    .with_site(&config.site.name, &config.site.site_owner)
    .with_bounce_probes(
        config.mailman.bounce_probes,
        config.mailman.bounce_probe_lifetime_secs,
        &config.mta.verp_format,
    );
    match command {
        Command::Migrate => {
            db.migrate().await.context(errors::MigrationFailure)?;
            println!("migrations applied");
        }

        Command::Serve => serve_database(db, config).await?,
        Command::Domains { command } => domains(&db, command).await?,
        Command::Lists { command } => lists(&db, &config, command).await?,
        Command::Members { command } => members(&db, command).await?,
        Command::User { command } => users(&db, command).await?,
        Command::Token { command } => tokens(&db, command).await?,
        Command::Bounce { command } => bounce::run(&db, command).await?,
        Command::Queue { command } => queue::run(&db, command).await?,
        Command::Requests { command } => requests::run(&db, command).await?,
        Command::Tasks { command } => tasks::run(&db, &config, command).await?,
        Command::Notify(options) => notify::run(&db, options).await?,
        Command::Digests { command } => digests::run(&db, command).await?,
        Command::Aliases { command } => aliases::run(&db, &config, command).await?,
        Command::Archive { command } => archive::run(&db, &config, command).await?,
        Command::Nntp { command } => nntp::run(&db, &config, command).await?,
        Command::Webhooks { command } => webhooks::run(&db, command).await?,
        Command::Import3(options) => import3::run(&db, options).await?,
        Command::Import21 {
            list_id,
            path,
            dry_run,
        } => import21::run(&db, &list_id, &path, dry_run).await?,
        Command::Version
        | Command::Conf { .. }
        | Command::Info
        | Command::Status
        | Command::Plugins
        | Command::Doctor(_) => {
            bail!("command does not use database")
        }
    }
    Ok(())
}
/// The mail role, opt-in (`mta.enabled`, fail-closed-validated at config
/// load): its listeners are bound here, synchronously, so a bind failure
/// aborts startup instead of dying silently in the background after the
/// HTTP server already reports healthy.
async fn spawn_mail_role(
    db: &Database,
    config: &Config,
    shutdown: &tokio::sync::watch::Receiver<bool>,
) -> Result<Option<tokio::task::JoinHandle<std::io::Result<()>>>> {
    if !config.mta.enabled {
        return Ok(None);
    }
    let role = listmngr_runners::MailRoleConfig::from_core(config)?;
    let lmtp_listener = listmngr_runners::bind_lmtp(&role).await?;
    let smtp_listener = listmngr_runners::bind_inbound_smtp(&role).await?;
    let (mail_db, mail_config, mail_shutdown) = (db.clone(), config.clone(), shutdown.clone());
    Ok(Some(tokio::spawn(async move {
        listmngr_runners::serve_mail_role(
            mail_db,
            mail_config,
            role,
            lmtp_listener,
            smtp_listener,
            mail_shutdown,
        )
        .await
    })))
}

/// Webhooks post from wherever `serve` runs, with or without the mail
/// role: the events come from the API and the web as much as from mail.
fn spawn_webhook_runner(
    db: &Database,
    config: &Config,
    shutdown: &tokio::sync::watch::Receiver<bool>,
) -> Option<tokio::task::JoinHandle<()>> {
    if !config.webhooks.enabled {
        return None;
    }
    let (hook_db, hook_config, hook_shutdown) =
        (db.clone(), config.webhooks.clone(), shutdown.clone());
    Some(tokio::spawn(async move {
        listmngr_runners::webhooks::run(hook_db, hook_config, hook_shutdown).await;
    }))
}

async fn serve_database(db: Database, config: Config) -> Result<()> {
    db.migrate().await.context(errors::MigrationFailure)?;
    let address: std::net::SocketAddr = config
        .web
        .listen
        .parse()
        .map_err(|_| listmngr_core::Error::Validation("invalid web.listen".into()))?;
    let listener = tokio::net::TcpListener::bind(address).await?;
    tracing::info!(%address,"HTTP server listening");
    aliases::publish_at_startup(&db, &config).await?;
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let mail_role = spawn_mail_role(&db, &config, &shutdown_rx).await?;
    let webhook_role = spawn_webhook_runner(&db, &config, &shutdown_rx);
    let tls_role = tls::prepare(&config.web.tls).await?;
    let router = listmngr_api::router(db, config);
    let tls_role = tls_role.map(|(tls_listener, acceptor)| {
        tokio::spawn(tls::serve(
            tls_listener,
            acceptor,
            router.clone(),
            shutdown_rx.clone(),
        ))
    });
    let http = std::future::IntoFuture::into_future(
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown_signal(shutdown_tx.clone())),
    );
    tokio::pin!(http);
    if let Some(mut task) = mail_role {
        tokio::select! {
            result = &mut http => {
                let _ = shutdown_tx.send(true);
                task.await.context("mail role task panicked")??;
                result?;
            }
            result = &mut task => {
                let _ = shutdown_tx.send(true);
                result.context("mail role task panicked")??;
            }
        }
    } else {
        http.await?;
    }
    let _ = shutdown_tx.send(true);
    if let Some(task) = webhook_role {
        task.await.context("webhook runner panicked")?;
    }
    if let Some(task) = tls_role {
        task.await.context("TLS listener panicked")??;
    }
    Ok(())
}

async fn domains(db: &Database, command: DomainCommand) -> Result<()> {
    match command {
        DomainCommand::Add { host, description } => println!(
            "{}",
            serde_json::to_string_pretty(&db.domains().create(&host, &description, None).await?)?
        ),
        DomainCommand::Rm { host } => {
            db.domains().delete(&host).await?;
            println!("removed {host}");
        }
        DomainCommand::Ls => {
            for d in db.domains().list().await? {
                println!("{}", d.mail_host);
            }
        }
    }
    Ok(())
}
async fn lists(db: &Database, config: &Config, command: ListCommand) -> Result<()> {
    match command {
        ListCommand::Create {
            list_id,
            display_name,
            style,
        } => {
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &db.lists()
                        .create(NewList {
                            list_id,
                            display_name,
                            style
                        })
                        .await?
                )?
            );
            aliases::refresh_after_list_change(db, config).await;
        }
        ListCommand::Remove { list_id } => {
            db.lists().delete(&list_id).await?;
            println!("removed {list_id}");
            aliases::refresh_after_list_change(db, config).await;
        }
        ListCommand::Ls => {
            for l in db.lists().list(None).await? {
                println!("{}", l.id);
            }
        }
    }
    Ok(())
}
async fn add_member(db: &Database, args: MemberArgs) -> Result<()> {
    let member = db
        .members()
        .create(NewMember {
            list_id: args.list_id,
            email: args.email,
            role: args.role,
            subscription_mode: args.mode,
            display_name: args.display_name,
        })
        .await?;
    println!("{}", serde_json::to_string_pretty(&member)?);
    Ok(())
}
async fn members(db: &Database, command: MemberCommand) -> Result<()> {
    match command {
        MemberCommand::Add(args) => add_member(db, args).await?,
        MemberCommand::Del { list_id, email } => {
            let email = Address::new(&email, String::new())?.email;
            let member = db
                .members()
                .find(&email)
                .await?
                .into_iter()
                .find(|m| m.list_id == list_id)
                .ok_or_else(|| listmngr_core::Error::NotFound("membership".into()))?;
            db.members().delete(member.id).await?;
            println!("removed {}", member.id);
        }
        MemberCommand::Ls { list_id, role } => {
            for m in db.members().roster(&list_id, role).await? {
                let address = db.addresses().get_by_id(m.address_id).await?;
                println!("{} {} {}", m.id, address.email, m.display_name);
            }
        }
        MemberCommand::Find { email } => {
            let email = Address::new(&email, String::new())?.email;
            for m in db.members().find(&email).await? {
                println!("{} {} {}", m.id, m.list_id, m.role);
            }
        }
        MemberCommand::Sync {
            list_id,
            file,
            role,
            mode,
            dry_run,
        } => sync_members(db, &list_id, &file, role, mode, dry_run).await?,
    }
    Ok(())
}
async fn sync_members(
    db: &Database,
    list_id: &ListId,
    file: &Path,
    role: MemberRole,
    mode: SubscriptionMode,
    dry_run: bool,
) -> Result<()> {
    let content = std::fs::read_to_string(file)?;
    let mut desired = Vec::new();
    let mut unique = std::collections::HashSet::new();
    for (index, row) in content.lines().enumerate() {
        let row = row.trim();
        if row.is_empty() || row.starts_with('#') {
            continue;
        }
        let address = Address::new(row, String::new())
            .with_context(|| format!("invalid member row {}", index + 1))?;
        if !unique.insert(address.email.clone()) {
            bail!(listmngr_core::Error::Validation(format!(
                "duplicate sync member row {}",
                index + 1
            )));
        }
        desired.push(address.email);
    }

    if dry_run {
        db.lists().get(list_id).await?;
        let mut current = std::collections::HashSet::new();
        for member in db.members().roster(list_id, role).await? {
            current.insert(db.addresses().get_by_id(member.address_id).await?.email);
        }
        let desired_set = desired
            .iter()
            .cloned()
            .collect::<std::collections::HashSet<_>>();
        let added = desired_set.difference(&current).count();
        let removed = current.difference(&desired_set).count();
        let retained = desired_set.intersection(&current).count();
        println!("dry-run role={role} add={added} remove={removed} retain={retained}");
        return Ok(());
    }

    let result = db
        .members()
        .mass_for_role(list_id, "sync", &desired, role, mode)
        .await?;
    println!(
        "synced role={role} added={} removed={} retained={}",
        result.added, result.removed, result.retained
    );
    Ok(())
}
async fn users(db: &Database, command: UserCommand) -> Result<()> {
    match command {
        UserCommand::Create {
            email,
            display_name,
            password,
            server_owner,
        } => println!(
            "{}",
            serde_json::to_string_pretty(
                &db.users()
                    .create(NewUser {
                        display_name,
                        email,
                        password: read_password(&password)?,
                        server_owner
                    })
                    .await?
            )?
        ),
        UserCommand::Passwd { id, password } => {
            let password = read_password(&password)?;
            db.users().set_password(id, &password).await?;
            println!("password changed");
        }
        UserCommand::Export { id } => {
            println!(
                "{}",
                serde_json::to_string_pretty(&db.export_user(id).await?)?
            );
        }
        UserCommand::Erase { id } => {
            let deleted = db
                .erase_user(id, &listmngr_db::AuditContext::system())
                .await?;
            println!(
                "erased {id}: {} memberships, {} addresses, {} tokens",
                deleted.memberships, deleted.addresses, deleted.tokens
            );
        }
    }
    Ok(())
}
async fn tokens(db: &Database, command: TokenCommand) -> Result<()> {
    match command {
        TokenCommand::Create {
            user_id,
            name,
            scopes,
        } => {
            let refs = scopes.iter().map(String::as_str).collect::<Vec<_>>();
            let issued = db.tokens().create(user_id, &name, &refs, None).await?;
            println!("{}", issued.token);
        }
        TokenCommand::Revoke { id } => {
            db.tokens().revoke(id).await?;
            println!("revoked {id}");
        }
    }
    Ok(())
}
