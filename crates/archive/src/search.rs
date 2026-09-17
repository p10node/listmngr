//! The archive's search index: one tantivy index under `[archive]
//! index_path`, one document per archived post, scoped by list.
//!
//! The database stays the source of truth: the index holds what a search
//! needs to rank and to name a hit (list, hash, thread, subject, sender,
//! date) and the body text for matching; a hit is then read back through
//! the archive's own authorization. Writes are batched by the owner of the
//! single [`Writer`] (the archive runner, or `listmngr archive reindex`),
//! reads reload on commit.
use listmngr_core::{Error, Result};
use std::path::Path;
use std::time::{Duration, Instant};
use tantivy::collector::{Count, TopDocs};
use tantivy::query::{BooleanQuery, Occur, QueryParser, RangeQuery, TermQuery};
use tantivy::schema::{
    FAST, Field, INDEXED, IndexRecordOption, STORED, STRING, Schema, TEXT, TantivyDocument, Value,
};
use tantivy::{Index, IndexReader, IndexWriter, ReloadPolicy, Term, doc};

/// One archived post as the index stores it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Document {
    pub list: String,
    pub hash: String,
    pub thread: String,
    pub subject: String,
    pub body: String,
    pub sender_name: String,
    pub sender_email: String,
    pub date_ms: i64,
}

/// One search hit, best first.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub hash: String,
    pub thread: String,
    pub subject: String,
    pub sender: String,
    pub date_ms: i64,
    pub score: f32,
}

/// A page of hits and how many match in all.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Results {
    pub hits: Vec<Hit>,
    pub total: usize,
}

/// What a search asks for. Every search is scoped to one list.
#[derive(Debug, Clone, Copy)]
pub struct Query<'a> {
    pub list: &'a str,
    /// The reader's words; every word must match (subject, body or sender).
    pub text: &'a str,
    pub thread: Option<&'a str>,
    /// Inclusive bounds on the post's date, milliseconds.
    pub since_ms: Option<i64>,
    pub until_ms: Option<i64>,
    pub limit: usize,
    pub offset: usize,
}

#[derive(Debug, Clone, Copy)]
struct Fields {
    key: Field,
    list: Field,
    hash: Field,
    thread: Field,
    subject: Field,
    body: Field,
    sender: Field,
    date: Field,
}

/// The index and a reader that follows its commits.
pub struct SearchIndex {
    index: Index,
    reader: IndexReader,
    fields: Fields,
}

impl std::fmt::Debug for SearchIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchIndex").finish_non_exhaustive()
    }
}

/// The single writer of an index, committing in batches.
pub struct Writer {
    inner: IndexWriter,
    fields: Fields,
    pending: usize,
    last_commit: Instant,
}

impl std::fmt::Debug for Writer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Writer")
            .field("pending", &self.pending)
            .finish_non_exhaustive()
    }
}

fn failure(error: impl std::fmt::Display) -> Error {
    Error::Validation(format!("search index: {error}"))
}

fn schema() -> (Schema, Fields) {
    let mut builder = Schema::builder();
    let key = builder.add_text_field("key", STRING);
    let list = builder.add_text_field("list", STRING | STORED);
    let hash = builder.add_text_field("hash", STRING | STORED);
    let thread = builder.add_text_field("thread", STRING | STORED);
    let subject = builder.add_text_field("subject", TEXT | STORED);
    let body = builder.add_text_field("body", TEXT);
    let sender = builder.add_text_field("sender", TEXT | STORED);
    let date = builder.add_i64_field("date", INDEXED | STORED | FAST);
    (
        builder.build(),
        Fields {
            key,
            list,
            hash,
            thread,
            subject,
            body,
            sender,
            date,
        },
    )
}

fn key_of(list: &str, hash: &str) -> String {
    format!("{list}\u{0}{hash}")
}

impl SearchIndex {
    /// Whether an index already lives in `dir`.
    #[must_use]
    pub fn exists(dir: &Path) -> bool {
        dir.join("meta.json").is_file()
    }

    /// Open the index in `dir`, creating the directory and an empty index
    /// when there is none.
    /// # Errors
    /// Returns a validation error when the directory cannot be used or
    /// holds an index of another schema.
    pub fn open(dir: &Path) -> Result<Self> {
        let (schema, fields) = schema();
        std::fs::create_dir_all(dir).map_err(failure)?;
        let index = if Self::exists(dir) {
            let index = Index::open_in_dir(dir).map_err(failure)?;
            if index.schema() != schema {
                return Err(Error::Validation(
                    "search index: the directory holds an index of another schema; run `listmngr archive reindex --rebuild`".into(),
                ));
            }
            index
        } else {
            Index::create_in_dir(dir, schema).map_err(failure)?
        };
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()
            .map_err(failure)?;
        Ok(Self {
            index,
            reader,
            fields,
        })
    }

    /// The index's one writer with a 50 MiB heap; a second holder in this
    /// or another process fails to acquire the lock.
    /// # Errors
    /// Returns a validation error when the lock is held.
    pub fn writer(&self) -> Result<Writer> {
        let inner = self.index.writer(50_000_000).map_err(failure)?;
        Ok(Writer {
            inner,
            fields: self.fields,
            pending: 0,
            last_commit: Instant::now(),
        })
    }

    /// A page of hits for `query`, best first, with the total.
    /// # Errors
    /// Returns a validation error for a query the index cannot run.
    pub fn search(&self, query: &Query<'_>) -> Result<Results> {
        let text = query.text.trim();
        if text.is_empty() || text.len() > 200 || query.limit == 0 || query.limit > 100 {
            return Err(Error::Validation("search query bounds".into()));
        }
        let fields = self.fields;
        let mut parser = QueryParser::for_index(
            &self.index,
            vec![fields.subject, fields.body, fields.sender],
        );
        parser.set_conjunction_by_default();
        // A reader's punctuation is never a syntax error: the lenient parse
        // keeps what it can and ignores the rest.
        let (words, _errors) = parser.parse_query_lenient(text);
        let mut clauses: Vec<(Occur, Box<dyn tantivy::query::Query>)> = vec![
            (
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(fields.list, query.list),
                    IndexRecordOption::Basic,
                )),
            ),
            (Occur::Must, words),
        ];
        if let Some(thread) = query.thread.filter(|t| !t.is_empty()) {
            clauses.push((
                Occur::Must,
                Box::new(TermQuery::new(
                    Term::from_field_text(fields.thread, thread),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        if query.since_ms.is_some() || query.until_ms.is_some() {
            let lower = query.since_ms.map_or(std::ops::Bound::Unbounded, |ms| {
                std::ops::Bound::Included(Term::from_field_i64(fields.date, ms))
            });
            let upper = query.until_ms.map_or(std::ops::Bound::Unbounded, |ms| {
                std::ops::Bound::Included(Term::from_field_i64(fields.date, ms))
            });
            clauses.push((Occur::Must, Box::new(RangeQuery::new(lower, upper))));
        }
        let combined = BooleanQuery::new(clauses);
        let searcher = self.reader.searcher();
        let (top, total) = searcher
            .search(
                &combined,
                &(
                    TopDocs::with_limit(query.limit)
                        .and_offset(query.offset)
                        .order_by_score(),
                    Count,
                ),
            )
            .map_err(failure)?;
        let mut hits = Vec::with_capacity(top.len());
        for (score, address) in top {
            let stored: TantivyDocument = searcher.doc(address).map_err(failure)?;
            let text_of = |field: Field| {
                stored
                    .get_first(field)
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_owned()
            };
            hits.push(Hit {
                hash: text_of(fields.hash),
                thread: text_of(fields.thread),
                subject: text_of(fields.subject),
                sender: text_of(fields.sender),
                date_ms: stored
                    .get_first(fields.date)
                    .and_then(|value| value.as_i64())
                    .unwrap_or_default(),
                score,
            });
        }
        Ok(Results { hits, total })
    }

    /// How many documents the index holds, after the latest commit.
    /// # Errors
    /// Returns a validation error when the segments cannot be read.
    pub fn len(&self) -> Result<usize> {
        let searcher = self.reader.searcher();
        Ok(searcher
            .segment_readers()
            .iter()
            .map(|segment| segment.num_docs() as usize)
            .sum())
    }

    /// Whether the index holds no document.
    /// # Errors
    /// Returns a validation error when the segments cannot be read.
    pub fn is_empty(&self) -> Result<bool> {
        Ok(self.len()? == 0)
    }

    /// Wait for the reader to see the latest commit (tests and the reindex
    /// command; the runner's readers follow commits on their own).
    /// # Errors
    /// Returns a validation error when the reader cannot reload.
    pub fn reload(&self) -> Result<()> {
        self.reader.reload().map_err(failure)
    }
}

impl Writer {
    /// Add or replace one post; the previous document with the same list
    /// and hash is removed in the same batch.
    /// # Errors
    /// Returns a validation error when the document cannot be added.
    pub fn add(&mut self, document: &Document) -> Result<()> {
        let key = key_of(&document.list, &document.hash);
        self.inner
            .delete_term(Term::from_field_text(self.fields.key, &key));
        let sender = if document.sender_name.is_empty() {
            document.sender_email.clone()
        } else {
            format!("{} {}", document.sender_name, document.sender_email)
        };
        self.inner
            .add_document(doc!(
                self.fields.key => key,
                self.fields.list => document.list.as_str(),
                self.fields.hash => document.hash.as_str(),
                self.fields.thread => document.thread.as_str(),
                self.fields.subject => document.subject.as_str(),
                self.fields.body => document.body.as_str(),
                self.fields.sender => sender,
                self.fields.date => document.date_ms,
            ))
            .map_err(failure)?;
        self.pending += 1;
        Ok(())
    }

    /// Remove one post.
    pub fn remove(&mut self, list: &str, hash: &str) {
        self.inner
            .delete_term(Term::from_field_text(self.fields.key, &key_of(list, hash)));
        self.pending += 1;
    }

    /// Remove every document of a list.
    pub fn remove_list(&mut self, list: &str) {
        self.inner
            .delete_term(Term::from_field_text(self.fields.list, list));
        self.pending += 1;
    }

    /// Remove every document (a rebuild starts here).
    /// # Errors
    /// Returns a validation error when the index cannot be cleared.
    pub fn clear(&mut self) -> Result<()> {
        self.inner.delete_all_documents().map_err(failure)?;
        self.pending += 1;
        Ok(())
    }

    /// Documents added or removed since the last commit.
    #[must_use]
    pub const fn pending(&self) -> usize {
        self.pending
    }

    /// Make every pending change visible to readers.
    /// # Errors
    /// Returns a validation error when the commit fails.
    pub fn commit(&mut self) -> Result<()> {
        if self.pending == 0 {
            return Ok(());
        }
        self.inner.commit().map_err(failure)?;
        self.pending = 0;
        self.last_commit = Instant::now();
        Ok(())
    }

    /// Commit when at least `max_pending` changes wait or the oldest has
    /// waited `max_age`; the batch bound for a steady stream of posts.
    /// # Errors
    /// Returns a validation error when the commit fails.
    pub fn commit_if_due(&mut self, max_pending: usize, max_age: Duration) -> Result<bool> {
        if self.pending == 0 {
            return Ok(false);
        }
        if self.pending >= max_pending || self.last_commit.elapsed() >= max_age {
            self.commit()?;
            return Ok(true);
        }
        Ok(false)
    }
}
