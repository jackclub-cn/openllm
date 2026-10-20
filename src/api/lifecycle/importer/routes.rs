use super::*;
impl<'a> ImportRun<'a> {
    pub(super) async fn routes(&mut self, routes: &[ConfigRoute]) {
        for doc in routes {
            let name = doc.name.trim().to_string();
            let strategy = match RouteStrategy::from_str(doc.strategy.trim()) {
                Ok(value) => value,
                Err(error) => {
                    self.record("route", &name, ACTION_ERROR, error);
                    continue;
                }
            };
            let mut targets = Vec::new();
            // Parallel to `targets`, but with provider ids resolved: this is
            // what the current database rows are compared against.
            let mut desired = Vec::new();
            let mut missing = None;
            for target in &doc.targets {
                match self.providers.get(&target.provider).copied() {
                    Some(provider_id) => {
                        targets.push(RouteTargetInput {
                            id: None,
                            provider_id,
                            upstream_model: target.upstream_model.clone(),
                            weight: target.weight,
                            priority: target.priority,
                            enabled: target.enabled,
                        });
                        desired.push((
                            provider_id,
                            target.upstream_model.clone(),
                            target.weight,
                            target.priority,
                            target.enabled,
                        ));
                    }
                    None => missing = Some(target.provider.clone()),
                }
            }
            if let Some(provider) = missing {
                self.record(
                    "route",
                    &name,
                    ACTION_SKIP,
                    format!("目标引用了不存在的提供商 '{provider}'，整条路由已跳过"),
                );
                continue;
            }
            let existing = self
                .routes
                .iter()
                .find(|(_, existing_name, _, _, _, _, _, _, _)| existing_name == &name)
                .or_else(|| {
                    self.routes
                        .iter()
                        .find(|(_, _, pattern, _, _, _, _, _, _)| pattern == &doc.model_pattern)
                })
                .map(
                    |(
                        id,
                        _,
                        _,
                        context_override,
                        output_override,
                        all_targets_busy_wait_seconds,
                        retry_jitter_ms,
                        max_waiting_requests,
                        keepalive_during_wait,
                    )| {
                        (
                            *id,
                            *context_override,
                            *output_override,
                            *all_targets_busy_wait_seconds,
                            *retry_jitter_ms,
                            *max_waiting_requests,
                            *keepalive_during_wait,
                        )
                    },
                );
            match existing {
                Some((
                    id,
                    current_context_override,
                    current_output_override,
                    current_all_targets_busy_wait_seconds,
                    current_retry_jitter_ms,
                    current_max_waiting_requests,
                    current_keepalive_during_wait,
                )) => {
                    let current = self.route_targets(id).await.unwrap_or_default();
                    if route_targets_match(&current, &desired)
                        && current_context_override == doc.context_override
                        && current_output_override == doc.output_override
                        && current_all_targets_busy_wait_seconds
                            == doc.all_targets_busy_wait_seconds
                        && current_retry_jitter_ms == doc.retry_jitter_ms
                        && current_max_waiting_requests == doc.max_waiting_requests
                        && (current_keepalive_during_wait != 0) == doc.keepalive_during_wait
                    {
                        self.record(
                            "route",
                            &name,
                            ACTION_UNCHANGED,
                            "路由配置未变化".to_string(),
                        );
                        continue;
                    }
                    if !self.apply {
                        self.record(
                            "route",
                            &name,
                            ACTION_UPDATE,
                            format!("将更新路由（{} 个目标）", doc.targets.len()),
                        );
                        continue;
                    }
                    let update = RouteUpdate {
                        name: Some(name.clone()),
                        model_pattern: Some(doc.model_pattern.clone()),
                        strategy: Some(strategy),
                        enabled: Some(doc.enabled),
                        context_override: Some(doc.context_override),
                        output_override: Some(doc.output_override),
                        all_targets_busy_wait_seconds: Some(doc.all_targets_busy_wait_seconds),
                        retry_jitter_ms: Some(doc.retry_jitter_ms),
                        max_waiting_requests: Some(doc.max_waiting_requests),
                        keepalive_during_wait: Some(doc.keepalive_during_wait),
                        targets: Some(targets),
                    };
                    match super::update_route(State(self.state.clone()), Path(id), Json(update))
                        .await
                    {
                        Ok(_) => self.record(
                            "route",
                            &name,
                            ACTION_UPDATE,
                            format!("已更新路由（{} 个目标）", doc.targets.len()),
                        ),
                        Err(error) => {
                            self.record("route", &name, ACTION_ERROR, format!("更新失败：{error}"))
                        }
                    }
                }
                None => {
                    if !self.apply {
                        self.record(
                            "route",
                            &name,
                            ACTION_CREATE,
                            format!("将新建路由（{} 个目标）", doc.targets.len()),
                        );
                        continue;
                    }
                    let input = RouteInput {
                        name: name.clone(),
                        model_pattern: doc.model_pattern.clone(),
                        strategy,
                        enabled: doc.enabled,
                        context_override: doc.context_override,
                        output_override: doc.output_override,
                        all_targets_busy_wait_seconds: Some(doc.all_targets_busy_wait_seconds),
                        retry_jitter_ms: Some(doc.retry_jitter_ms),
                        max_waiting_requests: Some(doc.max_waiting_requests),
                        keepalive_during_wait: Some(doc.keepalive_during_wait),
                        targets,
                    };
                    match super::create_route(State(self.state.clone()), Json(input)).await {
                        Ok((_, Json(view))) => {
                            self.routes.push((
                                view.id,
                                name.clone(),
                                doc.model_pattern.clone(),
                                view.context_override,
                                view.output_override,
                                view.all_targets_busy_wait_seconds,
                                view.retry_jitter_ms,
                                view.max_waiting_requests,
                                i64::from(view.keepalive_during_wait),
                            ));
                            self.record("route", &name, ACTION_CREATE, "已新建路由".to_string());
                        }
                        Err(error) => {
                            self.record("route", &name, ACTION_ERROR, format!("创建失败：{error}"))
                        }
                    }
                }
            }
        }
    }

    pub(super) async fn route_targets(
        &self,
        route_id: i64,
    ) -> AppResult<Vec<(i64, String, i64, i64, bool)>> {
        Ok(sqlx::query_as::<_, (i64, String, i64, i64, i64)>(
            "SELECT provider_id, upstream_model, weight, priority, enabled \
             FROM route_targets WHERE route_id = ? ORDER BY priority, id",
        )
        .bind(route_id)
        .fetch_all(&self.state.pool)
        .await?
        .into_iter()
        .map(|(provider_id, model, weight, priority, enabled)| {
            (provider_id, model, weight, priority, enabled != 0)
        })
        .collect())
    }
}
