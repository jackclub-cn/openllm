use super::*;
/// Prometheus exposition type. Monotonic totals must be declared as counters
/// so `rate()` / `increase()` behave correctly; point-in-time state stays a
/// gauge.
#[derive(Debug, Clone, Copy)]
enum MetricKind {
    Counter,
    Gauge,
}

impl MetricKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Counter => "counter",
            Self::Gauge => "gauge",
        }
    }
}

/// Emits the `# HELP` / `# TYPE` pair for a metric family.
///
/// The exposition type is derived from the name: monotonic `*_total` families
/// are counters, everything else is a point-in-time gauge.
pub(super) fn metric_header(output: &mut String, name: &str, help: &str) {
    let kind = if name.ends_with("_total") {
        MetricKind::Counter
    } else {
        MetricKind::Gauge
    };
    write!(
        output,
        "# HELP {name} {help}\n# TYPE {name} {}\n",
        kind.as_str()
    )
    .expect("writing to a String cannot fail");
}

pub(super) fn push_metric(
    output: &mut String,
    name: &str,
    labels: &[(&str, &str)],
    value: impl std::fmt::Display,
) {
    write!(output, "{name}").expect("writing to a String cannot fail");
    if !labels.is_empty() {
        output.push('{');
        for (index, (key, value)) in labels.iter().enumerate() {
            if index > 0 {
                output.push(',');
            }
            write!(output, "{key}=\"{}\"", escape_label(value))
                .expect("writing to a String cannot fail");
        }
        output.push('}');
    }
    writeln!(output, " {value}").expect("writing to a String cannot fail");
}

pub(super) fn escape_label(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '"' => escaped.push_str("\\\""),
            '\n' => escaped.push_str("\\n"),
            _ => escaped.push(character),
        }
    }
    escaped
}
