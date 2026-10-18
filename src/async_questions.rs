//! Durable state and rendering helpers for Codex's non-blocking question items.
//!
//! The native identity is the Responses API call id plus the question's array index.
//! Discord interaction ids are separate, durable random tokens so they stay useful
//! after the Discord worker restarts and never depend on a server request id.

use std::{fs, io::Write, path::Path};

use anyhow::{Context as _, Result, bail};
use serde::{Deserialize, Serialize};
use serenity::{
    all::{
        ActionRowComponent, ButtonStyle, ChannelId, ComponentInteraction, Context, CreateActionRow,
        CreateButton, CreateInteractionResponse, CreateInteractionResponseMessage, CreateMessage,
        InputTextStyle, ModalInteraction,
    },
    builder::{CreateInputText, CreateModal, EditInteractionResponse},
    http::Http,
};

use crate::i18n::Language;

const QUESTION_TOOL_NAME: &str = "request_user_input_async";
const REPLY_OPEN: &str = "<send_user_message_question_reply>";
const REPLY_CLOSE: &str = "</send_user_message_question_reply>";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AsyncQuestionInput {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AsyncQuestionRequest {
    pub call_id: String,
    #[serde(default)]
    pub thread_id: String,
    #[serde(default)]
    pub turn_id: String,
    pub questions: Vec<AsyncQuestion>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct AsyncQuestion {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<String>>,
    /// Stable across worker restarts and used only for Discord components.
    pub interaction_id: String,
    #[serde(default)]
    pub presentation_sent: bool,
    #[serde(default)]
    pub presentation_message_ids: Vec<Option<u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_target: Option<StoredReplyTarget>,
    #[serde(default)]
    pub awaiting_notice_sent: bool,
    #[serde(default)]
    pub delivery: QuestionDelivery,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuestionDelivery {
    #[default]
    Pending,
    Ready,
    AwaitingConfirmation,
    Sent,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct AsyncQuestionStore {
    #[serde(default)]
    pub requests: Vec<AsyncQuestionRequest>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingQuestion {
    pub call_id: String,
    pub thread_id: String,
    pub turn_id: String,
    pub question_index: usize,
    pub item_id: String,
    pub interaction_id: String,
    pub presentation_sent: bool,
    pub presentation_message_ids: Vec<Option<u64>>,
    pub reply_target: Option<StoredReplyTarget>,
    pub awaiting_notice_sent: bool,
    pub title: String,
    pub options: Option<Vec<String>>,
    pub delivery: QuestionDelivery,
    pub answer: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueueObservation {
    Accepted,
    Queued,
    Submitting,
    Unknown,
    Rejected,
    Missing,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconcileResult {
    Unchanged,
    MarkedSent,
    AwaitingConfirmation,
    ReleasedForRetry,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct StoredReplyTarget {
    pub channel_id: u64,
    pub message_id: u64,
    pub source_guild: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
struct NativeAnswer {
    answer: String,
    question: String,
    question_item_id: String,
}

impl AsyncQuestionStore {
    /// Load state from the supplied file path. Missing state starts empty.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .with_context(|| format!("parse async question state {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
        }
    }

    /// Save state atomically alongside the requested file.
    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .with_context(|| format!("create state directory {}", parent.display()))?;
        let bytes = serde_json::to_vec_pretty(self)?;
        let mut temp = tempfile::NamedTempFile::new_in(parent).with_context(|| {
            format!(
                "create temporary async question state in {}",
                parent.display()
            )
        })?;
        temp.write_all(&bytes).with_context(|| {
            format!(
                "write temporary async question state in {}",
                parent.display()
            )
        })?;
        temp.persist(path)
            .map_err(|error| error.error)
            .with_context(|| format!("replace async question state {}", path.display()))?;
        Ok(())
    }

    /// Add one completed async item. Replayed call ids are idempotently ignored.
    pub fn insert(
        &mut self,
        call_id: impl Into<String>,
        thread_id: impl Into<String>,
        turn_id: impl Into<String>,
        questions: Vec<AsyncQuestionInput>,
    ) -> Result<bool> {
        let call_id = call_id.into();
        if call_id.is_empty() {
            bail!("async question call id must not be empty");
        }
        if self
            .requests
            .iter()
            .any(|request| request.call_id == call_id)
        {
            return Ok(false);
        }
        if questions.is_empty() {
            bail!("async question request must contain at least one question");
        }
        let mut saved = Vec::with_capacity(questions.len());
        for question in questions {
            if question.title.trim().is_empty() {
                bail!("async question title must not be empty");
            }
            saved.push(AsyncQuestion {
                title: question.title,
                options: question.options,
                interaction_id: uuid::Uuid::new_v4().to_string(),
                presentation_sent: false,
                presentation_message_ids: Vec::new(),
                reply_target: None,
                awaiting_notice_sent: false,
                delivery: QuestionDelivery::Pending,
                answer: None,
            });
        }
        self.requests.push(AsyncQuestionRequest {
            call_id,
            thread_id: thread_id.into(),
            turn_id: turn_id.into(),
            questions: saved,
        });
        Ok(true)
    }

    pub fn pending(&self) -> Vec<PendingQuestion> {
        self.questions_with_delivery(QuestionDelivery::Pending)
    }

    pub fn ready(&self) -> Vec<PendingQuestion> {
        self.questions_with_delivery(QuestionDelivery::Ready)
    }

    pub fn awaiting_confirmation(&self) -> Vec<PendingQuestion> {
        self.questions_with_delivery(QuestionDelivery::AwaitingConfirmation)
    }

    fn questions_with_delivery(&self, delivery: QuestionDelivery) -> Vec<PendingQuestion> {
        self.requests
            .iter()
            .flat_map(|request| {
                request
                    .questions
                    .iter()
                    .enumerate()
                    .filter_map(move |(index, question)| {
                        (question.delivery == delivery).then(|| PendingQuestion {
                            call_id: request.call_id.clone(),
                            thread_id: request.thread_id.clone(),
                            turn_id: request.turn_id.clone(),
                            question_index: index,
                            item_id: native_item_id(&request.call_id, index),
                            interaction_id: question.interaction_id.clone(),
                            presentation_sent: question.presentation_sent,
                            presentation_message_ids: question.presentation_message_ids.clone(),
                            reply_target: question.reply_target.clone(),
                            awaiting_notice_sent: question.awaiting_notice_sent,
                            title: question.title.clone(),
                            options: question.options.clone(),
                            delivery: question.delivery,
                            answer: question.answer.clone(),
                        })
                    })
            })
            .collect()
    }

    /// Select a suggested answer by its zero-based index.
    pub fn answer_option(&mut self, interaction_id: &str, option_index: usize) -> Result<String> {
        let question = self.find_interaction_mut(interaction_id)?;
        let option = question
            .options
            .as_ref()
            .and_then(|options| options.get(option_index))
            .context("async question option is stale or out of range")?
            .clone();
        Self::set_answer(question, option.clone())?;
        Ok(option)
    }

    /// Submit a free-text answer for an optionless question (or the UI's free-text action).
    pub fn answer_freeform(&mut self, interaction_id: &str, answer: String) -> Result<()> {
        let question = self.find_interaction_mut(interaction_id)?;
        Self::set_answer(question, answer)
    }

    /// Build the official desktop-compatible envelope for all answers ready to queue.
    pub fn ready_reply_envelope(&self) -> Result<Option<String>> {
        self.reply_envelope_for(None)
    }

    /// Build a native reply containing only one ready question, preventing answers
    /// for concurrent cards from crossing in a shared Discord submit action.
    pub fn reply_envelope(&self, item_id: &str) -> Result<Option<String>> {
        self.reply_envelope_for(Some(item_id))
    }

    fn reply_envelope_for(&self, only_item_id: Option<&str>) -> Result<Option<String>> {
        let mut answers = Vec::new();
        for request in &self.requests {
            for (index, question) in request.questions.iter().enumerate() {
                let question_item_id = native_item_id(&request.call_id, index);
                if question.delivery != QuestionDelivery::Ready
                    || only_item_id.is_some_and(|wanted| wanted != question_item_id.as_str())
                {
                    continue;
                }
                if let Some(answer) = question.answer.as_ref() {
                    answers.push(NativeAnswer {
                        answer: answer.clone(),
                        question: question.title.clone(),
                        question_item_id,
                    });
                }
            }
        }
        if answers.is_empty() {
            return Ok(None);
        }
        Ok(Some(format!(
            "{REPLY_OPEN}\n{}\n{REPLY_CLOSE}",
            serde_json::to_string(&answers)?
        )))
    }

    pub fn question_for_interaction(&self, interaction_id: &str) -> Option<PendingQuestion> {
        self.requests.iter().find_map(|request| {
            request
                .questions
                .iter()
                .enumerate()
                .find_map(|(index, question)| {
                    (question.interaction_id == interaction_id).then(|| PendingQuestion {
                        call_id: request.call_id.clone(),
                        thread_id: request.thread_id.clone(),
                        turn_id: request.turn_id.clone(),
                        question_index: index,
                        item_id: native_item_id(&request.call_id, index),
                        interaction_id: question.interaction_id.clone(),
                        presentation_sent: question.presentation_sent,
                        presentation_message_ids: question.presentation_message_ids.clone(),
                        reply_target: question.reply_target.clone(),
                        awaiting_notice_sent: question.awaiting_notice_sent,
                        title: question.title.clone(),
                        options: question.options.clone(),
                        delivery: question.delivery,
                        answer: question.answer.clone(),
                    })
                })
        })
    }

    /// Mark a queued native reply as delivered. Repeated callbacks are harmless.
    pub fn mark_sent(&mut self, item_id: &str) -> Result<bool> {
        let Some(question) = self.find_item_mut(item_id) else {
            return Ok(false);
        };
        if question.delivery == QuestionDelivery::Sent {
            return Ok(false);
        }
        if question.delivery != QuestionDelivery::Ready || question.answer.is_none() {
            return Ok(false);
        }
        question.delivery = QuestionDelivery::Sent;
        Ok(true)
    }

    pub fn record_reply_target(&mut self, item_id: &str, target: StoredReplyTarget) -> Result<()> {
        let question = self
            .find_item_mut(item_id)
            .context("async question not found")?;
        if question.delivery != QuestionDelivery::Ready {
            bail!("async question is not ready for submission");
        }
        question.reply_target = Some(target);
        Ok(())
    }

    pub fn reconcile_delivery(
        &mut self,
        item_id: &str,
        observation: QueueObservation,
    ) -> ReconcileResult {
        let Some(question) = self.find_item_mut(item_id) else {
            return ReconcileResult::Unchanged;
        };
        if !matches!(
            question.delivery,
            QuestionDelivery::Ready | QuestionDelivery::AwaitingConfirmation
        ) {
            return ReconcileResult::Unchanged;
        }
        match observation {
            QueueObservation::Accepted | QueueObservation::Queued => {
                if question.delivery == QuestionDelivery::Sent {
                    return ReconcileResult::Unchanged;
                }
                question.delivery = QuestionDelivery::Sent;
                ReconcileResult::MarkedSent
            }
            QueueObservation::Submitting | QueueObservation::Unknown => {
                if question.delivery == QuestionDelivery::AwaitingConfirmation {
                    return ReconcileResult::Unchanged;
                }
                question.delivery = QuestionDelivery::AwaitingConfirmation;
                ReconcileResult::AwaitingConfirmation
            }
            QueueObservation::Rejected | QueueObservation::Missing => {
                question.delivery = QuestionDelivery::Pending;
                question.answer = None;
                question.reply_target = None;
                question.awaiting_notice_sent = false;
                ReconcileResult::ReleasedForRetry
            }
        }
    }

    pub fn mark_awaiting_notice_sent(&mut self, item_id: &str) -> bool {
        let Some(question) = self.find_item_mut(item_id) else {
            return false;
        };
        if question.delivery != QuestionDelivery::AwaitingConfirmation
            || question.awaiting_notice_sent
        {
            return false;
        }
        question.awaiting_notice_sent = true;
        true
    }

    pub fn presentation_pending_for_thread(&self, thread_id: &str) -> Vec<PendingQuestion> {
        self.requests
            .iter()
            .filter(|request| request.thread_id == thread_id)
            .flat_map(|request| {
                request
                    .questions
                    .iter()
                    .enumerate()
                    .filter_map(move |(index, question)| {
                        (!question.presentation_sent).then(|| PendingQuestion {
                            call_id: request.call_id.clone(),
                            thread_id: request.thread_id.clone(),
                            turn_id: request.turn_id.clone(),
                            question_index: index,
                            item_id: native_item_id(&request.call_id, index),
                            interaction_id: question.interaction_id.clone(),
                            presentation_sent: question.presentation_sent,
                            presentation_message_ids: question.presentation_message_ids.clone(),
                            reply_target: question.reply_target.clone(),
                            awaiting_notice_sent: question.awaiting_notice_sent,
                            title: question.title.clone(),
                            options: question.options.clone(),
                            delivery: question.delivery,
                            answer: question.answer.clone(),
                        })
                    })
            })
            .collect()
    }

    pub fn record_presentation_message(
        &mut self,
        item_id: &str,
        page: usize,
        message_id: u64,
        total: usize,
    ) -> Result<()> {
        let question = self
            .find_item_mut(item_id)
            .context("async question not found")?;
        question.presentation_message_ids.resize(total, None);
        if page >= total {
            bail!("presentation page index out of range");
        }
        question.presentation_message_ids[page] = Some(message_id);
        question.presentation_sent = question
            .presentation_message_ids
            .iter()
            .all(Option::is_some);
        Ok(())
    }

    pub fn mark_presentation_sent(&mut self, item_id: &str) -> bool {
        let Some(question) = self.find_item_mut(item_id) else {
            return false;
        };
        if question.presentation_sent {
            return false;
        }
        question.presentation_sent = true;
        true
    }

    pub fn presentation_pending(&self, call_id: &str) -> Vec<PendingQuestion> {
        self.questions_with_presentation(call_id, false)
    }

    fn questions_with_presentation(
        &self,
        call_id: &str,
        presentation_sent: bool,
    ) -> Vec<PendingQuestion> {
        self.requests
            .iter()
            .filter(|request| request.call_id == call_id)
            .flat_map(|request| {
                request
                    .questions
                    .iter()
                    .enumerate()
                    .filter_map(move |(index, question)| {
                        (question.presentation_sent == presentation_sent).then(|| PendingQuestion {
                            call_id: request.call_id.clone(),
                            thread_id: request.thread_id.clone(),
                            turn_id: request.turn_id.clone(),
                            question_index: index,
                            item_id: native_item_id(&request.call_id, index),
                            interaction_id: question.interaction_id.clone(),
                            presentation_sent: question.presentation_sent,
                            presentation_message_ids: question.presentation_message_ids.clone(),
                            reply_target: question.reply_target.clone(),
                            awaiting_notice_sent: question.awaiting_notice_sent,
                            title: question.title.clone(),
                            options: question.options.clone(),
                            delivery: question.delivery,
                            answer: question.answer.clone(),
                        })
                    })
            })
            .collect()
    }

    /// Release a failed delivery so the user can answer again or retry it.
    pub fn release_failed(&mut self, item_id: &str) -> bool {
        let Some(question) = self.find_item_mut(item_id) else {
            return false;
        };
        if question.delivery != QuestionDelivery::Ready {
            return false;
        }
        question.delivery = QuestionDelivery::Pending;
        question.answer = None;
        true
    }

    fn find_interaction_mut(&mut self, interaction_id: &str) -> Result<&mut AsyncQuestion> {
        let question = self
            .requests
            .iter_mut()
            .flat_map(|request| &mut request.questions)
            .find(|question| question.interaction_id == interaction_id)
            .context("async question interaction is stale")?;
        if question.delivery != QuestionDelivery::Pending {
            bail!("async question has already been answered or submitted");
        }
        Ok(question)
    }

    fn find_item_mut(&mut self, item_id: &str) -> Option<&mut AsyncQuestion> {
        self.requests.iter_mut().find_map(|request| {
            request
                .questions
                .iter_mut()
                .enumerate()
                .find_map(|(index, question)| {
                    (native_item_id(&request.call_id, index) == item_id).then_some(question)
                })
        })
    }

    fn set_answer(question: &mut AsyncQuestion, answer: String) -> Result<()> {
        if answer.trim().is_empty() {
            bail!("async question answer must not be empty");
        }
        question.answer = Some(answer);
        question.delivery = QuestionDelivery::Ready;
        Ok(())
    }
}

/// Match the native desktop's compact JSON.stringify([tool name, call id, index]).
pub fn native_item_id(call_id: &str, question_index: usize) -> String {
    serde_json::json!([QUESTION_TOOL_NAME, call_id, question_index]).to_string()
}

/// Stable Discord custom id for a suggested answer. The option text remains in the
/// rendered message, while the component carries only the durable interaction token.
pub fn option_component_id(interaction_id: &str, option_index: usize) -> String {
    format!("asyncq:o:{interaction_id}:{option_index}")
}

/// Stable Discord custom id for opening the free-text answer modal.
pub fn freeform_component_id(interaction_id: &str) -> String {
    format!("asyncq:f:{interaction_id}")
}

fn freeform_modal_id(interaction_id: &str) -> String {
    format!("asyncq:m:{interaction_id}")
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QuestionComponentAction {
    SelectOption {
        interaction_id: String,
        option_index: usize,
    },
    Freeform {
        interaction_id: String,
    },
}

/// Parse only ids emitted by the helpers above; malformed and stale ids are rejected.
pub fn parse_component_id(custom_id: &str) -> Option<QuestionComponentAction> {
    let rest = custom_id.strip_prefix("asyncq:")?;
    if let Some(interaction_id) = rest.strip_prefix("f:") {
        return (!interaction_id.is_empty()).then(|| QuestionComponentAction::Freeform {
            interaction_id: interaction_id.to_owned(),
        });
    }
    let rest = rest.strip_prefix("o:")?;
    let (interaction_id, option_index) = rest.rsplit_once(':')?;
    if interaction_id.is_empty() {
        return None;
    }
    Some(QuestionComponentAction::SelectOption {
        interaction_id: interaction_id.to_owned(),
        option_index: option_index.parse().ok()?,
    })
}

/// Discord select menus expose at most 25 entries. Return every option in ordered
/// pages, using each original index as the answer value so no choice is dropped.
pub fn option_pages(question: &PendingQuestion) -> Vec<Vec<(usize, String)>> {
    const DISCORD_SELECT_OPTION_LIMIT: usize = 25;
    question
        .options
        .as_deref()
        .unwrap_or_default()
        .iter()
        .enumerate()
        .map(|(index, option)| (index, option.clone()))
        .collect::<Vec<_>>()
        .chunks(DISCORD_SELECT_OPTION_LIMIT)
        .map(<[(usize, String)]>::to_vec)
        .collect()
}

/// Text pages preserve every character while respecting Discord's content length.
/// `max_chars` is supplied by the caller from the Discord delivery surface.
pub fn render_pages(
    question: &PendingQuestion,
    language: Language,
    max_chars: usize,
) -> Vec<String> {
    let mut body = format!(
        "{}\n{}",
        language.text("Question", "질문", "问题", "質問"),
        question.title
    );
    if let Some(options) = question
        .options
        .as_ref()
        .filter(|options| !options.is_empty())
    {
        for (index, option) in options.iter().enumerate() {
            body.push_str(&format!("\n{}. {option}", index + 1));
        }
        body.push_str(&format!(
            "\n{}",
            language.text(
                "Choose an option or submit a free-text answer.",
                "선택지를 고르거나 직접 답변을 입력하세요.",
                "请选择一个选项或输入自由文本答案。",
                "選択肢を選ぶか、自由入力で回答してください。"
            )
        ));
    } else {
        body.push_str(&format!(
            "\n{}",
            language.text(
                "Submit a free-text answer.",
                "직접 답변을 입력하세요.",
                "请输入自由文本答案。",
                "自由入力で回答してください。"
            )
        ));
    }
    split_content(&body, max_chars.max(1))
}

fn split_content(text: &str, max_chars: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut pages = Vec::new();
    let mut remaining = text;
    while remaining.chars().count() > max_chars {
        let mut split_at = remaining
            .char_indices()
            .nth(max_chars)
            .map(|(index, _)| index)
            .unwrap_or(remaining.len());
        if let Some(newline) = remaining[..split_at].rfind('\n') {
            if newline > 0 {
                split_at = newline + 1;
            }
        }
        pages.push(remaining[..split_at].to_owned());
        remaining = &remaining[split_at..];
    }
    pages.push(remaining.to_owned());
    pages
}

#[derive(Clone)]
struct PresentationMessage {
    content: String,
    options: Vec<(usize, String)>,
    freeform: bool,
}

fn presentation_messages(
    question: &PendingQuestion,
    language: Language,
) -> Vec<PresentationMessage> {
    let mut messages: Vec<_> = render_pages(question, language, 2_000)
        .into_iter()
        .map(|content| PresentationMessage {
            content,
            options: Vec::new(),
            freeform: false,
        })
        .collect();
    let pages = option_pages(question);
    for (page_index, options) in pages.iter().enumerate() {
        let mut body = format!(
            "{} {}/{}",
            language.text(
                "Choose an answer",
                "답변을 선택하세요",
                "请选择答案",
                "回答を選んでください"
            ),
            page_index + 1,
            pages.len()
        );
        for (index, label) in options {
            body.push_str(&format!("\n{}. {label}", index + 1));
        }
        let chunks = split_content(&body, 2_000);
        let chunk_count = chunks.len();
        for (chunk_index, content) in chunks.into_iter().enumerate() {
            let choices = if chunk_index + 1 == chunk_count {
                options.clone()
            } else {
                Vec::new()
            };
            messages.push(PresentationMessage {
                content,
                options: choices,
                freeform: false,
            });
        }
    }
    messages.push(PresentationMessage {
        content: language
            .text(
                "You can also answer in your own words.",
                "원하는 표현으로 직접 답할 수도 있습니다.",
                "你也可以用自己的话回答。",
                "自由な文章で回答することもできます。",
            )
            .to_owned(),
        options: Vec::new(),
        freeform: true,
    });
    messages
}

/// Persist the question before publishing, then checkpoint every sent page so
/// restart recovery resumes at the first page without a recorded Discord ID.
pub(crate) async fn publish(
    http: &Http,
    state: &crate::discord::BridgeState,
    thread_id: &str,
    turn_id: &str,
    item_id: &str,
    questions: Vec<AsyncQuestionInput>,
) -> Result<()> {
    let state_path = state.config.state_dir.join("async-questions.json");
    {
        let mut store = state.async_questions.lock().await;
        store.insert(item_id, thread_id, turn_id, questions)?;
        store.save(&state_path)?;
    }
    resume_presentations(http, state, thread_id).await
}

async fn resume_presentations(
    http: &Http,
    state: &crate::discord::BridgeState,
    thread_id: &str,
) -> Result<()> {
    let state_path = state.config.state_dir.join("async-questions.json");
    let pending = state
        .async_questions
        .lock()
        .await
        .presentation_pending_for_thread(thread_id);
    let home_channel = ChannelId::new(state.config.home_channel_id);
    for question in pending {
        let parts = presentation_messages(&question, state.config.language);
        for (page, part) in parts.iter().enumerate() {
            if question
                .presentation_message_ids
                .get(page)
                .is_some_and(Option::is_some)
            {
                continue;
            }
            let mut builder = CreateMessage::new().content(part.content.clone());
            if part.freeform {
                let button = CreateButton::new(freeform_component_id(&question.interaction_id))
                    .label(state.config.language.text(
                        "Write a different answer",
                        "다른 답변 직접 입력",
                        "输入其他答案",
                        "別の回答を入力",
                    ))
                    .style(ButtonStyle::Secondary);
                builder = builder.components(vec![CreateActionRow::Buttons(vec![button])]);
            } else if !part.options.is_empty() {
                builder =
                    builder.components(option_button_rows(&part.options, &question.interaction_id));
            }
            let message = home_channel
                .send_message(http, builder)
                .await
                .context("publish async question page")?;
            let mut store = state.async_questions.lock().await;
            store.record_presentation_message(
                &question.item_id,
                page,
                message.id.get(),
                parts.len(),
            )?;
            store.save(&state_path)?;
        }
    }
    Ok(())
}

pub(crate) async fn recover(http: &Http, state: &crate::discord::BridgeState) -> Result<()> {
    let thread_id = state.native.thread_id();
    resume_presentations(http, state, &thread_id).await?;
    let path = state.config.state_dir.join("async-questions.json");
    let store = state.async_questions.lock().await;
    let mut ready = store.ready();
    ready.extend(store.awaiting_confirmation());
    drop(store);
    for q in ready {
        if q.thread_id != thread_id {
            continue;
        }
        let observation = if let Some(target) = &q.reply_target {
            state
                .queued_inputs
                .lock()
                .await
                .iter()
                .find(|input| {
                    input.message_id == target.message_id && input.native_thread_id == q.thread_id
                })
                .map(|input| match input.delivery {
                    crate::discord::QueueDelivery::Accepted => QueueObservation::Accepted,
                    crate::discord::QueueDelivery::Queued => QueueObservation::Queued,
                    crate::discord::QueueDelivery::Submitting => QueueObservation::Submitting,
                    crate::discord::QueueDelivery::Unknown => QueueObservation::Unknown,
                    crate::discord::QueueDelivery::Rejected => QueueObservation::Rejected,
                })
                .unwrap_or(QueueObservation::Missing)
        } else {
            QueueObservation::Missing
        };
        let mut store = state.async_questions.lock().await;
        let result = store.reconcile_delivery(&q.item_id, observation);
        let notice = result == ReconcileResult::AwaitingConfirmation
            || (q.delivery == QuestionDelivery::AwaitingConfirmation && !q.awaiting_notice_sent);
        let target = store
            .question_for_interaction(&q.interaction_id)
            .and_then(|item| item.reply_target);
        store.save(&path)?;
        drop(store);
        if notice {
            if let Some(target) = target {
                ChannelId::new(target.channel_id).send_message(http, CreateMessage::new().content(state.config.language.text(
                    "Your answer is awaiting native confirmation. It has not been sent again.",
                    "답변이 Codex 확인을 기다리고 있습니다. 중복 제출하지 않았습니다.",
                    "你的答案正在等待 Codex 确认，没有重复提交。",
                    "回答は Codex の確認待ちです。再送信していません。"))).await.context("send async answer status")?;
                let mut store = state.async_questions.lock().await;
                store.mark_awaiting_notice_sent(&q.item_id);
                store.save(&path)?;
            }
        }
    }
    Ok(())
}

fn option_button_rows(options: &[(usize, String)], interaction_id: &str) -> Vec<CreateActionRow> {
    options
        .chunks(5)
        .map(|group| {
            CreateActionRow::Buttons(
                group
                    .iter()
                    .map(|(index, _)| {
                        CreateButton::new(option_component_id(interaction_id, *index))
                            .label((index + 1).to_string())
                            .style(ButtonStyle::Primary)
                    })
                    .collect(),
            )
        })
        .collect()
}

fn parse_modal_id(custom_id: &str) -> Option<&str> {
    custom_id
        .strip_prefix("asyncq:m:")
        .filter(|id| !id.is_empty())
}

fn question_is_current(
    state: &crate::discord::BridgeState,
    question: &PendingQuestion,
) -> Result<()> {
    if question.delivery != QuestionDelivery::Pending {
        bail!("this async question has already been answered or submitted");
    }
    if question.thread_id != state.native.thread_id() {
        bail!("this async question belongs to a different Codex conversation");
    }
    Ok(())
}

fn target_for_card(
    channel_id: ChannelId,
    message_id: serenity::all::MessageId,
    guild_id: Option<serenity::all::GuildId>,
) -> crate::discord::ReplyTarget {
    crate::discord::ReplyTarget {
        source_guild: guild_id.map(|guild| guild.get()),
        channel_id,
        message_id,
    }
}

async fn stage_answer(
    state: &crate::discord::BridgeState,
    interaction_id: &str,
    answer: String,
    target: &crate::discord::ReplyTarget,
) -> Result<(PendingQuestion, String)> {
    let path = state.config.state_dir.join("async-questions.json");
    let mut store = state.async_questions.lock().await;
    let mut staged = store.clone();
    let question = staged
        .question_for_interaction(interaction_id)
        .context("this async question is no longer available")?;
    question_is_current(state, &question)?;
    let item_id = question.item_id.clone();
    staged.answer_freeform(interaction_id, answer)?;
    staged.record_reply_target(
        &item_id,
        StoredReplyTarget {
            channel_id: target.channel_id.get(),
            message_id: target.message_id.get(),
            source_guild: target.source_guild,
        },
    )?;
    let envelope = staged
        .reply_envelope(&item_id)?
        .context("async question answer was already submitted")?;
    staged.save(&path)?;
    *store = staged;
    Ok((question, envelope))
}

async fn stage_option_answer(
    state: &crate::discord::BridgeState,
    interaction_id: &str,
    option_index: usize,
    target: &crate::discord::ReplyTarget,
) -> Result<(PendingQuestion, String)> {
    let path = state.config.state_dir.join("async-questions.json");
    let mut store = state.async_questions.lock().await;
    let mut staged = store.clone();
    let question = staged
        .question_for_interaction(interaction_id)
        .context("this async question is no longer available")?;
    question_is_current(state, &question)?;
    let item_id = question.item_id.clone();
    staged.answer_option(interaction_id, option_index)?;
    staged.record_reply_target(
        &item_id,
        StoredReplyTarget {
            channel_id: target.channel_id.get(),
            message_id: target.message_id.get(),
            source_guild: target.source_guild,
        },
    )?;
    let envelope = staged
        .reply_envelope(&item_id)?
        .context("async question answer was already submitted")?;
    staged.save(&path)?;
    *store = staged;
    Ok((question, envelope))
}

async fn submit_staged_answer(
    state: &crate::discord::BridgeState,
    question: &PendingQuestion,
    envelope: &str,
    target: crate::discord::ReplyTarget,
) -> Result<()> {
    if state.native.thread_id() != question.thread_id {
        bail!("this async question belongs to a different Codex conversation");
    }
    let state_path = state.config.state_dir.join("async-questions.json");
    match crate::discord::submit_async_answer(state, envelope, target.clone(), &question.thread_id)
        .await
    {
        Ok(()) => {
            let mut store = state.async_questions.lock().await;
            store.mark_sent(&question.item_id)?;
            store.save(&state_path)?;
            Ok(())
        }
        Err(error) => {
            let queue_delivery = state
                .queued_inputs
                .lock()
                .await
                .iter()
                .find(|queued| {
                    queued.message_id == target.message_id.get()
                        && queued.native_thread_id == question.thread_id
                })
                .map(|queued| queued.delivery);
            let observation = match queue_delivery {
                Some(crate::discord::QueueDelivery::Accepted) => QueueObservation::Accepted,
                Some(crate::discord::QueueDelivery::Queued) => QueueObservation::Queued,
                Some(crate::discord::QueueDelivery::Submitting) => QueueObservation::Submitting,
                Some(crate::discord::QueueDelivery::Unknown) => QueueObservation::Unknown,
                Some(crate::discord::QueueDelivery::Rejected) => QueueObservation::Rejected,
                None => QueueObservation::Missing,
            };
            let mut store = state.async_questions.lock().await;
            store.reconcile_delivery(&question.item_id, observation);
            store.save(&state_path)?;
            Err(error)
        }
    }
}

async fn respond_stale_component(
    ctx: &Context,
    interaction: &ComponentInteraction,
    state: &crate::discord::BridgeState,
    reason: &str,
) -> Result<()> {
    let content = state.config.language.text(
        "This question is no longer available. Please use its latest Discord card.",
        "이 질문은 더 이상 사용할 수 없습니다. 최신 Discord 질문 카드에서 답해주세요.",
        "此问题已不可用。请使用最新的 Discord 问题卡片。",
        "この質問は利用できません。最新の Discord 質問カードから回答してください。",
    );
    let _ = reason;
    interaction
        .create_response(
            ctx,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(content)
                    .ephemeral(true),
            ),
        )
        .await?;
    Ok(())
}

async fn respond_stale_modal(
    ctx: &Context,
    interaction: &ModalInteraction,
    state: &crate::discord::BridgeState,
) -> Result<()> {
    interaction
        .create_response(
            ctx,
            CreateInteractionResponse::Message(
                CreateInteractionResponseMessage::new()
                    .content(state.config.language.text(
                        "This question is no longer available. Please use its latest Discord card.",
                        "이 질문은 더 이상 사용할 수 없습니다. 최신 Discord 질문 카드에서 답해주세요.",
                        "此问题已不可用。请使用最新的 Discord 问题卡片。",
                        "この質問は利用できません。最新の Discord 質問カードから回答してください。",
                    ))
                    .ephemeral(true),
            ),
        )
        .await?;
    Ok(())
}

pub(crate) async fn handle_component(
    ctx: &Context,
    interaction: &ComponentInteraction,
    state: &std::sync::Arc<crate::discord::BridgeState>,
) -> Result<()> {
    let Some(action) = parse_component_id(&interaction.data.custom_id) else {
        bail!("invalid async question component id");
    };
    match action {
        QuestionComponentAction::Freeform { interaction_id } => {
            let question = state
                .async_questions
                .lock()
                .await
                .question_for_interaction(&interaction_id);
            let Some(question) = question else {
                return respond_stale_component(ctx, interaction, state, "missing").await;
            };
            if question_is_current(state, &question).is_err() {
                return respond_stale_component(ctx, interaction, state, "stale").await;
            }
            let language = state.config.language;
            let modal_id = freeform_modal_id(&interaction_id);
            let modal = CreateModal::new(
                modal_id,
                language.text(
                    "Answer the question",
                    "질문에 답하기",
                    "回答问题",
                    "質問に回答",
                ),
            )
            .components(vec![CreateActionRow::InputText(
                CreateInputText::new(
                    InputTextStyle::Paragraph,
                    language.text("Your answer", "답변", "你的答案", "回答"),
                    "answer",
                )
                .required(true)
                .min_length(1)
                .max_length(4_000),
            )]);
            interaction
                .create_response(ctx, CreateInteractionResponse::Modal(modal))
                .await?;
            Ok(())
        }
        QuestionComponentAction::SelectOption {
            interaction_id,
            option_index,
        } => {
            let target = target_for_card(
                interaction.message.channel_id,
                interaction.message.id,
                interaction.guild_id,
            );
            let (question, envelope) =
                match stage_option_answer(state, &interaction_id, option_index, &target).await {
                    Ok(answer) => answer,
                    Err(reason) => {
                        return respond_stale_component(
                            ctx,
                            interaction,
                            state,
                            &format!("{reason:#}"),
                        )
                        .await;
                    }
                };
            if let Err(error) = interaction
                .create_response(ctx, CreateInteractionResponse::Acknowledge)
                .await
            {
                tracing::warn!(?error, "failed to acknowledge async question selection");
            }
            if let Err(error) = submit_staged_answer(state, &question, &envelope, target).await {
                tracing::warn!(?error, "failed to submit async question answer");
                let delivery = state
                    .async_questions
                    .lock()
                    .await
                    .question_for_interaction(&interaction_id)
                    .map(|q| q.delivery);
                let text = if delivery == Some(QuestionDelivery::AwaitingConfirmation) {
                    state.config.language.text(
                        "Your answer is awaiting native confirmation. It has not been sent again.",
                        "답변이 Codex 확인을 기다리고 있습니다. 중복 제출하지 않았습니다.",
                        "你的答案正在等待 Codex 确认，没有重复提交。",
                        "回答は Codex の確認待ちです。再送信していません。",
                    )
                } else {
                    state.config.language.text(
                        "The answer could not be submitted. You can try again from the question card.",
                        "답변을 제출하지 못했습니다. 질문 카드에서 다시 시도할 수 있습니다.",
                        "无法提交答案。你可以在问题卡片上重试。",
                        "回答を送信できませんでした。質問カードから再試行できます。",
                    )
                };
                let _ = interaction
                    .create_followup(
                        ctx,
                        serenity::builder::CreateInteractionResponseFollowup::new()
                            .content(text)
                            .ephemeral(true),
                    )
                    .await;
            }
            Ok(())
        }
    }
}

pub(crate) async fn handle_modal(
    ctx: &Context,
    interaction: &ModalInteraction,
    state: &std::sync::Arc<crate::discord::BridgeState>,
) -> Result<()> {
    let Some(interaction_id) = parse_modal_id(&interaction.data.custom_id) else {
        bail!("invalid async question modal id");
    };
    let answer = interaction
        .data
        .components
        .iter()
        .flat_map(|row| &row.components)
        .find_map(|component| match component {
            ActionRowComponent::InputText(input) if input.custom_id == "answer" => {
                input.value.clone()
            }
            _ => None,
        });
    let Some(answer) = answer else {
        return respond_stale_modal(ctx, interaction, state).await;
    };
    let Some(message) = interaction.message.as_ref() else {
        return respond_stale_modal(ctx, interaction, state).await;
    };
    let target = target_for_card(message.channel_id, message.id, interaction.guild_id);
    let (question, envelope) = match stage_answer(state, interaction_id, answer, &target).await {
        Ok(staged) => staged,
        Err(error) => {
            tracing::debug!(?error, "async question modal answer is stale");
            return respond_stale_modal(ctx, interaction, state).await;
        }
    };
    if let Err(error) = interaction
        .create_response(
            ctx,
            CreateInteractionResponse::Defer(
                CreateInteractionResponseMessage::new().ephemeral(true),
            ),
        )
        .await
    {
        tracing::warn!(?error, "failed to acknowledge async question modal");
    }
    match submit_staged_answer(state, &question, &envelope, target).await {
        Ok(()) => {
            interaction
                .edit_response(
                    ctx,
                    EditInteractionResponse::new().content(state.config.language.text(
                        "Answer submitted.",
                        "답변을 제출했습니다.",
                        "答案已提交。",
                        "回答を送信しました。",
                    )),
                )
                .await?;
            Ok(())
        }
        Err(error) => {
            let delivery = state
                .async_questions
                .lock()
                .await
                .question_for_interaction(interaction_id)
                .map(|q| q.delivery);
            let text = if delivery == Some(QuestionDelivery::AwaitingConfirmation) {
                state.config.language.text(
                    "Your answer is awaiting native confirmation. It has not been sent again.",
                    "답변이 Codex 확인을 기다리고 있습니다. 중복 제출하지 않았습니다.",
                    "你的答案正在等待 Codex 确认，没有重复提交。",
                    "回答は Codex の確認待ちです。再送信していません。",
                )
            } else {
                state.config.language.text(
                    "The answer could not be submitted. Try again from the question card.",
                    "답변을 제출하지 못했습니다. 질문 카드에서 다시 시도하세요.",
                    "无法提交答案。请在问题卡片上重试。",
                    "回答を送信できませんでした。質問カードから再試行してください。",
                )
            };
            interaction
                .edit_response(ctx, EditInteractionResponse::new().content(text))
                .await?;
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn question() -> AsyncQuestionInput {
        AsyncQuestionInput {
            title: "Which environment?".into(),
            options: Some(vec!["Staging".into(), "Production".into()]),
        }
    }

    #[test]
    fn native_identity_uses_official_tool_call_and_question_index() {
        assert_eq!(
            native_item_id("call-id", 2),
            r#"["request_user_input_async","call-id",2]"#
        );
    }

    #[test]
    fn component_ids_round_trip_and_option_pages_keep_original_indexes() {
        let option_id = option_component_id("stable-token", 28);
        assert_eq!(
            parse_component_id(&option_id),
            Some(QuestionComponentAction::SelectOption {
                interaction_id: "stable-token".into(),
                option_index: 28,
            })
        );
        assert_eq!(
            parse_component_id(&freeform_component_id("stable-token")),
            Some(QuestionComponentAction::Freeform {
                interaction_id: "stable-token".into(),
            })
        );
        assert_eq!(parse_component_id("asyncq:o:stable-token:nope"), None);

        let mut store = AsyncQuestionStore::default();
        let choices = (0..27).map(|index| format!("Choice {index}")).collect();
        store
            .insert(
                "call-id",
                "thread-id",
                "turn-id",
                vec![AsyncQuestionInput {
                    title: "Pick one".into(),
                    options: Some(choices),
                }],
            )
            .unwrap();
        let pages = option_pages(&store.pending()[0]);
        assert_eq!(pages.iter().map(Vec::len).collect::<Vec<_>>(), vec![25, 2]);
        assert_eq!(
            pages[1],
            vec![(25, "Choice 25".into()), (26, "Choice 26".into())]
        );
    }

    #[test]
    fn insertion_is_idempotent_and_answer_becomes_one_ready_reply() {
        let mut store = AsyncQuestionStore::default();
        assert!(
            store
                .insert("call-id", "thread-id", "turn-id", vec![question()])
                .unwrap()
        );
        assert!(
            !store
                .insert("call-id", "thread-id", "turn-id", vec![question()])
                .unwrap()
        );
        let pending = store.pending().remove(0);
        assert_eq!(pending.delivery, QuestionDelivery::Pending);
        assert_eq!(
            store.answer_option(&pending.interaction_id, 1).unwrap(),
            "Production"
        );
        assert!(store.answer_option(&pending.interaction_id, 0).is_err());
        let envelope = store.ready_reply_envelope().unwrap().unwrap();
        let payload = envelope
            .strip_prefix("<send_user_message_question_reply>\n")
            .unwrap()
            .strip_suffix("\n</send_user_message_question_reply>")
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(payload).unwrap(),
            json!([{
                "answer": "Production",
                "question": "Which environment?",
                "questionItemId": r#"["request_user_input_async","call-id",0]"#
            }])
        );
        assert!(store.mark_sent(&pending.item_id).unwrap());
        assert!(!store.mark_sent(&pending.item_id).unwrap());
        assert!(store.pending().is_empty());
    }

    #[test]
    fn failed_delivery_releases_question_for_a_new_answer() {
        let mut store = AsyncQuestionStore::default();
        store
            .insert("call-id", "thread-id", "turn-id", vec![question()])
            .unwrap();
        let pending = store.pending().remove(0);
        store.answer_option(&pending.interaction_id, 0).unwrap();
        assert!(store.release_failed(&pending.item_id));
        assert_eq!(store.pending()[0].delivery, QuestionDelivery::Pending);
        store
            .answer_freeform(&pending.interaction_id, "Another choice".into())
            .unwrap();
        assert!(store.pending().is_empty());
        assert_eq!(store.ready()[0].answer.as_deref(), Some("Another choice"));
    }

    #[test]
    fn staged_answer_records_card_target_and_emits_question_scoped_reply() {
        let mut store = AsyncQuestionStore::default();
        store
            .insert("call-id", "thread-id", "turn-id", vec![question()])
            .unwrap();
        let q = store.pending()[0].clone();
        store.answer_option(&q.interaction_id, 1).unwrap();
        store
            .record_reply_target(
                &q.item_id,
                StoredReplyTarget {
                    channel_id: 44,
                    message_id: 55,
                    source_guild: Some(66),
                },
            )
            .unwrap();
        let envelope = store.reply_envelope(&q.item_id).unwrap().unwrap();
        let payload = envelope
            .strip_prefix("<send_user_message_question_reply>\n")
            .unwrap()
            .strip_suffix("\n</send_user_message_question_reply>")
            .unwrap();
        let answer: serde_json::Value = serde_json::from_str(payload).unwrap();
        assert_eq!(
            answer[0]["questionItemId"],
            r#"["request_user_input_async","call-id",0]"#
        );
        assert_eq!(
            store.ready()[0].reply_target.as_ref().unwrap().message_id,
            55
        );
        assert_eq!(store.ready().len(), 1);
    }

    #[test]
    fn reconciliation_distinguishes_durable_admission_unknown_and_retryable_failure() {
        let mut store = AsyncQuestionStore::default();
        store
            .insert("call-id", "thread-id", "turn-id", vec![question()])
            .unwrap();
        let q = store.pending()[0].clone();
        store.answer_option(&q.interaction_id, 0).unwrap();
        store
            .record_reply_target(
                &q.item_id,
                StoredReplyTarget {
                    channel_id: 4,
                    message_id: 9,
                    source_guild: None,
                },
            )
            .unwrap();
        assert_eq!(
            store.reconcile_delivery(&q.item_id, QueueObservation::Unknown),
            ReconcileResult::AwaitingConfirmation
        );
        assert_eq!(
            store.awaiting_confirmation()[0]
                .reply_target
                .as_ref()
                .unwrap()
                .message_id,
            9
        );
        assert_eq!(
            store.reconcile_delivery(&q.item_id, QueueObservation::Submitting),
            ReconcileResult::Unchanged
        );
        assert_eq!(
            store.reconcile_delivery(&q.item_id, QueueObservation::Accepted),
            ReconcileResult::MarkedSent
        );
        assert_eq!(
            store.find_item_mut(&q.item_id).unwrap().delivery,
            QuestionDelivery::Sent
        );

        assert_eq!(
            store.reconcile_delivery(&q.item_id, QueueObservation::Missing),
            ReconcileResult::Unchanged
        );
        let mut other = AsyncQuestionStore::default();
        other
            .insert("other-call", "thread-id", "turn-id", vec![question()])
            .unwrap();
        let retry = other.pending()[0].clone();
        other.answer_option(&retry.interaction_id, 0).unwrap();
        other
            .record_reply_target(
                &retry.item_id,
                StoredReplyTarget {
                    channel_id: 4,
                    message_id: 10,
                    source_guild: None,
                },
            )
            .unwrap();
        assert_eq!(
            other.reconcile_delivery(&retry.item_id, QueueObservation::Rejected),
            ReconcileResult::ReleasedForRetry
        );
        assert_eq!(other.pending()[0].delivery, QuestionDelivery::Pending);
        assert!(other.pending()[0].reply_target.is_none());
    }

    #[test]
    fn presentation_progress_records_each_page_and_resumes_only_missing_pages() {
        let mut store = AsyncQuestionStore::default();
        store
            .insert("call-id", "thread-id", "turn-id", vec![question()])
            .unwrap();
        let q = store.pending()[0].clone();
        let parts = presentation_messages(&q, Language::English);
        assert!(parts.len() >= 3);
        store
            .record_presentation_message(&q.item_id, 0, 111, parts.len())
            .unwrap();
        store
            .record_presentation_message(&q.item_id, 1, 112, parts.len())
            .unwrap();
        let saved = store.presentation_pending_for_thread("thread-id")[0].clone();
        let unsent: Vec<_> = parts
            .iter()
            .enumerate()
            .filter(|(i, _)| {
                saved
                    .presentation_message_ids
                    .get(*i)
                    .is_none_or(Option::is_none)
            })
            .map(|(i, _)| i)
            .collect();
        assert_eq!(unsent, (2..parts.len()).collect::<Vec<_>>());
        for page in unsent {
            store
                .record_presentation_message(&q.item_id, page, 200 + page as u64, parts.len())
                .unwrap();
        }
        assert!(store.requests[0].questions[0].presentation_sent);
        assert_eq!(
            store.requests[0].questions[0]
                .presentation_message_ids
                .len(),
            parts.len()
        );
    }

    #[test]
    fn rendering_splits_long_unicode_without_losing_content() {
        let mut store = AsyncQuestionStore::default();
        store
            .insert("call-id", "thread-id", "turn-id", vec![question()])
            .unwrap();
        let pending = store.pending().remove(0);
        let pages = render_pages(&pending, Language::Korean, 24);
        assert!(pages.len() > 1);
        assert!(pages.iter().all(|page| page.chars().count() <= 24));
        let joined = pages.join("");
        for expected in ["Which environment?", "Staging", "Production", "직접"] {
            assert!(joined.contains(expected));
        }
    }

    #[test]
    fn state_round_trips_across_store_reopen() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("async-questions.json");
        let mut store = AsyncQuestionStore::default();
        store
            .insert("call-id", "thread-id", "turn-id", vec![question()])
            .unwrap();
        let identity = store.pending()[0].interaction_id.clone();
        store.answer_option(&identity, 0).unwrap();
        store.save(&path).unwrap();
        let reopened = AsyncQuestionStore::load(&path).unwrap();
        assert_eq!(reopened.ready()[0].interaction_id, identity);
        assert_eq!(
            reopened.ready_reply_envelope().unwrap(),
            store.ready_reply_envelope().unwrap()
        );
    }
}
