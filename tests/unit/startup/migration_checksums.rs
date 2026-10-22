use super::*;
use sha2::{Digest, Sha384};

fn checksum(sql: &str) -> Vec<u8> {
    Sha384::digest(sql.as_bytes()).to_vec()
}

async fn migration_pool() -> SqlitePool {
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap()
}

#[tokio::test]
async fn crlf_checksums_from_older_windows_builds_are_normalized() {
    let directory = tempfile::tempdir().unwrap();
    let lf = "CREATE TABLE migration_check (id INTEGER);\n";
    std::fs::write(directory.path().join("0001_init.sql"), lf).unwrap();
    let migrator = sqlx::migrate::Migrator::new(directory.path())
        .await
        .unwrap();
    let pool = migration_pool().await;
    migrator.run(&pool).await.unwrap();

    let legacy = checksum(&lf.replace('\n', "\r\n"));
    sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = 1")
        .bind(&legacy)
        .execute(&pool)
        .await
        .unwrap();

    assert_eq!(
        normalize_legacy_migration_checksums(&pool, &migrator)
            .await
            .unwrap(),
        1
    );
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored, migrator.iter().next().unwrap().checksum.as_ref());
    migrator.run(&pool).await.unwrap();
}

#[tokio::test]
async fn unrelated_checksum_changes_are_not_normalized() {
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(
        directory.path().join("0001_init.sql"),
        "CREATE TABLE migration_check (id INTEGER);\n",
    )
    .unwrap();
    let migrator = sqlx::migrate::Migrator::new(directory.path())
        .await
        .unwrap();
    let pool = migration_pool().await;
    migrator.run(&pool).await.unwrap();

    let modified = checksum("CREATE TABLE changed (id INTEGER);\n");
    sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = 1")
        .bind(&modified)
        .execute(&pool)
        .await
        .unwrap();

    assert_eq!(
        normalize_legacy_migration_checksums(&pool, &migrator)
            .await
            .unwrap(),
        0
    );
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT checksum FROM _sqlx_migrations WHERE version = 1")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(stored, modified);
    assert!(migrator.run(&pool).await.is_err());
}
