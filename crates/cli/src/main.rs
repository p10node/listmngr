#![forbid(unsafe_code)]

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use listmngr_core::{Config, ListId, MemberRole, SubscriptionMode, TokenId, UserId};
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
        #[arg(long)]
        password: String,
        #[arg(long)]
        server_owner: bool,
    },
    Passwd {
        id: UserId,
        #[arg(long)]
        password: String,
    },
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
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
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
        command => run_database(command, config).await?,
    }
    Ok(())
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
            db.migrate().await?;
            println!("migrations applied");
        }
        Command::Status => {
            sqlx::query("SELECT 1").execute(db.pool()).await?;
            println!("database: ready");
        }
        Command::Serve => {
            db.migrate().await?;
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
        Command::Version | Command::Conf { .. } | Command::Info => {
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
            for m in db.members().find(&email).await? {
                println!("{} {} {}", m.id, m.list_id, m.role);
            }
        }
        MemberCommand::Sync {
            list_id,
            file,
            role,
        } => sync_members(db, &list_id, &file, role).await?,
    }
    Ok(())
}
async fn sync_members(
    db: &Database,
    list_id: &ListId,
    file: &Path,
    role: MemberRole,
) -> Result<()> {
    let content = std::fs::read_to_string(file)?;
    let desired = content
        .lines()
        .map(str::trim)
        .filter(|v| !v.is_empty() && !v.starts_with('#'))
        .map(str::to_ascii_lowercase)
        .collect::<std::collections::HashSet<_>>();
    let mut retained = std::collections::HashSet::new();
    for email in &desired {
        for member in db.members().find(email).await? {
            if member.list_id == *list_id && member.role == role {
                retained.insert(member.id);
            }
        }
    }
    for member in db.members().roster(list_id, role).await? {
        if !retained.contains(&member.id) {
            db.members().delete(member.id).await?;
        }
    }
    for email in &desired {
        if !db
            .members()
            .find(email)
            .await?
            .iter()
            .any(|m| m.list_id == *list_id && m.role == role)
        {
            add_member(
                db,
                MemberArgs {
                    list_id: list_id.clone(),
                    email: email.clone(),
                    role,
                    mode: SubscriptionMode::AsAddress,
                    display_name: String::new(),
                },
            )
            .await?;
        }
    }
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
                        password,
                        server_owner
                    })
                    .await?
            )?
        ),
        UserCommand::Passwd { id, password } => {
            db.users().set_password(id, &password).await?;
            println!("password changed");
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
