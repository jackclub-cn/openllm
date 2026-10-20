use super::*;
impl<'a> ImportRun<'a> {
    pub(super) async fn webhooks(&mut self, webhooks: &[ConfigWebhook]) {
        for doc in webhooks {
            let name = doc.name.trim().to_string();
            let headers = headers_to_apply(&doc.headers, self.secrets_included);
            let existing = self
                .webhooks
                .iter()
                .find(|(_, _, url)| url == &doc.url)
                .or_else(|| {
                    self.webhooks
                        .iter()
                        .find(|(_, existing, _)| existing == &name)
                })
                .map(|(id, _, _)| *id);
            match existing {
                Some(id) => {
                    let facts = match self.webhook_facts(id).await {
                        Ok(facts) => facts,
                        Err(error) => {
                            self.record(
                                "webhook",
                                &name,
                                ACTION_ERROR,
                                format!("读取现有 Webhook 失败：{error}"),
                            );
                            continue;
                        }
                    };
                    if facts.url == doc.url
                        && facts.enabled == doc.enabled
                        && same_set(&facts.event_types, &doc.event_types)
                        && facts.kind == doc.kind
                        && facts.body_fields == doc.body_fields
                        && (!self.secrets_included
                            || header_values_match(&facts.headers, &doc.headers))
                        && (!self.secrets_included
                            || facts.secret.as_deref() == doc.secret.as_deref())
                    {
                        self.record(
                            "webhook",
                            &name,
                            ACTION_UNCHANGED,
                            "Webhook 配置未变化".to_string(),
                        );
                        continue;
                    }
                    if !self.apply {
                        self.record(
                            "webhook",
                            &name,
                            ACTION_UPDATE,
                            "将更新 Webhook".to_string(),
                        );
                        continue;
                    }
                    let update = WebhookUpdate {
                        name: Some(name.clone()),
                        url: Some(doc.url.clone()),
                        secret: if self.secrets_included {
                            doc.secret.clone()
                        } else {
                            None
                        },
                        headers,
                        kind: Some(doc.kind.clone()),
                        body_fields: Some(doc.body_fields.clone()),
                        clear_secret: None,
                        event_types: Some(doc.event_types.clone()),
                        enabled: Some(doc.enabled),
                    };
                    match super::update_webhook(State(self.state.clone()), Path(id), Json(update))
                        .await
                    {
                        Ok(_) => self.record(
                            "webhook",
                            &name,
                            ACTION_UPDATE,
                            "已更新 Webhook".to_string(),
                        ),
                        Err(error) => self.record(
                            "webhook",
                            &name,
                            ACTION_ERROR,
                            format!("更新失败：{error}"),
                        ),
                    }
                }
                None => {
                    if !self.apply {
                        self.record(
                            "webhook",
                            &name,
                            ACTION_CREATE,
                            "将新建 Webhook".to_string(),
                        );
                        continue;
                    }
                    let input = WebhookInput {
                        name: name.clone(),
                        url: doc.url.clone(),
                        secret: if self.secrets_included {
                            doc.secret.clone()
                        } else {
                            None
                        },
                        headers,
                        kind: Some(doc.kind.clone()),
                        body_fields: Some(doc.body_fields.clone()),
                        event_types: Some(doc.event_types.clone()),
                        enabled: doc.enabled,
                    };
                    match super::create_webhook(State(self.state.clone()), Json(input)).await {
                        Ok((_, Json(view))) => {
                            self.webhooks.push((view.id, name.clone(), doc.url.clone()));
                            self.record(
                                "webhook",
                                &name,
                                ACTION_CREATE,
                                "已新建 Webhook".to_string(),
                            );
                        }
                        Err(error) => self.record(
                            "webhook",
                            &name,
                            ACTION_ERROR,
                            format!("创建失败：{error}"),
                        ),
                    }
                }
            }
        }
    }

    /// Current state of one webhook, used to decide whether a document would
    /// change anything.
    pub(super) async fn webhook_facts(&self, id: i64) -> AppResult<WebhookFacts> {
        let (url, enabled, event_types, headers, kind, body_fields, secret) =
            sqlx::query_as::<_, (String, i64, String, String, String, String, Option<String>)>(
                "SELECT url, enabled, event_types, headers, kind, body_fields, secret \
             FROM webhooks WHERE id = ?",
            )
            .bind(id)
            .fetch_one(&self.state.pool)
            .await?;
        Ok(WebhookFacts {
            url,
            enabled: enabled != 0,
            event_types: parse_string_list(&event_types),
            headers: parse_json_or_empty(&headers),
            kind,
            body_fields: parse_json_or_empty(&body_fields),
            secret: secret.filter(|secret| !secret.is_empty()),
        })
    }
}
