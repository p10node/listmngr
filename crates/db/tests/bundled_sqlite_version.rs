//! RUSTSEC-2026-0459 (`SQLite` CVE-2026-119375) is fixed in `SQLite` 3.54.0, which
//! the vendored `libsqlite3-sys` bundles; the build must not fall back to an
//! older amalgamation.

#[tokio::test]
async fn the_bundled_sqlite_has_the_cve_2026_119375_fix() {
    sqlx::any::install_default_drivers();
    let pool = sqlx::AnyPool::connect("sqlite::memory:").await.unwrap();
    let version: String = sqlx::query_scalar("SELECT sqlite_version()")
        .fetch_one(&pool)
        .await
        .unwrap();
    let parts: Vec<u32> = version.split('.').map(|p| p.parse().unwrap()).collect();
    assert!(
        (parts[0], parts[1]) >= (3, 54),
        "bundled SQLite {version} predates the 3.54.0 fix"
    );
}
