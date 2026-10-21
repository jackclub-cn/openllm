use super::*;
use crate::state::{ActivityList, RequestActivitySnapshot};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub struct RecentActivityQuery {
    pub limit: Option<usize>,
}

pub async fn recent_activity(
    State(state): State<AppState>,
    Query(query): Query<RecentActivityQuery>,
) -> Json<ActivityList> {
    Json(
        state
            .activity
            .recent(query.limit.unwrap_or(50).clamp(1, 200)),
    )
}

pub async fn get_request_activity(
    State(state): State<AppState>,
    Path(request_id): Path<String>,
) -> AppResult<Json<RequestActivitySnapshot>> {
    state
        .activity
        .snapshot(&request_id)
        .map(Json)
        .ok_or_else(|| AppError::NotFound("request activity not found".to_string()))
}
