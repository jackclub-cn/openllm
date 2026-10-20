use super::*;
impl<'a> ImportRun<'a> {
    pub(super) async fn pools(&mut self, pools: &[ConfigPool]) {
        for doc in pools {
            let label = format!("{} / {}", doc.provider, doc.name);
            let Some(provider_id) = self.providers.get(&doc.provider).copied() else {
                self.record(
                    "provider_pool",
                    &label,
                    ACTION_SKIP,
                    format!("提供商 '{}' 不存在，配额池已跳过", doc.provider),
                );
                continue;
            };
            let mut allocations = Vec::new();
            for allocation in &doc.allocations {
                match self.api_keys.get(&allocation.api_key) {
                    Some(ids) if ids.len() == 1 => allocations.push(ProviderPoolAllocationInput {
                        api_key_id: ids[0],
                        weight_pct: allocation.weight_pct,
                        policy: allocation.policy.clone(),
                    }),
                    Some(ids) if ids.is_empty() => self.problems.push(format!(
                        "配额池 '{}' 的分配引用了不存在的网关 API Key '{}'，该分配已跳过",
                        doc.name, allocation.api_key
                    )),
                    Some(_) => self.problems.push(format!(
                        "配额池 '{}' 的分配引用了不唯一的网关 API Key '{}'，该分配已跳过",
                        doc.name, allocation.api_key
                    )),
                    None => self.problems.push(format!(
                        "配额池 '{}' 的分配引用了不存在的网关 API Key '{}'，该分配已跳过",
                        doc.name, allocation.api_key
                    )),
                }
            }
            let existing = self
                .pools
                .iter()
                .find(|(_, existing_provider, existing_name)| {
                    *existing_provider == provider_id && existing_name == &doc.name
                })
                .map(|(id, _, _)| *id);
            match existing {
                Some(id) => {
                    let current = self.pool_allocations(id).await.unwrap_or_default();
                    if pool_allocations_match(&current, &allocations) {
                        self.record(
                            "provider_pool",
                            &label,
                            ACTION_UNCHANGED,
                            "配额池配置未变化".to_string(),
                        );
                        continue;
                    }
                    if !self.apply {
                        self.record(
                            "provider_pool",
                            &label,
                            ACTION_UPDATE,
                            format!("将更新配额池（{} 个分配）", allocations.len()),
                        );
                        continue;
                    }
                    let update = ProviderPoolUpdate {
                        name: Some(doc.name.clone()),
                        enabled: Some(doc.enabled),
                        window_seconds: Some(doc.window_seconds),
                        request_limit: Some(doc.request_limit),
                        token_limit: Some(doc.token_limit),
                        saturation_threshold_pct: Some(doc.saturation_threshold_pct),
                        allocations: Some(allocations),
                    };
                    match super::update_provider_pool(
                        State(self.state.clone()),
                        Path(id),
                        Json(update),
                    )
                    .await
                    {
                        Ok(_) => self.record(
                            "provider_pool",
                            &label,
                            ACTION_UPDATE,
                            "已更新配额池".to_string(),
                        ),
                        Err(error) => self.record(
                            "provider_pool",
                            &label,
                            ACTION_ERROR,
                            format!("更新失败：{error}"),
                        ),
                    }
                }
                None => {
                    if !self.apply {
                        self.record(
                            "provider_pool",
                            &label,
                            ACTION_CREATE,
                            format!("将新建配额池（{} 个分配）", allocations.len()),
                        );
                        continue;
                    }
                    let input = ProviderPoolInput {
                        provider_id,
                        name: doc.name.clone(),
                        enabled: doc.enabled,
                        window_seconds: doc.window_seconds,
                        request_limit: doc.request_limit,
                        token_limit: doc.token_limit,
                        saturation_threshold_pct: doc.saturation_threshold_pct,
                        allocations,
                    };
                    match super::create_provider_pool(State(self.state.clone()), Json(input)).await
                    {
                        Ok(Json(view)) => {
                            self.pools.push((view.id, provider_id, doc.name.clone()));
                            self.record(
                                "provider_pool",
                                &label,
                                ACTION_CREATE,
                                "已新建配额池".to_string(),
                            );
                        }
                        Err(error) => self.record(
                            "provider_pool",
                            &label,
                            ACTION_ERROR,
                            format!("创建失败：{error}"),
                        ),
                    }
                }
            }
        }
    }

    pub(super) async fn pool_allocations(
        &self,
        pool_id: i64,
    ) -> AppResult<Vec<(i64, i64, String)>> {
        Ok(sqlx::query_as::<_, (i64, i64, String)>(
            "SELECT api_key_id, weight_pct, policy FROM provider_pool_allocations \
             WHERE pool_id = ? ORDER BY api_key_id",
        )
        .bind(pool_id)
        .fetch_all(&self.state.pool)
        .await?)
    }
}
