//! `listmngr archive`: the search index's rebuild, and mbox import and
//! export. `reindex` needs the index's write lock, so `serve` must not be
//! holding it; `import` writes the database only (run `reindex`, or let
//! the runner's next batch, index the new posts); `export` reads it.
use anyhow::{Context as _, Result};
use clap::Subcommand;
use listmngr_archive::search::SearchIndex;
use listmngr_core::{Config, ListId};
use listmngr_db::Database;
use listmngr_db::archive::browse::month_bounds;
use listmngr_db::archive::import::ExportSelection;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Rebuild the search index from every archived post and print how
    /// many were indexed. Uses `[archive] index_path` unless overridden.
    Reindex {
        /// Where the index lives; created when missing.
        #[arg(long)]
        index: Option<PathBuf>,
    },
    /// Import an mbox (mboxrd; `.gz` accepted) into a list's archive.
    /// Posts already archived are skipped; the rest are stored a batch per
    /// transaction. Prints the counts.
    Import {
        list: String,
        /// The mbox file to read.
        file: PathBuf,
        /// Messages per transaction.
        #[arg(long, default_value_t = 500)]
        batch: usize,
    },
    /// Export a list's archive as mboxrd: everything, one thread, or one
    /// month, to a file or standard output, optionally gzipped.
    Export {
        list: String,
        /// One thread, by its root's Message-ID-Hash.
        #[arg(long, conflicts_with = "month")]
        thread: Option<String>,
        /// One month, as `YYYY-MM`.
        #[arg(long)]
        month: Option<String>,
        /// gzip the output.
        #[arg(long)]
        gzip: bool,
        /// Where to write; standard output when omitted.
        #[arg(long, short)]
        output: Option<PathBuf>,
    },
}

pub async fn run(db: &Database, config: &Config, command: Command) -> Result<()> {
    match command {
        Command::Reindex { index } => {
            let path = index.unwrap_or_else(|| PathBuf::from(&config.archive.index_path));
            let index = open(&path)?;
            let count = listmngr_archive::reindex(db, &index)
                .await
                .context("rebuilding the search index")?;
            println!("indexed {count} messages into {}", path.display());
        }
        Command::Import { list, file, batch } => {
            let list: ListId = list.parse()?;
            let outcome = import(db, &list, &file, batch).await?;
            println!(
                "imported {} messages into {list} ({} skipped)",
                outcome.imported, outcome.skipped
            );
        }
        Command::Export {
            list,
            thread,
            month,
            gzip,
            output,
        } => {
            let list: ListId = list.parse()?;
            let selection = selection(thread, month)?;
            let count = export(db, &list, &selection, gzip, output.as_deref()).await?;
            eprintln!("exported {count} messages from {list}");
        }
    }
    Ok(())
}

fn open(path: &Path) -> Result<SearchIndex> {
    SearchIndex::open(path)
        .with_context(|| format!("opening the search index at {}", path.display()))
}

/// The messages an export takes, from the command line.
pub fn selection(thread: Option<String>, month: Option<String>) -> Result<ExportSelection> {
    if let Some(thread) = thread {
        if thread.is_empty() || thread.len() > 200 {
            anyhow::bail!("thread id bounds");
        }
        return Ok(ExportSelection::Thread(thread));
    }
    if let Some(month) = month {
        let (from_ms, until_ms) = parse_month(&month)?;
        return Ok(ExportSelection::Between { from_ms, until_ms });
    }
    Ok(ExportSelection::All)
}

/// `YYYY-MM` as the month's millisecond bounds.
pub fn parse_month(value: &str) -> Result<(i64, i64)> {
    let (year, month) = value.split_once('-').context("the month is YYYY-MM")?;
    let year: i32 = year.parse().context("the month is YYYY-MM")?;
    let month: u32 = month.parse().context("the month is YYYY-MM")?;
    if !(1970..=9999).contains(&year) {
        anyhow::bail!("the month is YYYY-MM");
    }
    Ok(month_bounds(year, month)?)
}

async fn import(
    db: &Database,
    list: &ListId,
    file: &Path,
    batch: usize,
) -> Result<listmngr_db::archive::import::Outcome> {
    let opened =
        std::fs::File::open(file).with_context(|| format!("opening {}", file.display()))?;
    let now_ms = chrono::Utc::now().timestamp_millis();
    let gz = file
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("gz"));
    // `Send`, so that the future stays `Send` across the batch awaits.
    let input: Box<dyn BufRead + Send> = if gz {
        Box::new(BufReader::new(flate2::read::GzDecoder::new(opened)))
    } else {
        Box::new(BufReader::new(opened))
    };
    let mut last_report = 0;
    let outcome = listmngr_archive::mbox::import(
        db,
        list,
        listmngr_archive::mbox::Reader::new(input),
        batch,
        now_ms,
        |outcome| {
            let done = outcome.imported + outcome.skipped;
            if done - last_report >= 10_000 {
                last_report = done;
                eprintln!("{done} messages so far");
            }
        },
    )
    .await
    .context("importing the mbox")?;
    Ok(outcome)
}

async fn export(
    db: &Database,
    list: &ListId,
    selection: &ExportSelection,
    gzip: bool,
    output: Option<&Path>,
) -> Result<u64> {
    // `Send`, so that the future stays `Send` across the paging awaits;
    // `StdoutLock` is not, and standard output is line-safe enough here.
    let sink: Box<dyn Write + Send> = match output {
        Some(path) => Box::new(
            std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?,
        ),
        None => Box::new(std::io::stdout()),
    };
    let mut sink: Box<dyn Write + Send> = if gzip {
        Box::new(flate2::write::GzEncoder::new(
            sink,
            flate2::Compression::default(),
        ))
    } else {
        sink
    };
    let mut count = 0;
    let mut after: Option<(i64, String)> = None;
    loop {
        let rows = db
            .archive()
            .export_rows(list, selection, after.as_ref(), 500)
            .await?;
        let Some(newest) = rows.last() else {
            break;
        };
        after = Some((newest.created_at, newest.hash.clone()));
        for row in &rows {
            listmngr_archive::mbox::write_message(&mut sink, &row.raw)
                .context("writing the mbox")?;
            count += 1;
        }
    }
    sink.flush().context("writing the mbox")?;
    Ok(count)
}
