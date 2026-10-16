//! Operator commands that run instead of the server.
//!
//! These exist so a deploy pipeline never has to guess: whether a schema change
//! is coming, whether it can be rolled back, and which files a restore could
//! use. Every command is read-only except `--backup` (writes a new file) and
//! `--restore-backup` (swaps the database file while nothing is serving).
//!
//! Exit codes are part of the interface:
//! - `0` nothing to do / the action succeeded
//! - `1` a problem the operator must fix (unreadable database, downgrade, refused restore)
//! - `2` pending migrations: starting the gateway will change the schema

use anyhow::Context;

use crate::Config;
/// `--check-migrations`: what a restart would do to the schema.
pub async fn check_migrations(config: &Config, json: bool) -> anyhow::Result<i32> {
    let report = crate::startup::migration_report(config).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("数据库:         {}", report.database);
        println!(
            "数据文件:       {}",
            if report.data_present {
                "存在且包含用户数据"
            } else {
                "不存在或仅含空 schema"
            }
        );
        println!("已应用迁移:     {}", report.applied.len());
        println!("待执行迁移:     {}", report.pending.len());
        for migration in &report.pending {
            println!("                {} {}", migration.version, migration.description);
        }
        println!("未知迁移:       {}", report.unknown_applied.len());
        if !report.unknown_applied.is_empty() {
            println!("                （数据库由更新的版本迁移过）");
        }
        println!(
            "启动时会先备份: {}",
            if report.backup_would_be_written {
                "是"
            } else {
                "否"
            }
        );
        for problem in &report.problems {
            println!("问题:           {problem}");
        }
        if report.up_to_date {
            println!("\n结论: 数据库 schema 已是最新，可以直接启动。");
        } else if report.problems.is_empty() {
            println!(
                "\n结论: 启动会执行 {} 个迁移{}。建议先备份再升级。",
                report.pending.len(),
                if report.backup_would_be_written {
                    "（启动时会自动写入预迁移备份）"
                } else {
                    ""
                }
            );
        } else {
            println!("\n结论: 不要启动本版本，先处理上面的问题。");
        }
    }
    if !report.problems.is_empty() {
        Ok(1)
    } else if report.pending.is_empty() {
        Ok(0)
    } else {
        Ok(2)
    }
}

/// `--list-backups`: what a restore could use.
pub async fn list_backups(config: &Config, json: bool) -> anyhow::Result<i32> {
    let database_path = crate::startup::database_path(config);
    let backups = crate::startup::list_backups(&database_path).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&backups)?);
    } else {
        println!("数据库: {}", database_path.display());
        if backups.is_empty() {
            println!("没有备份。可用 `openllm --backup` 生成一份，或等待下一次启动时的自动预迁移备份。");
        }
        for backup in &backups {
            println!(
                "{}  {:>10}  {}  {}",
                backup.name,
                human_size(backup.size_bytes),
                backup.kind,
                backup.created_at
            );
        }
    }
    Ok(0)
}

/// `--self-check`: run the read-only diagnostics without starting the server.
pub async fn self_check(config: &Config, json: bool) -> anyhow::Result<i32> {
    let database_path = crate::startup::database_path(config);
    if !database_path.exists() {
        anyhow::bail!(
            "{} does not exist yet; start the gateway once before running --self-check",
            database_path.display()
        );
    }
    let pool = crate::startup::open_operator_read_pool(&database_path).await?;
    let state = crate::state::AppState::new(pool.clone(), None);
    let report = crate::api::diagnostics::run_self_check(&state).await;
    pool.close().await;

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "总体: {}（pass {} / warn {} / fail {}）",
            report.overall, report.counts.pass, report.counts.warn, report.counts.fail
        );
        for item in &report.checks {
            println!("[{:4}] {}: {}", item.status, item.title, item.detail);
            if let Some(hint) = item.hint {
                println!("       建议: {hint}");
            }
        }
    }

    Ok(match report.overall {
        "fail" => 1,
        "warn" => 2,
        _ => 0,
    })
}

/// `--backup`: write an on-demand backup. Safe to run while the gateway serves.
pub async fn create_backup(config: &Config, json: bool) -> anyhow::Result<i32> {
    let database_path = crate::startup::database_path(config);
    if !database_path.exists() {
        anyhow::bail!("{} does not exist yet", database_path.display());
    }
    let pool = crate::startup::open_operator_pool(&database_path).await?;
    let result = crate::startup::create_backup(&pool, &database_path).await;
    pool.close().await;
    let path = result?;
    let metadata = std::fs::metadata(&path)
        .with_context(|| format!("failed to inspect {}", path.display()))?;
    if json {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
            "path": path.to_string_lossy().replace('\\', "/"),
            "size_bytes": metadata.len(),
        }))?);
    } else {
        println!(
            "已写入备份: {} ({})",
            path.display(),
            human_size(metadata.len())
        );
        println!("恢复命令:   openllm --restore-backup {}", path
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default());
    }
    Ok(0)
}

/// `--restore-backup`: replace the database file with a verified backup.
///
/// Must not run while another instance is serving the same database, so the
/// command refuses rather than producing two writers on one file.
pub async fn restore_backup(config: &Config, backup: &str, json: bool) -> anyhow::Result<i32> {
    let database_path = crate::startup::database_path(config);
    if crate::startup::database_is_served(&database_path).await? {
        anyhow::bail!(
            "another instance is serving {}; stop it (and wait for the drain) before restoring",
            database_path.display()
        );
    }
    let backup_path = crate::startup::resolve_backup_path(&database_path, backup, true)?;
    let outcome = crate::startup::restore_backup(&database_path, &backup_path).await?;
    if json {
        println!("{}", serde_json::to_string_pretty(&serde_json::json!({
            "restored_from": outcome.restored_from.to_string_lossy().replace('\\', "/"),
            "restored_to": outcome.restored_to.to_string_lossy().replace('\\', "/"),
            "previous": outcome.previous.as_ref().map(|path| path.to_string_lossy().replace('\\', "/")),
            "integrity": outcome.report.integrity,
            "providers": outcome.report.providers,
            "routes": outcome.report.routes,
            "api_keys": outcome.report.api_keys,
            "cleared_leases": outcome.cleared_leases,
        }))?);
    } else {
        println!("已从备份恢复: {}", outcome.restored_from.display());
        println!("恢复目标:     {}", outcome.restored_to.display());
        match &outcome.previous {
            Some(previous) => println!("原数据库保留在: {}", previous.display()),
            None => println!("原数据库:     不存在（全新部署）"),
        }
        println!(
            "校验:         完整性 {}，表 {} 个，provider {} / route {} / api key {}",
            outcome.report.integrity,
            outcome.report.tables.len(),
            outcome.report.providers,
            outcome.report.routes,
            outcome.report.api_keys,
        );
        if outcome.cleared_leases > 0 {
            println!(
                "实例租约:     清除 {} 条（备份中的租约属于当时的进程）",
                outcome.cleared_leases
            );
        }
        println!("\n下一步: 启动网关，确认 GET /api/ready 返回 200 后再对外放量。");
    }
    Ok(0)
}

/// Render a byte count for humans.
fn human_size(bytes: u64) -> String {    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// Path of the database a config refers to, for callers that only print.
pub fn described_database_path(config: &Config) -> String {
    crate::startup::database_path(config)
        .to_string_lossy()
        .replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::sqlite::SqlitePoolOptions;

    #[tokio::test]
    async fn self_check_runs_against_an_existing_database() {
        let directory = tempfile::tempdir().unwrap();
        let config = Config {
            bind: "127.0.0.1:8080".parse().unwrap(),
            data_dir: directory.path().to_path_buf(),
            database_url: None,
            admin_token: None,
        };
        let database_path = crate::startup::database_path(&config);
        std::fs::create_dir_all(database_path.parent().unwrap()).unwrap();
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(
                sqlx::sqlite::SqliteConnectOptions::new()
                    .filename(&database_path)
                    .create_if_missing(true),
            )
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        pool.close().await;

        let code = self_check(&config, true).await.unwrap();
        assert_eq!(
            code, 1,
            "a fresh database has no routable model yet, so the self-check must fail"
        );
    }
}
