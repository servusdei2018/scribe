//! `sphinx.ext.duration`: per-document reading timings and report rendering.

use std::collections::BTreeMap;
use scribe_config::Config;

/// Accumulates reading durations keyed by docname.
#[derive(Clone, Debug, Default)]
pub struct DurationReport {
    durations: BTreeMap<String, f64>,
}

impl DurationReport {
    /// Record the reading duration of one document.
    pub fn note(&mut self, docname: impl Into<String>, seconds: f64) {
        self.durations.insert(docname.into(), seconds);
    }

    /// All recorded durations.
    #[must_use]
    pub fn durations(&self) -> &BTreeMap<String, f64> {
        &self.durations
    }

    /// Whether any timing was recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.durations.is_empty()
    }

    /// Render the upstream-shaped plain-text report gated by configuration.
    /// Returns an empty string when both sections are disabled or no durations
    /// were recorded.
    #[must_use]
    pub fn render(&self, config: &Config) -> String {
        if self.durations.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        if config.get_bool("duration_print_total").unwrap_or(true) {
            let total: f64 = self.durations.values().sum();
            let minutes = (total / 60.0).floor() as u64;
            let seconds = total - minutes as f64 * 60.0;
            let count = self.durations.len();
            out.push_str("\n====================== total reading duration ==========================\n");
            out.push_str(&format!(
                "Total time reading {count} file{}: {minutes}m {seconds:.3}s\n",
                if count == 1 { "" } else { "s" }
            ));
        }
        if config.get_bool("duration_print_slowest").unwrap_or(true) {
            let mut sorted: Vec<(&str, f64)> = self
                .durations
                .iter()
                .map(|(name, seconds)| (name.as_str(), *seconds))
                .collect();
            sorted.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(b.0)));
            let configured = config.get_int("duration_n_slowest").unwrap_or(5).max(0) as usize;
            let limit = if configured == 0 { sorted.len() } else { configured.min(sorted.len()) };
            out.push_str("\n\n====================== slowest reading durations =======================\n");
            for (name, seconds) in sorted.iter().take(limit) {
                out.push_str(&format!("{seconds:.3}s {name}\n"));
            }
            out.push('\n');
        }
        out
    }

    /// Render the optional JSON payload (`duration_write_json`).
    #[must_use]
    pub fn to_json(&self) -> String {
        let mut entries: Vec<(&str, serde_json::Value)> = self
            .durations
            .iter()
            .map(|(name, seconds)| {
                let rounded = (seconds * 1000.0).round() / 1000.0;
                (name.as_str(), serde_json::json!(rounded))
            })
            .collect();
        entries.sort_by(|a, b| a.0.cmp(b.0));
        let object: serde_json::Map<String, serde_json::Value> = entries
            .into_iter()
            .map(|(name, value)| (name.to_owned(), value))
            .collect();
        serde_json::to_string_pretty(&serde_json::Value::Object(object)).unwrap_or_else(|_| "{}".to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scribe_config::ConfigValue;

    #[test]
    fn report_is_gated_and_sorted() {
        let mut report = DurationReport::default();
        report.note("slow", 2.5);
        report.note("fast", 0.1);
        let config = Config::new();
        let text = report.render(&config);
        assert!(text.contains("total reading duration"));
        assert!(text.contains("slowest reading durations"));
        assert!(text.find("2.500s slow").unwrap() < text.find("0.100s fast").unwrap());
        let mut quiet = Config::new();
        quiet.set("duration_print_total", ConfigValue::Bool(false)).unwrap();
        quiet.set("duration_print_slowest", ConfigValue::Bool(false)).unwrap();
        assert!(report.render(&quiet).is_empty());
    }

    #[test]
    fn json_output_is_sorted_and_rounded() {
        let mut report = DurationReport::default();
        report.note("b", 1.23456);
        report.note("a", 0.5);
        assert_eq!(report.to_json(), "{\n  \"a\": 0.5,\n  \"b\": 1.235\n}");
    }
}
