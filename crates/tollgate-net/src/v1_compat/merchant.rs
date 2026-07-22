//! Advertisement builder for the FIPS mesh / Go v1 tag format.
//!
//! Go v1 uses `["step", "60000"]` while Rust v2 uses `["step_size", "60000"]`.
//! We emit BOTH so either client can parse it (Issue #42, Fix 3).

use serde::Serialize;

/// One tag kind advertised by a TollGate gateway. Go v1 uses short names
/// (`step`); the Rust protocol uses descriptive names (`step_size`). Both are
/// emitted so any client can discover the billing step.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub enum TagKind {
    /// The billing step size in milliseconds.
    StepSize,
    /// Alias used by Go v1 clients.
    Step,
    /// The resource metric being metered.
    Metric,
    /// A custom tag with an arbitrary name.
    #[allow(dead_code)]
    Custom(String),
}

/// A single key-value advertisement tag, serialised as `[name, value]` on the
/// wire (matching the Go v1 / FIPS tag format).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Tag {
    pub kind: TagKind,
    pub values: Vec<String>,
}

impl Tag {
    /// Build a custom tag with one or more values.
    #[allow(dead_code)]
    pub fn custom(kind: TagKind, values: impl IntoIterator<Item = String>) -> Self {
        Self {
            kind,
            values: values.into_iter().collect(),
        }
    }

    /// The tag's name as seen on the wire.
    fn name(&self) -> &str {
        match &self.kind {
            TagKind::StepSize => "step_size",
            TagKind::Step => "step",
            TagKind::Metric => "metric",
            TagKind::Custom(name) => name,
        }
    }
}

/// Configuration snapshot used to build an advertisement.
#[derive(Debug, Clone)]
pub struct AdvertConfig {
    /// Billing step size in milliseconds (e.g. 60000).
    pub step_size: u64,
    /// The resource metric ("milliseconds", "bytes", …).
    pub metric: String,
}

/// Build the advertisement tag list for a gateway, emitting BOTH `step_size`
/// and `step` so Go v1 and Rust clients can parse it interchangeably.
///
/// Returns tags in a stable order: `step_size`, `step`, `metric`.
pub fn build_advertisement(config: &AdvertConfig) -> Vec<Tag> {
    let step_str = config.step_size.to_string();
    let metric_str = config.metric.clone();
    vec![
        Tag {
            kind: TagKind::StepSize,
            values: vec![step_str.clone()],
        },
        Tag {
            kind: TagKind::Step,
            values: vec![step_str],
        },
        Tag {
            kind: TagKind::Metric,
            values: vec![metric_str],
        },
    ]
}

/// Serialize the advertisement tags into the `[["name", "value"], …]` JSON
/// array format used by the Go v1 REST API and FIPS mesh announcements.
pub fn advertisement_json(tags: &[Tag]) -> serde_json::Value {
    serde_json::Value::Array(
        tags.iter()
            .map(|t| {
                serde_json::json!([t.name(), t.values.join(",")])
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertisement_includes_both_step_size_and_step_tags() {
        let config = AdvertConfig {
            step_size: 60000,
            metric: "milliseconds".to_string(),
        };
        let tags = build_advertisement(&config);

        // Must include both step_size and step (Issue #42 Fix 3).
        let names: Vec<&str> = tags.iter().map(|t| t.name()).collect();
        assert!(names.contains(&"step_size"), "must include step_size tag");
        assert!(names.contains(&"step"), "must include step alias tag");
        assert!(names.contains(&"metric"), "must include metric tag");
    }

    #[test]
    fn both_step_tags_carry_same_value() {
        let config = AdvertConfig {
            step_size: 60000,
            metric: "milliseconds".to_string(),
        };
        let tags = build_advertisement(&config);
        let step_size_tag = tags
            .iter()
            .find(|t| t.name() == "step_size")
            .expect("step_size tag");
        let step_tag = tags
            .iter()
            .find(|t| t.name() == "step")
            .expect("step tag");
        assert_eq!(step_size_tag.values, step_tag.values);
        assert_eq!(step_tag.values, vec!["60000"]);
    }

    #[test]
    fn advertisement_json_serialises_as_name_value_pairs() {
        let config = AdvertConfig {
            step_size: 30000,
            metric: "milliseconds".to_string(),
        };
        let tags = build_advertisement(&config);
        let json = advertisement_json(&tags);
        let arr = json.as_array().expect("array");
        assert!(arr.len() >= 2);

        // Find the step_size pair.
        let has_step_size = arr.iter().any(|pair| {
            pair.as_array().map_or(false, |p| {
                p.first().and_then(|v| v.as_str()) == Some("step_size")
                    && p.get(1).and_then(|v| v.as_str()) == Some("30000")
            })
        });
        assert!(has_step_size, "json must contain [\"step_size\", \"30000\"]");

        let has_step = arr.iter().any(|pair| {
            pair.as_array().map_or(false, |p| {
                p.first().and_then(|v| v.as_str()) == Some("step")
                    && p.get(1).and_then(|v| v.as_str()) == Some("30000")
            })
        });
        assert!(has_step, "json must contain [\"step\", \"30000\"]");
    }

    #[test]
    fn step_tag_is_not_removed_when_step_size_present() {
        // Regression: the issue says "Do NOT remove the step_size tag (add step
        // alongside it)". Verify both survive.
        let config = AdvertConfig {
            step_size: 60000,
            metric: "bytes".to_string(),
        };
        let tags = build_advertisement(&config);
        let count_step_size = tags.iter().filter(|t| t.name() == "step_size").count();
        let count_step = tags.iter().filter(|t| t.name() == "step").count();
        assert_eq!(count_step_size, 1, "exactly one step_size tag");
        assert_eq!(count_step, 1, "exactly one step tag");
    }
}
