use anyhow::Result;
use clap::Args;
use listmngr_core::ListId;
use listmngr_db::Database;

/// Mailman's `notify`: remind owners and moderators of what is waiting.
#[derive(Debug, Args)]
pub struct Options {
    /// Only these lists (default: every list).
    #[arg(long = "list", value_name = "LIST_ID")]
    lists: Vec<ListId>,
    /// Print what is waiting without sending anything.
    #[arg(long)]
    dry_run: bool,
}
pub async fn run(db: &Database, options: Options) -> Result<()> {
    let lists = if options.lists.is_empty() {
        db.lists()
            .list(None)
            .await?
            .into_iter()
            .map(|list| list.id)
            .collect()
    } else {
        options.lists
    };
    let now_ms = chrono::Utc::now().timestamp_millis();
    let mut notified = 0_usize;
    for list in &lists {
        let pending = db.tasks().pending(list).await?;
        if pending.total() == 0 {
            continue;
        }
        if options.dry_run {
            println!("{list}: {}", serde_json::to_string(&pending)?);
        } else if db.tasks().notify_list(list, now_ms).await? {
            notified += 1;
            println!(
                "{list}: {} request(s) waiting, owners notified",
                pending.total()
            );
        }
    }
    if !options.dry_run {
        println!("{notified} list(s) notified");
    }
    Ok(())
}
