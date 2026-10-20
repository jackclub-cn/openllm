use super::*;
impl<'a> ImportRun<'a> {
    pub(super) async fn settings(&mut self, settings: ConfigSettings) {
        if let Some(guardrails) = settings.guardrails {
            let unchanged = self
                .state
                .guardrail_settings()
                .await
                .map(|current| current == guardrails)
                .unwrap_or(false);
            if unchanged {
                self.record(
                    "setting",
                    "guardrails",
                    ACTION_UNCHANGED,
                    "护栏设置未变化".to_string(),
                );
            } else if !self.apply {
                self.record(
                    "setting",
                    "guardrails",
                    ACTION_UPDATE,
                    "将更新护栏设置".to_string(),
                );
            } else {
                let result =
                    super::update_guardrails_settings(State(self.state.clone()), Json(guardrails))
                        .await;
                self.settings_result("guardrails", "护栏设置", result);
            }
        }
        if let Some(inspector) = settings.inspector {
            let unchanged = self
                .state
                .inspector_settings()
                .await
                .map(|current| current == inspector)
                .unwrap_or(false);
            if unchanged {
                self.record(
                    "setting",
                    "inspector",
                    ACTION_UNCHANGED,
                    "请求预览设置未变化".to_string(),
                );
            } else if !self.apply {
                self.record(
                    "setting",
                    "inspector",
                    ACTION_UPDATE,
                    "将更新请求预览设置".to_string(),
                );
            } else {
                let result =
                    super::update_inspector_settings(State(self.state.clone()), Json(inspector))
                        .await;
                self.settings_result("inspector", "请求预览设置", result);
            }
        }
        if let Some(resilience) = settings.resilience {
            let unchanged = self
                .state
                .resilience_settings()
                .await
                .map(|current| current == resilience)
                .unwrap_or(false);
            if unchanged {
                self.record(
                    "setting",
                    "resilience",
                    ACTION_UNCHANGED,
                    "重试与流恢复设置未变化".to_string(),
                );
            } else if !self.apply {
                self.record(
                    "setting",
                    "resilience",
                    ACTION_UPDATE,
                    "将更新重试与流恢复设置".to_string(),
                );
            } else {
                let result =
                    super::update_resilience_settings(State(self.state.clone()), Json(resilience))
                        .await;
                self.settings_result("resilience", "重试与流恢复设置", result);
            }
        }
        if let Some(days) = settings.usage_retention_days {
            let current = stored_usage_retention_days(self.state).await.ok().flatten();
            if current == Some(days) {
                self.record(
                    "setting",
                    "usage_retention_days",
                    ACTION_UNCHANGED,
                    format!("用量日志保留 {days} 天，未变化"),
                );
            } else if self.apply {
                let result = super::update_runtime_settings(
                    State(self.state.clone()),
                    Json(RuntimeSettingsUpdate {
                        usage_retention_days: Some(days),
                    }),
                )
                .await;
                self.settings_result("usage_retention_days", "用量日志保留设置", result);
            } else {
                self.record(
                    "setting",
                    "usage_retention_days",
                    ACTION_UPDATE,
                    format!("将用量日志保留时间设置为 {days} 天"),
                );
            }
        }
    }

    pub(super) fn settings_result<T: std::fmt::Debug>(
        &mut self,
        key: &'static str,
        label: &str,
        result: AppResult<T>,
    ) {
        match result {
            Ok(_) => self.record("setting", key, ACTION_UPDATE, format!("已更新{label}")),
            Err(error) => {
                let detail = format!("更新{label}失败：{error}");
                self.record("setting", key, ACTION_ERROR, detail.clone());
                self.problems.push(detail);
            }
        }
    }
}
