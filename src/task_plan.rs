use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::i18n::{Language, MessageKey};

pub const DISCORD_MESSAGE_LIMIT: usize = 2_000;
pub const DISCORD_EMBED_DESCRIPTION_LIMIT: usize = 4_096;
pub const PLAN_ACTIVE_COLOR: u32 = 0x3498DB;
pub const PLAN_COMPLETED_COLOR: u32 = 0x2ECC71;

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
            Some(_) => bail!(
                "{}",
                Language::English.message(MessageKey::TaskPlanExplanationType)
            ),
        };
        let raw_steps = params
            .get("plan")
            .and_then(Value::as_array)
            .context(Language::English.message(MessageKey::TaskPlanArrayRequired))?;
        let steps = raw_steps
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let step = required_string(value, "step")?;
                let status = match required_string(value, "status")?.as_str() {
                    "pending" => PlanStatus::Pending,
                    "inProgress" => PlanStatus::InProgress,
                    "completed" => PlanStatus::Completed,
                    _ => bail!(
                        "{}",
                        Language::English
                            .message(MessageKey::TaskPlanUnknownStatus)
                            .replace("{step}", &(index + 1).to_string())
                    ),
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
        let title = language.message(MessageKey::TaskPlanTitle);
        let mut output = title.to_owned();
        if let Some(explanation) = self.explanation.as_deref().filter(|text| !text.is_empty()) {
            output.push('\n');
            output.push_str(explanation);
        }
        for item in &self.steps {
            let (mark, label) = match (language, item.status) {
                (_, PlanStatus::Pending) => ("○", language.message(MessageKey::TaskPlanPending)),
                (_, PlanStatus::InProgress) => {
                    ("▶", language.message(MessageKey::TaskPlanInProgress))
                }
                (_, PlanStatus::Completed) => ("✅", language.message(MessageKey::TaskPlanDone)),
            };
            output.push('\n');
            output.push_str(mark);
            output.push(' ');
            output.push_str(label);
            output.push_str(": ");
            if item.status == PlanStatus::Completed {
                output.push_str("~~");
                output.push_str(&item.step);
                output.push_str("~~");
            } else {
                output.push_str(&item.step);
            }
        }
        output
    }

    pub fn render_cards(&self, language: Language) -> Vec<PlanCard> {
        let completed = self
            .steps
            .iter()
            .filter(|step| step.status == PlanStatus::Completed)
            .count();
        let total = self.steps.len();
        let title = language.message(MessageKey::TaskPlanTitle);
        let completed_label = language.message(MessageKey::TaskPlanCompleted);
        let title = format!("{title} · {completed}/{total} {completed_label}");
        let color = if completed == total {
            PLAN_COMPLETED_COLOR
        } else {
            PLAN_ACTIVE_COLOR
        };
        let description = self.render(language);
        let body = description
            .split_once('\n')
            .map(|(_, body)| body)
            .unwrap_or_default();
        let body = if body.is_empty() { "\u{200b}" } else { body };
        split_embed_description(body)
            .into_iter()
            .map(|description| PlanCard {
                title: title.clone(),
                description,
                color,
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlanCard {
    pub title: String,
    pub description: String,
    pub color: u32,
}

pub fn split_embed_description(text: &str) -> Vec<String> {
    if text.is_empty() {
        return vec!["\u{200b}".to_owned()];
    }
    let mut chunks = Vec::new();
    let mut rest = text;
    while rest.encode_utf16().count() > DISCORD_EMBED_DESCRIPTION_LIMIT {
        let mut end_byte = 0;
        let mut code_units = 0;
        for (index, character) in rest.char_indices() {
            if code_units + character.len_utf16() > DISCORD_EMBED_DESCRIPTION_LIMIT {
                break;
            }
            code_units += character.len_utf16();
            end_byte = index + character.len_utf8();
        }
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
                "Task plan\nReview\n○ Pending: One\n▶ In progress: Two\n✅ Completed: ~~Three~~",
            ),
            (
                Language::Korean,
                "작업 계획\nReview\n○ 대기: One\n▶ 진행 중: Two\n✅ 완료: ~~Three~~",
            ),
            (
                Language::Chinese,
                "任务计划\nReview\n○ 待处理: One\n▶ 进行中: Two\n✅ 已完成: ~~Three~~",
            ),
            (
                Language::Japanese,
                "作業計画\nReview\n○ 未着手: One\n▶ 進行中: Two\n✅ 完了: ~~Three~~",
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
    fn builds_colored_cards_with_completion_count_and_full_status_text() {
        let plan = TaskPlan::from_params(&serde_json::json!({
            "threadId": "thread-test", "turnId": "turn-test", "explanation": "Review",
            "plan": [
                {"step": "One", "status": "completed"},
                {"step": "Two", "status": "inProgress"},
                {"step": "Three", "status": "pending"}
            ]
        }))
        .unwrap();
        let cards = plan.render_cards(Language::English);
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].title, "Task plan · 1/3 completed");
        assert_eq!(
            cards[0].description,
            "Review\n✅ Completed: ~~One~~\n▶ In progress: Two\n○ Pending: Three"
        );
        assert_eq!(cards[0].color, PLAN_ACTIVE_COLOR);

        let complete = TaskPlan::from_params(&serde_json::json!({
            "threadId": "thread-test", "turnId": "turn-test", "explanation": null,
            "plan": [{"step": "One", "status": "completed"}]
        }))
        .unwrap();
        assert_eq!(
            complete.render_cards(Language::English)[0].color,
            PLAN_COMPLETED_COLOR
        );
    }

    #[test]
    fn splits_long_card_descriptions_at_embed_limits_without_losing_text() {
        let description = format!("{}\n{}", "한字🙂".repeat(1_500), "x".repeat(4_500));
        let chunks = split_embed_description(&description);
        assert!(chunks.len() > 1);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.encode_utf16().count() <= DISCORD_EMBED_DESCRIPTION_LIMIT)
        );
        assert_eq!(chunks.concat(), description);
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
