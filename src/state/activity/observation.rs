use super::*;

impl RequestActivityStore {
    pub fn context_window(
        &self,
        request_id: &str,
        window: Option<i64>,
        input: Option<i64>,
    ) -> Option<ActivityEvent> {
        self.update(request_id, "updated", |record| {
            let Some(context) = record.snapshot.context.as_mut() else {
                return false;
            };
            context.window_tokens = window.filter(|value| *value > 0);
            context.input_limit = input.filter(|value| *value > 0);
            context.window_source = Some("route".to_string());
            true
        })
    }

    #[cfg(test)]
    pub fn observe_usage(&self, request_id: &str, input_reported: bool, cache_reported: bool) {
        let mut inner = self.lock();
        if let Some(record) = inner.records.get_mut(request_id) {
            record.input_reported |= input_reported;
            record.cache_reported |= cache_reported;
        }
    }

    pub fn report_usage(
        &self,
        request_id: &str,
        usage: Usage,
        input_reported: bool,
        cache_reported: bool,
    ) -> Option<ActivityEvent> {
        self.update(request_id, "updated", |record| {
            let changed = (input_reported
                && record.snapshot.prompt_tokens != Some(usage.prompt_tokens))
                || (cache_reported
                    && (!record.cache_reported
                        || record.snapshot.cache.read_tokens != usage.cache_read_tokens
                        || record.snapshot.cache.write_tokens != usage.cache_write_tokens));
            record.input_reported |= input_reported;
            record.cache_reported |= cache_reported;
            if !changed {
                return false;
            }
            if input_reported {
                record.snapshot.prompt_tokens = Some(usage.prompt_tokens);
            }
            record.apply_usage(Usage {
                prompt_tokens: record.snapshot.prompt_tokens.unwrap_or_default(),
                completion_tokens: usage.completion_tokens,
                cache_read_tokens: if cache_reported {
                    usage.cache_read_tokens
                } else {
                    record.snapshot.cache.read_tokens
                },
                cache_write_tokens: if cache_reported {
                    usage.cache_write_tokens
                } else {
                    record.snapshot.cache.write_tokens
                },
                ..usage
            });
            true
        })
    }
}

impl ActivityRecord {
    pub(super) fn apply_usage(&mut self, usage: Usage) {
        self.snapshot.cache.state = if self.cache_reported {
            "reported"
        } else {
            "unknown"
        }
        .to_string();
        self.snapshot.cache.read_tokens = usage.cache_read_tokens;
        self.snapshot.cache.write_tokens = usage.cache_write_tokens;
        self.snapshot.cache.hit_rate =
            (self.cache_reported && self.input_reported && usage.prompt_tokens > 0).then(|| {
                usage.cache_read_tokens.clamp(0, usage.prompt_tokens) as f64
                    / usage.prompt_tokens as f64
            });
        if self.input_reported
            && let Some(context) = self.snapshot.context.as_mut()
        {
            context.calibrate(usage.prompt_tokens);
        }
    }
}
