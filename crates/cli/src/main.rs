#![forbid(unsafe_code)]

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use listmngr_core::{
    Address, Config, Error as CoreError, ListId, MemberRole, SubscriptionMode, TokenId, UserId,
};
use listmngr_db::{Database, NewList, NewMember, NewUser};
use std::io::Read;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

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
    Version,
    Conf {
        #[arg(long)]
        key: Option<String>,
    },
    Info,
    Status,
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
}

#[derive(Debug, Args)]
struct PasswordInput {
    #[arg(long, conflicts_with = "password_fd")]
    password_stdin: bool,
    #[arg(long, value_name = "FD", conflicts_with = "password_stdin")]
    password_fd: Option<u32>,
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
async fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(error) if error.use_stderr() => {
            eprintln!("error[CLI-USAGE]: invalid command line");
            return ExitCode::from(2);
        }
        Err(error) => {
            let _ = error.print();
            return ExitCode::SUCCESS;
        }
    };
    match execute(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => report_error(&error),
    }
}

async fn execute(cli: Cli) -> Result<()> {
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
        Command::Status => status(&config).await?,
        command => run_database(command, config).await?,
    }
    Ok(())
}

fn report_error(error: &anyhow::Error) -> ExitCode {
    let rendered = error.to_string();
    let (code, id, message) = if rendered == "status-unreachable" {
        (3, "CLI-STATUS-UNREACHABLE", "service is unreachable")
    } else if rendered == "status-unhealthy" {
        (4, "CLI-STATUS-UNHEALTHY", "service is unhealthy")
    } else if rendered == "status-not-ready" {
        (
            5,
            "CLI-STATUS-NOT-READY",
            "service is healthy but not ready",
        )
    } else if rendered.starts_with("migration failed") {
        (10, "CLI-MIGRATION", "database migration failed")
    } else if let Some(error) = error.downcast_ref::<CoreError>() {
        classify_core_error(error)
    } else if error.downcast_ref::<std::io::Error>().is_some() {
        (9, "CLI-IO", "input/output operation failed")
    } else {
        (1, "CLI-INTERNAL", "operation failed")
    };
    eprintln!("error[{id}]: {message}");
    ExitCode::from(code)
}

fn classify_core_error(error: &CoreError) -> (u8, &'static str, &'static str) {
    match error {
        CoreError::Config(_) | CoreError::InvalidListId(_) | CoreError::Validation(_) => {
            (2, "CLI-VALIDATION", "invalid input")
        }
        CoreError::Conflict(_) => (6, "CLI-CONFLICT", "operation conflicts with existing data"),
        CoreError::NotFound(_) => (7, "CLI-NOT-FOUND", "resource not found"),
        CoreError::Authentication | CoreError::Forbidden(_) => {
            (8, "CLI-AUTH", "authentication or authorization failed")
        }
        CoreError::RateLimited => (8, "CLI-RATE-LIMITED", "rate limit exceeded"),
        CoreError::Database(_) => (10, "CLI-DATABASE", "database operation failed"),
    }
}

async fn status(config: &Config) -> Result<()> {
    let address: SocketAddr = config.web.listen.parse().context("invalid web.listen")?;
    match http_status(address, "/healthz").await {
        Err(_) => bail!("status-unreachable"),
        Ok(code) if !(200..300).contains(&code) => bail!("status-unhealthy"),
        Ok(_) => {}
    }
    match http_status(address, "/readyz").await {
        Ok(code) if (200..300).contains(&code) => {
            println!("service: healthy and ready");
            Ok(())
        }
        Ok(_) => bail!("status-not-ready"),
        Err(_) => bail!("status-unreachable"),
    }
}

async fn http_status(address: SocketAddr, path: &str) -> std::io::Result<u16> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .map_err(std::io::Error::other)?;
    client
        .get(format!("http://{address}{path}"))
        .send()
        .await
        .map(|response| response.status().as_u16())
        .map_err(std::io::Error::other)
}
fn print_config(config: &Config, key: Option<&str>) -> Result<()> {
    let value = config.redacted_json();
    if let Some(key) = key {
        let mut selected = &value;
        for part in key.split('.') {
            selected = selected
                .get(part)
                .with_context(|| format!("unknown configuration key: {key}"))?;
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
async fn run_database(command: Command, config: Config) -> Result<()> {
    let db = Database::connect_with_security(
        &config.database.url,
        config.database.max_connections,
        &config.security,
    )
    .await?;
    match command {
        Command::Migrate => {
            db.migrate().await.context("migration failed")?;
            println!("migrations applied");
        }
        Command::Serve => {
            db.migrate().await.context("migration failed")?;
            let address: std::net::SocketAddr =
                config.web.listen.parse().context("invalid web.listen")?;
            let listener = tokio::net::TcpListener::bind(address).await?;
            tracing::info!(%address,"HTTP server listening");
            axum::serve(
                listener,
                listmngr_api::router(db, config, 600)
                    .into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await?;
        }
        Command::Domains { command } => domains(&db, command).await?,
        Command::Lists { command } => lists(&db, command).await?,
        Command::Members { command } => members(&db, command).await?,
        Command::User { command } => users(&db, command).await?,
        Command::Token { command } => tokens(&db, command).await?,
        Command::Version | Command::Conf { .. } | Command::Info | Command::Status => {
            bail!("command does not use database")
        }
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
async fn lists(db: &Database, command: ListCommand) -> Result<()> {
    match command {
        ListCommand::Create {
            list_id,
            display_name,
            style,
        } => println!(
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
        ),
        ListCommand::Remove { list_id } => {
            db.lists().delete(&list_id).await?;
            println!("removed {list_id}");
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
            let member = db
                .members()
                .find(&email)
                .await?
                .into_iter()
                .find(|m| m.list_id == list_id)
                .context("membership not found")?;
            db.members().delete(member.id).await?;
            println!("removed {}", member.id);
        }
        MemberCommand::Ls { list_id, role } => {
            for m in db.members().roster(&list_id, role).await? {
                println!("{} {}", m.id, m.display_name);
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
            bail!("duplicate sync member row {}: {}", index + 1, address.email);
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
        } => {
            let password = read_password(password)?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &db.users()
                        .create(NewUser {
                            display_name,
                            email,
                            password,
                            server_owner
                        })
                        .await?
                )?
            );
        }
        UserCommand::Passwd { id, password } => {
            let password = read_password(password)?;
            db.users().set_password(id, &password).await?;
            println!("password changed");
        }
    }
    Ok(())
}

fn read_password(input: PasswordInput) -> Result<String> {
    let mut password = if input.password_stdin {
        let mut value = String::new();
        std::io::stdin().take(16_385).read_to_string(&mut value)?;
        value
    } else if let Some(fd) = input.password_fd {
        read_password_fd(fd)?
    } else {
        rpassword::prompt_password("Password: ")?
    };
    while password.ends_with(['\n', '\r']) {
        password.pop();
    }
    if password.len() > 16_384 {
        bail!(CoreError::Validation("password is too long".into()));
    }
    Ok(password)
}

#[cfg(unix)]
fn read_password_fd(fd: u32) -> Result<String> {
    let mut value = String::new();
    std::fs::File::open(format!("/dev/fd/{fd}"))?
        .take(16_385)
        .read_to_string(&mut value)?;
    Ok(value)
}

#[cfg(not(unix))]
fn read_password_fd(_fd: u32) -> Result<String> {
    bail!(CoreError::Validation(
        "password file descriptors are unsupported on this platform".into()
    ))
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
