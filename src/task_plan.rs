use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::i18n::Language;

pub const DISCORD_MESSAGE_LIMIT: usize = 2_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskPlan {
    pub thread_id: String,
    pub turn_id: String,
    pub explanation: Option<String>,
    pub steps: Vec<PlanStep>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanStep {
    pub step: String,
    pub status: PlanStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanStatus {
    Pending,
    InProgress,
    Completed,
}

impl TaskPlan {
    pub fn from_params(params: &Value) -> Result<Self> {
        let thread_id = required_string(params, "threadId")?;
        let turn_id = required_string(params, "turnId")?;
        let explanation = match params.get("explanation") {
            None | Some(Value::Null) => None,
            Some(Value::String(value)) => Some(value.clone()),
            Some(_) => bail!("plan explanation must be a string or null"),
        };
        let raw_steps = params
            .get("plan")
            .and_then(Value::as_array)
            .context("plan must be an array")?;
        let steps = raw_steps
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let step = required_string(value, "step")?;
                let status = match required_string(value, "status")?.as_str() {
                    "pending" => PlanStatus::Pending,
                    "inProgress" => PlanStatus::InProgress,
                    "completed" => PlanStatus::Completed,
                    _ => bail!("unknown task plan status at step {}", index + 1),
                };
                Ok(PlanStep { step, status })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(Self {
            thread_id,
            turn_id,
            explanation,
            steps,
        })
    }

    pub fn render(&self, language: Language) -> String {
        let title = language.text("Task plan", "작업 계획", "任务计划", "作業計画");
        let mut output = title.to_owned();
        if let Some(explanation) = self.explanation.as_deref().filter(|text| !text.is_empty()) {
            output.push('\n');
            output.push_str(explanation);
        }
        for item in &self.steps {
            let (mark, label) = match (language, item.status) {
                (Language::English, PlanStatus::Pending) => ("○", "Pending"),
                (Language::English, PlanStatus::InProgress) => ("▶", "In progress"),
                (Language::English, PlanStatus::Completed) => ("✓", "Completed"),
                (Language::Korean, PlanStatus::Pending) => ("○", "대기"),
                (Language::Korean, PlanStatus::InProgress) => ("▶", "진행 중"),
                (Language::Korean, PlanStatus::Completed) => ("✓", "완료"),
                (Language::Chinese, PlanStatus::Pending) => ("○", "待处理"),
                (Language::Chinese, PlanStatus::InProgress) => ("▶", "进行中"),
                (Language::Chinese, PlanStatus::Completed) => ("✓", "已完成"),
                (Language::Japanese, PlanStatus::Pending) => ("○", "未着手"),
                (Language::Japanese, PlanStatus::InProgress) => ("▶", "進行中"),
                (Language::Japanese, PlanStatus::Completed) => ("✓", "完了"),
            };
            output.push('\n');
            output.push_str(mark);
            output.push(' ');
            output.push_str(label);
            output.push_str(": ");
            output.push_str(&item.step);
        }
        output
    }
}

pub fn event_thread_is_foreign(params: &Value, native_thread_id: &str) -> bool {
    params
        .get("threadId")
        .and_then(Value::as_str)
        .is_some_and(|thread_id| thread_id != native_thread_id)
}

pub fn accepts_observation(previous: Option<u64>, next: u64) -> bool {
    previous.is_none_or(|previous| next > previous)
}

pub fn split_for_discord(text: &str) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut chunks = Vec::new();
    let mut rest = text;
    while rest.chars().count() > DISCORD_MESSAGE_LIMIT {
        let end_byte = rest
            .char_indices()
            .nth(DISCORD_MESSAGE_LIMIT)
            .map(|(index, _)| index)
            .unwrap_or(rest.len());
        let boundary = rest[..end_byte]
            .rfind('\n')
            .filter(|index| *index > 0)
            .map(|index| index + 1)
            .unwrap_or(end_byte);
        chunks.push(rest[..boundary].to_owned());
        rest = &rest[boundary..];
    }
    if !rest.is_empty() {
        chunks.push(rest.to_owned());
    }
    chunks
}

fn required_string(value: &Value, key: &str) -> Result<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .with_context(|| format!("{key} must be a string"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(status: &str) -> Value {
        serde_json::json!({
            "threadId": "thread-test",
            "turnId": "turn-test",
            "explanation": null,
            "plan": [{"step": "Prepare the change", "status": status}]
        })
    }

    #[test]
    fn parses_all_native_status_values_and_rejects_unknown_values() {
        for (raw, expected) in [
            ("pending", PlanStatus::Pending),
            ("inProgress", PlanStatus::InProgress),
            ("completed", PlanStatus::Completed),
        ] {
            assert_eq!(
                TaskPlan::from_params(&params(raw)).unwrap().steps[0].status,
                expected
            );
        }
        assert!(TaskPlan::from_params(&params("blocked")).is_err());
    }

    #[test]
    fn renders_status_labels_in_all_supported_languages() {
        let plan = TaskPlan::from_params(&serde_json::json!({
            "threadId": "thread-test", "turnId": "turn-test", "explanation": "Review",
            "plan": [
                {"step": "One", "status": "pending"},
                {"step": "Two", "status": "inProgress"},
                {"step": "Three", "status": "completed"}
            ]
        }))
        .unwrap();
        for (language, expected) in [
            (
                Language::English,
                "Task plan\nReview\n○ Pending: One\n▶ In progress: Two\n✓ Completed: Three",
            ),
            (
                Language::Korean,
                "작업 계획\nReview\n○ 대기: One\n▶ 진행 중: Two\n✓ 완료: Three",
            ),
            (
                Language::Chinese,
                "任务计划\nReview\n○ 待处理: One\n▶ 进行中: Two\n✓ 已完成: Three",
            ),
            (
                Language::Japanese,
                "作業計画\nReview\n○ 未着手: One\n▶ 進行中: Two\n✓ 完了: Three",
            ),
        ] {
            assert_eq!(plan.render(language), expected);
        }
    }

    #[test]
    fn identifies_foreign_thread_events_and_keeps_connection_wide_events() {
        assert!(!event_thread_is_foreign(
            &serde_json::json!({"threadId": "thread-test"}),
            "thread-test"
        ));
        assert!(event_thread_is_foreign(
            &serde_json::json!({"threadId": "child-thread"}),
            "thread-test"
        ));
        assert!(!event_thread_is_foreign(
            &serde_json::json!({"status": "connected"}),
            "thread-test"
        ));
    }

    #[test]
    fn rejects_duplicate_and_out_of_order_observations() {
        assert!(accepts_observation(None, 1));
        assert!(accepts_observation(Some(8), 9));
        assert!(!accepts_observation(Some(8), 8));
        assert!(!accepts_observation(Some(8), 7));
    }

    #[test]
    fn splits_long_multilingual_content_without_truncation_or_oversized_chunks() {
        let text = format!("{}\n{}", "한字🙂".repeat(900), "次".repeat(1_300));
        let chunks = split_for_discord(&text);
        assert!(chunks.len() > 1);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.chars().count() <= DISCORD_MESSAGE_LIMIT)
        );
        let joined = chunks.concat();
        assert_eq!(joined, text);
    }
}
