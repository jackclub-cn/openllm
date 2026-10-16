use super::*;

/// How many audit rows are retained. Old rows are pruned after each write so a
/// long-lived gateway cannot accumulate unbounded configuration history.
const AUDIT_HISTORY_LIMIT: i64 = 5_000;

/// The actor label recorded for configuration changes.
///
/// The gateway authenticates admin traffic with one shared token, so there is
/// no per-user identity to record. "admin" means the request required the admin
/// token; "local" means the admin API was open.
fn audit_actor(state: &AppState) -> &'static str {
    if state.admin_token.is_some() {
        "admin"
    } else {
        "local"
    }
}

/// Appends one configuration change to the audit trail.
///
/// Auditing must never break the operation it describes: a failed insert is
/// logged and swallowed so a full disk or locked database cannot turn a
/// successful change into an error.
pub(crate) async fn record_audit(
    state: &AppState,
    action: &str,
    entity: &str,
    entity_id: Option<&str>,
    summary: &str,
    detail: Option<Value>,
) {
    let detail = detail.map(|value| value.to_string());
    let insert = sqlx::query(
        "INSERT INTO audit_logs (action, entity, entity_id, summary, detail, actor) \
         VALUES (?, ?, ?, ?, ?, ?)",
    )
    .bind(action)
    .bind(entity)
    .bind(entity_id)
    .bind(summary)
    .bind(detail)
    .bind(audit_actor(state))
    .execute(&state.pool)
    .await;
    if let Err(error) = insert {
        tracing::warn!(%error, action, entity, "failed to record audit log entry");
        return;
    }
    let prune = sqlx::query(
        "DELETE FROM audit_logs WHERE id NOT IN ( \
             SELECT id FROM audit_logs ORDER BY id DESC LIMIT ? \
         )",
    )
    .bind(AUDIT_HISTORY_LIMIT)
    .execute(&state.pool)
    .await;
    if let Err(error) = prune {
        tracing::warn!(%error, "failed to prune audit log entries");
    }
}

pub async fn list_audit_logs(
    State(state): State<AppState>,
    Query(query): Query<AuditLogQuery>,
) -> AppResult<Json<AuditLogPage>> {
    let page = query.page.unwrap_or(1).max(1);
    let page_size = query.page_size.unwrap_or(50).clamp(1, 200);
    let offset = (page - 1) * page_size;

    let entity = query
        .entity
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let action = query
        .action
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let search = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("%{}%", value.replace('%', "\\%").replace('_', "\\_")));

    let mut count = QueryBuilder::<Sqlite>::new("SELECT COUNT(*) FROM audit_logs WHERE 1 = 1");
    apply_audit_filters(&mut count, entity, action, search.as_deref());
    let total = count
        .build_query_scalar::<i64>()
        .fetch_one(&state.pool)
        .await?;

    let mut items = QueryBuilder::<Sqlite>::new(
        "SELECT id, created_at, action, entity, entity_id, summary, detail, actor \
         FROM audit_logs WHERE 1 = 1",
    );
    apply_audit_filters(&mut items, entity, action, search.as_deref());
    items
        .push(" ORDER BY id DESC LIMIT ")
        .push_bind(page_size)
        .push(" OFFSET ")
        .push_bind(offset);
    let rows = items
        .build_query_as::<AuditLog>()
        .fetch_all(&state.pool)
        .await?;

    Ok(Json(AuditLogPage {
        items: rows.into_iter().map(Into::into).collect(),
        total,
        page,
        page_size,
    }))
}

/// Largest number of audit rows returned by one export.
///
/// The table is pruned to `AUDIT_HISTORY_LIMIT` rows after every write, so this
/// still returns the entire retained trail; the truncation header is kept for
/// safety in case that retention ever grows past this cap.
const AUDIT_EXPORT_LIMIT: i64 = AUDIT_HISTORY_LIMIT;

/// Streams the retained audit trail as CSV so an operator can archive it or
/// pull it into a spreadsheet for review.
///
/// It reuses the same filters as the paged listing (`entity`, `action`,
/// `search`) so what you see in the console is what you export.
pub async fn export_audit_logs(
    State(state): State<AppState>,
    Query(query): Query<AuditLogQuery>,
) -> AppResult<Response> {
    let entity = query
        .entity
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let action = query
        .action
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let search = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("%{}%", value.replace('%', "\\%").replace('_', "\\_")));

    let mut items = QueryBuilder::<Sqlite>::new(
        "SELECT id, created_at, action, entity, entity_id, summary, detail, actor \
         FROM audit_logs WHERE 1 = 1",
    );
    apply_audit_filters(&mut items, entity, action, search.as_deref());
    items
        .push(" ORDER BY id DESC LIMIT ")
        .push_bind(AUDIT_EXPORT_LIMIT);
    let rows = items
        .build_query_as::<AuditLog>()
        .fetch_all(&state.pool)
        .await?;
    let truncated = rows.len() as i64 == AUDIT_EXPORT_LIMIT;

    let mut csv =
        String::from("\u{feff}created_at,id,action,entity,entity_id,actor,summary,detail\r\n");
    for row in rows {
        let fields = [
            row.created_at,
            row.id.to_string(),
            row.action,
            row.entity,
            row.entity_id.unwrap_or_default(),
            row.actor,
            row.summary,
            row.detail.unwrap_or_default(),
        ];
        csv.push_str(
            &fields
                .into_iter()
                .map(|value| csv_field(&value))
                .collect::<Vec<_>>()
                .join(","),
        );
        csv.push_str("\r\n");
    }

    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/csv; charset=utf-8")
        .header(
            header::CONTENT_DISPOSITION,
            "attachment; filename=\"openllm-audit-logs.csv\"",
        );
    if truncated {
        response = response.header("x-openllm-export-truncated", "true");
    }
    Ok(response
        .body(Body::from(csv))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response()))
}

fn apply_audit_filters(
    query: &mut QueryBuilder<'_, Sqlite>,
    entity: Option<&str>,
    action: Option<&str>,
    search: Option<&str>,
) {
    if let Some(entity) = entity {
        query.push(" AND entity = ").push_bind(entity.to_string());
    }
    if let Some(action) = action {
        query.push(" AND action = ").push_bind(action.to_string());
    }
    if let Some(search) = search {
        query
            .push(" AND (summary LIKE ")
            .push_bind(search.to_string())
            .push(" ESCAPE '\\' OR entity_id LIKE ")
            .push_bind(search.to_string())
            .push(" ESCAPE '\\')");
    }
}
