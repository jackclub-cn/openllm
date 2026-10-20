use super::*;

pub(in crate::api) async fn replace_route_targets(
    tx: &mut sqlx::Transaction<'_, Sqlite>,
    route_id: i64,
    targets: &[RouteTargetInput],
) -> AppResult<()> {
    validate_targets(targets)?;
    sqlx::query("DELETE FROM route_targets WHERE route_id = ?")
        .bind(route_id)
        .execute(&mut **tx)
        .await?;

    for target in targets {
        sqlx::query(
            "INSERT INTO route_targets (route_id, provider_id, upstream_model, weight, priority, enabled) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(route_id)
        .bind(target.provider_id)
        .bind(target.upstream_model.trim())
        .bind(target.weight)
        .bind(target.priority)
        .bind(target.enabled as i64)
        .execute(&mut **tx)
        .await
        .map_err(map_sqlite_conflict)?;
    }
    Ok(())
}
