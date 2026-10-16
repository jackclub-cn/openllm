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
            println!(
                "                {} {}",
                migration.version, migration.description
            );
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
            println!(
                "没有备份。可用 `openllm --backup` 生成一份，或等待下一次启动时的自动预迁移备份。"
            );
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

/// `--export-config`: write the portable configuration document to a file.
pub async fn export_config(
    config: &Config,
    output: &std::path::Path,
    include_secrets: bool,
    json: bool,
) -> anyhow::Result<i32> {
    let database_path = crate::startup::database_path(config);
    if !database_path.exists() {
        anyhow::bail!("{} does not exist yet", database_path.display());
    }
    let pool = crate::startup::open_operator_read_pool(&database_path).await?;
    let state = crate::state::AppState::new(pool.clone(), None);
    let document = crate::api::lifecycle::build_config_document(&state, include_secrets).await?;
    pool.close().await;
    let rendered = serde_json::to_string_pretty(&document)?;

    if output == std::path::Path::new("-") {
        println!("{rendered}");
    } else {
        std::fs::write(output, format!("{rendered}\n"))
            .with_context(|| format!("failed to write {}", output.display()))?;
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "output": output.to_string_lossy().replace('\\', "/"),
                    "include_secrets": include_secrets,
                    "providers": document.providers.len(),
                    "routes": document.routes.len(),
                    "provider_pools": document.provider_pools.len(),
                    "webhooks": document.webhooks.len(),
                }))?
            );
        } else {
            println!(
                "已导出配置: {}（provider {} / route {} / pool {} / webhook {}{}）",
                output.display(),
                document.providers.len(),
                document.routes.len(),
                document.provider_pools.len(),
                document.webhooks.len(),
                if include_secrets {
                    "，含明文凭据"
                } else {
                    ""
                }
            );
        }
    }
    Ok(0)
}

/// `--import-config`: validate or apply a portable configuration document.
pub async fn import_config(
    config: &Config,
    input: &std::path::Path,
    apply: bool,
    json: bool,
) -> anyhow::Result<i32> {
    let database_path = crate::startup::database_path(config);
    if !database_path.exists() {
        anyhow::bail!("{} does not exist yet", database_path.display());
    }
    let raw = std::fs::read_to_string(input)
        .with_context(|| format!("failed to read {}", input.display()))?;
    let document = serde_json::from_str(&raw)
        .with_context(|| format!("{} is not a valid configuration document", input.display()))?;
    let pool = crate::startup::open_operator_pool(&database_path).await?;
    let state = crate::state::AppState::new(pool.clone(), None);
    let axum::Json(plan) = crate::api::lifecycle::import_config(
        axum::extract::State(state),
        axum::Json(crate::models::ImportConfigRequest { document, apply }),
    )
    .await?;
    pool.close().await;

    if json {
        println!("{}", serde_json::to_string_pretty(&plan)?);
    } else {
        println!(
            "{}配置导入（create {} / update {} / unchanged {} / skip {} / error {}）",
            if plan.applied {
                "已应用"
            } else {
                "仅校验"
            },
            plan.counts.create,
            plan.counts.update,
            plan.counts.unchanged,
            plan.counts.skip,
            plan.counts.error
        );
        for problem in &plan.problems {
            println!("问题: {problem}");
        }
        if !plan.applied && plan.valid && plan.counts.error == 0 {
            println!("校验通过；加 --apply 才会写入数据库。");
        }
    }

    Ok(if plan.valid && plan.counts.error == 0 {
        0
    } else {
        1
    })
}

/// `--snapshot`: write the same diagnostic support bundle as the HTTP endpoint.
pub async fn diagnostic_snapshot(
    config: &Config,
    output: &std::path::Path,
    include_secrets: bool,
    json: bool,
) -> anyhow::Result<i32> {
    let database_path = crate::startup::database_path(config);
    if !database_path.exists() {
        anyhow::bail!("{} does not exist yet", database_path.display());
    }
    // Secret-bearing snapshots record an audit row, so they need the normal
    // operator pool. Redacted snapshots stay strictly read-only.
    let pool = if include_secrets {
        crate::startup::open_operator_pool(&database_path).await?
    } else {
        crate::startup::open_operator_read_pool(&database_path).await?
    };
    let state = crate::state::AppState::new(pool.clone(), None);
    let axum::Json(snapshot) = crate::api::diagnostics::diagnostic_snapshot(
        axum::extract::State(state),
        axum::extract::Query(crate::api::diagnostics::SnapshotQuery { include_secrets }),
    )
    .await?;
    pool.close().await;
    let rendered = serde_json::to_string_pretty(&snapshot)?;

    if output == std::path::Path::new("-") {
        println!("{rendered}");
    } else {
        std::fs::write(output, format!("{rendered}\n"))
            .with_context(|| format!("failed to write {}", output.display()))?;
        if json {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "output": output.to_string_lossy().replace('\\', "/"),
                    "include_secrets": include_secrets,
                    "overall": snapshot.self_check.overall,
                    "failed_checks": snapshot.self_check.counts.fail,
                    "warnings": snapshot.self_check.counts.warn,
                }))?
            );
        } else {
            println!(
                "已导出诊断快照: {}（自检 {} / fail {} / warn {}{}）",
                output.display(),
                snapshot.self_check.overall,
                snapshot.self_check.counts.fail,
                snapshot.self_check.counts.warn,
                if include_secrets {
                    "，含明文凭据"
                } else {
                    ""
                }
            );
        }
    }
    Ok(0)
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
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "path": path.to_string_lossy().replace('\\', "/"),
                "size_bytes": metadata.len(),
            }))?
        );
    } else {
        println!(
            "已写入备份: {} ({})",
            path.display(),
            human_size(metadata.len())
        );
        println!(
            "恢复命令:   openllm --restore-backup {}",
            path.file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default()
        );
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
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "restored_from": outcome.restored_from.to_string_lossy().replace('\\', "/"),
                "restored_to": outcome.restored_to.to_string_lossy().replace('\\', "/"),
                "previous": outcome.previous.as_ref().map(|path| path.to_string_lossy().replace('\\', "/")),
                "integrity": outcome.report.integrity,
                "providers": outcome.report.providers,
                "routes": outcome.report.routes,
                "api_keys": outcome.report.api_keys,
                "cleared_leases": outcome.cleared_leases,
            }))?
        );
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
fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
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

    async fn database_config() -> (tempfile::TempDir, Config) {
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
        (directory, config)
    }

    #[tokio::test]
    async fn self_check_runs_against_an_existing_database() {
        let (_directory, config) = database_config().await;
        let code = self_check(&config, true).await.unwrap();
        assert_eq!(
            code, 1,
            "a fresh database has no routable model yet, so the self-check must fail"
        );
    }

    #[tokio::test]
    async fn config_export_and_import_work_offline() {
        let (_directory, config) = database_config().await;
        let output = config.data_dir.join("openllm-config.json");

        assert_eq!(
            export_config(&config, &output, false, true).await.unwrap(),
            0
        );
        let raw = std::fs::read_to_string(&output).unwrap();
        assert!(raw.contains("\"openllm_config_version\": 1"));

        assert_eq!(
            import_config(&config, &output, false, true).await.unwrap(),
            0,
            "validating the same document against its source database must pass"
        );
    }

    #[tokio::test]
    async fn diagnostic_snapshot_works_offline() {
        let (_directory, config) = database_config().await;
        let output = config.data_dir.join("openllm-snapshot.json");

        assert_eq!(
            diagnostic_snapshot(&config, &output, false, true)
                .await
                .unwrap(),
            0
        );
        let document: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&output).unwrap()).unwrap();
        assert!(document.get("self_check").is_some());
        assert!(document.get("resilience").is_some());
        assert_eq!(document["secrets_included"], false);
    }
}
