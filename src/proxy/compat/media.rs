use super::*;

/// Downlevels the image detail level Codex uses for full-resolution input.
///
/// `original` is part of the newer OpenAI image-detail enum, but several
/// OpenAI-compatible upstreams still accept only `auto`, `low`, or `high`.
pub(crate) fn normalize_image_detail_original(body: &mut Value, supported: &[String]) -> usize {
    let replacement = image_detail_replacement(supported);
    fn replace(detail: &mut Value, replacement: &str) -> usize {
        if detail
            .as_str()
            .is_some_and(|value| value.eq_ignore_ascii_case("original"))
        {
            *detail = json!(replacement);
            1
        } else {
            0
        }
    }

    fn visit(value: &mut Value, replacement: &str) -> usize {
        match value {
            Value::Array(items) => items.iter_mut().map(|item| visit(item, replacement)).sum(),
            Value::Object(object) => {
                let image_part = object
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| matches!(kind, "image_url" | "input_image"));
                let mut changes = 0;
                if image_part {
                    if let Some(detail) = object.get_mut("detail") {
                        changes += replace(detail, replacement);
                    }
                    if let Some(detail) = object
                        .get_mut("image_url")
                        .and_then(Value::as_object_mut)
                        .and_then(|image| image.get_mut("detail"))
                    {
                        changes += replace(detail, replacement);
                    }
                }
                changes
                    + object
                        .values_mut()
                        .map(|value| visit(value, replacement))
                        .sum::<usize>()
            }
            _ => 0,
        }
    }

    visit(body, &replacement)
}

pub(crate) fn rejected_image_detail_options(
    status: StatusCode,
    body: &[u8],
) -> Option<Vec<String>> {
    let (text, values) = super::rejected_enum_options(status, body)?;
    super::is_image_detail_rejection(&text, &values).then_some(values)
}

fn image_detail_replacement(supported: &[String]) -> String {
    for preferred in ["high", "auto"] {
        if let Some(value) = supported
            .iter()
            .find(|value| value.eq_ignore_ascii_case(preferred))
        {
            return value.clone();
        }
    }
    supported
        .iter()
        .find(|value| !value.eq_ignore_ascii_case("original"))
        .cloned()
        .unwrap_or_else(|| "high".to_string())
}
