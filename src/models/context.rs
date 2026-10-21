use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextCategory {
    pub key: String,
    pub tokens: i64,
    #[serde(default)]
    pub items: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBreakdown {
    pub window_tokens: Option<i64>,
    #[serde(default)]
    pub input_limit: Option<i64>,
    #[serde(default)]
    pub window_source: Option<String>,
    pub input_tokens: i64,
    pub estimated: bool,
    #[serde(default = "input_estimated_default")]
    pub input_estimated: bool,
    pub categories: Vec<ContextCategory>,
    #[serde(default)]
    pub unmetered_items: usize,
}

impl ContextBreakdown {
    pub fn calibrate(&mut self, actual_input_tokens: i64) {
        if actual_input_tokens < 0 {
            return;
        }
        if self.unmetered_items > 0 {
            self.categories
                .retain(|category| category.key != "unattributed");
            let text_estimate = self
                .categories
                .iter()
                .map(|category| category.tokens)
                .sum::<i64>();
            if actual_input_tokens >= text_estimate {
                self.categories.push(ContextCategory {
                    key: "unattributed".to_string(),
                    tokens: actual_input_tokens - text_estimate,
                    items: self.unmetered_items,
                });
                self.input_tokens = actual_input_tokens;
                self.input_estimated = false;
                return;
            }
        }
        let estimated_total = self
            .categories
            .iter()
            .map(|category| category.tokens.max(0))
            .sum::<i64>();
        if estimated_total <= 0 {
            self.categories = vec![ContextCategory {
                key: "unattributed".to_string(),
                tokens: actual_input_tokens,
                items: 0,
            }];
        } else {
            let mut allocated = 0i64;
            let count = self.categories.len();
            for (index, category) in self.categories.iter_mut().enumerate() {
                if index + 1 == count {
                    category.tokens = actual_input_tokens.saturating_sub(allocated);
                } else {
                    category.tokens = ((category.tokens.max(0) as i128
                        * actual_input_tokens as i128)
                        / estimated_total as i128) as i64;
                    allocated = allocated.saturating_add(category.tokens);
                }
            }
        }
        self.input_tokens = actual_input_tokens;
        self.input_estimated = false;
    }
}

fn input_estimated_default() -> bool {
    true
}
