use listmngr_db::{Database, NewList};
use std::collections::{BTreeMap, BTreeSet};

const PHASE_ONE_SCHEMA: &str = include_str!("fixtures/phase1-schema.snapshot");

fn canonical_schema(mut lines: Vec<String>) -> String {
    lines.sort();
    format!("{}\n", lines.join("\n"))
}

fn expected_phase_one_schema() -> String {
    // Preserve the frozen Phase 1 corpus; additive migrations have their own corpus.
    canonical_schema(
        PHASE_ONE_SCHEMA
            .lines()
            .chain(include_str!("fixtures/phase2-queue-schema.snapshot").lines())
            .chain(include_str!("fixtures/phase2-mail-policy-schema.snapshot").lines())
            .chain(include_str!("fixtures/phase2-delivery-attempt-schema.snapshot").lines())
            .chain(include_str!("fixtures/phase4-composed-schema.snapshot").lines())
            .chain(include_str!("fixtures/owner-schema.snapshot").lines())
            .chain(include_str!("fixtures/message-size-schema.snapshot").lines())
            .chain(include_str!("fixtures/dmarc-munge-schema.snapshot").lines())
            .chain(include_str!("fixtures/smtp-bounces-schema.snapshot").lines())
            .chain(include_str!("fixtures/smtp-failure-metadata-schema.snapshot").lines())
            .chain(include_str!("fixtures/welcome-schema.snapshot").lines())
            .chain(include_str!("fixtures/goodbye-schema.snapshot").lines())
            .chain(include_str!("fixtures/bounce-score-schema.snapshot").lines())
            .chain(include_str!("fixtures/bounce-disable-schema.snapshot").lines())
            .chain(include_str!("fixtures/bounce-disable-notice-schema.snapshot").lines())
            .chain(include_str!("fixtures/bounce-increment-notice-schema.snapshot").lines())
            .chain(include_str!("fixtures/bounce-maintenance-schema.snapshot").lines())
            .chain(include_str!("fixtures/dsn-issuance-schema.snapshot").lines())
            .chain(include_str!("fixtures/dsn-plan-schema.snapshot").lines())
            .chain(include_str!("fixtures/recipient-limit-schema.snapshot").lines())
            .chain(include_str!("fixtures/moderation-rules-schema.snapshot").lines())
            .chain(include_str!("fixtures/posting-pipeline-schema.snapshot").lines())
            .chain(include_str!("fixtures/hold-notices-schema.snapshot").lines())
            .chain(include_str!("fixtures/alter-messages-schema.snapshot").lines())
            .chain(include_str!("fixtures/topics-schema.snapshot").lines())
            .chain(include_str!("fixtures/site-secrets-schema.snapshot").lines())
            .chain(include_str!("fixtures/subscription-state-schema.snapshot").lines())
            .chain(include_str!("fixtures/subscription-invitation-schema.snapshot").lines())
            .chain(include_str!("fixtures/autoresponder-schema.snapshot").lines())
            .chain(include_str!("fixtures/bounce-probes-schema.snapshot").lines())
            .chain(include_str!("fixtures/digest-settings-schema.snapshot").lines())
            .chain(include_str!("fixtures/admin-notify-mchanges-schema.snapshot").lines())
            .chain(include_str!("fixtures/moderation-forward-schema.snapshot").lines())
            .chain(include_str!("fixtures/session-inventory-schema.snapshot").lines())
            .chain(include_str!("fixtures/account-tokens-schema.snapshot").lines())
            .chain(include_str!("fixtures/totp-schema.snapshot").lines())
            .chain(include_str!("fixtures/passkeys-schema.snapshot").lines())
            .chain(include_str!("fixtures/oidc-schema.snapshot").lines())
            .chain(include_str!("fixtures/archive-render-schema.snapshot").lines())
            .chain(include_str!("fixtures/archive-views-schema.snapshot").lines())
            .chain(include_str!("fixtures/archive-interactions-schema.snapshot").lines())
            .chain(include_str!("fixtures/archive-admin-schema.snapshot").lines())
            .chain(include_str!("fixtures/usenet-schema.snapshot").lines())
            .map(str::to_owned)
            .collect(),
    )
}

async fn sqlite_semantic_schema(db: &Database) -> String {
    let columns: Vec<String> = sqlx::query_scalar(
        r"SELECT 'C|' || m.name || '|' || p.name || '|' ||
        CASE WHEN upper(p.type) LIKE '%INT%' THEN 'integer'
             WHEN upper(p.type) IN ('REAL','DOUBLE','DOUBLE PRECISION','FLOAT') THEN 'real'
             ELSE 'text' END || '|' ||
        CASE WHEN p.[notnull]=1 OR p.pk>0 THEN 'required' ELSE 'optional' END || '|' ||
        CASE replace(replace(COALESCE(p.dflt_value,''),'''',''),'::text','')
             WHEN '' THEN '-' ELSE replace(replace(COALESCE(p.dflt_value,''),'''',''),'::text','') END
        FROM sqlite_master m JOIN pragma_table_info(m.name) p
        WHERE m.type='table' AND m.name NOT LIKE 'sqlite_%' AND m.name NOT LIKE '_sqlx_%'",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    let foreign_keys: Vec<String> = sqlx::query_scalar(
        r"SELECT 'F|' || m.name || '|' || f.[from] || '|' || f.[table] || '|' || f.[to] || '|' || lower(f.on_delete)
        FROM sqlite_master m JOIN pragma_foreign_key_list(m.name) f
        WHERE m.type='table' AND m.name NOT LIKE 'sqlite_%' AND m.name NOT LIKE '_sqlx_%'",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    let unique_keys: Vec<String> = sqlx::query_scalar(
        r"SELECT 'U|' || indexes.table_name || '|' || group_concat(indexes.column_name, ',')
        FROM (
          SELECT m.name AS table_name, il.name AS index_name, ii.name AS column_name, ii.seqno
          FROM sqlite_master m
          JOIN pragma_index_list(m.name) il
          JOIN pragma_index_info(il.name) ii
          WHERE m.type='table' AND m.name NOT LIKE 'sqlite_%' AND m.name NOT LIKE '_sqlx_%'
            AND il.[unique]=1
          ORDER BY m.name, il.name, ii.seqno
        ) indexes
        GROUP BY indexes.table_name, indexes.index_name
        UNION
        SELECT 'U|' || m.name || '|' || p.name
        FROM sqlite_master m JOIN pragma_table_info(m.name) p
        WHERE m.type='table' AND m.name NOT LIKE 'sqlite_%' AND m.name NOT LIKE '_sqlx_%'
          AND p.pk=1 AND upper(p.type)='INTEGER'
          AND NOT EXISTS (SELECT 1 FROM pragma_table_info(m.name) pk WHERE pk.pk>1)",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    canonical_schema(
        columns
            .into_iter()
            .chain(foreign_keys)
            .chain(unique_keys)
            .collect(),
    )
}

async fn postgres_semantic_schema(db: &Database) -> String {
    let columns: Vec<String> = sqlx::query_scalar(
        r"SELECT 'C|' || c.table_name || '|' || c.column_name || '|' ||
        CASE WHEN c.data_type IN ('smallint','integer','bigint') THEN 'integer'
             WHEN c.data_type IN ('real','double precision','numeric','decimal') THEN 'real'
             ELSE 'text' END || '|' ||
        CASE c.is_nullable WHEN 'NO' THEN 'required' ELSE 'optional' END || '|' ||
        COALESCE(NULLIF(regexp_replace(regexp_replace(c.column_default, '^''(.*)''::[a-z ]+$', '\1'), '^\((.*)\)$', '\1'), ''), '-')
        FROM information_schema.columns c
        WHERE c.table_schema=current_schema() AND c.table_name NOT LIKE '\_sqlx\_%' ESCAPE '\'",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    let foreign_keys: Vec<String> = sqlx::query_scalar(
        r"SELECT 'F|' || source.relname || '|' || source_col.attname || '|' || target.relname || '|' || target_col.attname || '|' ||
          CASE con.confdeltype WHEN 'a' THEN 'no action' WHEN 'r' THEN 'restrict' WHEN 'c' THEN 'cascade'
               WHEN 'n' THEN 'set null' WHEN 'd' THEN 'set default' END
        FROM pg_constraint con
        JOIN pg_class source ON source.oid=con.conrelid
        JOIN pg_namespace namespace ON namespace.oid=source.relnamespace
        JOIN pg_class target ON target.oid=con.confrelid
        JOIN generate_subscripts(con.conkey, 1) position ON true
        JOIN pg_attribute source_col ON source_col.attrelid=source.oid AND source_col.attnum=con.conkey[position]
        JOIN pg_attribute target_col ON target_col.attrelid=target.oid AND target_col.attnum=con.confkey[position]
        WHERE namespace.nspname=current_schema() AND con.contype='f'",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    let unique_keys: Vec<String> = sqlx::query_scalar(
        r"SELECT 'U|' || tbl.relname || '|' || string_agg(att.attname,',' ORDER BY ord.n)
        FROM pg_index idx JOIN pg_class tbl ON tbl.oid=idx.indrelid
        JOIN pg_namespace ns ON ns.oid=tbl.relnamespace
        JOIN LATERAL unnest(idx.indkey) WITH ORDINALITY ord(attnum,n) ON true
        JOIN pg_attribute att ON att.attrelid=tbl.oid AND att.attnum=ord.attnum
        WHERE ns.nspname=current_schema() AND idx.indisunique AND tbl.relname NOT LIKE '\_sqlx\_%' ESCAPE '\'
        GROUP BY tbl.relname,idx.indexrelid",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    canonical_schema(
        columns
            .into_iter()
            .chain(foreign_keys)
            .chain(unique_keys)
            .collect(),
    )
}

fn parsed_corpus() -> (BTreeMap<String, BTreeSet<String>>, Vec<Vec<&'static str>>) {
    let mut columns: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut records = Vec::new();
    for line in PHASE_ONE_SCHEMA.lines() {
        let fields = line.split('|').collect::<Vec<_>>();
        match fields.first().copied() {
            Some("C") => {
                assert_eq!(fields.len(), 6, "malformed column corpus row: {line}");
                assert!(matches!(fields[3], "integer" | "real" | "text"));
                assert!(matches!(fields[4], "required" | "optional"));
                columns
                    .entry(fields[1].to_owned())
                    .or_default()
                    .insert(fields[2].to_owned());
            }
            Some("F") => assert_eq!(fields.len(), 6, "malformed FK corpus row: {line}"),
            Some("U") => assert_eq!(fields.len(), 3, "malformed unique corpus row: {line}"),
            _ => panic!("unknown semantic corpus row: {line}"),
        }
        records.push(fields);
    }
    (columns, records)
}

#[test]
fn shared_schema_corpus_is_complete_and_internally_consistent() {
    let (columns, records) = parsed_corpus();
    assert_eq!(
        columns.keys().map(String::as_str).collect::<BTreeSet<_>>(),
        BTreeSet::from([
            "addresses",
            "api_tokens",
            "audit_log",
            "bans",
            "domain_owners",
            "domains",
            "header_matches",
            "list_archivers",
            "list_styles",
            "mailing_lists",
            "members",
            "preferences",
            "templates",
            "user_credentials",
            "users",
        ])
    );
    for record in records {
        match record[0] {
            "F" => {
                assert!(columns[record[1]].contains(record[2]));
                assert!(columns[record[3]].contains(record[4]));
                assert!(matches!(
                    record[5],
                    "cascade" | "restrict" | "set null" | "no action" | "set default"
                ));
            }
            "U" => {
                for column in record[2].split(',') {
                    assert!(columns[record[1]].contains(column));
                }
            }
            _ => {}
        }
    }
    let preference_rows = PHASE_ONE_SCHEMA
        .lines()
        .filter(|line| line.starts_with("C|preferences|"))
        .collect::<Vec<_>>();
    assert_eq!(preference_rows.len(), 8);
    assert_eq!(
        preference_rows
            .iter()
            .filter(|line| !line.starts_with("C|preferences|id|") && line.contains("|optional|-"))
            .count(),
        7
    );
}

#[tokio::test]
async fn sqlite_phase_one_schema_matches_complete_semantic_snapshot() {
    let db = Database::connect("sqlite::memory:", 1).await.unwrap();
    db.migrate().await.unwrap();
    assert_eq!(
        sqlite_semantic_schema(&db).await,
        expected_phase_one_schema()
    );
}

#[tokio::test]
#[ignore = "requires live TEST_POSTGRES_URL; compile-only by default, never counts as a PostgreSQL run"]
async fn live_postgres_matches_the_exact_sqlite_semantic_corpus() {
    let url = std::env::var("TEST_POSTGRES_URL")
        .expect("TEST_POSTGRES_URL is required for the ignored live PostgreSQL contract");
    assert!(url.starts_with("postgres://") || url.starts_with("postgresql://"));
    let db = Database::connect(&url, 1)
        .await
        .expect("connect TEST_POSTGRES_URL");
    db.migrate().await.expect("first PostgreSQL migration");
    db.migrate().await.expect("repeated PostgreSQL migration");
    assert_eq!(
        postgres_semantic_schema(&db).await,
        expected_phase_one_schema()
    );

    let run_id = uuid::Uuid::now_v7();
    for sequence in 0..2 {
        let host = format!("phase1-schema-pg-{run_id}-{sequence}.invalid");
        let list_id = format!("contract.{host}").parse().unwrap();
        db.domains()
            .create(&host, "PostgreSQL contract", None)
            .await
            .unwrap();
        db.lists()
            .create(NewList {
                list_id,
                display_name: "Contract".into(),
                style: "legacy-default".into(),
            })
            .await
            .unwrap();
        assert_eq!(db.lists().by_domain(&host).await.unwrap().len(), 1);
        let list = db.lists().by_domain(&host).await.unwrap().remove(0);
        db.lists().delete(&list.id).await.unwrap();
        db.domains().delete(&host).await.unwrap();
    }
}
