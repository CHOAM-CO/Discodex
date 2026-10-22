use std::{
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    fs,
    future::Future,
    io::Write,
    path::Path,
    sync::{
        Arc, Mutex, Weak,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow, bail};
use opus2::{Application, Channels, Decoder, Encoder};
use rtc::{
    media::Sample,
    media_stream::MediaStreamTrack,
    peer_connection::configuration::{
        interceptor_registry::register_default_interceptors,
        media_engine::{MIME_TYPE_OPUS, MediaEngine},
    },
    rtp_transceiver::rtp_sender::{
        RTCRtpCodec, RTCRtpCodingParameters, RTCRtpEncodingParameters, RtpCodecKind,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use serenity::{
    all::{
        AttachmentId, ChannelId, ChannelType, CreateAttachment, CreateMessage, CreateThread,
        EditAttachments, EditMessage, GetMessages, GuildId, MessageId, Nonce, UserId,
    },
    async_trait,
    http::Http,
};
use songbird::{
    Call, CoreEvent, Event, EventContext, EventHandler as VoiceEventHandler, Songbird, TrackEvent,
    events::context_data::{DisconnectReason, RtcpData, RtpData},
    input::Input,
    model::payload::{ClientDisconnect, Speaking},
    packet::{
        Packet,
        rtcp::{RtcpPacket, report::ReportBlockPacket},
    },
};
use tokio::{
    sync::{Mutex as AsyncMutex, broadcast, watch},
    task::JoinHandle,
};
use tracing::{debug, error, info, warn};
use uuid::Uuid;
use webrtc::{
    data_channel::{DataChannel, DataChannelEvent},
    media_stream::{
        track_local::static_sample::TrackLocalStaticSample,
        track_remote::{TrackRemote, TrackRemoteEvent},
    },
    peer_connection::{
        PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCConfiguration,
        RTCIceGatheringState, RTCPeerConnectionState, RTCSessionDescription,
    },
};

use crate::i18n::Language;
use crate::reply_files::{PreparedReply, ReplyFile, attachment_batches};
use crate::voice_input::{VoiceInputGate, VoiceInputPhase};
use crate::{config::Config, installation_check::Checks, native::Native};

mod opus_source;

const MEDIA_SAMPLE_RATE: u32 = 48_000;
const OPUS_FRAME: Duration = Duration::from_millis(20);
const OPUS_FRAME_SAMPLES: usize = 960;
const OPUS_OUTPUT_CAPACITY: usize = 4_000;
const REALTIME_SDP_TIMEOUT: Duration = Duration::from_secs(30);
const CANONICAL_DRAIN_IDLE: Duration = Duration::from_millis(500);
const RECEIVED_RTP_CACHE_CAPACITY: usize = 512;
const VOICE_SETTINGS_FILE: &str = "voice-settings.json";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct VoiceSettings {
    #[serde(default)]
    voice: Option<String>,
}

fn selected_voice_from_file(path: &Path, configured: Option<&str>) -> Result<Option<String>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<VoiceSettings>(&bytes)
            .map(|settings| settings.voice)
            .with_context(|| format!("parse voice selection {}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(configured.map(str::to_owned))
        }
        Err(error) => {
            Err(error).with_context(|| format!("read voice selection {}", path.display()))
        }
    }
}

fn persist_voice_selection(path: &Path, voice: Option<&str>) -> Result<()> {
    let parent = path
        .parent()
        .context("voice selection path has no parent")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create voice settings directory {}", parent.display()))?;
    let settings = VoiceSettings {
        voice: voice.map(str::to_owned),
    };
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .with_context(|| format!("create temporary voice settings in {}", parent.display()))?;
    serde_json::to_writer_pretty(&mut temporary, &settings).context("serialize voice selection")?;
    temporary
        .write_all(b"\n")
        .context("finish voice settings file")?;
    temporary
        .as_file()
        .sync_all()
        .context("sync voice settings file")?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("replace voice settings {}", path.display()))?;
    Ok(())
}

fn received_before_start(params: &Value, start_order: u64) -> bool {
    crate::native::state_observation_order(params).is_some_and(|(_, order)| order < start_order)
}

fn outside_call_connection(
    params: &Value,
    start_order: u64,
    start_generation: Option<u64>,
) -> bool {
    crate::native::state_observation_order(params).is_some_and(|(generation, order)| {
        order < start_order || start_generation.is_some_and(|expected| generation != expected)
    })
}

/// Discord voice is admitted only after Songbird maps the packet SSRC to a
/// configured operator's Discord user ID.
#[derive(Clone, Debug, Default)]
pub struct SpeakerRegistry {
    operators: HashSet<u64>,
    ssrc_users: HashMap<u32, u64>,
    unresolved_reported: HashSet<u32>,
}

impl SpeakerRegistry {
    pub fn new(operators: impl IntoIterator<Item = u64>) -> Self {
        Self {
            operators: operators.into_iter().collect(),
            ssrc_users: HashMap::new(),
            unresolved_reported: HashSet::new(),
        }
    }

    pub fn observe(&mut self, ssrc: u32, user_id: u64) {
        self.ssrc_users.insert(ssrc, user_id);
        self.unresolved_reported.remove(&ssrc);
    }

    pub fn operator_for(&self, ssrc: u32) -> Option<u64> {
        let user_id = *self.ssrc_users.get(&ssrc)?;
        self.operators.contains(&user_id).then_some(user_id)
    }

    pub fn unresolved(&self, ssrc: u32) -> bool {
        !self.ssrc_users.contains_key(&ssrc)
    }

    pub fn remove_user(&mut self, user_id: u64) -> Vec<u32> {
        let removed: Vec<u32> = self
            .ssrc_users
            .iter()
            .filter_map(|(ssrc, mapped_user)| (*mapped_user == user_id).then_some(*ssrc))
            .collect();
        for ssrc in &removed {
            self.ssrc_users.remove(ssrc);
            self.unresolved_reported.remove(ssrc);
        }
        removed
    }

    pub fn take_unresolved_report_slot(&mut self, ssrc: u32) -> bool {
        self.unresolved_reported.insert(ssrc)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscriptStatus {
    Partial,
    Complete,
    Incomplete,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptItem {
    /// In-memory key, which is the native item ID for canonical items.
    pub source_id: String,
    /// `None` only for flat transcript events that carry no native item ID.
    pub native_source_id: Option<String>,
    pub role: String,
    pub text: String,
    pub status: TranscriptStatus,
    pub discord_message_ids: Vec<MessageId>,
    pub speaker_ids: Vec<u64>,
    agent_label: String,
    speaker_labels: Vec<String>,
    posted_content: Option<Vec<String>>,
}

/// Keeps a realtime source item tied to one Discord message as its text moves
/// from a delta to the native canonical completion event.
#[derive(Clone, Debug)]
pub struct TranscriptLedger {
    language: Language,
    items: HashMap<String, TranscriptItem>,
    prepared_replies: HashMap<String, (String, PreparedReply)>,
    posted_reply_files: HashMap<String, Vec<PostedReplyFile>>,
    // Native source text can differ from the rendered Discord content when
    // local-file Markdown is replaced by uploaded attachments.
    posted_source_text: HashMap<String, String>,
    // Recorded only when an actual flat source is promoted to a native item.
    canonical_sources: HashMap<String, String>,
    post_attempts: HashMap<String, Vec<TranscriptPostAttempt>>,
    processed_native_observations: HashSet<(u64, u64)>,
    flat_partial_ids: HashMap<String, String>,
    pending_flat_done: VecDeque<(String, String, String, bool)>,
    flat_event_sequence: u64,
}

impl Default for TranscriptLedger {
    fn default() -> Self {
        Self::with_language(Language::English)
    }
}

impl TranscriptLedger {
    fn with_language(language: Language) -> Self {
        Self {
            language,
            items: HashMap::new(),
            prepared_replies: HashMap::new(),
            posted_reply_files: HashMap::new(),
            posted_source_text: HashMap::new(),
            canonical_sources: HashMap::new(),
            post_attempts: HashMap::new(),
            processed_native_observations: HashSet::new(),
            flat_partial_ids: HashMap::new(),
            pending_flat_done: VecDeque::new(),
            flat_event_sequence: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PostedReplyFile {
    page: usize,
    id: AttachmentId,
    filename: String,
    digest: [u8; 32],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReplyFileEdit {
    Keep(AttachmentId),
    Upload(usize),
}

fn reply_file_digest(file: &ReplyFile) -> [u8; 32] {
    use sha2::Digest as _;
    sha2::Sha256::digest(&file.data).into()
}

fn reply_file_edit_plan(files: &[ReplyFile], existing: &[PostedReplyFile]) -> Vec<ReplyFileEdit> {
    let mut unmatched = existing.to_vec();
    files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            let digest = reply_file_digest(file);
            if let Some(position) = unmatched
                .iter()
                .position(|sent| sent.filename == file.filename && sent.digest == digest)
            {
                ReplyFileEdit::Keep(unmatched.remove(position).id)
            } else {
                ReplyFileEdit::Upload(index)
            }
        })
        .collect()
}

fn match_reply_attachment_ids(
    files: &[ReplyFile],
    attachments: &[(AttachmentId, String, u32)],
) -> Result<Vec<Option<AttachmentId>>> {
    if files.len() != attachments.len() {
        bail!("Discord returned a different number of voice transcript attachments");
    }
    let mut used = vec![false; attachments.len()];
    files
        .iter()
        .map(|file| {
            let expected_size = u32::try_from(file.data.len())
                .context("voice transcript attachment exceeds Discord size metadata")?;
            let matches = attachments
                .iter()
                .enumerate()
                .filter(|(_, (_, filename, size))| {
                    filename == &file.filename && *size == expected_size
                })
                .map(|(index, (id, _, _))| (index, *id))
                .collect::<Vec<_>>();
            let Some((index, id)) = matches.iter().copied().find(|(index, _)| !used[*index]) else {
                bail!("Discord voice transcript attachment metadata does not match the sent files");
            };
            used[index] = true;
            // Equal names and sizes cannot identify a file's bytes from response
            // metadata. Delivery succeeded; omit only the reusable-ID cache entry
            // so a later edit uploads those files again without renaming them.
            Ok((matches.len() == 1).then_some(id))
        })
        .collect()
}

fn posted_reply_files_from_attachments(
    files: &[ReplyFile],
    page: usize,
    attachments: &[serenity::all::Attachment],
) -> Result<Vec<PostedReplyFile>> {
    let metadata = attachments
        .iter()
        .map(|attachment| (attachment.id, attachment.filename.clone(), attachment.size))
        .collect::<Vec<_>>();
    let ids = match_reply_attachment_ids(files, &metadata)?;
    Ok(files
        .iter()
        .zip(ids)
        .filter_map(|(file, id)| {
            id.map(|id| PostedReplyFile {
                page,
                id,
                filename: file.filename.clone(),
                digest: reply_file_digest(file),
            })
        })
        .collect())
}

#[derive(Clone, Debug)]
struct TranscriptPostAttempt {
    page: usize,
    content: String,
    nonce: String,
    message_id: Option<MessageId>,
}

impl TranscriptLedger {
    fn start_canonical(&mut self, source_id: &str, role: &str) {
        let item = self
            .items
            .entry(source_id.to_owned())
            .or_insert_with(|| TranscriptItem {
                source_id: source_id.to_owned(),
                native_source_id: Some(source_id.to_owned()),
                role: role.to_owned(),
                text: String::new(),
                status: TranscriptStatus::Partial,
                discord_message_ids: Vec::new(),
                speaker_ids: Vec::new(),
                agent_label: String::new(),
                speaker_labels: Vec::new(),
                posted_content: None,
            });
        item.role = role.to_owned();
    }

    fn has_active_canonical_role(&self, role: &str) -> bool {
        self.items.values().any(|item| {
            item.native_source_id.is_some()
                && item.role == role
                && item.status == TranscriptStatus::Partial
        })
    }

    pub fn partial(&mut self, source_id: &str, delta: &str) -> TranscriptItem {
        let item = self
            .items
            .entry(source_id.to_owned())
            .or_insert_with(|| TranscriptItem {
                source_id: source_id.to_owned(),
                native_source_id: Some(source_id.to_owned()),
                role: "unknown".into(),
                text: String::new(),
                status: TranscriptStatus::Partial,
                discord_message_ids: Vec::new(),
                speaker_ids: Vec::new(),
                agent_label: String::new(),
                speaker_labels: Vec::new(),
                posted_content: None,
            });
        if matches!(
            item.status,
            TranscriptStatus::Partial | TranscriptStatus::Incomplete
        ) {
            item.text.push_str(delta);
        }
        item.clone()
    }

    fn partial_flat(&mut self, event_key: &str, role: &str, delta: &str) -> TranscriptItem {
        let item = self
            .items
            .entry(event_key.to_owned())
            .or_insert_with(|| TranscriptItem {
                source_id: event_key.to_owned(),
                native_source_id: None,
                role: role.to_owned(),
                text: String::new(),
                status: TranscriptStatus::Partial,
                discord_message_ids: Vec::new(),
                speaker_ids: Vec::new(),
                agent_label: String::new(),
                speaker_labels: Vec::new(),
                posted_content: None,
            });
        item.role = role.to_owned();
        if item.status == TranscriptStatus::Partial {
            item.text.push_str(delta);
        }
        item.clone()
    }

    /// Returns a source item until its current canonical Discord content is recorded.
    pub fn complete(&mut self, source_id: &str, role: &str, text: &str) -> Option<TranscriptItem> {
        if let Some(item) = self.items.get(source_id) {
            if item.status == TranscriptStatus::Complete
                && item.role == role
                && item.text == text
                && (self.posted_source_text.get(source_id).map(String::as_str) == Some(text)
                    || item.posted_content.as_deref()
                        == Some(
                            transcript_content_pages_for_language(item, self.language).as_slice(),
                        ))
                && !self.has_pending_posts(source_id)
            {
                return None;
            }
        }
        let item = self
            .items
            .entry(source_id.to_owned())
            .or_insert_with(|| TranscriptItem {
                source_id: source_id.to_owned(),
                native_source_id: Some(source_id.to_owned()),
                role: role.into(),
                text: text.into(),
                status: TranscriptStatus::Complete,
                discord_message_ids: Vec::new(),
                speaker_ids: Vec::new(),
                agent_label: String::new(),
                speaker_labels: Vec::new(),
                posted_content: None,
            });
        item.role = role.to_owned();
        item.text = text.to_owned();
        item.status = TranscriptStatus::Complete;
        Some(item.clone())
    }

    fn finish_flat(&mut self, event_key: &str, role: &str, text: &str) -> TranscriptItem {
        let item = self
            .items
            .entry(event_key.to_owned())
            .or_insert_with(|| TranscriptItem {
                source_id: event_key.to_owned(),
                native_source_id: None,
                role: role.to_owned(),
                text: text.to_owned(),
                status: TranscriptStatus::Partial,
                discord_message_ids: Vec::new(),
                speaker_ids: Vec::new(),
                agent_label: String::new(),
                speaker_labels: Vec::new(),
                posted_content: None,
            });
        item.role = role.to_owned();
        item.text = text.to_owned();
        // Only a canonical item completion can confirm this native-ID-less text.
        item.status = TranscriptStatus::Partial;
        item.clone()
    }

    fn observe_flat_delta(&mut self, role: &str, delta: &str) -> Option<TranscriptItem> {
        if self.has_active_canonical_role(role) {
            return None;
        }
        let event_key = self
            .flat_partial_ids
            .entry(role.to_owned())
            .or_insert_with(|| {
                self.flat_event_sequence += 1;
                format!("transcript-event-{}", self.flat_event_sequence)
            })
            .clone();
        Some(self.partial_flat(&event_key, role, delta))
    }

    fn observe_flat_done(&mut self, role: &str, text: &str) -> Option<TranscriptItem> {
        let already_canonical = self.items.values().any(|item| {
            item.native_source_id.is_some()
                && item.status == TranscriptStatus::Complete
                && item.role == role
                && item.text == text
        });
        let has_active_flat = self.flat_partial_ids.get(role).is_some_and(|event_key| {
            self.items
                .get(event_key)
                .is_some_and(|item| item.native_source_id.is_none())
        });
        if already_canonical && !has_active_flat {
            self.flat_partial_ids.remove(role);
            return None;
        }
        let canonical_active = self.has_active_canonical_role(role);
        let event_key = self.flat_partial_ids.remove(role).unwrap_or_else(|| {
            self.flat_event_sequence += 1;
            format!("transcript-event-{}", self.flat_event_sequence)
        });
        let item = (!canonical_active || self.items.contains_key(&event_key))
            .then(|| self.finish_flat(&event_key, role, text));
        let has_flat_item = self.items.contains_key(&event_key);
        self.pending_flat_done.push_back((
            role.to_owned(),
            text.to_owned(),
            event_key,
            has_flat_item,
        ));
        item
    }

    fn observe_canonical_completion(
        &mut self,
        item_id: &str,
        role: &str,
        text: &str,
    ) -> Option<TranscriptItem> {
        if self
            .items
            .get(item_id)
            .is_some_and(|item| item.status == TranscriptStatus::Complete)
        {
            return self.complete(item_id, role, text);
        }
        let flat_match = self
            .pending_flat_done
            .iter()
            .position(|(flat_role, flat_text, _, _)| flat_role == role && flat_text == text)
            .and_then(|index| self.pending_flat_done.remove(index));
        if let Some((_, _, event_key, true)) = flat_match {
            self.canonicalize_flat(&event_key, item_id, role, text)
        } else {
            self.complete(item_id, role, text)
        }
    }

    fn canonicalize_flat(
        &mut self,
        event_key: &str,
        source_id: &str,
        role: &str,
        text: &str,
    ) -> Option<TranscriptItem> {
        let fallback = self.items.get(event_key)?.clone();
        let mut canonical = self.complete(source_id, role, text)?;
        self.items.remove(event_key);
        self.canonical_sources
            .insert(event_key.to_owned(), source_id.to_owned());
        for message_id in fallback.discord_message_ids {
            if !canonical.discord_message_ids.contains(&message_id) {
                canonical.discord_message_ids.push(message_id);
            }
        }
        canonical.speaker_ids = fallback.speaker_ids.clone();
        canonical.posted_content = None;
        self.posted_source_text.remove(event_key);
        self.posted_source_text.remove(source_id);
        if let Some(cached) = self.prepared_replies.remove(event_key) {
            self.prepared_replies
                .entry(source_id.to_owned())
                .or_insert(cached);
        }
        if let Some(files) = self.posted_reply_files.remove(event_key) {
            self.posted_reply_files
                .entry(source_id.to_owned())
                .or_insert(files);
        }
        if let Some(stored) = self.items.get_mut(source_id) {
            stored.discord_message_ids = canonical.discord_message_ids.clone();
            stored.posted_content = None;
        }
        Some(canonical)
    }

    fn merge_speaker_ids(&mut self, source_id: &str, speaker_ids: impl IntoIterator<Item = u64>) {
        if let Some(item) = self.items.get_mut(source_id) {
            for speaker_id in speaker_ids {
                if !item.speaker_ids.contains(&speaker_id) {
                    item.speaker_ids.push(speaker_id);
                }
            }
            item.speaker_ids.sort_unstable();
        }
    }

    pub fn bind_discord_message(&mut self, source_id: &str, message_id: MessageId) {
        if let Some(item) = self.items.get_mut(source_id) {
            if !item.discord_message_ids.contains(&message_id) {
                item.discord_message_ids.push(message_id);
            }
        }
    }

    fn needs_discord_lookup(&self, item: &TranscriptItem, contents: &[String]) -> bool {
        item.discord_message_ids.len() < contents.len() || self.has_pending_posts(&item.source_id)
    }

    fn record_post_attempt(&mut self, source_id: &str, page: usize, content: &str) -> usize {
        let attempts = self.post_attempts.entry(source_id.to_owned()).or_default();
        let index = attempts.len();
        attempts.push(TranscriptPostAttempt {
            page,
            content: content.to_owned(),
            nonce: format!("c{}", &Uuid::new_v4().simple().to_string()[..24]),
            message_id: None,
        });
        index
    }

    fn confirm_post_attempt(&mut self, source_id: &str, index: usize, message_id: MessageId) {
        if let Some(attempt) = self
            .post_attempts
            .get_mut(source_id)
            .and_then(|attempts| attempts.get_mut(index))
        {
            attempt.message_id = Some(message_id);
        }
    }

    fn pending_posts(&self, source_id: &str) -> Vec<(String, usize, usize, String, String)> {
        let mut pending = Vec::new();
        for (original_source, attempts) in &self.post_attempts {
            if original_source != source_id
                && self
                    .canonical_sources
                    .get(original_source)
                    .map(String::as_str)
                    != Some(source_id)
            {
                continue;
            }
            for (index, attempt) in attempts.iter().enumerate() {
                if attempt.message_id.is_none() {
                    pending.push((
                        original_source.clone(),
                        index,
                        attempt.page,
                        attempt.content.clone(),
                        attempt.nonce.clone(),
                    ));
                }
            }
        }
        pending.sort_by_key(|(source, index, page, _, _)| (*page, source.clone(), *index));
        pending
    }

    fn has_pending_posts(&self, source_id: &str) -> bool {
        !self.pending_posts(source_id).is_empty()
    }

    fn bind_discord_page(
        &mut self,
        source_id: &str,
        page: usize,
        message_id: MessageId,
    ) -> Result<()> {
        let item = self
            .items
            .get_mut(source_id)
            .context("recovered transcript source is missing")?;
        if item.discord_message_ids.contains(&message_id) {
            return Ok(());
        }
        if page > item.discord_message_ids.len() {
            bail!("recovered transcript page has no known preceding page IDs");
        }
        item.discord_message_ids.insert(page, message_id);
        Ok(())
    }

    fn recover_discord_messages(
        &mut self,
        item: &TranscriptItem,
        _contents: &[String],
        messages: Vec<(MessageId, String, UserId, Option<Nonce>)>,
        expected_author: UserId,
    ) -> Result<()> {
        let mut known_message_ids = self
            .items
            .values()
            .flat_map(|record| record.discord_message_ids.iter().copied())
            .collect::<HashSet<_>>();
        let mut found = messages;
        found.sort_by_key(|(message_id, _, _, _)| message_id.get());
        let pending = self.pending_posts(&item.source_id);
        // Validate every target before mutating any source/page relation.
        // Original nonce/body and the authenticated author must all establish
        // exactly one result. Missing optional metadata remains unconfirmed.
        for (_, _, _, original_body, original_nonce) in &pending {
            let original_attempts = self
                .post_attempts
                .values()
                .flat_map(|attempts| attempts.iter())
                .filter(|attempt| {
                    attempt.message_id.is_none()
                        && &attempt.content == original_body
                        && &attempt.nonce == original_nonce
                })
                .count();
            let candidate_ids = found
                .iter()
                .filter(|message| {
                    Self::matches_original_post(
                        message,
                        expected_author,
                        original_body,
                        original_nonce,
                        &known_message_ids,
                    )
                })
                .count();
            if original_attempts != 1 || candidate_ids != 1 {
                bail!("original transcript POST is unconfirmed or ambiguous in Discord metadata");
            }
        }
        for (original_source, attempt_index, page, original_body, original_nonce) in pending {
            let Some(index) = found.iter().position(|message| {
                Self::matches_original_post(
                    message,
                    expected_author,
                    &original_body,
                    &original_nonce,
                    &known_message_ids,
                )
            }) else {
                bail!("original transcript POST evidence changed before binding");
            };
            let (message_id, _, _, _) = found.remove(index);
            self.bind_discord_page(&item.source_id, page, message_id)?;
            self.confirm_post_attempt(&original_source, attempt_index, message_id);
            known_message_ids.insert(message_id);
        }
        Ok(())
    }

    fn matches_original_post(
        message: &(MessageId, String, UserId, Option<Nonce>),
        expected_author: UserId,
        original_body: &str,
        original_nonce: &str,
        known_message_ids: &HashSet<MessageId>,
    ) -> bool {
        let (message_id, body, author, nonce) = message;
        !known_message_ids.contains(message_id)
            && *author == expected_author
            && body == original_body
            && nonce.as_ref().is_some_and(
                |nonce| matches!(nonce, Nonce::String(value) if value == original_nonce),
            )
    }

    async fn prepared_reply_for<F, Fut>(
        &mut self,
        source_id: &str,
        source_text: &str,
        prepare: F,
    ) -> PreparedReply
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = PreparedReply>,
    {
        if let Some((cached_source, reply)) = self.prepared_replies.get(source_id)
            && cached_source == source_text
        {
            return reply.clone();
        }
        let reply = prepare().await;
        self.prepared_replies.insert(
            source_id.to_owned(),
            (source_text.to_owned(), reply.clone()),
        );
        reply
    }

    fn mark_posted(&mut self, source_id: &str, content: Vec<String>) {
        if let Some(item) = self.items.get_mut(source_id) {
            item.posted_content = Some(content);
        }
        self.posted_source_text.remove(source_id);
    }

    fn mark_posted_reply_files(
        &mut self,
        source_id: &str,
        page: usize,
        files: Vec<PostedReplyFile>,
    ) {
        let posted = self
            .posted_reply_files
            .entry(source_id.to_owned())
            .or_default();
        posted.retain(|file| file.page != page);
        posted.extend(files);
    }

    fn clear_posted_reply_files_page(&mut self, source_id: &str, page: usize) {
        if let Some(posted) = self.posted_reply_files.get_mut(source_id) {
            posted.retain(|file| file.page != page);
        }
    }

    fn mark_posted_source_text(
        &mut self,
        source_id: &str,
        source_text: &str,
        content: Vec<String>,
    ) {
        self.mark_posted(source_id, content);
        self.posted_source_text
            .insert(source_id.to_owned(), source_text.to_owned());
        // Sent payload bytes are no longer needed; the source-text marker makes
        // canonical replays a no-op, while failures retain the prepared bytes.
        self.prepared_replies.remove(source_id);
        // Sent payload bytes are no longer needed; the source-text marker makes
        // canonical replays a no-op, while failures retain the prepared bytes.
        self.prepared_replies.remove(source_id);
    }

    pub fn mark_open_items_incomplete(&mut self) -> Vec<TranscriptItem> {
        self.items
            .values_mut()
            .filter_map(|item| {
                if item.status == TranscriptStatus::Partial {
                    item.status = TranscriptStatus::Incomplete;
                    Some(item.clone())
                } else {
                    None
                }
            })
            .collect()
    }
}

#[derive(Clone)]
pub struct VoiceBridge {
    config: Arc<Config>,
    native: Native,
    http: Arc<Http>,
    songbird: Arc<Songbird>,
    active: Arc<AsyncMutex<HashMap<u64, ActiveCall>>>,
    lifecycle: Arc<AsyncMutex<()>>,
    pub(crate) input_gate: Arc<VoiceInputGate>,
    // The bridge owns these original call records beyond any reader task.
    transcript_calls: Arc<Mutex<HashMap<Uuid, Arc<TranscriptCall>>>>,
    installation_checks: Option<Arc<Checks>>,
}

struct TranscriptCall {
    call_instance_id: Uuid,
    language: Language,
    reply_file_config: Option<Arc<Config>>,
    installation_checks: Option<Arc<Checks>>,
    native_thread_id: String,
    transcript_thread_id: ChannelId,
    guild_id: GuildId,
    agent_name: String,
    speaker_name_cache: Mutex<HashMap<u64, String>>,
    audio_stats: Arc<AudioStats>,
    superseded: AtomicBool,
    start_order: u64,
    start_generation: Option<u64>,
    has_prior_call: bool,
    superseded_order: AtomicU64,
    native_item_owners: Arc<Mutex<HashMap<(String, String), (Uuid, Weak<TranscriptCall>)>>>,
    native_session_history: Arc<Mutex<NativeSessionHistory>>,
    weak_self: Weak<TranscriptCall>,
    // 0: not registered, 1: receiving, 2: receive responsibility ended.
    // Final incomplete cleanup may still be using the original ledger in state 2.
    reader_state: AtomicU8,
    ledger: AsyncMutex<TranscriptLedger>,
    failures: Mutex<Vec<TranscriptDeliveryFailure>>,
    pending_native_events: Mutex<VecDeque<Value>>,
}

#[derive(Default)]
struct NativeSessionHistory {
    owners: HashMap<(String, String), (Uuid, Weak<TranscriptCall>)>,
    call_sessions: HashMap<Uuid, String>,
    unassigned: Vec<UnassignedNativeEvent>,
}

struct UnassignedNativeEvent {
    observed_by: Uuid,
    observed_transcript_thread_id: ChannelId,
    observed_calls: HashMap<Uuid, ChannelId>,
    event: Value,
    assigned_owner: Option<Uuid>,
}

struct TranscriptDeliveryFailure {
    item: TranscriptItem,
    error: String,
    resolved: bool,
}

struct TranscriptReaderRegistration(Arc<TranscriptCall>);

impl TranscriptReaderRegistration {
    fn register(call: &Arc<TranscriptCall>) -> Result<Self> {
        call.reader_state
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| anyhow!("transcript reader is already registered or ended"))?;
        Ok(Self(call.clone()))
    }
}

impl Drop for TranscriptReaderRegistration {
    fn drop(&mut self) {
        self.0.reader_state.store(2, Ordering::Release);
    }
}

impl TranscriptCall {
    #[cfg(test)]
    fn begin(
        calls: &Mutex<HashMap<Uuid, Arc<Self>>>,
        call_instance_id: Uuid,
        native_thread_id: String,
        transcript_thread_id: ChannelId,
        received_fence: &AtomicU64,
    ) -> Result<Arc<Self>> {
        Self::begin_with_generation(
            calls,
            call_instance_id,
            native_thread_id,
            transcript_thread_id,
            received_fence,
            None,
        )
    }

    #[cfg(test)]
    fn begin_with_generation(
        calls: &Mutex<HashMap<Uuid, Arc<Self>>>,
        call_instance_id: Uuid,
        native_thread_id: String,
        transcript_thread_id: ChannelId,
        received_fence: &AtomicU64,
        start_generation: Option<u64>,
    ) -> Result<Arc<Self>> {
        Self::begin_with_checks(
            calls,
            call_instance_id,
            native_thread_id,
            transcript_thread_id,
            received_fence,
            start_generation,
            None,
            None,
            GuildId::new(1),
            "Codex".to_owned(),
            Language::English,
            Arc::new(AudioStats::default()),
        )
    }

    fn begin_with_checks(
        calls: &Mutex<HashMap<Uuid, Arc<Self>>>,
        call_instance_id: Uuid,
        native_thread_id: String,
        transcript_thread_id: ChannelId,
        received_fence: &AtomicU64,
        start_generation: Option<u64>,
        installation_checks: Option<Arc<Checks>>,
        reply_file_config: Option<Arc<Config>>,
        guild_id: GuildId,
        agent_name: String,
        language: Language,
        audio_stats: Arc<AudioStats>,
    ) -> Result<Arc<Self>> {
        let mut calls = calls
            .lock()
            .map_err(|_| anyhow!("transcript call history lock poisoned"))?;
        let native_item_owners = calls
            .values()
            .next()
            .map(|call| call.native_item_owners.clone())
            .unwrap_or_else(|| Arc::new(Mutex::new(HashMap::new())));
        let native_session_history = calls
            .values()
            .next()
            .map(|call| call.native_session_history.clone())
            .unwrap_or_else(|| Arc::new(Mutex::new(NativeSessionHistory::default())));
        // Admission claims and call boundaries share this lock. No other
        // call's async ledger (which may be awaiting HTTP) is locked here.
        let _ownership = native_item_owners
            .lock()
            .map_err(|_| anyhow!("transcript native item ownership lock poisoned"))?;
        let start_order = received_fence.load(Ordering::Acquire);
        let has_prior_call = calls
            .values()
            .any(|call| call.native_thread_id == native_thread_id);
        // Set at the writer-ready realtime/start frame commit, independently of broadcast delivery.
        for previous in calls.values() {
            if previous.native_thread_id == native_thread_id
                && !previous.superseded.load(Ordering::Acquire)
            {
                previous
                    .superseded_order
                    .store(start_order, Ordering::Release);
                previous.superseded.store(true, Ordering::Release);
            }
        }
        let call = Arc::new_cyclic(|weak_self| Self {
            call_instance_id,
            language,
            reply_file_config,
            installation_checks,
            native_thread_id,
            transcript_thread_id,
            guild_id,
            agent_name,
            speaker_name_cache: Mutex::new(HashMap::new()),
            audio_stats,
            superseded: AtomicBool::new(false),
            start_order,
            start_generation,
            has_prior_call,
            superseded_order: AtomicU64::new(0),
            native_item_owners: native_item_owners.clone(),
            native_session_history: native_session_history.clone(),
            weak_self: weak_self.clone(),
            reader_state: AtomicU8::new(0),
            ledger: AsyncMutex::new(TranscriptLedger::with_language(language)),
            failures: Mutex::new(Vec::new()),
            pending_native_events: Mutex::new(VecDeque::new()),
        });
        calls.insert(call_instance_id, call.clone());
        Ok(call)
    }

    async fn display_name_for(&self, http: &Http, user_id: u64) -> String {
        if let Ok(cache) = self.speaker_name_cache.lock() {
            if let Some(name) = cache.get(&user_id) {
                return name.clone();
            }
        }
        let resolved = match self.guild_id.member(http, UserId::new(user_id)).await {
            Ok(member) => discord_member_display_name(
                member.nick.as_deref(),
                member.user.global_name.as_deref(),
                &member.user.name,
            ),
            Err(error) => {
                warn!(guild_id = self.guild_id.get(), user_id, %error, "could not resolve transcript speaker name");
                format!("Discord user {user_id}")
            }
        };
        if let Ok(mut cache) = self.speaker_name_cache.lock() {
            cache.insert(user_id, resolved.clone());
        }
        resolved
    }

    fn accepts_event(&self, params: &Value) -> Result<bool> {
        let mut owners = self
            .native_item_owners
            .lock()
            .map_err(|_| anyhow!("transcript native item ownership lock poisoned"))?;
        let superseded = self.superseded.load(Ordering::Acquire);
        let received_order = crate::native::state_observation_order(params).map(|(_, order)| order);
        if received_before_start(params, self.start_order) {
            return Ok(false);
        }
        let cutoff = self.superseded_order.load(Ordering::Acquire);
        let in_call = received_order
            .map(|order| cutoff == 0 || order < cutoff)
            .unwrap_or(!superseded);
        let Some(item_id) = params
            .get("itemId")
            .and_then(Value::as_str)
            .or_else(|| params.pointer("/item/id").and_then(Value::as_str))
        else {
            return Ok(in_call
                && !outside_call_connection(params, self.start_order, self.start_generation));
        };
        let key = (self.native_thread_id.clone(), item_id.to_owned());
        let sessions = self
            .native_session_history
            .lock()
            .map_err(|_| anyhow!("native session history lock poisoned"))?;
        // Actual session proof is checked before an earlier sessionless claim.
        if let Some(session_id) = params
            .pointer("/item/realtimeSessionId")
            .and_then(Value::as_str)
        {
            return Ok(sessions
                .owners
                .get(&(self.native_thread_id.clone(), session_id.to_owned()))
                .is_some_and(|(owner, _)| *owner == self.call_instance_id));
        }
        if sessions.unassigned.iter().any(|original| {
            original.assigned_owner.is_none()
                && original
                    .event
                    .pointer("/params/threadId")
                    .and_then(Value::as_str)
                    == Some(self.native_thread_id.as_str())
                && original
                    .event
                    .pointer("/params/item/id")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        original
                            .event
                            .pointer("/params/itemId")
                            .and_then(Value::as_str)
                    })
                    == Some(item_id)
        }) {
            return Ok(false);
        }
        if let Some(owner) = owners.get(&key) {
            return Ok(owner.0 == self.call_instance_id);
        }
        if !in_call || outside_call_connection(params, self.start_order, self.start_generation) {
            return Ok(false);
        }
        owners.insert(key, (self.call_instance_id, self.weak_self.clone()));
        Ok(true)
    }

    fn within_original_interval(&self, params: &Value) -> bool {
        if outside_call_connection(params, self.start_order, self.start_generation) {
            return false;
        }
        let cutoff = self.superseded_order.load(Ordering::Acquire);
        crate::native::state_observation_order(params)
            .map(|(_, order)| cutoff == 0 || order < cutoff)
            .unwrap_or(!self.superseded.load(Ordering::Acquire))
    }

    fn native_event_owner(&self, method: &str, event: &Value) -> Result<Option<Arc<Self>>> {
        let params = event.get("params").unwrap_or(&Value::Null);
        let Some(thread_id) = params.get("threadId").and_then(Value::as_str) else {
            return Ok(None);
        };
        let item_id = params
            .get("itemId")
            .and_then(Value::as_str)
            .or_else(|| params.pointer("/item/id").and_then(Value::as_str));
        let session_control = method == "thread/realtime/started"
            || (matches!(
                method,
                "thread/realtime/item/started" | "thread/realtime/item/completed"
            ) && params.pointer("/item/type").and_then(Value::as_str)
                == Some("realtimeSessionStarted"));
        let session_id = params
            .pointer("/item/realtimeSessionId")
            .and_then(Value::as_str)
            .or_else(|| {
                (method == "thread/realtime/started")
                    .then(|| params.get("realtimeSessionId").and_then(Value::as_str))
                    .flatten()
            });
        // Every claim/control uses the same lock as begin. No ledger or HTTP
        // await occurs while item ownership/session history is locked.
        let mut items = self
            .native_item_owners
            .lock()
            .map_err(|_| anyhow!("transcript native item ownership lock poisoned"))?;
        let mut history = self
            .native_session_history
            .lock()
            .map_err(|_| anyhow!("native session history lock poisoned"))?;
        // A received canonical session relation can correct a prior sessionless
        // routing claim. Original ledgers/IDs/failures are never moved or erased.
        if let Some(session_id) = session_id {
            if let Some((owner_id, owner)) = history
                .owners
                .get(&(thread_id.to_owned(), session_id.to_owned()))
                .cloned()
            {
                let owner = owner
                    .upgrade()
                    .context("retained native session owner is missing")?;
                if let Some(item_id) = item_id {
                    if let Some(previous) = items
                        .get(&(thread_id.to_owned(), item_id.to_owned()))
                        .cloned()
                    {
                        if previous.0 != owner_id {
                            let previous_owner = previous
                                .1
                                .upgrade()
                                .context("retained previous native transcript owner is missing")?;
                            Self::reroute_pending_native_item(
                                &previous_owner,
                                &owner,
                                thread_id,
                                item_id,
                                session_id,
                            )?;
                            warn!(previous_call = %previous.0, original_call = %owner_id,
                                native_thread_id = thread_id, native_item_id = item_id, realtime_session_id = session_id,
                                "observed session corrects earlier item routing; prior raw and delivery history are retained");
                        }
                    }
                    items.insert(
                        (thread_id.to_owned(), item_id.to_owned()),
                        (owner_id, owner.weak_self.clone()),
                    );
                }
                self.resolve_unassigned_native_events(
                    &mut items,
                    &mut history,
                    thread_id,
                    session_id,
                    &owner,
                    item_id,
                )?;
                return Ok(Some(owner));
            }
        } else {
            let waiting = item_id.is_some_and(|item_id| {
                history.unassigned.iter().any(|original| {
                    original.assigned_owner.is_none()
                        && original
                            .event
                            .pointer("/params/threadId")
                            .and_then(Value::as_str)
                            == Some(thread_id)
                        && original
                            .event
                            .pointer("/params/item/id")
                            .and_then(Value::as_str)
                            .or_else(|| {
                                original
                                    .event
                                    .pointer("/params/itemId")
                                    .and_then(Value::as_str)
                            })
                            == Some(item_id)
                })
            });
            if !waiting {
                if let Some(owner) =
                    item_id.and_then(|id| items.get(&(thread_id.to_owned(), id.to_owned())))
                {
                    return owner
                        .1
                        .upgrade()
                        .context("retained native transcript owner is missing")
                        .map(Some);
                }
            }
            let unseen_realtime_delta = self.has_prior_call
                && method == "thread/realtime/item/transcript/delta"
                && params.get("item").and_then(Value::as_object).is_none()
                && item_id.is_some();
            if thread_id == self.native_thread_id && (waiting || unseen_realtime_delta) {
                self.retain_unassigned_event(&mut history, event, thread_id);
            }
            return Ok(None);
        }
        let session_id = session_id.expect("sessionless events returned above");
        let session_key = (thread_id.to_owned(), session_id.to_owned());
        let admissible_control = session_control
            && thread_id == self.native_thread_id
            && self.within_original_interval(params)
            && history
                .call_sessions
                .get(&self.call_instance_id)
                .is_none_or(|known| known == session_id);
        if admissible_control {
            history
                .owners
                .insert(session_key, (self.call_instance_id, self.weak_self.clone()));
            history
                .call_sessions
                .entry(self.call_instance_id)
                .or_insert_with(|| session_id.to_owned());
            if let Some(item_id) = item_id {
                items.insert(
                    (thread_id.to_owned(), item_id.to_owned()),
                    (self.call_instance_id, self.weak_self.clone()),
                );
            }
            let owner = self
                .weak_self
                .upgrade()
                .context("current native transcript owner is missing")?;
            self.resolve_unassigned_native_events(
                &mut items,
                &mut history,
                thread_id,
                session_id,
                &owner,
                item_id,
            )?;
            return self
                .weak_self
                .upgrade()
                .context("current native transcript owner is missing")
                .map(Some);
        }
        if thread_id == self.native_thread_id {
            self.retain_unassigned_event(&mut history, event, thread_id);
        }
        Ok(None)
    }

    fn resolve_unassigned_native_events(
        &self,
        items: &mut HashMap<(String, String), (Uuid, Weak<Self>)>,
        history: &mut NativeSessionHistory,
        thread_id: &str,
        session_id: &str,
        owner: &Arc<Self>,
        incoming_item_id: Option<&str>,
    ) -> Result<()> {
        let mut proven_items = HashSet::new();
        if let Some(item_id) = incoming_item_id {
            proven_items.insert(item_id.to_owned());
        }
        // Prove exact item/session relations first without enqueueing. The next
        // pass preserves original receive order, including delta before canonical.
        for pending in &history.unassigned {
            let params = pending.event.get("params").unwrap_or(&Value::Null);
            if pending.assigned_owner.is_some()
                || params.get("threadId").and_then(Value::as_str) != Some(thread_id)
                || params
                    .pointer("/item/realtimeSessionId")
                    .and_then(Value::as_str)
                    != Some(session_id)
            {
                continue;
            }
            if let Some(item_id) = params
                .get("itemId")
                .and_then(Value::as_str)
                .or_else(|| params.pointer("/item/id").and_then(Value::as_str))
            {
                proven_items.insert(item_id.to_owned());
                if let Some(previous) = items
                    .get(&(thread_id.to_owned(), item_id.to_owned()))
                    .cloned()
                {
                    if previous.0 != owner.call_instance_id {
                        let previous_owner = previous
                            .1
                            .upgrade()
                            .context("retained previous native transcript owner is missing")?;
                        Self::reroute_pending_native_item(
                            &previous_owner,
                            owner,
                            thread_id,
                            item_id,
                            session_id,
                        )?;
                        warn!(previous_call = %previous.0, original_call = %owner.call_instance_id,
                            native_thread_id = thread_id, native_item_id = item_id, realtime_session_id = session_id,
                            "observed session corrects earlier item routing; prior raw and delivery history are retained");
                    }
                }
                items.insert(
                    (thread_id.to_owned(), item_id.to_owned()),
                    (owner.call_instance_id, owner.weak_self.clone()),
                );
            }
        }
        for pending in &mut history.unassigned {
            let params = pending.event.get("params").unwrap_or(&Value::Null);
            if pending.assigned_owner.is_some()
                || params.get("threadId").and_then(Value::as_str) != Some(thread_id)
            {
                continue;
            }
            let original_session = params
                .pointer("/item/realtimeSessionId")
                .and_then(Value::as_str)
                .or_else(|| {
                    (pending.event.get("method").and_then(Value::as_str)
                        == Some("thread/realtime/started"))
                    .then(|| params.get("realtimeSessionId").and_then(Value::as_str))
                    .flatten()
                });
            let original_id = params
                .get("itemId")
                .and_then(Value::as_str)
                .or_else(|| params.pointer("/item/id").and_then(Value::as_str));
            if original_session == Some(session_id)
                || (original_session.is_none()
                    && original_id.is_some_and(|id| proven_items.contains(id)))
            {
                owner
                    .pending_native_events
                    .lock()
                    .map_err(|_| anyhow!("startup transcript backlog lock poisoned"))?
                    .push_back(pending.event.clone());
                pending.assigned_owner = Some(owner.call_instance_id);
            }
        }
        Ok(())
    }

    fn reroute_pending_native_item(
        previous: &Arc<Self>,
        owner: &Arc<Self>,
        thread_id: &str,
        item_id: &str,
        session_id: &str,
    ) -> Result<()> {
        let mut captured = previous
            .pending_native_events
            .lock()
            .map_err(|_| anyhow!("previous transcript backlog lock poisoned"))?
            .iter()
            .filter(|event| {
                let params = event.get("params").unwrap_or(&Value::Null);
                params.get("threadId").and_then(Value::as_str) == Some(thread_id)
                    && params
                        .get("itemId")
                        .and_then(Value::as_str)
                        .or_else(|| params.pointer("/item/id").and_then(Value::as_str))
                        == Some(item_id)
                    && params
                        .pointer("/item/realtimeSessionId")
                        .and_then(Value::as_str)
                        .is_none_or(|known| known == session_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        if captured.is_empty() {
            return Ok(());
        }
        // Leave originals in place until the destination has a full copy. Never
        // nest two call queue locks or remove newly enqueued, uncaptured events.
        owner
            .pending_native_events
            .lock()
            .map_err(|_| anyhow!("original transcript backlog lock poisoned"))?
            .extend(captured.iter().cloned());
        previous
            .pending_native_events
            .lock()
            .map_err(|_| anyhow!("previous transcript backlog lock poisoned"))?
            .retain(|event| {
                if let Some(index) = captured.iter().position(|original| original == event) {
                    captured.remove(index);
                    false
                } else {
                    true
                }
            });
        Ok(())
    }

    fn enqueue_native_event(&self, proposed: Arc<Self>, event: Value) -> Result<Option<Arc<Self>>> {
        // Recheck under the same lock as a route correction, closing the gap
        // between selecting an owner and appending to that owner's queue.
        let items = self
            .native_item_owners
            .lock()
            .map_err(|_| anyhow!("transcript native item ownership lock poisoned"))?;
        let mut history = self
            .native_session_history
            .lock()
            .map_err(|_| anyhow!("native session history lock poisoned"))?;
        let params = event.get("params").unwrap_or(&Value::Null);
        let thread_id = params.get("threadId").and_then(Value::as_str);
        let item_id = params
            .get("itemId")
            .and_then(Value::as_str)
            .or_else(|| params.pointer("/item/id").and_then(Value::as_str));
        let session_id = params
            .pointer("/item/realtimeSessionId")
            .and_then(Value::as_str)
            .or_else(|| {
                (event.get("method").and_then(Value::as_str) == Some("thread/realtime/started"))
                    .then(|| params.get("realtimeSessionId").and_then(Value::as_str))
                    .flatten()
            });
        let waiting = thread_id.is_some_and(|thread_id| {
            session_id.is_some_and(|session_id| {
                !history
                    .owners
                    .contains_key(&(thread_id.to_owned(), session_id.to_owned()))
            }) || (session_id.is_none()
                && item_id.is_some_and(|item_id| {
                    history.unassigned.iter().any(|original| {
                        original.assigned_owner.is_none()
                            && original
                                .event
                                .pointer("/params/threadId")
                                .and_then(Value::as_str)
                                == Some(thread_id)
                            && original
                                .event
                                .pointer("/params/item/id")
                                .and_then(Value::as_str)
                                .or_else(|| {
                                    original
                                        .event
                                        .pointer("/params/itemId")
                                        .and_then(Value::as_str)
                                })
                                == Some(item_id)
                    })
                }))
        });
        if waiting {
            self.retain_unassigned_event(
                &mut history,
                &event,
                thread_id.expect("waiting event has a thread"),
            );
            return Ok(None);
        }
        let current = thread_id.and_then(|thread_id| {
            if let Some(session_id) = session_id {
                history
                    .owners
                    .get(&(thread_id.to_owned(), session_id.to_owned()))
            } else {
                item_id.and_then(|item_id| items.get(&(thread_id.to_owned(), item_id.to_owned())))
            }
        });
        let owner = if let Some((_, owner)) = current {
            owner
                .upgrade()
                .context("retained queued native transcript owner is missing")?
        } else {
            proposed
        };
        owner
            .pending_native_events
            .lock()
            .map_err(|_| anyhow!("startup transcript backlog lock poisoned"))?
            .push_back(event);
        Ok(Some(owner))
    }

    fn retain_unassigned_event(
        &self,
        history: &mut NativeSessionHistory,
        event: &Value,
        thread_id: &str,
    ) {
        let params = event.get("params").unwrap_or(&Value::Null);
        let observation = crate::native::state_observation_order(params);
        let previous = observation.and_then(|observation| {
            history.unassigned.iter_mut().find(|original| {
                original
                    .event
                    .pointer("/params/threadId")
                    .and_then(Value::as_str)
                    == Some(thread_id)
                    && original
                        .event
                        .get("params")
                        .and_then(crate::native::state_observation_order)
                        == Some(observation)
            })
        });
        if let Some(previous) = previous {
            previous
                .observed_calls
                .entry(self.call_instance_id)
                .or_insert(self.transcript_thread_id);
        } else {
            history.unassigned.push(UnassignedNativeEvent {
                observed_by: self.call_instance_id,
                observed_transcript_thread_id: self.transcript_thread_id,
                observed_calls: HashMap::from([(self.call_instance_id, self.transcript_thread_id)]),
                event: event.clone(),
                assigned_owner: None,
            });
            let realtime_session_id = params
                .pointer("/item/realtimeSessionId")
                .and_then(Value::as_str);
            let direct_item_id = params.get("itemId").and_then(Value::as_str);
            let native_item_id =
                direct_item_id.or_else(|| params.pointer("/item/id").and_then(Value::as_str));
            warn!(call_instance_id = %self.call_instance_id, native_thread_id = thread_id,
                realtime_session_id = ?realtime_session_id, native_item_id = ?native_item_id,
                "retained native event with an unassigned realtime session owner");
        }
    }

    fn observe_installation_realtime_item(&self, method: &str, params: &Value) {
        let Some(checks) = &self.installation_checks else {
            return;
        };
        if method != "thread/realtime/item/completed"
            || params.get("threadId").and_then(Value::as_str)
                != Some(self.native_thread_id.as_str())
            || !self.accepts_event(params).unwrap_or(false)
        {
            return;
        }
        let Some(item) = params.get("item") else {
            return;
        };
        if item.get("type").and_then(Value::as_str) == Some("realtimeSessionClosed")
            && item.get("outcome").and_then(Value::as_str) == Some("failed")
            && item
                .get("id")
                .and_then(Value::as_str)
                .is_some_and(|id| !id.is_empty())
            && item
                .get("realtimeSessionId")
                .and_then(Value::as_str)
                .is_some_and(|session| !session.is_empty())
        {
            checks.note_voice_error(
                &self.native_thread_id,
                &self.call_instance_id,
                "native canonical realtime session closed with failed outcome",
            );
            return;
        }
        if item.get("type").and_then(Value::as_str) != Some("bemItemPromoted") {
            return;
        }
        let (Some(id), Some(session), Some(turn), Some(backing_item)) = (
            item.get("id").and_then(Value::as_str),
            item.get("realtimeSessionId").and_then(Value::as_str),
            item.get("turn_id").and_then(Value::as_str),
            item.get("item_id").and_then(Value::as_str),
        ) else {
            return;
        };
        let presentation = item.get("presentation");
        let valid_presentation = match presentation
            .and_then(|value| value.get("type"))
            .and_then(Value::as_str)
        {
            Some("wholeItem" | "inlineMarkdown") => true,
            Some("inlineVisualization") => presentation
                .and_then(|value| value.get("index"))
                .and_then(Value::as_u64)
                .is_some_and(|index| index <= u64::from(u32::MAX)),
            _ => false,
        };
        if [id, session, turn, backing_item]
            .iter()
            .any(|value| value.is_empty())
            || !valid_presentation
        {
            return;
        }
        let generation = params
            .get("_discodex_connection_generation")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        checks.note_promotion(
            &self.native_thread_id,
            &self.call_instance_id,
            session,
            turn,
            backing_item,
            generation,
        );
    }

    fn observe_installation_transcript(
        &self,
        ledger: &TranscriptLedger,
        params: &Value,
        source_id: &str,
        canonical_completed_event: bool,
        result: &Result<()>,
    ) {
        let Some(checks) = &self.installation_checks else {
            return;
        };
        if params.get("threadId").and_then(Value::as_str) != Some(self.native_thread_id.as_str()) {
            return;
        }
        let Some(canonical) = params.get("item") else {
            return;
        };
        if canonical.get("type").and_then(Value::as_str) != Some("transcriptSegment") {
            return;
        }
        let (Some(item_id), Some(session), Some(role), Some(text)) = (
            canonical.get("id").and_then(Value::as_str),
            canonical.get("realtimeSessionId").and_then(Value::as_str),
            canonical.get("role").and_then(Value::as_str),
            canonical.get("text").and_then(Value::as_str),
        ) else {
            return;
        };
        if item_id.is_empty() || session.is_empty() || !matches!(role, "user" | "assistant") {
            return;
        }
        let Some(item) = ledger.items.get(source_id) else {
            return;
        };
        let complete = canonical_completed_event
            && item.status == TranscriptStatus::Complete
            && item.native_source_id.as_deref() == Some(item_id)
            && item.role == role
            && item.text == text;
        let pages = transcript_content_pages_for_language(item, self.language);
        let rendered_source_posted = ledger
            .posted_source_text
            .get(source_id)
            .is_some_and(|posted| posted == &item.text);
        let posted_content_matches =
            rendered_source_posted || item.posted_content.as_deref() == Some(pages.as_slice());
        let posted_page_count = item
            .posted_content
            .as_ref()
            .is_some_and(|posted| item.discord_message_ids.len() >= posted.len());
        let all_pages_posted = result.is_ok()
            && posted_content_matches
            && posted_page_count
            && !ledger.has_pending_posts(source_id);
        let error = result.as_ref().err().map(|error| format!("{error:#}"));
        checks.note_transcript(
            &self.native_thread_id,
            &self.call_instance_id,
            session,
            role,
            item_id,
            !text.trim().is_empty(),
            complete,
            all_pages_posted,
            error.as_deref(),
        );
    }

    fn record_delivery(
        &self,
        ledger: &TranscriptLedger,
        source_id: &str,
        result: &Result<()>,
    ) -> Result<()> {
        let mut failures = self
            .failures
            .lock()
            .map_err(|_| anyhow!("transcript delivery history lock poisoned"))?;
        match result {
            Ok(()) => {
                // A newly confirmed POST cannot resolve a different pending
                // original attempt that the successful query did not recover.
                if ledger.has_pending_posts(source_id) {
                    return Ok(());
                }
                for previous in failures.iter_mut().filter(|failure| {
                    failure.item.source_id == source_id
                        || ledger
                            .canonical_sources
                            .get(&failure.item.source_id)
                            .is_some_and(|canonical_id| canonical_id == source_id)
                }) {
                    previous.resolved = true;
                }
            }
            Err(error) => {
                let item = ledger
                    .items
                    .get(source_id)
                    .context("failed transcript source is missing from its call ledger")?
                    .clone();
                warn!(
                    call_instance_id = %self.call_instance_id,
                    native_thread_id = %self.native_thread_id,
                    transcript_thread_id = self.transcript_thread_id.get(),
                    source_id,
                    native_source_id = ?item.native_source_id,
                    discord_message_ids = ?item.discord_message_ids,
                    error = %error,
                    "retained failed transcript delivery in private call history"
                );
                failures.push(TranscriptDeliveryFailure {
                    item,
                    error: format!("{error:#}"),
                    resolved: false,
                });
            }
        }
        Ok(())
    }

    fn delivery_result(&self) -> Result<()> {
        let failures = self
            .failures
            .lock()
            .map_err(|_| anyhow!("transcript delivery history lock poisoned"))?;
        if let Some(failure) = failures.iter().find(|failure| !failure.resolved) {
            bail!("transcript delivery remains failed: {}", failure.error);
        }
        let history = self
            .native_session_history
            .lock()
            .map_err(|_| anyhow!("native session history lock poisoned"))?;
        if history.unassigned.iter().any(|event| {
            event.observed_calls.contains_key(&self.call_instance_id)
                && event.assigned_owner.is_none()
        }) {
            bail!("received native transcript events still have an unassigned call owner");
        }
        drop(history);
        if !self
            .pending_native_events
            .lock()
            .map_err(|_| anyhow!("startup transcript backlog lock poisoned"))?
            .is_empty()
        {
            bail!("retained native transcript events still await original delivery");
        }
        Ok(())
    }
}

struct ActiveCall {
    call_instance_id: Uuid,
    native_thread_id: String,
    channel_id: ChannelId,
    transcript_thread_id: ChannelId,
    transcript_parent_channel_id: ChannelId,
    transcript_anchor_message_id: MessageId,
    peer: Arc<dyn PeerConnection>,
    _events_channel: Arc<dyn DataChannel>,
    _output_track: songbird::tracks::TrackHandle,
    media_cancel: watch::Sender<bool>,
    transcript_cancel: watch::Sender<bool>,
    native_close_cancel: watch::Sender<bool>,
    media_task: JoinHandle<()>,
    transcript_task: JoinHandle<Result<()>>,
    native_close_task: JoinHandle<()>,
    data_channel_task: JoinHandle<()>,
    playback: AudioPipe,
    audio_stats: Arc<AudioStats>,
}

#[derive(Default)]
struct TranscriptSpeakerAttribution {
    pending: HashSet<u64>,
    active: HashMap<String, HashSet<u64>>,
}

#[derive(Default)]
struct AudioStats {
    transcript_speakers: Mutex<TranscriptSpeakerAttribution>,
    installation_checks: Option<Arc<Checks>>,
    native_thread_id: Option<String>,
    call_instance_id: Option<Uuid>,
    bot_ssrc: Mutex<Option<u32>>,
    input_lengths: Mutex<BTreeMap<usize, u64>>,
    input_frames: AtomicU64,
    input_samples: AtomicU64,
    input_non_silent_samples: AtomicU64,
    forwarded_frames: AtomicU64,
    forwarded_samples: AtomicU64,
    forwarded_non_silent_samples: AtomicU64,
    opus_packets_sent: AtomicU64,
    opus_bytes_sent: AtomicU64,
    native_opus_packets: AtomicU64,
    native_decoded_samples: AtomicU64,
    native_non_silent_samples: AtomicU64,
    data_channel_open: AtomicU64,
    data_channel_event_types: Mutex<BTreeMap<String, u64>>,
    data_channel_events: Mutex<Vec<Value>>,
}

#[derive(Clone, Debug, Default)]
pub struct VoiceAudioStats {
    pub input_frame_lengths: BTreeMap<usize, u64>,
    pub input_frames: u64,
    pub input_samples: u64,
    pub input_non_silent_samples: u64,
    pub forwarded_frames: u64,
    pub forwarded_samples: u64,
    pub forwarded_non_silent_samples: u64,
    pub opus_packets_sent: u64,
    pub opus_bytes_sent: u64,
    pub native_opus_packets: u64,
    pub native_decoded_samples: u64,
    pub native_non_silent_samples: u64,
    pub data_channel_open: bool,
    pub data_channel_event_types: BTreeMap<String, u64>,
    pub data_channel_events: Vec<Value>,
}

fn sorted_speaker_ids(speakers: &HashSet<u64>) -> Vec<u64> {
    let mut speakers: Vec<_> = speakers.iter().copied().collect();
    speakers.sort_unstable();
    speakers
}

impl AudioStats {
    fn note_transcript_speakers(&self, speakers: impl IntoIterator<Item = u64>) {
        let Ok(mut attribution) = self.transcript_speakers.lock() else {
            return;
        };
        for speaker in speakers {
            if attribution.active.is_empty() {
                attribution.pending.insert(speaker);
            } else {
                for active in attribution.active.values_mut() {
                    active.insert(speaker);
                }
            }
        }
    }

    fn start_transcript_item(&self, item_id: &str) -> Vec<u64> {
        let Ok(mut attribution) = self.transcript_speakers.lock() else {
            return Vec::new();
        };
        let pending = std::mem::take(&mut attribution.pending);
        attribution
            .active
            .entry(item_id.to_owned())
            .or_default()
            .extend(pending);
        attribution
            .active
            .get(item_id)
            .map(sorted_speaker_ids)
            .unwrap_or_default()
    }

    fn ensure_transcript_item(&self, item_id: &str) -> Vec<u64> {
        let already_active = self
            .transcript_speakers
            .lock()
            .map(|attribution| attribution.active.contains_key(item_id))
            .unwrap_or(false);
        if !already_active {
            return self.start_transcript_item(item_id);
        }
        self.transcript_speakers
            .lock()
            .ok()
            .and_then(|attribution| attribution.active.get(item_id).map(sorted_speaker_ids))
            .unwrap_or_default()
    }

    fn finish_transcript_item(&self, item_id: &str) -> Vec<u64> {
        let Ok(mut attribution) = self.transcript_speakers.lock() else {
            return Vec::new();
        };
        attribution
            .active
            .remove(item_id)
            .map(|speakers| sorted_speaker_ids(&speakers))
            .unwrap_or_else(|| sorted_speaker_ids(&std::mem::take(&mut attribution.pending)))
    }

    fn note_voice_in(&self, nonzero_samples: u64) {
        if nonzero_samples == 0 {
            return;
        }
        if let (Some(checks), Some(thread), Some(call)) = (
            &self.installation_checks,
            &self.native_thread_id,
            &self.call_instance_id,
        ) {
            checks.note_voice_in(thread, call, nonzero_samples);
        }
    }

    fn note_voice_out(&self, media_samples: u64) {
        if media_samples == 0 {
            return;
        }
        if let (Some(checks), Some(thread), Some(call)) = (
            &self.installation_checks,
            &self.native_thread_id,
            &self.call_instance_id,
        ) {
            checks.note_voice_out(thread, call, media_samples);
        }
    }

    fn note_voice_end(&self) {
        if let (Some(checks), Some(thread), Some(call)) = (
            &self.installation_checks,
            &self.native_thread_id,
            &self.call_instance_id,
        ) {
            checks.note_voice_end(thread, call);
        }
    }

    fn note_voice_error(&self, error: &str) {
        if let (Some(checks), Some(thread), Some(call)) = (
            &self.installation_checks,
            &self.native_thread_id,
            &self.call_instance_id,
        ) {
            checks.note_voice_error(thread, call, error);
        }
    }

    fn note_voice_connected(&self, ssrc: u32) {
        if let Ok(mut cached) = self.bot_ssrc.lock() {
            *cached = Some(ssrc);
        }
        if let (Some(checks), Some(thread), Some(call)) = (
            &self.installation_checks,
            &self.native_thread_id,
            &self.call_instance_id,
        ) {
            checks.note_voice_connected(thread, call, ssrc);
        }
    }

    fn note_receiver_report(&self, packet: &RtcpData) {
        let (Some(checks), Some(thread), Some(call)) = (
            &self.installation_checks,
            &self.native_thread_id,
            &self.call_instance_id,
        ) else {
            return;
        };
        let Some(ssrc) = self.bot_ssrc.lock().ok().and_then(|cached| *cached) else {
            return;
        };
        let Some(sequences) = discord_receiver_report_sequences(
            &packet.packet,
            packet.payload_offset,
            packet.payload_end_pad,
            ssrc,
        ) else {
            return;
        };
        for sequence in sequences {
            checks.note_voice_rr(thread, call, ssrc, sequence);
        }
    }

    fn snapshot(&self) -> VoiceAudioStats {
        VoiceAudioStats {
            input_frame_lengths: self
                .input_lengths
                .lock()
                .map(|m| m.clone())
                .unwrap_or_default(),
            input_frames: self.input_frames.load(Ordering::Relaxed),
            input_samples: self.input_samples.load(Ordering::Relaxed),
            input_non_silent_samples: self.input_non_silent_samples.load(Ordering::Relaxed),
            forwarded_frames: self.forwarded_frames.load(Ordering::Relaxed),
            forwarded_samples: self.forwarded_samples.load(Ordering::Relaxed),
            forwarded_non_silent_samples: self.forwarded_non_silent_samples.load(Ordering::Relaxed),
            opus_packets_sent: self.opus_packets_sent.load(Ordering::Relaxed),
            opus_bytes_sent: self.opus_bytes_sent.load(Ordering::Relaxed),
            native_opus_packets: self.native_opus_packets.load(Ordering::Relaxed),
            native_decoded_samples: self.native_decoded_samples.load(Ordering::Relaxed),
            native_non_silent_samples: self.native_non_silent_samples.load(Ordering::Relaxed),
            data_channel_open: self.data_channel_open.load(Ordering::Relaxed) != 0,
            data_channel_event_types: self
                .data_channel_event_types
                .lock()
                .map(|m| m.clone())
                .unwrap_or_default(),
            data_channel_events: self
                .data_channel_events
                .lock()
                .map(|events| events.clone())
                .unwrap_or_default(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VoiceSessionInfo {
    pub native_thread_id: String,
    pub voice_channel_id: u64,
    pub transcript_thread_id: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceJoinResult {
    pub status: &'static str,
    pub voice_call_active: bool,
    pub native_thread_id: String,
    pub voice_channel_id: u64,
    pub transcript_thread_id: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VoiceJoinAction {
    Start,
    AlreadyConnected,
}

fn plan_voice_join(
    active_channel: Option<ChannelId>,
    channel_id: ChannelId,
    language: Language,
) -> Result<VoiceJoinAction> {
    match active_channel {
        None => Ok(VoiceJoinAction::Start),
        Some(active) if active == channel_id => Ok(VoiceJoinAction::AlreadyConnected),
        Some(_) => bail!("{}", language.text(
            "A voice call is already connected in another channel. End it before joining a different channel.",
            "다른 채널의 음성 통화에 이미 연결되어 있습니다. 다른 채널에 들어가려면 먼저 현재 통화를 종료하세요.",
            "已连接另一个频道的语音通话。请先结束当前通话，再加入其他频道。",
            "別のチャンネルで音声通話に接続中です。別のチャンネルに入る前に現在の通話を終了してください。",
        )),
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceOperationResult {
    pub attempted: bool,
    pub ok: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl VoiceOperationResult {
    fn not_attempted() -> Self {
        Self {
            attempted: false,
            ok: None,
            error: None,
        }
    }

    fn succeeded() -> Self {
        Self {
            attempted: true,
            ok: Some(true),
            error: None,
        }
    }

    fn failed(error: impl std::fmt::Display) -> Self {
        Self {
            attempted: true,
            ok: Some(false),
            error: Some(error.to_string()),
        }
    }
}

fn transcript_task_result(
    result: std::result::Result<Result<()>, tokio::task::JoinError>,
) -> VoiceOperationResult {
    match result {
        Ok(Ok(())) => VoiceOperationResult::succeeded(),
        Ok(Err(error)) => VoiceOperationResult::failed(error),
        Err(error) => VoiceOperationResult::failed(error),
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceLeaveResult {
    /// `ended`, `already_ended`, or `partial`.
    pub status: &'static str,
    pub voice_call_active: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub native_thread_id: Option<String>,
    pub native_realtime_stop: VoiceOperationResult,
    pub peer_connection_close: VoiceOperationResult,
    pub discord_voice_leave: VoiceOperationResult,
    pub transcript_delivery: VoiceOperationResult,
    pub transcript_summary_update: VoiceOperationResult,
    pub transcript_footer: VoiceOperationResult,
}

impl VoiceLeaveResult {
    // The plain leave wrapper gates resource cleanup, while the structured
    // result keeps recording failures partial for tool callers and logging.
    fn cleanup_ok(&self) -> bool {
        !self.voice_call_active
            && [
                &self.native_realtime_stop,
                &self.peer_connection_close,
                &self.discord_voice_leave,
            ]
            .iter()
            .all(|result| {
                result.ok == Some(true)
                    || (self.status == "already_ended" && !result.attempted && result.ok.is_none())
            })
    }

    fn completion_status(
        native: &VoiceOperationResult,
        peer: &VoiceOperationResult,
        discord: &VoiceOperationResult,
        transcript: &VoiceOperationResult,
        footer: &VoiceOperationResult,
        summary_update: &VoiceOperationResult,
    ) -> &'static str {
        let required_operations_succeeded = [native, peer, discord, transcript, footer]
            .iter()
            .all(|result| result.ok == Some(true));
        let summary_update_succeeded = summary_update.ok == Some(true)
            || (!summary_update.attempted && summary_update.ok.is_none());
        if required_operations_succeeded && summary_update_succeeded {
            "ended"
        } else {
            "partial"
        }
    }
}

impl VoiceBridge {
    pub fn new(
        config: Arc<Config>,
        native: Native,
        http: Arc<Http>,
        songbird: Arc<Songbird>,
    ) -> Self {
        Self {
            config,
            native,
            http,
            songbird,
            active: Arc::new(AsyncMutex::new(HashMap::new())),
            lifecycle: Arc::new(AsyncMutex::new(())),
            input_gate: Arc::new(VoiceInputGate::new()),
            transcript_calls: Arc::new(Mutex::new(HashMap::new())),
            installation_checks: None,
        }
    }

    pub fn selected_voice(&self) -> Result<Option<String>> {
        let path = self.config.state_dir.join(VOICE_SETTINGS_FILE);
        selected_voice_from_file(&path, self.config.voice.as_deref())
    }

    pub fn set_voice(&self, voice: Option<&str>) -> Result<()> {
        let path = self.config.state_dir.join(VOICE_SETTINGS_FILE);
        persist_voice_selection(&path, voice)
    }

    pub fn with_installation_checks(mut self, checks: Arc<Checks>) -> Self {
        self.installation_checks = Some(checks);
        self
    }

    pub async fn join(
        &self,
        guild_id: GuildId,
        channel_id: ChannelId,
        requested_by: UserId,
    ) -> Result<()> {
        authorize_voice_join(&self.config, guild_id, channel_id, requested_by)?;
        self.join_with_result(guild_id, channel_id)
            .await
            .map(|_| ())
    }

    pub async fn join_from_agent(
        &self,
        guild_id: GuildId,
        channel_id: ChannelId,
    ) -> Result<VoiceJoinResult> {
        authorize_voice_channel(&self.config, guild_id, channel_id)?;
        self.join_with_result(guild_id, channel_id).await
    }

    async fn join_with_result(
        &self,
        guild_id: GuildId,
        channel_id: ChannelId,
    ) -> Result<VoiceJoinResult> {
        let _lifecycle = self.lifecycle.lock().await;
        let language = self.config.language;
        {
            let active = self.active.lock().await;
            let call = active.get(&guild_id.get());
            if plan_voice_join(call.map(|call| call.channel_id), channel_id, language)?
                == VoiceJoinAction::AlreadyConnected
            {
                let call = call.expect("already connected call exists");
                return Ok(VoiceJoinResult {
                    status: "already_connected",
                    voice_call_active: true,
                    native_thread_id: call.native_thread_id.clone(),
                    voice_channel_id: call.channel_id.get(),
                    transcript_thread_id: call.transcript_thread_id.get(),
                });
            }
        }
        if matches!(
            *self.input_gate.state.lock().await,
            VoiceInputPhase::Blocked
        ) {
            bail!("{}", language.text(
                "The previous voice connection has not been confirmed as ended. Retry with `/discodex voice-leave`.",
                "이전 음성 연결의 종료가 확인되지 않았습니다. `/discodex voice-leave`로 종료를 다시 요청하세요.",
                "尚未确认上一次语音连接已结束。请使用 `/discodex voice-leave` 重试。",
                "前回の音声接続が終了したことを確認できていません。`/discodex voice-leave` でもう一度終了してください。",
            ));
        }
        self.input_gate.set(VoiceInputPhase::Starting).await;
        let result = self.join_locked(guild_id, channel_id).await;
        let phase = match &result {
            Ok(()) => {
                let active = self.active.lock().await;
                let call = active
                    .get(&guild_id.get())
                    .expect("successful join installs its call");
                VoiceInputPhase::Active {
                    call_id: call.call_instance_id,
                    thread_id: call.native_thread_id.clone(),
                    transcript_channel_id: call.transcript_thread_id.get(),
                }
            }
            Err(_) => {
                // A failed start may already have created realtime. Only a definite
                // stop permits ordinary input to reach the persistent thread again.
                match self
                    .native
                    .request(
                        "thread/realtime/stop",
                        json!({"threadId": self.native.thread_id()}),
                    )
                    .await
                {
                    Ok(_) => VoiceInputPhase::Idle,
                    Err(error) => {
                        warn!(%error, "failed voice startup could not confirm native stop; input remains queued");
                        VoiceInputPhase::Blocked
                    }
                }
            }
        };
        self.input_gate.set(phase).await;
        result?;
        let active = self.active.lock().await;
        let call = active
            .get(&guild_id.get())
            .expect("successful join installs its call");
        Ok(VoiceJoinResult {
            status: "joined",
            voice_call_active: true,
            native_thread_id: call.native_thread_id.clone(),
            voice_channel_id: call.channel_id.get(),
            transcript_thread_id: call.transcript_thread_id.get(),
        })
    }

    async fn join_locked(&self, guild_id: GuildId, channel_id: ChannelId) -> Result<()> {
        let selected_voice = self.selected_voice()?;
        let language = self.config.language;
        let thread_id = self.native.thread_id();
        let call_instance_id = Uuid::new_v4();
        let call_context =
            realtime_call_start_context(call_instance_id, &thread_id, self.config.language);
        if let Some(checks) = &self.installation_checks {
            checks.note_voice_begin(&thread_id, &call_instance_id);
        }
        let audio_stats = Arc::new(AudioStats {
            installation_checks: self.installation_checks.clone(),
            native_thread_id: Some(thread_id.clone()),
            call_instance_id: Some(call_instance_id),
            ..Default::default()
        });
        let (transcript_thread_id, transcript_anchor_message_id) =
            match create_transcript_thread(&self.http, &self.config).await {
                Ok(thread) => thread,
                Err(error) => {
                    audio_stats.note_voice_error(&format!("{error:#}"));
                    audio_stats.note_voice_end();
                    return Err(error);
                }
            };
        let playback = AudioPipe::new()?;
        let (
            peer,
            local_audio_track,
            local_audio_ssrc,
            data_channel,
            native_events,
            native_close_events,
            data_channel_task,
            transcript_call,
        ) = match negotiate_realtime(
            &self.native,
            &thread_id,
            selected_voice.as_deref(),
            playback.clone(),
            audio_stats.clone(),
            &self.transcript_calls,
            call_instance_id,
            &call_context,
            transcript_thread_id,
            guild_id,
            self.config.language,
            self.config.name.clone(),
            self.config.clone(),
        )
        .await
        {
            Ok(session) => session,
            Err(error) => {
                audio_stats.note_voice_error(&format!("negotiate native realtime: {error:#}"));
                audio_stats.note_voice_end();
                playback.close();
                // Keep input paused while retained original recording delivery
                // finishes; a recording error must not replace the startup error.
                if let Err(record_error) =
                    drain_retained_startup_events(&self.transcript_calls, &self.http).await
                {
                    warn!(%record_error, "retained original-owner startup delivery remains incomplete");
                }
                return Err(error);
            }
        };

        let operator_audio = OperatorAudioBuffer::default();
        let receiver = DiscordAudioReceiver::new(
            self.config.operator_ids.clone(),
            operator_audio.clone(),
            audio_stats.clone(),
        );
        let call = self.songbird.get_or_insert(guild_id);
        {
            let mut handler = call.lock().await;
            handler.add_global_event(CoreEvent::SpeakingStateUpdate.into(), receiver.clone());
            handler.add_global_event(CoreEvent::RtpPacket.into(), receiver.clone());
            handler.add_global_event(CoreEvent::VoiceTick.into(), receiver.clone());
            handler.add_global_event(CoreEvent::DriverConnect.into(), receiver.clone());
            handler.add_global_event(CoreEvent::DriverReconnect.into(), receiver.clone());
            handler.add_global_event(CoreEvent::DriverDisconnect.into(), receiver.clone());
            handler.add_global_event(CoreEvent::RtcpPacket.into(), receiver.clone());
            handler.add_global_event(CoreEvent::ClientDisconnect.into(), receiver);
        }
        let (media_cancel, media_cancel_rx) = watch::channel(false);
        let (transcript_cancel, transcript_cancel_rx) = watch::channel(false);
        let (native_close_cancel, native_close_cancel_rx) = watch::channel(false);
        let (native_close_signal, native_close_signal_rx) = watch::channel(None::<String>);
        let media_task = tokio::spawn(forward_operator_audio(
            operator_audio,
            local_audio_track,
            local_audio_ssrc,
            media_cancel_rx,
            audio_stats.clone(),
        ));
        let discord_start = async {
            let output =
                start_discord_playback(&call, playback.clone(), audio_stats.clone()).await?;
            if let Err(error) = self.songbird.join(guild_id, channel_id).await {
                let _ = output.stop();
                bail!(
                    "{}: {error}",
                    language.text(
                        "Could not join the Discord voice channel",
                        "Discord 음성 채널 입장 실패",
                        "无法加入 Discord 语音频道",
                        "Discordの音声チャンネルに参加できませんでした",
                    )
                );
            }
            Ok::<_, anyhow::Error>(output)
        }
        .await;
        let output_track = match discord_start {
            Ok(output) => output,
            Err(error) => {
                audio_stats.note_voice_error(&format!("start Discord voice: {error:#}"));
                audio_stats.note_voice_end();
                let _ = media_cancel.send(true);
                media_task.abort();
                playback.close();
                let _ = tokio::time::timeout(
                    Duration::from_secs(5),
                    self.native
                        .request("thread/realtime/stop", json!({"threadId": thread_id})),
                )
                .await;
                let _ = tokio::time::timeout(Duration::from_secs(2), peer.close()).await;
                data_channel_task.abort();
                let _ = self.songbird.remove(guild_id).await;
                transcript_call.reader_state.store(2, Ordering::Release);
                let mut retained = StartupTranscriptEvents {
                    events: Some(native_events),
                    calls: &self.transcript_calls,
                    committed_call: Some(transcript_call),
                };
                let _ = retained.retain_pending();
                drop(retained);
                if let Err(record_error) =
                    drain_retained_startup_events(&self.transcript_calls, &self.http).await
                {
                    warn!(%record_error, "failed Discord startup recording delivery remains incomplete");
                }
                return Err(error);
            }
        };
        let transcript_start_order = transcript_call.start_order;
        let transcript_start_generation = transcript_call.start_generation;
        let transcript_task = tokio::spawn(handle_native_realtime_events(
            native_events,
            transcript_call,
            self.http.clone(),
            playback.clone(),
            transcript_cancel_rx,
            native_close_signal_rx,
        ));
        let native_close_task = tokio::spawn(monitor_native_realtime_close(
            native_close_events,
            native_close_cancel_rx,
            Arc::downgrade(&self.active),
            Arc::downgrade(&self.lifecycle),
            self.input_gate.clone(),
            self.songbird.clone(),
            self.http.clone(),
            guild_id,
            call_instance_id,
            thread_id.clone(),
            transcript_thread_id,
            self.config.language,
            native_close_signal,
            transcript_start_order,
            transcript_start_generation,
        ));
        let mut active = self.active.lock().await;
        active.insert(
            guild_id.get(),
            ActiveCall {
                call_instance_id,
                native_thread_id: thread_id.clone(),
                channel_id,
                transcript_thread_id,
                transcript_parent_channel_id: ChannelId::new(self.config.transcript_channel_id),
                transcript_anchor_message_id,
                peer,
                _events_channel: data_channel,
                _output_track: output_track,
                media_cancel,
                transcript_cancel,
                native_close_cancel,
                media_task,
                transcript_task,
                native_close_task,
                data_channel_task,
                playback,
                audio_stats,
            },
        );
        info!(guild = guild_id.get(), channel = channel_id.get(), thread = %thread_id, transcript_thread = transcript_thread_id.get(), "Codex realtime voice connected");
        Ok(())
    }

    /// Leaving Discord voice closes realtime transport but does not interrupt the
    /// backing Codex turn on the persistent native thread.
    pub async fn leave(&self, guild_id: GuildId) -> Result<()> {
        let result = self.leave_with_result(guild_id, None).await;
        if result.status == "partial" {
            warn!(result = ?serde_json::to_value(&result), "voice leave was partial");
        }
        if !result.cleanup_ok() {
            bail!("{}", self.config.language.text(
                "Some voice call cleanup steps failed. See the operation log for details.",
                "음성 통화 종료 처리 중 일부 작업이 실패했습니다. 운영 기록에서 상세 결과를 확인하세요.",
                "部分语音通话清理步骤失败。详情请查看操作日志。",
                "音声通話の終了処理の一部に失敗しました。詳細は操作ログを確認してください。",
            ));
        }
        Ok(())
    }

    /// Returns the operation-level result for an agent tool caller. A repeated
    /// request reports the observed inactive state and does not stop another
    /// realtime session on the persistent native thread.
    pub async fn leave_with_result(
        &self,
        guild_id: GuildId,
        summary: Option<String>,
    ) -> VoiceLeaveResult {
        let _lifecycle = self.lifecycle.lock().await;
        let was_blocked = matches!(
            *self.input_gate.state.lock().await,
            VoiceInputPhase::Blocked
        );
        self.input_gate.set(VoiceInputPhase::Ending).await;
        let mut result = self.leave_locked(guild_id, summary).await;
        if was_blocked && !result.native_realtime_stop.attempted {
            result.native_realtime_stop = match self
                .native
                .request(
                    "thread/realtime/stop",
                    json!({"threadId": self.native.thread_id()}),
                )
                .await
            {
                Ok(_) => VoiceOperationResult::succeeded(),
                Err(error) => VoiceOperationResult::failed(error),
            };
            if result.native_realtime_stop.ok != Some(true) {
                result.status = "partial";
            }
        }
        let stopped = result.native_realtime_stop.ok == Some(true)
            || (!was_blocked && !result.native_realtime_stop.attempted);
        self.input_gate
            .set(if stopped {
                VoiceInputPhase::Idle
            } else {
                VoiceInputPhase::Blocked
            })
            .await;
        result
    }

    async fn leave_locked(&self, guild_id: GuildId, summary: Option<String>) -> VoiceLeaveResult {
        let leave_requested_at = Instant::now();
        let Some(active) = self.active.lock().await.remove(&guild_id.get()) else {
            let discord_voice_leave = if self.songbird.get(guild_id).is_some() {
                match self.songbird.remove(guild_id).await {
                    Ok(_) => VoiceOperationResult::succeeded(),
                    Err(error) => VoiceOperationResult::failed(error),
                }
            } else {
                VoiceOperationResult::not_attempted()
            };
            let status = if discord_voice_leave.ok == Some(false) {
                "partial"
            } else {
                "already_ended"
            };
            info!(
                guild = guild_id.get(),
                status, "voice_leave handler completed without an active call"
            );
            return VoiceLeaveResult {
                status,
                voice_call_active: false,
                native_thread_id: None,
                native_realtime_stop: VoiceOperationResult::not_attempted(),
                peer_connection_close: VoiceOperationResult::not_attempted(),
                discord_voice_leave,
                transcript_delivery: VoiceOperationResult::not_attempted(),
                transcript_summary_update: VoiceOperationResult::not_attempted(),
                transcript_footer: VoiceOperationResult::not_attempted(),
            };
        };
        info!(guild = guild_id.get(), "voice_leave requested");
        let _ = active.native_close_cancel.send(true);
        let _ = active.media_cancel.send(true);
        let _ = active._output_track.stop();
        active.playback.close();
        let discord_voice_leave = match self.songbird.remove(guild_id).await {
            Ok(_) => VoiceOperationResult::succeeded(),
            Err(error) => VoiceOperationResult::failed(error),
        };
        info!(
            guild = guild_id.get(),
            elapsed_ms = leave_requested_at.elapsed().as_millis() as u64,
            ok = ?discord_voice_leave.ok,
            "Discord voice leave completed"
        );
        let mut native_close_task = active.native_close_task;
        if tokio::time::timeout(Duration::from_secs(1), &mut native_close_task)
            .await
            .is_err()
        {
            native_close_task.abort();
        }
        let _ = active.media_task.await;
        let stop_result = self
            .native
            .request(
                "thread/realtime/stop",
                json!({
                    "threadId": active.native_thread_id.clone()
                }),
            )
            .await;
        let native_realtime_stop = match stop_result {
            Ok(_) => VoiceOperationResult::succeeded(),
            Err(error) => VoiceOperationResult::failed(error),
        };
        // The native stop acknowledgement can precede its canonical final item and
        // closed notification. Let the event reader drain that ordered stream first.
        let mut transcript_task = active.transcript_task;
        let transcript_delivery =
            match tokio::time::timeout(Duration::from_secs(5), &mut transcript_task).await {
                Ok(result) => transcript_task_result(result),
                Err(_) => {
                    let _ = active.transcript_cancel.send(true);
                    match tokio::time::timeout(Duration::from_secs(1), &mut transcript_task).await {
                        Ok(result) => transcript_task_result(result),
                        Err(_) => {
                            transcript_task.abort();
                            VoiceOperationResult::failed("transcript reader did not finish")
                        }
                    }
                }
            };
        active.audio_stats.note_voice_end();
        let peer_connection_close = match active.peer.close().await {
            Ok(()) => VoiceOperationResult::succeeded(),
            Err(error) => VoiceOperationResult::failed(error),
        };
        let mut data_channel_task = active.data_channel_task;
        if tokio::time::timeout(Duration::from_secs(2), &mut data_channel_task)
            .await
            .is_err()
        {
            data_channel_task.abort();
            warn!(
                "WebRTC data-channel event reader did not stop after closing the peer connection"
            );
        }
        let transcript_summary_update = match (summary, transcript_delivery.ok) {
            (Some(_), Some(false) | None) => VoiceOperationResult::failed(
                "transcript delivery did not finish successfully; summary was not applied",
            ),
            (Some(summary), Some(true)) => match tokio::time::timeout(
                Duration::from_secs(5),
                active.transcript_parent_channel_id.edit_message(
                    &self.http,
                    active.transcript_anchor_message_id,
                    EditMessage::new()
                        .content(summary)
                        .allowed_mentions(transcript_empty_mentions()),
                ),
            )
            .await
            {
                Ok(Ok(_)) => VoiceOperationResult::succeeded(),
                Ok(Err(error)) => VoiceOperationResult::failed(error),
                Err(_) => VoiceOperationResult::failed("transcript summary edit timed out"),
            },
            (None, _) => VoiceOperationResult::not_attempted(),
        };
        let footer_result = ChannelId::new(active.transcript_thread_id.get())
            .say(
                &self.http,
                format!(
                    "{} <#{}>",
                    self.config.language.text(
                        "Voice call ended · voice channel",
                        "음성 통화 종료 · 음성 채널",
                        "语音通话已结束 · 语音频道",
                        "音声通話終了 · 音声チャンネル",
                    ),
                    active.channel_id.get(),
                ),
            )
            .await;
        let transcript_footer = match footer_result {
            Ok(_) => VoiceOperationResult::succeeded(),
            Err(error) => VoiceOperationResult::failed(error),
        };
        let status = VoiceLeaveResult::completion_status(
            &native_realtime_stop,
            &peer_connection_close,
            &discord_voice_leave,
            &transcript_delivery,
            &transcript_footer,
            &transcript_summary_update,
        );
        info!(guild = guild_id.get(), thread = %active.native_thread_id, status, "voice_leave handler completed");
        VoiceLeaveResult {
            status,
            voice_call_active: false,
            native_thread_id: Some(active.native_thread_id),
            native_realtime_stop,
            peer_connection_close,
            discord_voice_leave,
            transcript_delivery,
            transcript_summary_update,
            transcript_footer,
        }
    }

    pub async fn is_active(&self, guild_id: GuildId) -> bool {
        self.active.lock().await.contains_key(&guild_id.get())
    }

    pub(crate) fn native_transcript_destination_for_event(
        &self,
        method: &str,
        event: &Value,
    ) -> Result<Option<ChannelId>> {
        let params = event.get("params").unwrap_or(&Value::Null);
        let Some(thread_id) = params.get("threadId").and_then(Value::as_str) else {
            return Ok(None);
        };
        let observer = self
            .transcript_calls
            .lock()
            .map_err(|_| anyhow!("transcript call history lock poisoned"))?
            .values()
            .filter(|call| call.native_thread_id == thread_id)
            .max_by_key(|call| call.start_order)
            .cloned();
        let Some(observer) = observer else {
            return Ok(None);
        };
        Ok(observer
            .native_event_owner(method, event)?
            .map(|owner| owner.transcript_thread_id))
    }

    pub async fn session_info(&self, guild_id: GuildId) -> Option<VoiceSessionInfo> {
        self.active
            .lock()
            .await
            .get(&guild_id.get())
            .map(|active| VoiceSessionInfo {
                native_thread_id: active.native_thread_id.clone(),
                voice_channel_id: active.channel_id.get(),
                transcript_thread_id: active.transcript_thread_id.get(),
            })
    }

    pub async fn audio_stats(&self, guild_id: GuildId) -> Option<VoiceAudioStats> {
        self.active
            .lock()
            .await
            .get(&guild_id.get())
            .map(|active| active.audio_stats.snapshot())
    }
}

fn authorize_voice_join(
    config: &Config,
    guild_id: GuildId,
    channel_id: ChannelId,
    requested_by: UserId,
) -> Result<()> {
    authorize_voice_channel(config, guild_id, channel_id)?;
    let language = config.language;
    if !config.operator_ids.contains(&requested_by.get()) {
        bail!(
            "{}",
            language.text(
                "Only a configured operator can start a voice call.",
                "설정된 조작자만 음성 통화를 시작할 수 있습니다.",
                "只有已配置的操作员可以开始语音通话。",
                "設定済みの操作者のみ音声通話を開始できます。",
            )
        );
    }
    Ok(())
}

fn authorize_voice_channel(
    config: &Config,
    guild_id: GuildId,
    channel_id: ChannelId,
) -> Result<()> {
    let language = config.language;
    if guild_id.get() != config.guild_id {
        bail!(
            "{}",
            language.text(
                "Voice calls are allowed only in the configured Discord server.",
                "음성 입장은 설정된 Discord 서버에서만 가능합니다.",
                "只能在已配置的 Discord 服务器中进行语音通话。",
                "設定済みの Discord サーバーでのみ音声通話を開始できます。",
            )
        );
    }
    if !config.voice_channel_ids.contains(&channel_id.get()) {
        bail!(
            "{}",
            language.text(
                "The requested voice channel is not in the allowlist.",
                "요청한 음성 채널은 allowlist에 없습니다.",
                "请求的语音频道不在允许列表中。",
                "指定された音声チャンネルは許可リストにありません。",
            )
        );
    }
    Ok(())
}

#[derive(Default)]
struct ReceivedAudioState {
    /// Public RtpPacket events retain the DAVE-adjusted payload bounds. Songbird's
    /// VoiceTick packet view reconstructs those bounds from the transport cipher and
    /// therefore cannot be used for Opus input when DAVE leaves a trailer behind.
    packets: HashMap<(u32, u16), RtpData>,
    packet_order: VecDeque<(u32, u16)>,
    decoders: HashMap<u32, Decoder>,
    passthrough_ssrcs: HashSet<u32>,
}

impl ReceivedAudioState {
    fn cache_packet(&mut self, packet: RtpData) {
        let rtp = packet.rtp();
        let key = (rtp.get_ssrc(), rtp.get_sequence().into());
        if !self.packets.contains_key(&key) {
            self.packet_order.push_back(key);
        }
        self.packets.insert(key, packet);
        while self.packets.len() > RECEIVED_RTP_CACHE_CAPACITY {
            let Some(oldest) = self.packet_order.pop_front() else {
                break;
            };
            self.packets.remove(&oldest);
        }
    }

    fn take_packet(&mut self, key: (u32, u16)) -> Option<RtpData> {
        let packet = self.packets.remove(&key);
        if packet.is_some()
            && let Some(index) = self.packet_order.iter().position(|queued| *queued == key)
        {
            self.packet_order.remove(index);
        }
        packet
    }

    fn decoder(&mut self, ssrc: u32) -> &mut Decoder {
        if self.passthrough_ssrcs.remove(&ssrc) {
            self.decoders.remove(&ssrc);
        }
        self.decoders.entry(ssrc).or_insert_with(|| {
            Decoder::new(MEDIA_SAMPLE_RATE, Channels::Stereo)
                .expect("48 kHz stereo Opus decoder must be available")
        })
    }

    fn mark_passthrough(&mut self, ssrc: u32) {
        self.decoders.remove(&ssrc);
        self.passthrough_ssrcs.insert(ssrc);
    }

    fn remove_ssrc(&mut self, ssrc: u32) {
        self.decoders.remove(&ssrc);
        self.passthrough_ssrcs.remove(&ssrc);
        self.packets
            .retain(|(packet_ssrc, _), _| *packet_ssrc != ssrc);
        self.packet_order
            .retain(|(packet_ssrc, _)| *packet_ssrc != ssrc);
    }
}

#[derive(Clone)]
struct DiscordAudioReceiver {
    speakers: Arc<Mutex<SpeakerRegistry>>,
    received_audio: Arc<Mutex<ReceivedAudioState>>,
    operator_audio: OperatorAudioBuffer,
    audio_stats: Arc<AudioStats>,
}

impl DiscordAudioReceiver {
    fn new(
        operators: Vec<u64>,
        operator_audio: OperatorAudioBuffer,
        audio_stats: Arc<AudioStats>,
    ) -> Self {
        Self {
            speakers: Arc::new(Mutex::new(SpeakerRegistry::new(operators))),
            received_audio: Arc::new(Mutex::new(ReceivedAudioState::default())),
            operator_audio,
            audio_stats,
        }
    }
}

/// Songbird 0.6's successful RTCP decryption returns an absolute packet start
/// (8), despite RtcpData documenting a payload-relative offset. A zero start
/// is its failed-decryption/Pass fallback and cannot establish receiver progress.
fn discord_receiver_report_sequences(
    packet: &[u8],
    payload_offset: usize,
    payload_end_pad: usize,
    bot_ssrc: u32,
) -> Option<Vec<u32>> {
    if payload_offset != 8 || payload_end_pad == 0 {
        return None;
    }
    let end = packet.len().checked_sub(payload_end_pad)?;
    let plaintext = packet.get(..end)?;
    if plaintext.len() < payload_offset {
        return None;
    }
    let mut offset = 0_usize;
    let mut sequences = Vec::new();
    while offset < plaintext.len() {
        let header = plaintext.get(offset..offset.checked_add(4)?)?;
        if header[0] >> 6 != 2 {
            return None;
        }
        let length =
            (usize::from(u16::from_be_bytes([header[2], header[3]])) + 1).checked_mul(4)?;
        let packet_end = offset.checked_add(length)?;
        let member = plaintext.get(offset..packet_end)?;
        let unpadded = if header[0] & 0x20 != 0 {
            // RFC 3550 permits padding only on the final compound member.
            if packet_end != plaintext.len() {
                return None;
            }
            let padding = usize::from(*member.last()?);
            if padding == 0 || padding > length.checked_sub(4)? {
                return None;
            }
            member.get(..length.checked_sub(padding)?)?
        } else {
            member
        };
        if header[1] == 201 {
            let RtcpPacket::ReceiverReport(report) = RtcpPacket::new(member)? else {
                return None;
            };
            let body = unpadded.get(8..)?;
            let report_bytes = usize::from(report.get_rx_report_count()).checked_mul(24)?;
            // Validate every declared report before accepting any block.
            let reports = body.get(..report_bytes)?;
            for block in reports.chunks_exact(24) {
                let report = ReportBlockPacket::new(block)?;
                if report.get_ssrc() == bot_ssrc {
                    sequences.push(
                        (u32::from(report.get_cycles()) << 16) | u32::from(report.get_sequence()),
                    );
                }
            }
        }
        offset = packet_end;
    }
    Some(sequences)
}

fn trimmed_opus_payload(
    payload: &[u8],
    payload_offset: usize,
    payload_end_pad: usize,
) -> Result<&[u8]> {
    let payload_end = payload
        .len()
        .checked_sub(payload_end_pad)
        .ok_or_else(|| anyhow!("RTP payload end padding exceeds packet payload"))?;
    // RtpPacket events already place payload_offset after the RTP extension.
    // DAVE also adjusts payload_end_pad; do not parse the Opus bytes as a header.
    payload
        .get(payload_offset..payload_end)
        .ok_or_else(|| anyhow!("RTP payload bounds are outside the packet payload"))
}

fn forwardable_opus_payload(
    payload: &[u8],
    payload_offset: usize,
    payload_end_pad: usize,
) -> Result<(Vec<u8>, usize)> {
    let opus = trimmed_opus_payload(payload, payload_offset, payload_end_pad)?;
    let samples = opus2::packet::get_nb_samples(opus, MEDIA_SAMPLE_RATE)
        .context("read Discord Opus packet duration")?;
    if samples != OPUS_FRAME_SAMPLES {
        bail!("Discord Opus packet duration is not 20 ms");
    }
    Ok((opus.to_vec(), samples))
}

fn decode_opus_payload(
    decoder: &mut Decoder,
    payload: &[u8],
    payload_offset: usize,
    payload_end_pad: usize,
) -> Result<Vec<i16>> {
    let opus = trimmed_opus_payload(payload, payload_offset, payload_end_pad)?;
    let mut pcm = vec![0_i16; OPUS_FRAME_SAMPLES * 6 * 2];
    let samples_per_channel = decoder
        .decode(opus, &mut pcm, false)
        .context("decode Discord Opus payload")?;
    pcm.truncate(samples_per_channel * 2);
    Ok(pcm)
}

pub fn decode_discord_rtp(decoder: &mut Decoder, packet: &RtpData) -> Result<Vec<i16>> {
    let rtp = packet.rtp();
    let payload = rtp.payload();
    decode_opus_payload(
        decoder,
        payload,
        packet.payload_offset,
        packet.payload_end_pad,
    )
}

fn decode_opus_plc(decoder: &mut Decoder) -> Result<Vec<i16>> {
    let mut pcm = vec![0_i16; OPUS_FRAME_SAMPLES * 2];
    let samples_per_channel = decoder
        .decode(&[], &mut pcm, false)
        .context("decode Discord Opus packet loss concealment")?;
    pcm.truncate(samples_per_channel * 2);
    Ok(pcm)
}

#[async_trait]
impl VoiceEventHandler for DiscordAudioReceiver {
    async fn act(&self, context: &EventContext<'_>) -> Option<Event> {
        match context {
            EventContext::DriverConnect(data) | EventContext::DriverReconnect(data) => {
                self.audio_stats.note_voice_connected(data.ssrc);
            }
            EventContext::DriverDisconnect(data) => {
                if let Some(reason) = data.reason {
                    if !matches!(
                        reason,
                        DisconnectReason::Requested | DisconnectReason::AttemptDiscarded
                    ) {
                        self.audio_stats
                            .note_voice_error(&format!("Songbird driver disconnected: {reason:?}"));
                    }
                }
            }
            EventContext::RtcpPacket(packet) => self.audio_stats.note_receiver_report(packet),
            EventContext::SpeakingStateUpdate(Speaking {
                ssrc,
                user_id: Some(user_id),
                ..
            }) => {
                self.speakers
                    .lock()
                    .expect("speaker map poisoned")
                    .observe(*ssrc, user_id.0);
            }
            EventContext::SpeakingStateUpdate(Speaking {
                ssrc,
                user_id: None,
                ..
            }) => {
                warn!(
                    ssrc,
                    "Discord voice reported an SSRC without a user mapping"
                );
            }
            EventContext::ClientDisconnect(ClientDisconnect { user_id, .. }) => {
                let removed_ssrcs = self
                    .speakers
                    .lock()
                    .expect("speaker map poisoned")
                    .remove_user(user_id.0);
                let mut received_audio = self
                    .received_audio
                    .lock()
                    .expect("received audio state poisoned");
                for ssrc in removed_ssrcs {
                    received_audio.remove_ssrc(ssrc);
                }
            }
            EventContext::RtpPacket(packet) => {
                self.received_audio
                    .lock()
                    .expect("received audio state poisoned")
                    .cache_packet(packet.clone());
            }
            EventContext::VoiceTick(tick) => {
                let mut speakers = self.speakers.lock().expect("speaker map poisoned");
                let mut received_audio = self
                    .received_audio
                    .lock()
                    .expect("received audio state poisoned");
                let mut operators = Vec::new();
                for (ssrc, data) in &tick.speaking {
                    let packet = data.packet.as_ref().map(|packet| {
                        let rtp = packet.rtp();
                        let key = (rtp.get_ssrc(), rtp.get_sequence().into());
                        received_audio.take_packet(key)
                    });
                    let Some(user_id) = speakers.operator_for(*ssrc) else {
                        if speakers.unresolved(*ssrc) && speakers.take_unresolved_report_slot(*ssrc)
                        {
                            warn!(
                                ssrc,
                                "dropping Discord voice frame: SSRC has no user mapping"
                            );
                        }
                        continue;
                    };
                    operators.push((*ssrc, user_id, packet));
                }

                // A normal single-speaker 20 ms Discord Opus frame can be
                // copied byte-for-byte into the WebRTC Opus track. Multiple
                // independent streams still use the established PCM mixer.
                let mut forwarded_opus = false;
                if let [(ssrc, user_id, Some(Some(packet)))] = operators.as_slice() {
                    let rtp = packet.rtp();
                    let payload = rtp.payload();
                    let direct = forwardable_opus_payload(
                        payload,
                        packet.payload_offset,
                        packet.payload_end_pad,
                    );
                    match direct {
                        Ok((opus, samples_per_channel)) => {
                            let samples = (samples_per_channel * 2) as u64;
                            received_audio.mark_passthrough(*ssrc);
                            if self.operator_audio.push_opus(
                                opus,
                                samples_per_channel,
                                Instant::now(),
                            ) {
                                self.audio_stats
                                    .input_frames
                                    .fetch_add(1, Ordering::Relaxed);
                                self.audio_stats
                                    .input_samples
                                    .fetch_add(samples, Ordering::Relaxed);
                                if let Ok(mut lengths) = self.audio_stats.input_lengths.lock() {
                                    *lengths.entry(samples as usize).or_default() += 1;
                                }
                                self.audio_stats
                                    .forwarded_frames
                                    .fetch_add(1, Ordering::Relaxed);
                                self.audio_stats
                                    .forwarded_samples
                                    .fetch_add(samples, Ordering::Relaxed);
                                self.audio_stats.note_transcript_speakers([*user_id]);
                                forwarded_opus = true;
                            }
                        }
                        Err(error) => {
                            warn!(%error, ssrc, user_id, "Discord Opus packet is not eligible for direct forwarding; using PCM path");
                        }
                    }
                }

                if !forwarded_opus {
                    let mut mixed: Vec<i32> = Vec::new();
                    let mut contributors = 0_i32;
                    let mut contributing_users = Vec::new();
                    for (ssrc, user_id, packet) in operators {
                        let pcm = match packet {
                            Some(Some(packet)) => {
                                match decode_discord_rtp(received_audio.decoder(ssrc), &packet) {
                                    Ok(pcm) => pcm,
                                    Err(error) => {
                                        warn!(%error, ssrc, user_id, "dropping Discord voice frame: Opus decode failed");
                                        continue;
                                    }
                                }
                            }
                            Some(None) => {
                                warn!(
                                    ssrc,
                                    user_id,
                                    "dropping Discord voice frame: corrected RTP bounds were not cached"
                                );
                                continue;
                            }
                            None => match decode_opus_plc(received_audio.decoder(ssrc)) {
                                Ok(pcm) => pcm,
                                Err(error) => {
                                    warn!(%error, ssrc, user_id, "dropping Discord voice loss frame: Opus PLC failed");
                                    continue;
                                }
                            },
                        };
                        if !pcm.is_empty() {
                            self.audio_stats
                                .input_frames
                                .fetch_add(1, Ordering::Relaxed);
                            self.audio_stats
                                .input_samples
                                .fetch_add(pcm.len() as u64, Ordering::Relaxed);
                            self.audio_stats.input_non_silent_samples.fetch_add(
                                pcm.iter().filter(|sample| **sample != 0).count() as u64,
                                Ordering::Relaxed,
                            );
                            if let Ok(mut lengths) = self.audio_stats.input_lengths.lock() {
                                *lengths.entry(pcm.len()).or_default() += 1;
                            }
                            if mixed.len() < pcm.len() {
                                mixed.resize(pcm.len(), 0);
                            }
                            for (mix, sample) in mixed.iter_mut().zip(pcm.iter().copied()) {
                                *mix += i32::from(sample);
                            }
                            contributors += 1;
                            contributing_users.push(user_id);
                        }
                    }
                    if contributors > 0 {
                        let pcm: Vec<i16> = mixed
                            .into_iter()
                            .map(|sample| {
                                (sample / contributors)
                                    .clamp(i32::from(i16::MIN), i32::from(i16::MAX))
                                    as i16
                            })
                            .collect();
                        let samples = pcm.len() as u64;
                        let non_silent = pcm.iter().filter(|sample| **sample != 0).count() as u64;
                        if self.operator_audio.push(pcm, Instant::now()) {
                            self.audio_stats
                                .note_transcript_speakers(contributing_users);
                            self.audio_stats
                                .forwarded_frames
                                .fetch_add(1, Ordering::Relaxed);
                            self.audio_stats
                                .forwarded_samples
                                .fetch_add(samples, Ordering::Relaxed);
                            self.audio_stats
                                .forwarded_non_silent_samples
                                .fetch_add(non_silent, Ordering::Relaxed);
                        }
                    }
                }
            }
            _ => {}
        }
        None
    }
}

#[derive(Clone)]
struct VoicePeerEvents {
    gathering_complete: watch::Sender<bool>,
    connection_state: watch::Sender<RTCPeerConnectionState>,
    playback: AudioPipe,
    audio_stats: Arc<AudioStats>,
}

#[async_trait]
impl PeerConnectionEventHandler for VoicePeerEvents {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            let _ = self.gathering_complete.send(true);
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        self.connection_state.send_replace(state);
        if matches!(
            state,
            RTCPeerConnectionState::Failed
                | RTCPeerConnectionState::Disconnected
                | RTCPeerConnectionState::Closed
        ) {
            warn!(?state, "Codex WebRTC peer connection state changed");
            if state != RTCPeerConnectionState::Closed {
                self.audio_stats
                    .note_voice_error(&format!("Codex WebRTC peer state: {state:?}"));
            }
        }
    }

    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let playback = self.playback.clone();
        let audio_stats = self.audio_stats.clone();
        tokio::spawn(async move {
            if let Err(error) = receive_native_audio(track, playback, audio_stats.clone()).await {
                audio_stats.note_voice_error(&format!("native realtime audio track: {error:#}"));
                warn!(%error, "native realtime audio track ended");
            }
        });
    }
}

async fn create_peer(
    playback: AudioPipe,
    audio_stats: Arc<AudioStats>,
) -> Result<(
    Arc<dyn PeerConnection>,
    Arc<TrackLocalStaticSample>,
    u32,
    Arc<dyn DataChannel>,
    watch::Receiver<bool>,
    watch::Receiver<RTCPeerConnectionState>,
    JoinHandle<()>,
)> {
    let mut media_engine = MediaEngine::default();
    media_engine
        .register_default_codecs()
        .context("register WebRTC codecs")?;
    let registry =
        register_default_interceptors(rtc::interceptor::Registry::new(), &mut media_engine)
            .context("register WebRTC default interceptors")?;
    let (gathering_complete_tx, gathering_complete_rx) = watch::channel(false);
    let (connection_state_tx, connection_state_rx) = watch::channel(RTCPeerConnectionState::New);
    let peer: Arc<dyn PeerConnection> = Arc::new(
        PeerConnectionBuilder::new()
            .with_configuration(RTCConfiguration::default())
            .with_media_engine(media_engine)
            .with_interceptor_registry(registry)
            .with_handler(Arc::new(VoicePeerEvents {
                gathering_complete: gathering_complete_tx,
                connection_state: connection_state_tx,
                playback: playback.clone(),
                audio_stats: audio_stats.clone(),
            }))
            .with_udp_addrs(vec!["0.0.0.0:0".to_owned()])
            .build()
            .await
            .context("build WebRTC peer connection")?,
    );

    let ssrc = u32::from_be_bytes(
        Uuid::new_v4().as_bytes()[..4]
            .try_into()
            .expect("UUID has at least four bytes"),
    );
    let audio_track = Arc::new(
        TrackLocalStaticSample::new(MediaStreamTrack::new(
            "discodex".to_owned(),
            "discord-operator-audio".to_owned(),
            "Discord operator audio".to_owned(),
            RtpCodecKind::Audio,
            vec![RTCRtpEncodingParameters {
                rtp_coding_parameters: RTCRtpCodingParameters {
                    ssrc: Some(ssrc),
                    ..Default::default()
                },
                codec: RTCRtpCodec {
                    mime_type: MIME_TYPE_OPUS.to_owned(),
                    clock_rate: MEDIA_SAMPLE_RATE,
                    channels: 2,
                    sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
                    rtcp_feedback: vec![],
                },
                ..Default::default()
            }],
        ))
        .context("create Discord operator Opus track")?,
    );
    let events = peer
        .create_data_channel("codex-realtime-events", None)
        .await
        .context("create Codex realtime events data channel")?;
    peer.add_track(audio_track.clone())
        .await
        .context("add Discord audio RTP track")?;

    let data_channel_task = tokio::spawn(read_data_channel_events(
        events.clone(),
        playback,
        audio_stats,
    ));

    Ok((
        peer,
        audio_track,
        ssrc,
        events,
        gathering_complete_rx,
        connection_state_rx,
        data_channel_task,
    ))
}

async fn read_data_channel_events(
    channel: Arc<dyn DataChannel>,
    playback: AudioPipe,
    audio_stats: Arc<AudioStats>,
) {
    loop {
        match channel.poll().await {
            Some(DataChannelEvent::OnOpen) => {
                audio_stats.data_channel_open.store(1, Ordering::Relaxed);
            }
            Some(DataChannelEvent::OnMessage(message)) => {
                observe_data_channel_message(&message.data, &playback, &audio_stats);
            }
            Some(DataChannelEvent::OnClose) | None => return,
            Some(DataChannelEvent::OnError) => {
                audio_stats.note_voice_error("WebRTC realtime data channel error");
                debug!("WebRTC realtime data channel failed");
            }
            Some(DataChannelEvent::OnClosing) => {
                debug!("WebRTC realtime data channel is closing");
            }
            Some(_) => {}
        }
    }
}

fn observe_data_channel_message(data: &[u8], playback: &AudioPipe, audio_stats: &AudioStats) {
    let decoded_event = serde_json::from_slice::<Value>(data).ok();
    let event_type = decoded_event
        .as_ref()
        .and_then(|event| event.get("type").and_then(Value::as_str).map(str::to_owned))
        .unwrap_or_else(|| "<untyped>".to_owned());
    if let Ok(mut event_types) = audio_stats.data_channel_event_types.lock() {
        *event_types.entry(event_type.clone()).or_default() += 1;
    }
    if let Some(event) = decoded_event {
        let mut observed = json!({"type": event_type});
        for key in [
            "event_id",
            "item_id",
            "itemId",
            "role",
            "transcript",
            "input_transcript",
            "inputTranscript",
            "handoff_id",
            "handoffId",
            "call_id",
        ] {
            if let Some(value) = event.get(key) {
                observed[key] = value.clone();
            }
        }
        if matches!(
            event_type.as_str(),
            "session.closed" | "error" | "session.error"
        ) {
            if let Some(object) = event.as_object() {
                let mut keys: Vec<_> = object.keys().cloned().collect();
                keys.sort();
                observed["eventKeys"] = json!(keys);
            }
            for key in ["reason", "code", "message", "status", "usage"] {
                if let Some(value) = event.get(key) {
                    observed[key] = value.clone();
                }
            }
            if let Some(error) = event.get("error") {
                observed["error"] = match error {
                    Value::Object(error) => {
                        let mut details = serde_json::Map::new();
                        for key in ["type", "code", "message", "param", "reason", "status"] {
                            if let Some(value) = error.get(key) {
                                details.insert(key.to_owned(), value.clone());
                            }
                        }
                        Value::Object(details)
                    }
                    value => value.clone(),
                };
            }
            if let Some(session) = event.get("session").and_then(Value::as_object) {
                let mut details = serde_json::Map::new();
                for key in ["id", "status", "close_reason", "closed_reason", "reason"] {
                    if let Some(value) = session.get(key) {
                        details.insert(key.to_owned(), value.clone());
                    }
                }
                if let Some(error) = session.get("error") {
                    details.insert(
                        "error".to_owned(),
                        match error {
                            Value::Object(error) => {
                                let mut safe_error = serde_json::Map::new();
                                for key in ["type", "code", "message", "param", "reason"] {
                                    if let Some(value) = error.get(key) {
                                        safe_error.insert(key.to_owned(), value.clone());
                                    }
                                }
                                Value::Object(safe_error)
                            }
                            value => value.clone(),
                        },
                    );
                }
                observed["session"] = Value::Object(details);
            }
        }
        if let Some(item) = event.get("item") {
            let mut item_observation = json!({});
            for key in [
                "id",
                "type",
                "role",
                "transcript",
                "text",
                "call_id",
                "name",
            ] {
                if let Some(value) = item.get(key) {
                    item_observation[key] = value.clone();
                }
            }
            observed["item"] = item_observation;
        }
        if let Ok(mut events) = audio_stats.data_channel_events.lock() {
            if events.len() < 256 {
                events.push(observed);
            }
        }
    }
    debug!(data_channel_event_type = %event_type, "received WebRTC realtime data-channel event");
    if is_native_audio_interrupt(data) {
        playback.clear();
    }
}

// The recording receiver is independent of the SDP consumer. If startup
// fails or is cancelled, keep only exact known ended-owner native items.
struct StartupTranscriptEvents<'a> {
    events: Option<broadcast::Receiver<Value>>,
    calls: &'a Mutex<HashMap<Uuid, Arc<TranscriptCall>>>,
    committed_call: Option<Arc<TranscriptCall>>,
}

impl StartupTranscriptEvents<'_> {
    fn into_receiver(mut self) -> broadcast::Receiver<Value> {
        self.committed_call.take();
        self.events
            .take()
            .expect("startup recording receiver is present")
    }

    fn retain_pending(&mut self) -> Result<()> {
        let retained: Vec<_> = self
            .calls
            .lock()
            .map_err(|_| anyhow!("transcript call history lock poisoned"))?
            .values()
            .cloned()
            .collect();
        while let Some(events) = self.events.as_mut() {
            let event = match events.try_recv() {
                Ok(event) => event,
                Err(broadcast::error::TryRecvError::Lagged(count)) => {
                    warn!(
                        count,
                        "startup recording backlog overflowed; lost items cannot be reconstructed"
                    );
                    continue;
                }
                Err(
                    broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed,
                ) => break,
            };
            let method = event
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default();
            if !matches!(
                method,
                "thread/realtime/started"
                    | "thread/realtime/item/started"
                    | "thread/realtime/item/transcript/delta"
                    | "item/agentMessage/delta"
                    | "thread/realtime/item/completed"
                    | "item/completed"
            ) {
                continue;
            }
            let thread_id = event.pointer("/params/threadId").and_then(Value::as_str);
            let observer = self.committed_call.as_ref().or_else(|| {
                retained
                    .iter()
                    .filter(|call| Some(call.native_thread_id.as_str()) == thread_id)
                    .max_by_key(|call| call.start_order)
            });
            if let Some(observer) = observer {
                if let Some(owner) = observer.native_event_owner(method, &event)? {
                    observer.enqueue_native_event(owner, event)?;
                }
            }
        }
        Ok(())
    }
}

impl Drop for StartupTranscriptEvents<'_> {
    fn drop(&mut self) {
        if let Some(call) = self.committed_call.as_ref() {
            // The request boundary stays committed. A failed/cancelled startup
            // has no recording receiver to register later, so mark it ended.
            let _ = call
                .reader_state
                .compare_exchange(0, 2, Ordering::AcqRel, Ordering::Acquire);
        }
        if self.events.is_some() {
            if let Err(error) = self.retain_pending() {
                warn!(%error, "could not retain original-owner startup recording events");
            }
        }
    }
}

async fn drain_pending_native_events(call: &Arc<TranscriptCall>, http: &Arc<Http>) -> Result<()> {
    loop {
        if call
            .pending_native_events
            .lock()
            .map_err(|_| anyhow!("startup transcript backlog lock poisoned"))?
            .is_empty()
        {
            return Ok(());
        }
        // Claim no queue item until the original ledger is acquired. Keep its
        // front through the handler await, so cancellation retains the event.
        let mut ledger = call.ledger.lock().await;
        let event = call
            .pending_native_events
            .lock()
            .map_err(|_| anyhow!("startup transcript backlog lock poisoned"))?
            .front()
            .cloned();
        let Some(event) = event else {
            return Ok(());
        };
        let method = event
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let params = event.get("params").unwrap_or(&Value::Null);
        if call.accepts_event(params)? {
            handle_native_transcript_item(http, call, &mut ledger, method, params).await?;
        }
        let mut pending = call
            .pending_native_events
            .lock()
            .map_err(|_| anyhow!("startup transcript backlog lock poisoned"))?;
        // A session correction may have moved the old front during this await.
        if pending.front() == Some(&event) {
            pending.pop_front();
        }
    }
}

async fn drain_retained_startup_events(
    calls: &Mutex<HashMap<Uuid, Arc<TranscriptCall>>>,
    http: &Arc<Http>,
) -> Result<()> {
    let retained: Vec<_> = calls
        .lock()
        .map_err(|_| anyhow!("transcript call history lock poisoned"))?
        .values()
        .filter(|call| call.reader_state.load(Ordering::Acquire) == 2)
        .cloned()
        .collect();
    for owner in retained {
        drain_pending_native_events(&owner, http).await?;
    }
    Ok(())
}

struct RealtimeCallStartContext {
    start_instructions: String,
    initial_developer_item: String,
}

fn realtime_call_start_context(
    call_instance_id: Uuid,
    native_thread_id: &str,
    language: Language,
) -> RealtimeCallStartContext {
    let call_id = call_instance_id.to_string();
    let call_state = language
        .text(
            "This is a new Discord voice call. callInstanceId={call_id}. The call is starting and has not ended.",
            "새 Discord 음성 통화입니다. callInstanceId={call_id}. 현재 상태는 연결 시작 중이며 종료되지 않았습니다.",
            "这是一次新的 Discord 语音通话。callInstanceId={call_id}。当前正在连接，通话尚未结束。",
            "これは新しい Discord 音声通話です。callInstanceId={call_id}。接続を開始したところで、通話はまだ終了していません。",
        )
        .replace("{call_id}", &call_id);
    let continuity = language
        .text(
            "A successful voice_leave from an earlier call is historical. Keep the existing native Codex conversation thread {native_thread_id}; do not create a new conversation thread.",
            "이전 voice_leave 성공은 이전 통화의 과거 기록입니다. Codex의 기존 native 대화 thread {native_thread_id}를 유지하고 새 대화 thread를 만들지 마세요.",
            "之前通话中成功的 voice_leave 仅属于历史记录。请保留现有的 native Codex 对话线程 {native_thread_id}，不要创建新对话线程。",
            "以前の通話で成功した voice_leave は過去の記録です。既存の native Codex 会話スレッド {native_thread_id} を維持し、新しい会話スレッドを作成しないでください。",
        )
        .replace("{native_thread_id}", native_thread_id);
    RealtimeCallStartContext {
        start_instructions: format!("{call_state} {continuity}"),
        initial_developer_item: format!("{call_state} {continuity}"),
    }
}
fn realtime_start_params(
    thread_id: &str,
    call_instance_id: Uuid,
    voice: Option<&str>,
    offer_sdp: &str,
    call_context: &RealtimeCallStartContext,
) -> Value {
    let mut params = json!({
        "threadId": thread_id,
        "realtimeSessionId": call_instance_id.to_string(),
        "version": "v3",
        "outputModality": "audio",
        "transport": {"type": "webrtc", "sdp": offer_sdp},
        "flushTranscriptTailOnSessionEnd": true,
        "realtimeStartInstructions": call_context.start_instructions,
        "initialItems": [{
            "role": "developer",
            "text": call_context.initial_developer_item
        }]
    });
    if let Some(voice) = voice {
        params["voice"] = Value::String(voice.to_owned());
    }
    params
}

async fn negotiate_realtime(
    native: &Native,
    thread_id: &str,
    voice: Option<&str>,
    playback: AudioPipe,
    audio_stats: Arc<AudioStats>,
    transcript_calls: &Mutex<HashMap<Uuid, Arc<TranscriptCall>>>,
    call_instance_id: Uuid,
    call_context: &RealtimeCallStartContext,
    transcript_thread_id: ChannelId,
    guild_id: GuildId,
    language: Language,
    agent_name: String,
    reply_file_config: Arc<Config>,
) -> Result<(
    Arc<dyn PeerConnection>,
    Arc<TrackLocalStaticSample>,
    u32,
    Arc<dyn DataChannel>,
    broadcast::Receiver<Value>,
    broadcast::Receiver<Value>,
    JoinHandle<()>,
    Arc<TranscriptCall>,
)> {
    let mut events = native.subscribe();
    let mut recording_events = StartupTranscriptEvents {
        events: Some(events.resubscribe()),
        calls: transcript_calls,
        committed_call: None,
    };
    let native_close_events = events.resubscribe();
    let (
        peer,
        track,
        ssrc,
        data_channel,
        mut gathering_complete,
        mut connection_state,
        data_channel_task,
    ) = create_peer(playback, audio_stats.clone()).await?;
    let offer = match create_complete_offer(&peer, &mut gathering_complete).await {
        Ok(offer) => offer,
        Err(error) => {
            let _ = peer.close().await;
            let _ = data_channel_task.await;
            return Err(error);
        }
    };
    let params = realtime_start_params(thread_id, call_instance_id, voice, &offer, call_context);
    // The hook runs only after connectivity, the writer lock and sink readiness.
    // Its synchronous begin is immediately followed by native start_send.
    let transcript_call = match native
        .request_with_send_commit("thread/realtime/start", params, |fence| {
            let call = TranscriptCall::begin_with_checks(
                transcript_calls,
                call_instance_id,
                thread_id.to_owned(),
                transcript_thread_id,
                fence,
                Some(native.current_generation()),
                audio_stats.installation_checks.clone(),
                Some(reply_file_config.clone()),
                guild_id,
                agent_name.clone(),
                language,
                audio_stats.clone(),
            )?;
            recording_events.committed_call = Some(call.clone());
            Ok(call)
        })
        .await
    {
        Ok((_, call)) => call,
        Err(error) => {
            let _ = peer.close().await;
            let _ = data_channel_task.await;
            return Err(error).context("start Codex realtime in the profile's persistent thread");
        }
    };
    let answer_sdp = match await_realtime_sdp_for_connection(
        &mut events,
        thread_id,
        transcript_call.start_order,
        transcript_call.start_generation,
    )
    .await
    {
        Ok(answer) => answer,
        Err(error) => {
            match tokio::time::timeout(
                Duration::from_secs(5),
                native.request("thread/realtime/stop", json!({"threadId": thread_id})),
            )
            .await
            {
                Ok(Ok(_)) => {}
                Ok(Err(stop_error)) => {
                    warn!(%stop_error, "could not stop native realtime after startup failed");
                }
                Err(_) => warn!("timed out stopping native realtime after startup failed"),
            }
            match tokio::time::timeout(Duration::from_secs(2), peer.close()).await {
                Ok(Ok(())) => {}
                Ok(Err(close_error)) => {
                    warn!(%close_error, "could not close WebRTC peer after native realtime startup failed");
                }
                Err(_) => {
                    warn!("timed out closing WebRTC peer after native realtime startup failed")
                }
            }
            data_channel_task.abort();
            return Err(error);
        }
    };
    let answer = match RTCSessionDescription::answer(answer_sdp) {
        Ok(answer) => answer,
        Err(error) => {
            let _ = native
                .request("thread/realtime/stop", json!({"threadId": thread_id}))
                .await;
            let _ = peer.close().await;
            let _ = data_channel_task.await;
            return Err(error).context("parse native realtime SDP answer");
        }
    };
    if let Err(error) = peer.set_remote_description(answer).await {
        let _ = native
            .request("thread/realtime/stop", json!({"threadId": thread_id}))
            .await;
        let _ = peer.close().await;
        let _ = data_channel_task.await;
        return Err(error).context("apply Codex WebRTC SDP answer");
    }
    if let Err(error) = await_peer_connected(&mut connection_state, language).await {
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            native.request("thread/realtime/stop", json!({"threadId": thread_id})),
        )
        .await;
        let _ = tokio::time::timeout(Duration::from_secs(2), peer.close()).await;
        data_channel_task.abort();
        return Err(error);
    }
    Ok((
        peer,
        track,
        ssrc,
        data_channel,
        recording_events.into_receiver(),
        native_close_events,
        data_channel_task,
        transcript_call,
    ))
}

async fn await_peer_connected(
    states: &mut watch::Receiver<RTCPeerConnectionState>,
    language: Language,
) -> Result<()> {
    tokio::time::timeout(REALTIME_SDP_TIMEOUT, async {
        loop {
            match *states.borrow_and_update() {
                RTCPeerConnectionState::Connected => return Ok(()),
                RTCPeerConnectionState::Failed | RTCPeerConnectionState::Closed => {
                    bail!(
                        "{}",
                        language.text(
                            "Codex voice connection setup failed.",
                            "Codex 음성 연결 준비에 실패했습니다.",
                            "Codex 语音连接准备失败。",
                            "Codex 音声接続の準備に失敗しました。",
                        )
                    )
                }
                _ => {}
            }
            states
                .changed()
                .await
                .context("WebRTC connection state stream closed")?;
        }
    })
    .await
    .context(language.text(
        "Timed out while preparing the Codex voice connection.",
        "Codex 음성 연결 준비 시간이 초과됐습니다.",
        "准备 Codex 语音连接时超时。",
        "Codex 音声接続の準備中にタイムアウトしました。",
    ))?
}

async fn create_complete_offer(
    peer: &Arc<dyn PeerConnection>,
    gathering_complete: &mut watch::Receiver<bool>,
) -> Result<String> {
    let offer = peer
        .create_offer(None)
        .await
        .context("create WebRTC SDP offer")?;
    peer.set_local_description(offer)
        .await
        .context("set WebRTC local description")?;
    tokio::time::timeout(REALTIME_SDP_TIMEOUT, async {
        while !*gathering_complete.borrow() {
            gathering_complete
                .changed()
                .await
                .context("WebRTC ICE gathering state channel closed")?;
        }
        Ok::<(), anyhow::Error>(())
    })
    .await
    .context("timed out waiting for complete WebRTC ICE gathering")??;
    peer.local_description()
        .await
        .map(|sdp| sdp.sdp)
        .ok_or_else(|| anyhow!("WebRTC completed ICE gathering without a local SDP"))
}

#[cfg(test)]
async fn await_realtime_sdp(
    events: &mut broadcast::Receiver<Value>,
    thread_id: &str,
    start_order: u64,
) -> Result<String> {
    await_realtime_sdp_for_connection(events, thread_id, start_order, None).await
}

async fn await_realtime_sdp_for_connection(
    events: &mut broadcast::Receiver<Value>,
    thread_id: &str,
    start_order: u64,
    start_generation: Option<u64>,
) -> Result<String> {
    let started_at = Instant::now();
    let mut observed_events = 0_u64;
    let mut matching_events = 0_u64;
    let mut realtime_events = 0_u64;
    let mut generic_error_events = 0_u64;
    let mut started_seen = false;
    let mut last_realtime_method = None::<String>;
    let mut last_realtime_elapsed_ms = None::<u128>;
    let wait = async {
        loop {
            let event = events
                .recv()
                .await
                .context("native event stream ended while waiting for realtime SDP")?;
            observed_events += 1;
            let method = event
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let params = event.get("params").unwrap_or(&Value::Null);
            if params.get("threadId").and_then(Value::as_str) != Some(thread_id) {
                continue;
            }
            if outside_call_connection(params, start_order, start_generation) {
                continue;
            }
            matching_events += 1;
            if method.starts_with("thread/realtime/") {
                realtime_events += 1;
                last_realtime_method = Some(method.to_owned());
                last_realtime_elapsed_ms = Some(started_at.elapsed().as_millis());
                started_seen |= method == "thread/realtime/started";
            }
            match method {
                "thread/realtime/sdp" => {
                    return params
                        .get("sdp")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                        .ok_or_else(|| anyhow!("native realtime SDP event had no SDP"));
                }
                "thread/realtime/error" => {
                    let message = params
                        .get("message")
                        .and_then(Value::as_str)
                        .or_else(|| params.pointer("/error/message").and_then(Value::as_str))
                        .or_else(|| params.get("error").and_then(Value::as_str))
                        .unwrap_or("unknown native realtime error");
                    bail!("native realtime startup failed: {message}");
                }
                // Generic turn errors can share this thread while realtime
                // startup is in flight. Record only their type/count and let
                // the dedicated realtime lifecycle decide startup outcome.
                "error" => {
                    generic_error_events += 1;
                }
                "thread/realtime/closed" => {
                    let reason = params
                        .get("reason")
                        .and_then(Value::as_str)
                        .unwrap_or("native realtime transport closed before SDP answer");
                    bail!("native realtime closed before SDP answer: {reason}");
                }
                _ => {}
            }
        }
    };
    match tokio::time::timeout(REALTIME_SDP_TIMEOUT, wait).await {
        Ok(result) => result,
        Err(_) => {
            tracing::warn!(
                observed_events,
                matching_events,
                realtime_events,
                generic_error_events,
                started_seen,
                sdp_observed = false,
                last_realtime_method = last_realtime_method.as_deref().unwrap_or("none"),
                last_realtime_elapsed_ms = last_realtime_elapsed_ms.unwrap_or_default(),
                "timed out after 30 seconds waiting for native realtime SDP"
            );
            Err(anyhow!(
                "timed out after 30 seconds waiting for native realtime SDP"
            ))
        }
    }
}

async fn forward_operator_audio(
    input: OperatorAudioBuffer,
    track: Arc<TrackLocalStaticSample>,
    ssrc: u32,
    mut cancelled: watch::Receiver<bool>,
    audio_stats: Arc<AudioStats>,
) {
    let mut encoder = None;
    let mut encoded = vec![0u8; OPUS_OUTPUT_CAPACITY];
    let mut ticker = tokio::time::interval(OPUS_FRAME);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = cancelled.changed() => return,
            _ = ticker.tick() => {
                let output = input.take_output(Instant::now());
                let (data, duration, input_observation_samples) = match output {
                    Some(OperatorAudioOutput::Opus { data, duration, samples }) => {
                        encoder = None;
                        (data, duration, samples)
                    }
                    None => (vec![0xf8, 0xff, 0xfe], OPUS_FRAME, 0),
                    Some(OperatorAudioOutput::Pcm(frame)) => {
                        if encoder.is_none() {
                            encoder = match Encoder::new(MEDIA_SAMPLE_RATE, Channels::Stereo, Application::Voip) {
                                Ok(encoder) => Some(encoder),
                                Err(error) => {
                                    audio_stats.note_voice_error(&format!("create Discord Opus encoder: {error}"));
                                    error!(%error, "create Opus encoder for Discord voice");
                                    return;
                                }
                            };
                        }
                        let encoder = encoder.as_mut().expect("encoder initialized");
                        match encoder.encode(&frame, &mut encoded) {
                            Ok(size) => (
                                encoded[..size].to_vec(),
                                OPUS_FRAME,
                                frame.iter().filter(|sample| **sample != 0).count(),
                            ),
                            Err(error) => {
                                audio_stats.note_voice_error(&format!("encode Discord operator audio: {error}"));
                                warn!(%error, "encode Discord operator audio with Opus");
                                continue;
                            }
                        }
                    }
                };
                let opus_bytes = data.len() as u64;
                let sample = Sample {
                    data: data.into(),
                    duration,
                    ..Default::default()
                };
                if let Err(error) = track.write_sample(ssrc, 111, &sample, &[]).await {
                    audio_stats.note_voice_error(&format!("write Discord operator audio to WebRTC: {error}"));
                    warn!(%error, "write Discord operator audio to WebRTC Opus track");
                    return;
                }
                audio_stats.note_voice_in(input_observation_samples as u64);
                audio_stats.opus_packets_sent.fetch_add(1, Ordering::Relaxed);
                audio_stats.opus_bytes_sent.fetch_add(opus_bytes, Ordering::Relaxed);
            }
        }
    }
}

const OPERATOR_AUDIO_MAX_AGE: Duration = Duration::from_millis(200);
const OPERATOR_AUDIO_MAX_SAMPLES: usize = OPUS_FRAME_SAMPLES * 2 * 10;

enum OperatorAudioChunk {
    Pcm(VecDeque<i16>),
    Opus {
        data: Vec<u8>,
        duration: Duration,
        samples: usize,
    },
}

enum OperatorAudioOutput {
    Pcm(Vec<i16>),
    Opus {
        data: Vec<u8>,
        duration: Duration,
        samples: usize,
    },
}

#[derive(Clone, Default)]
struct OperatorAudioBuffer(Arc<Mutex<VecDeque<(Instant, OperatorAudioChunk)>>>);

impl OperatorAudioBuffer {
    fn push(&self, pcm: Vec<i16>, now: Instant) -> bool {
        let mut frames = self.0.lock().expect("operator audio buffer poisoned");
        Self::expire(&mut frames, now);
        frames.push_back((now, OperatorAudioChunk::Pcm(pcm.into())));
        let count = Self::sample_count(&frames);
        Self::discard(
            &mut frames,
            count.saturating_sub(OPERATOR_AUDIO_MAX_SAMPLES),
        );
        true
    }

    fn push_opus(&self, data: Vec<u8>, samples_per_channel: usize, now: Instant) -> bool {
        if data.is_empty() || samples_per_channel == 0 {
            return false;
        }
        let samples = samples_per_channel.saturating_mul(2);
        let duration =
            Duration::from_secs_f64(samples_per_channel as f64 / f64::from(MEDIA_SAMPLE_RATE));
        let mut frames = self.0.lock().expect("operator audio buffer poisoned");
        Self::expire(&mut frames, now);
        frames.push_back((
            now,
            OperatorAudioChunk::Opus {
                data,
                duration,
                samples,
            },
        ));
        let count = Self::sample_count(&frames);
        Self::discard(
            &mut frames,
            count.saturating_sub(OPERATOR_AUDIO_MAX_SAMPLES),
        );
        true
    }

    fn sample_count(frames: &VecDeque<(Instant, OperatorAudioChunk)>) -> usize {
        frames
            .iter()
            .map(|(_, chunk)| match chunk {
                OperatorAudioChunk::Pcm(pcm) => pcm.len(),
                OperatorAudioChunk::Opus { samples, .. } => *samples,
            })
            .sum()
    }

    fn expire(frames: &mut VecDeque<(Instant, OperatorAudioChunk)>, now: Instant) {
        while frames.front().is_some_and(|(received, chunk)| {
            let empty = matches!(chunk, OperatorAudioChunk::Pcm(pcm) if pcm.is_empty());
            empty || now.saturating_duration_since(*received) >= OPERATOR_AUDIO_MAX_AGE
        }) {
            frames.pop_front();
        }
    }

    fn discard(frames: &mut VecDeque<(Instant, OperatorAudioChunk)>, mut count: usize) {
        while count > 0 {
            let Some((_, chunk)) = frames.front_mut() else {
                break;
            };
            match chunk {
                OperatorAudioChunk::Pcm(pcm) => {
                    let take = count.min(pcm.len());
                    pcm.drain(..take);
                    count -= take;
                    if pcm.is_empty() {
                        frames.pop_front();
                    }
                }
                OperatorAudioChunk::Opus { samples, .. } => {
                    count = count.saturating_sub(*samples);
                    frames.pop_front();
                }
            }
        }
    }

    fn take_output(&self, now: Instant) -> Option<OperatorAudioOutput> {
        let mut frames = self.0.lock().expect("operator audio buffer poisoned");
        Self::expire(&mut frames, now);
        if matches!(frames.front(), Some((_, OperatorAudioChunk::Opus { .. }))) {
            let (_, chunk) = frames.pop_front().expect("front checked");
            let OperatorAudioChunk::Opus {
                data,
                duration,
                samples,
            } = chunk
            else {
                unreachable!()
            };
            return Some(OperatorAudioOutput::Opus {
                data,
                duration,
                samples,
            });
        }
        let needed = OPUS_FRAME_SAMPLES * 2;
        let available: usize = frames
            .iter()
            .take_while(|(_, chunk)| matches!(chunk, OperatorAudioChunk::Pcm(_)))
            .map(|(_, chunk)| match chunk {
                OperatorAudioChunk::Pcm(pcm) => pcm.len(),
                _ => 0,
            })
            .sum();
        let flush_partial = available > 0
            && available < needed
            && frames
                .iter()
                .skip_while(|(_, chunk)| matches!(chunk, OperatorAudioChunk::Pcm(_)))
                .any(|(_, chunk)| matches!(chunk, OperatorAudioChunk::Opus { .. }));
        if available < needed && !flush_partial {
            return None;
        }
        let target = available.min(needed);
        let mut output = Vec::with_capacity(needed);
        while output.len() < target {
            let (_, chunk) = frames.front_mut().expect("sample count checked");
            let OperatorAudioChunk::Pcm(pcm) = chunk else {
                unreachable!("available PCM checked")
            };
            let take = (needed - output.len()).min(pcm.len());
            output.extend(pcm.drain(..take));
            if pcm.is_empty() {
                frames.pop_front();
            }
        }
        output.resize(needed, 0);
        Some(OperatorAudioOutput::Pcm(output))
    }
}

async fn receive_native_audio(
    track: Arc<dyn TrackRemote>,
    playback: AudioPipe,
    audio_stats: Arc<AudioStats>,
) -> Result<()> {
    loop {
        match track.poll().await {
            Some(TrackRemoteEvent::OnRtpPacket(packet)) => {
                audio_stats
                    .native_opus_packets
                    .fetch_add(1, Ordering::Relaxed);
                playback.push_opus(
                    packet.header.sequence_number,
                    packet.header.timestamp,
                    &packet.payload,
                    Instant::now(),
                )?;
            }
            Some(TrackRemoteEvent::OnEnded) | Some(TrackRemoteEvent::OnEnding) | None => {
                return Ok(());
            }
            Some(_) => {}
        }
    }
}

async fn monitor_native_realtime_close(
    mut events: broadcast::Receiver<Value>,
    mut cancelled: watch::Receiver<bool>,
    active: Weak<AsyncMutex<HashMap<u64, ActiveCall>>>,
    lifecycle: Weak<AsyncMutex<()>>,
    input_gate: Arc<VoiceInputGate>,
    songbird: Arc<Songbird>,
    http: Arc<Http>,
    guild_id: GuildId,
    call_instance_id: Uuid,
    thread_id: String,
    transcript_thread_id: ChannelId,
    language: Language,
    native_close_signal: watch::Sender<Option<String>>,
    start_order: u64,
    start_generation: Option<u64>,
) {
    loop {
        let event = tokio::select! {
            changed = cancelled.changed() => {
                if changed.is_err() || *cancelled.borrow() {
                    return;
                }
                continue;
            }
            event = events.recv() => event,
        };
        let event = match event {
            Ok(event) => event,
            Err(broadcast::error::RecvError::Lagged(count)) => {
                warn!(count, thread = %thread_id, "missed native events while watching voice transport closure");
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => return,
        };
        if event.get("method").and_then(Value::as_str) != Some("thread/realtime/closed") {
            continue;
        }
        let params = event.get("params").unwrap_or(&Value::Null);
        if params.get("threadId").and_then(Value::as_str) != Some(thread_id.as_str()) {
            continue;
        }
        if outside_call_connection(params, start_order, start_generation) {
            continue;
        }
        let reason = params
            .get("reason")
            .and_then(Value::as_str)
            .or_else(|| params.get("message").and_then(Value::as_str))
            .or_else(|| params.get("error").and_then(Value::as_str))
            .or_else(|| {
                params
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
            })
            .unwrap_or("native realtime transport closed")
            .to_owned();
        let _ = native_close_signal.send(Some(reason.clone()));

        let Some(lifecycle) = lifecycle.upgrade() else {
            return;
        };
        let _lifecycle = tokio::select! {
            changed = cancelled.changed() => {
                if changed.is_err() || *cancelled.borrow() {
                    return;
                }
                continue;
            }
            guard = lifecycle.lock() => guard,
        };
        let Some(active) = active.upgrade() else {
            return;
        };
        let mut active_calls = active.lock().await;
        if !active_calls
            .get(&guild_id.get())
            .is_some_and(|call| call.call_instance_id == call_instance_id)
        {
            return;
        }
        let Some(active) = active_calls.remove(&guild_id.get()) else {
            return;
        };
        drop(active_calls);
        input_gate.set(VoiceInputPhase::Ending).await;

        let _ = active.media_cancel.send(true);
        let _ = active._output_track.stop();
        active.playback.close();
        let mut media_task = active.media_task;
        if tokio::time::timeout(Duration::from_secs(1), &mut media_task)
            .await
            .is_err()
        {
            media_task.abort();
        }
        let peer_result = active.peer.close().await;
        let mut data_channel_task = active.data_channel_task;
        if tokio::time::timeout(Duration::from_secs(2), &mut data_channel_task)
            .await
            .is_err()
        {
            data_channel_task.abort();
        }
        let discord_result = songbird.remove(guild_id).await;
        let mut transcript_task = active.transcript_task;
        let transcript_result =
            match tokio::time::timeout(Duration::from_secs(5), &mut transcript_task).await {
                Ok(result) => transcript_task_result(result),
                Err(_) => {
                    let _ = active.transcript_cancel.send(true);
                    transcript_task.abort();
                    VoiceOperationResult::failed(
                        "transcript cleanup timed out after native realtime closed",
                    )
                }
            };
        if transcript_result.ok == Some(false) {
            warn!(result = ?serde_json::to_value(&transcript_result), guild = guild_id.get(), "transcript cleanup failed after native realtime closed");
        }
        active.audio_stats.note_voice_end();
        let peer_summary = match peer_result {
            Ok(()) => language.text("closed", "닫힘", "已关闭", "終了").to_owned(),
            Err(error) => {
                warn!(%error, guild = guild_id.get(), "peer connection close failed after native realtime closed");
                language
                    .text("close failed", "닫기 실패", "关闭失败", "終了に失敗")
                    .to_owned()
            }
        };
        let discord_summary = match discord_result {
            Ok(_) => language
                .text("left", "퇴장함", "已离开", "退出済み")
                .to_owned(),
            Err(error) => {
                warn!(%error, guild = guild_id.get(), "Discord voice leave failed after native realtime closed");
                language
                    .text("leave failed", "퇴장 실패", "退出失败", "退出に失敗")
                    .to_owned()
            }
        };
        let report = language
            .text(
                "The realtime voice connection closed; cleanup is complete.\nPeer connection: {peer_summary}\nDiscord voice: {discord_summary}",
                "실시간 음성 연결이 종료되어 정리했습니다.\nPeer 연결: {peer_summary}\nDiscord 음성: {discord_summary}",
                "实时语音连接已关闭，清理完成。\nPeer 连接：{peer_summary}\nDiscord 语音：{discord_summary}",
                "リアルタイム音声接続が終了したため、クリーンアップしました。\nPeer 接続: {peer_summary}\nDiscord 音声: {discord_summary}",
            )
            .replace("{peer_summary}", &peer_summary)
            .replace("{discord_summary}", &discord_summary);
        if let Err(error) = transcript_thread_id.say(&http, report).await {
            warn!(%error, %reason, thread = %thread_id, "could not record native realtime closure and cleanup result");
        }
        if discord_summary != language.text("left", "퇴장함", "已离开", "退出済み") {
            warn!(%reason, %discord_summary, guild = guild_id.get(), "native realtime closed but Discord voice cleanup failed");
        }
        info!(%reason, guild = guild_id.get(), thread = %thread_id, "cleaned Discord voice resources after native realtime closed");
        input_gate.set(VoiceInputPhase::Idle).await;
        return;
    }
}

async fn handle_native_realtime_events(
    mut events: broadcast::Receiver<Value>,
    call: Arc<TranscriptCall>,
    http: Arc<Http>,
    playback: AudioPipe,
    mut cancelled: watch::Receiver<bool>,
    mut native_close_signal: watch::Receiver<Option<String>>,
) -> Result<()> {
    // RAII backstop precedes every async ledger guard: error/abort drops the
    // guard first, then marks receive responsibility ended.
    let _reader_registration = TranscriptReaderRegistration::register(&call)?;
    let retained = {
        let owners = call
            .native_item_owners
            .lock()
            .map_err(|_| anyhow!("transcript native item ownership lock poisoned"))?;
        let mut seen = HashSet::new();
        owners
            .values()
            .filter_map(|(id, owner)| {
                if *id == call.call_instance_id || !seen.insert(*id) {
                    return None;
                }
                owner
                    .upgrade()
                    .filter(|owner| owner.reader_state.load(Ordering::Acquire) == 2)
            })
            .collect::<Vec<_>>()
    };
    for owner in retained {
        drain_pending_native_events(&owner, &http).await?;
    }
    let thread_id = &call.native_thread_id;
    let transcript_thread_id = call.transcript_thread_id;
    let mut native_closed = false;
    loop {
        native_closed |= call.superseded.load(Ordering::Acquire);
        let received = tokio::select! {
            _ = cancelled.changed() => break,
            changed = native_close_signal.changed(), if !native_closed => {
                if changed.is_ok() && native_close_signal.borrow().is_some() {
                    native_closed = true;
                }
                continue;
            }
            received = async {
                if native_closed {
                    tokio::time::timeout(CANONICAL_DRAIN_IDLE, events.recv()).await.ok()
                } else {
                    Some(events.recv().await)
                }
            } => received,
        };
        let Some(received) = received else {
            break;
        };
        let event = match received {
            Ok(event) => event,
            Err(broadcast::error::RecvError::Lagged(count)) => {
                warn!(
                    count,
                    "missed native voice events; current canonical items remain authoritative"
                );
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => break,
        };
        let method = event
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let params = event.get("params").cloned().unwrap_or(Value::Null);
        // Actual original T/item ownership is checked before the current T filter
        // or current ledger. A live original reader continues to own its events.
        if let Some(owner) = call.native_event_owner(method, &event)? {
            if owner.call_instance_id != call.call_instance_id {
                if let Some(owner) = call.enqueue_native_event(owner, event)? {
                    // A live original reader remains responsible. Its exit drains
                    // the preserved copy; ended owners use the same retained front now.
                    if owner.call_instance_id == call.call_instance_id
                        || owner.reader_state.load(Ordering::Acquire) == 2
                    {
                        drain_pending_native_events(&owner, &http).await?;
                    }
                }
                continue;
            }
        }
        if params
            .get("threadId")
            .and_then(Value::as_str)
            .is_some_and(|id| id != thread_id.as_str())
        {
            continue;
        }
        if !call.accepts_event(&params)? {
            // A local event may have selected this call just before another
            // receiver corrected its owner. Re-resolve and preserve that raw.
            if let Some(owner) = call.native_event_owner(method, &event)? {
                if let Some(owner) = call.enqueue_native_event(owner, event)? {
                    if owner.call_instance_id == call.call_instance_id
                        || owner.reader_state.load(Ordering::Acquire) == 2
                    {
                        drain_pending_native_events(&owner, &http).await?;
                    }
                }
            }
            continue;
        }
        // An admissible session control may just have assigned an earlier
        // original record. Its native item/raw are delivered via the same owner.
        drain_pending_native_events(&call, &http).await?;
        let mut transcript = call.ledger.lock().await;
        match method {
            "thread/realtime/started" if native_closed => break,
            "thread/realtime/started" => {}
            "thread/realtime/transcript/delta" => {
                let (Some(role), Some(delta)) = (
                    params.get("role").and_then(Value::as_str),
                    params.get("delta").and_then(Value::as_str),
                ) else {
                    continue;
                };
                let Some(item) = transcript.observe_flat_delta(role, delta) else {
                    continue;
                };
                deliver_transcript(&http, &call, &mut transcript, &params, false, item).await?;
            }
            "thread/realtime/transcript/done" => {
                let (Some(role), Some(text)) = (
                    params.get("role").and_then(Value::as_str),
                    params.get("text").and_then(Value::as_str),
                ) else {
                    continue;
                };
                let Some(item) = transcript.observe_flat_done(role, text) else {
                    continue;
                };
                deliver_transcript(&http, &call, &mut transcript, &params, false, item).await?;
            }
            "thread/realtime/item/started"
            | "thread/realtime/item/transcript/delta"
            | "item/agentMessage/delta"
            | "thread/realtime/item/completed"
            | "item/completed" => {
                handle_native_transcript_item(&http, &call, &mut transcript, method, &params)
                    .await?;
            }
            "thread/realtime/closed" => {
                playback.clear();
                native_closed = true;
            }
            "thread/realtime/error" => {
                let message = params
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown realtime error");
                if let Some(checks) = &call.installation_checks {
                    checks.note_voice_error(
                        &call.native_thread_id,
                        &call.call_instance_id,
                        message,
                    );
                }
                error!(%message, "Codex realtime session error");
                if let Err(error) = transcript_thread_id
                    .say(
                        &http,
                        call.language.text(
                            "A realtime voice connection error occurred. Details are in the operation log.",
                            "실시간 음성 연결 오류가 발생했습니다. 상세 원인은 운영 기록에 보존했습니다.",
                            "实时语音连接发生错误。详情已保存在操作日志中。",
                            "リアルタイム音声接続エラーが発生しました。詳細は操作ログに記録されています。",
                        ),
                    )
                    .await
                {
                    warn!(%error, "Discord could not record native realtime error");
                }
            }
            _ => {}
        }
    }
    // The receive loop is finished. Final cleanup and any late known-owner
    // delivery now serialize on this original ledger instead of dropping the event.
    call.reader_state.store(2, Ordering::Release);
    drain_pending_native_events(&call, &http).await?;
    let mut transcript = call.ledger.lock().await;
    let incomplete_items = transcript.mark_open_items_incomplete();
    let should_report_incomplete = has_nonempty_incomplete_transcript(&incomplete_items);
    drop(transcript);
    if should_report_incomplete {
        transcript_thread_id
            .say(
                &http,
                call.language.text(
                    "An unfinished transcript remained when the call ended, so it was not posted.",
                    "음성 종료 전에 끝나지 않은 전사가 있어 게시하지 않았습니다.",
                    "通话结束时仍有未完成的转录，因此未予发布。",
                    "通話終了時に未完了の文字起こしが残っていたため、投稿しませんでした。",
                ),
            )
            .await
            .context("could not post incomplete transcript notice")?;
    }
    call.delivery_result()
}

fn should_publish_transcript(item: &TranscriptItem) -> bool {
    item.status == TranscriptStatus::Complete && !item.text.is_empty()
}

fn has_nonempty_incomplete_transcript(items: &[TranscriptItem]) -> bool {
    items
        .iter()
        .any(|item| item.status == TranscriptStatus::Incomplete && !item.text.is_empty())
}

fn observe_native_transcript_item(
    transcript: &mut TranscriptLedger,
    method: &str,
    params: &Value,
) -> Option<TranscriptItem> {
    if let Some(observation) = crate::native::state_observation_order(params) {
        if !transcript.processed_native_observations.insert(observation) {
            // Another receiver can carry the same broadcast observation after
            // the original reader ended. Never append its delta twice. If a
            // previous delivery await was cancelled, retain/resume current raw.
            let item_id = params
                .get("itemId")
                .and_then(Value::as_str)
                .or_else(|| params.pointer("/item/id").and_then(Value::as_str))?;
            let item = transcript.items.get(item_id)?.clone();
            return (!item.text.is_empty()
                && (transcript.has_pending_posts(item_id)
                    || item.posted_content.as_deref()
                        != Some(
                            transcript_content_pages_for_language(&item, transcript.language)
                                .as_slice(),
                        )))
            .then_some(item);
        }
    }
    match method {
        "thread/realtime/item/started" => {
            let item = params.get("item")?;
            if item.get("type").and_then(Value::as_str) != Some("transcriptSegment") {
                return None;
            }
            let item_id = item.get("id").and_then(Value::as_str)?;
            let role = item.get("role").and_then(Value::as_str)?;
            transcript.start_canonical(item_id, role);
            None
        }
        "thread/realtime/item/transcript/delta" | "item/agentMessage/delta" => {
            let item_id = params.get("itemId").and_then(Value::as_str)?;
            let delta = params.get("delta").and_then(Value::as_str)?;
            Some(transcript.partial(item_id, delta))
        }
        "thread/realtime/item/completed" | "item/completed" => {
            let item = params.get("item")?;
            if item.get("delivery").and_then(Value::as_str) == Some("async") {
                return None;
            }
            let item_id = item.get("id").and_then(Value::as_str)?;
            let item_type = item.get("type").and_then(Value::as_str).unwrap_or_default();
            let role = match item_type {
                "transcriptSegment" => item
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown"),
                "agentMessage" => "assistant",
                _ => return None,
            };
            let text = item.get("text").and_then(Value::as_str).unwrap_or_default();
            transcript.observe_canonical_completion(item_id, role, text)
        }
        _ => None,
    }
}

fn transcript_attribution_item_id<'a>(
    method: &str,
    params: &'a Value,
    transcript: &TranscriptLedger,
) -> Option<&'a str> {
    match method {
        "thread/realtime/item/started"
            if params.pointer("/item/type").and_then(Value::as_str)
                == Some("transcriptSegment")
                && params.pointer("/item/role").and_then(Value::as_str) == Some("user") =>
        {
            params.pointer("/item/id").and_then(Value::as_str)
        }
        "thread/realtime/item/transcript/delta" => {
            let item_id = params.get("itemId").and_then(Value::as_str)?;
            (params.get("role").and_then(Value::as_str) == Some("user")
                || transcript
                    .items
                    .get(item_id)
                    .is_some_and(|item| item.role == "user"))
            .then_some(item_id)
        }
        "thread/realtime/item/completed" | "item/completed"
            if params.pointer("/item/type").and_then(Value::as_str)
                == Some("transcriptSegment")
                && params.pointer("/item/role").and_then(Value::as_str) == Some("user") =>
        {
            params.pointer("/item/id").and_then(Value::as_str)
        }
        _ => None,
    }
}

async fn handle_native_transcript_item(
    http: &Arc<Http>,
    call: &TranscriptCall,
    transcript: &mut TranscriptLedger,
    method: &str,
    params: &Value,
) -> Result<()> {
    call.observe_installation_realtime_item(method, params);
    let item_id = transcript_attribution_item_id(method, params, transcript);
    let speaker_ids = item_id.map(|item_id| match method {
        "thread/realtime/item/started" => call.audio_stats.start_transcript_item(item_id),
        "thread/realtime/item/completed" | "item/completed" => {
            call.audio_stats.finish_transcript_item(item_id)
        }
        _ => call.audio_stats.ensure_transcript_item(item_id),
    });
    if let Some(item) = observe_native_transcript_item(transcript, method, params) {
        if let Some(speaker_ids) = speaker_ids.as_ref() {
            transcript.merge_speaker_ids(&item.source_id, speaker_ids.iter().copied());
        }
        let mut item = item;
        if let Some(speaker_ids) = speaker_ids {
            item.speaker_ids = speaker_ids;
            if let Some(stored) = transcript.items.get(&item.source_id) {
                item.speaker_ids = stored.speaker_ids.clone();
            }
        }
        deliver_transcript(
            http,
            call,
            transcript,
            params,
            method == "thread/realtime/item/completed",
            item,
        )
        .await?;
    } else if let (Some(item_id), Some(speaker_ids)) = (item_id, speaker_ids) {
        transcript.merge_speaker_ids(item_id, speaker_ids);
    }
    Ok(())
}

async fn deliver_transcript(
    http: &Arc<Http>,
    call: &TranscriptCall,
    ledger: &mut TranscriptLedger,
    original_params: &Value,
    canonical_completed_event: bool,
    item: TranscriptItem,
) -> Result<()> {
    if !should_publish_transcript(&item) {
        return Ok(());
    }
    // Recheck immediately before any Discord lookup/edit/post. A call can be
    // superseded while this reader is returning from its previous HTTP await.
    if !call.accepts_event(original_params)? {
        return Ok(());
    }
    let source_id = item.source_id.clone();
    let mut item = item;
    item.agent_label = call.agent_name.clone();
    if item.role == "user" {
        item.speaker_ids.sort_unstable();
        item.speaker_ids.dedup();
        item.speaker_labels.clear();
        for speaker_id in item.speaker_ids.iter().copied() {
            item.speaker_labels
                .push(call.display_name_for(http, speaker_id).await);
        }
    }
    if let Some(stored) = ledger.items.get_mut(&source_id) {
        stored.agent_label = item.agent_label.clone();
        stored.speaker_ids = item.speaker_ids.clone();
        stored.speaker_labels = item.speaker_labels.clone();
    }
    let prepared = if item.role == "assistant" && item.status == TranscriptStatus::Complete {
        if let Some(config) = call.reply_file_config.as_deref() {
            ledger
                .prepared_reply_for(&source_id, &item.text, || {
                    crate::reply_files::prepare_reply(config, &item.text)
                })
                .await
        } else {
            PreparedReply {
                content: item.text.clone(),
                files: Vec::new(),
            }
        }
    } else {
        PreparedReply {
            content: item.text.clone(),
            files: Vec::new(),
        }
    };
    let result = upsert_transcript(
        http,
        call.transcript_thread_id,
        ledger,
        item,
        prepared,
        call.language,
    )
    .await;
    call.observe_installation_transcript(
        ledger,
        original_params,
        &source_id,
        canonical_completed_event,
        &result,
    );
    call.record_delivery(ledger, &source_id, &result)?;
    // Delivery failure is retained and reported at reader exit; only a failure
    // to retain it ends this reader immediately with an error.
    Ok(())
}

struct TranscriptReplyRender {
    contents: Vec<String>,
}

fn render_transcript_reply(
    item: &TranscriptItem,
    prepared: &PreparedReply,
    language: Language,
) -> TranscriptReplyRender {
    let upload_files = item.role == "assistant" && item.status == TranscriptStatus::Complete;
    let mut rendered = item.clone();
    if upload_files {
        rendered.text.clone_from(&prepared.content);
    }
    let mut contents = transcript_content_pages_for_language(&rendered, language);
    let file_pages = if upload_files {
        attachment_batches(&prepared.files).len()
    } else {
        0
    };
    let total_pages = contents.len().max(file_pages);
    while contents.len() < total_pages {
        let page = contents.len() + 1;
        let label = language.text(
            "Attachments continued",
            "첨부 파일 계속",
            "附件续页",
            "添付ファイルの続き",
        );
        contents.push(format!(
            "**{}**\n{} · {}",
            escape_markdown_header(&item.agent_label),
            label,
            page,
        ));
    }
    TranscriptReplyRender { contents }
}

fn transcript_files_for_page<'a>(
    item: &TranscriptItem,
    prepared: &'a PreparedReply,
    page: usize,
) -> Option<&'a [ReplyFile]> {
    if item.role != "assistant" || item.status != TranscriptStatus::Complete {
        return None;
    }
    let batches = attachment_batches(&prepared.files);
    batches
        .get(page)
        .map(|range| &prepared.files[range.clone()])
        .or_else(|| Some(&prepared.files[prepared.files.len()..]))
}

async fn upsert_transcript(
    http: &Arc<Http>,
    thread_id: ChannelId,
    ledger: &mut TranscriptLedger,
    item: TranscriptItem,
    prepared: PreparedReply,
    language: Language,
) -> Result<()> {
    let rendered = render_transcript_reply(&item, &prepared, language);
    let contents = rendered.contents;
    let mut failures = Vec::new();
    if ledger.needs_discord_lookup(&item, &contents) {
        // Unknown original POSTs are queried first. A failed lookup prevents
        // new POSTs, but does not remove authority to edit already-known IDs.
        let lookup: Result<()> = async {
            let found = thread_id
                .messages(http, GetMessages::new().limit(100))
                .await
                .context("look up a possibly delivered native transcript source")?
                .into_iter()
                .map(|message| {
                    (
                        message.id,
                        message.content,
                        message.author.id,
                        message.nonce,
                    )
                })
                .collect::<Vec<_>>();
            if ledger.has_pending_posts(&item.source_id) {
                let expected_author = http
                    .get_current_user()
                    .await
                    .context("authenticate the original transcript message author")?
                    .id;
                ledger.recover_discord_messages(&item, &contents, found, expected_author)?;
            }
            Ok(())
        }
        .await;
        if let Err(error) = lookup {
            failures.push(error);
        }
    }
    let known_ids = ledger
        .items
        .get(&item.source_id)
        .map(|record| record.discord_message_ids.clone())
        .unwrap_or_default();
    let existing_reply_files = ledger
        .posted_reply_files
        .get(&item.source_id)
        .cloned()
        .unwrap_or_default();
    for (index, content) in contents.iter().enumerate() {
        if let Some(message_id) = known_ids.get(index) {
            let mut edit = serenity::builder::EditMessage::new()
                .content(content)
                .allowed_mentions(transcript_empty_mentions());
            if let Some(files) = transcript_files_for_page(&item, &prepared, index) {
                let page_files = existing_reply_files
                    .iter()
                    .filter(|file| file.page == index)
                    .cloned()
                    .collect::<Vec<_>>();
                let plan = reply_file_edit_plan(files, &page_files);
                let attachments = plan.into_iter().fold(
                    EditAttachments::new(),
                    |attachments, action| match action {
                        ReplyFileEdit::Keep(id) => attachments.keep(id),
                        ReplyFileEdit::Upload(file_index) => {
                            let file = &files[file_index];
                            attachments.add(CreateAttachment::bytes(
                                file.data.clone(),
                                file.filename.clone(),
                            ))
                        }
                    },
                );
                // Explicit replacement removes stale uploads when the assistant
                // edits away a local-file link and avoids attachment duplication.
                edit = edit.attachments(attachments);
            }
            match thread_id
                .edit_message(http, *message_id, edit)
                .await
                .with_context(|| {
                    format!(
                        "edit Discord page {} for native transcript item {}",
                        index + 1,
                        item.source_id
                    )
                }) {
                Ok(message) => {
                    if let Some(files) = transcript_files_for_page(&item, &prepared, index) {
                        match posted_reply_files_from_attachments(
                            files,
                            index,
                            &message.attachments,
                        ) {
                            Ok(sent) => {
                                ledger.mark_posted_reply_files(&item.source_id, index, sent)
                            }
                            Err(error) => {
                                ledger.clear_posted_reply_files_page(&item.source_id, index);
                                failures.push(error);
                            }
                        }
                    }
                }
                Err(error) => failures.push(error),
            }
        } else if failures.is_empty() && !ledger.has_pending_posts(&item.source_id) {
            let attempt = ledger.record_post_attempt(&item.source_id, index, content);
            let nonce = ledger.post_attempts[&item.source_id][attempt].nonce.clone();
            let mut create = CreateMessage::new()
                .content(content)
                .nonce(Nonce::String(nonce))
                .allowed_mentions(transcript_empty_mentions());
            if let Some(files) = transcript_files_for_page(&item, &prepared, index) {
                for file in files {
                    create = create.add_file(CreateAttachment::bytes(
                        file.data.clone(),
                        file.filename.clone(),
                    ));
                }
            }
            match thread_id.send_message(http, create).await.with_context(|| {
                format!(
                    "post Discord page {} for native transcript item {}",
                    index + 1,
                    item.source_id
                )
            }) {
                Ok(message) => {
                    ledger.confirm_post_attempt(&item.source_id, attempt, message.id);
                    if let Err(error) = ledger.bind_discord_page(&item.source_id, index, message.id)
                    {
                        failures.push(error);
                    }
                    if let Some(files) = transcript_files_for_page(&item, &prepared, index) {
                        match posted_reply_files_from_attachments(
                            files,
                            index,
                            &message.attachments,
                        ) {
                            Ok(sent) => {
                                ledger.mark_posted_reply_files(&item.source_id, index, sent)
                            }
                            Err(error) => {
                                ledger.clear_posted_reply_files_page(&item.source_id, index);
                                failures.push(error);
                            }
                        }
                    }
                }
                Err(error) => failures.push(error),
            }
        }
    }
    for (index, message_id) in known_ids.iter().enumerate().skip(contents.len()) {
        let retired = format!(
            "**{}**\n{}",
            language.text(
                "Transcript · previous segment",
                "전사 · 이전 부분",
                "转录 · 先前片段",
                "文字起こし · 前の部分",
            ),
            language.text(
                "(This page was merged into the confirmed transcript.)",
                "(이 페이지는 확정 전사로 통합되었습니다.)",
                "（此页内容已合并到已确认的转录中。）",
                "（このページは確定済みの文字起こしに統合されました。）",
            ),
        );
        if let Err(error) = thread_id
            .edit_message(
                http,
                *message_id,
                serenity::builder::EditMessage::new()
                    .content(retired)
                    .attachments(EditAttachments::new())
                    .allowed_mentions(transcript_empty_mentions()),
            )
            .await
            .with_context(|| {
                format!(
                    "retire extra Discord page {} for native transcript item {}",
                    index + 1,
                    item.source_id
                )
            })
        {
            failures.push(error);
        }
    }
    if ledger.has_pending_posts(&item.source_id) && failures.is_empty() {
        failures.push(anyhow!("original transcript POST remains unconfirmed"));
    }
    if !failures.is_empty() {
        bail!(
            "{}",
            failures
                .iter()
                .map(|error| format!("{error:#}"))
                .collect::<Vec<_>>()
                .join("; ")
        );
    }
    ledger.mark_posted_source_text(&item.source_id, &item.text, contents);
    Ok(())
}

fn discord_member_display_name(
    nickname: Option<&str>,
    global_name: Option<&str>,
    username: &str,
) -> String {
    nickname
        .filter(|name| !name.trim().is_empty())
        .or_else(|| global_name.filter(|name| !name.trim().is_empty()))
        .unwrap_or(username)
        .to_owned()
}

fn transcript_empty_mentions() -> serenity::builder::CreateAllowedMentions {
    serenity::builder::CreateAllowedMentions::new()
        .all_users(false)
        .all_roles(false)
        .everyone(false)
        .replied_user(false)
}

fn escape_markdown_header(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars().filter(|character| !character.is_control()) {
        if matches!(
            character,
            '\\' | '`'
                | '*'
                | '_'
                | '{'
                | '}'
                | '['
                | ']'
                | '('
                | ')'
                | '#'
                | '+'
                | '-'
                | '.'
                | '!'
                | '|'
                | '>'
                | '~'
        ) {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

fn transcript_content_pages(item: &TranscriptItem) -> Vec<String> {
    transcript_content_pages_for_language(item, Language::English)
}

fn transcript_content_pages_for_language(item: &TranscriptItem, language: Language) -> Vec<String> {
    let phase = match item.status {
        TranscriptStatus::Incomplete => language.text(
            " · unconfirmed at call end",
            " · 종료 시 미확정",
            " · 通话结束时未确认",
            " · 通話終了時に未確定",
        ),
        TranscriptStatus::Partial => language.text(
            " · partial transcript",
            " · 부분 전사",
            " · 部分转录",
            " · 部分文字起こし",
        ),
        TranscriptStatus::Complete => "",
    };
    let role = match item.role.as_str() {
        "user" => {
            if item.speaker_labels.is_empty() {
                language
                    .text(
                        "Discord speaker",
                        "Discord 사용자",
                        "Discord 发言者",
                        "Discordの話者",
                    )
                    .to_owned()
            } else {
                item.speaker_labels.join(" + ")
            }
        }
        "assistant" => {
            if item.agent_label.is_empty() {
                "Codex".to_owned()
            } else {
                item.agent_label.clone()
            }
        }
        _ => language
            .text("Transcript", "전사", "转录", "文字起こし")
            .to_owned(),
    };
    let head = format!("**{}{phase}**", escape_markdown_header(&role));
    let page_payload = 1_750_usize
        .saturating_sub(head.chars().count() + 24)
        .max(256);
    let chunks = split_unicode(&item.text, page_payload);
    let page_count = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(index, chunk)| {
            let page = if page_count > 1 {
                match language.code() {
                    "ko" => format!(" · {}/{}쪽", index + 1, page_count),
                    "zh" => format!(" · 第{}页，共{}页", index + 1, page_count),
                    "ja" => format!(" · {}/{}ページ", index + 1, page_count),
                    _ => format!(" · page {} of {}", index + 1, page_count),
                }
            } else {
                String::new()
            };
            format!("{head}{page}\n{chunk}")
        })
        .collect()
}

fn split_unicode(text: &str, max_chars: usize) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::new();
    let mut chunk = String::new();
    let mut count = 0;
    for ch in text.chars() {
        if count >= max_chars {
            out.push(std::mem::take(&mut chunk));
            count = 0;
        }
        chunk.push(ch);
        count += 1;
    }
    if !chunk.is_empty() {
        out.push(chunk);
    }
    out
}

async fn create_transcript_thread(
    http: &Arc<Http>,
    config: &Config,
) -> Result<(ChannelId, MessageId)> {
    let parent = ChannelId::new(config.transcript_channel_id);
    let anchor = parent
        .send_message(
            http,
            CreateMessage::new().content(format!(
                "{} · {}",
                config.language.text(
                    "Voice call transcript",
                    "음성 통화 기록",
                    "语音通话转录",
                    "音声通話の文字起こし",
                ),
                config.name
            )),
        )
        .await
        .context("create a public transcript anchor in the configured Discord channel")?;
    let thread = parent
        .create_thread_from_message(
            http,
            anchor.id,
            CreateThread::new(format!(
                "{} {}",
                config.name,
                config
                    .language
                    .text("voice call", "음성 통화", "语音通话", "音声通話"),
            ))
            .kind(ChannelType::PublicThread),
        )
        .await
        .with_context(|| {
            format!(
                "create a public transcript thread from anchor message {}",
                anchor.id
            )
        })?;
    if thread.kind != ChannelType::PublicThread {
        bail!(
            "Discord returned {:?} for a transcript thread; expected PublicThread",
            thread.kind
        );
    }
    thread
        .id
        .say(
            http,
            format!(
                "{} · {}",
                config.language.text(
                    "Voice call started",
                    "음성 통화 시작",
                    "语音通话已开始",
                    "音声通話開始",
                ),
                config.name,
            ),
        )
        .await
        .context("write voice call start record")?;
    Ok((thread.id, anchor.id))
}

fn is_native_audio_interrupt(data: &[u8]) -> bool {
    let Ok(value) = serde_json::from_slice::<Value>(data) else {
        return false;
    };
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    kind == "response.cancelled"
        || kind == "output_audio_buffer.cleared"
        || (kind == "response.done"
            && value.pointer("/response/status").and_then(Value::as_str) == Some("cancelled"))
}

#[derive(Clone, Default)]
struct AudioPipe(Arc<Mutex<AudioPipeState>>);

#[derive(Default)]
struct AudioPipeState {
    closed: bool,
    generation: u64,
    opus_source: Option<opus_source::OpusSource>,
}

impl AudioPipe {
    fn new() -> Result<Self> {
        let source = opus_source::OpusSource::new()?;
        Ok(Self(Arc::new(Mutex::new(AudioPipeState {
            opus_source: Some(source),
            ..AudioPipeState::default()
        }))))
    }

    fn install_opus_input(&self, audio_stats: Arc<AudioStats>) -> Result<Input> {
        let source = self
            .0
            .lock()
            .expect("audio pipe poisoned")
            .opus_source
            .clone()
            .context("live Opus source is not initialized")?;
        source.set_media_callback(Arc::new(move |samples| audio_stats.note_voice_out(samples)));
        source.input()
    }

    fn push_opus(&self, sequence: u16, timestamp: u32, payload: &[u8], now: Instant) -> Result<()> {
        let state = self.0.lock().expect("audio pipe poisoned");
        if state.closed {
            return Ok(());
        }
        let generation = state.generation;
        let source = state
            .opus_source
            .as_ref()
            .context("Discord Opus output is not initialized")?;
        source.push(generation, sequence, timestamp, payload, now)
    }

    fn clear(&self) {
        let mut state = self.0.lock().expect("audio pipe poisoned");
        state.generation = state.generation.wrapping_add(1);
        if let Some(source) = &state.opus_source {
            source.interrupt(state.generation);
        }
    }

    fn close(&self) {
        let mut state = self.0.lock().expect("audio pipe poisoned");
        state.closed = true;
        if let Some(source) = &state.opus_source {
            source.close();
        }
    }
}

async fn start_discord_playback(
    call: &Arc<AsyncMutex<Call>>,
    playback: AudioPipe,
    audio_stats: Arc<AudioStats>,
) -> Result<songbird::tracks::TrackHandle> {
    let input = playback.install_opus_input(audio_stats.clone())?;
    let mut call = call.lock().await;
    let track = call.play_input(input);
    track
        .add_event(
            Event::Track(TrackEvent::Error),
            PlaybackErrorObserver(audio_stats),
        )
        .context("observe Songbird output track errors")?;
    Ok(track)
}

struct PlaybackErrorObserver(Arc<AudioStats>);

#[async_trait]
impl VoiceEventHandler for PlaybackErrorObserver {
    async fn act(&self, context: &EventContext<'_>) -> Option<Event> {
        if let EventContext::Track(tracks) = context {
            self.0
                .note_voice_error("Songbird voice output track failed");
            for (state, _) in tracks.iter() {
                error!(
                    playing = ?state.playing,
                    ready = ?state.ready,
                    play_time = ?state.play_time,
                    position = ?state.position,
                    "Songbird voice output track failed"
                );
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_settings_absence_falls_back_to_configured_voice() {
        let directory = tempfile::tempdir().expect("temporary state directory");
        let path = directory.path().join(VOICE_SETTINGS_FILE);
        assert_eq!(
            selected_voice_from_file(&path, Some("configured-voice")).unwrap(),
            Some("configured-voice".to_owned())
        );
        assert_eq!(selected_voice_from_file(&path, None).unwrap(), None);
    }

    #[test]
    fn voice_settings_override_and_explicit_null_survive_reopen() {
        let directory = tempfile::tempdir().expect("temporary state directory");
        let path = directory.path().join(VOICE_SETTINGS_FILE);
        persist_voice_selection(&path, Some("selected-voice")).unwrap();
        assert_eq!(
            selected_voice_from_file(&path, Some("configured-voice")).unwrap(),
            Some("selected-voice".to_owned())
        );

        persist_voice_selection(&path, None).unwrap();
        assert_eq!(
            selected_voice_from_file(&path, Some("configured-voice")).unwrap(),
            None
        );
    }

    #[test]
    fn malformed_voice_settings_report_an_error_instead_of_silently_falling_back() {
        let directory = tempfile::tempdir().expect("temporary state directory");
        let path = directory.path().join(VOICE_SETTINGS_FILE);
        fs::write(&path, b"{ invalid json }").unwrap();
        assert!(selected_voice_from_file(&path, Some("configured-voice")).is_err());
    }

    fn receiver_report_packet(blocks: &[(u32, u32)]) -> Vec<u8> {
        assert!(blocks.len() <= 31);
        let mut packet = vec![0x80 | blocks.len() as u8, 201, 0, 0];
        let words_minus_one = ((8 + 24 * blocks.len()) / 4 - 1) as u16;
        packet[2..4].copy_from_slice(&words_minus_one.to_be_bytes());
        packet.extend_from_slice(&999_u32.to_be_bytes()); // Reporter, not the bot stream.
        for (ssrc, sequence) in blocks {
            packet.extend_from_slice(&ssrc.to_be_bytes());
            packet.extend_from_slice(&[0; 4]); // Loss fraction and cumulative loss.
            packet.extend_from_slice(&sequence.to_be_bytes());
            packet.extend_from_slice(&[0; 12]); // Jitter and sender-report timing.
        }
        packet
    }

    fn decrypted_rtcp_event(mut plaintext: Vec<u8>) -> Vec<u8> {
        plaintext.extend_from_slice(&[0xA5; 20]); // AEAD tag and nonce suffix.
        plaintext
    }

    #[test]
    fn receiver_report_reads_extended_sequence_for_the_actual_bot_source() {
        let packet = decrypted_rtcp_event(receiver_report_packet(&[
            (40, 0x0001_fffe),
            (41, 0x0002_0003),
        ]));
        assert_eq!(
            discord_receiver_report_sequences(&packet, 8, 20, 40),
            Some(vec![0x0001_fffe])
        );
        assert_eq!(
            discord_receiver_report_sequences(&packet, 8, 20, 41),
            Some(vec![0x0002_0003])
        );
        assert_eq!(
            discord_receiver_report_sequences(&packet, 8, 20, 999),
            Some(vec![])
        );
        assert_eq!(
            discord_receiver_report_sequences(&packet, 8, 20, 42),
            Some(vec![])
        );
    }

    #[test]
    fn receiver_report_checks_compound_lengths_padding_and_all_report_blocks() {
        let mut compound = receiver_report_packet(&[(40, 6)]);
        let mut padded = receiver_report_packet(&[(40, 7)]);
        padded[0] |= 0x20;
        padded.extend_from_slice(&[0, 0, 0, 4]);
        padded[2..4].copy_from_slice(&8_u16.to_be_bytes());
        compound.extend_from_slice(&padded);
        assert_eq!(
            discord_receiver_report_sequences(&decrypted_rtcp_event(compound.clone()), 8, 20, 40),
            Some(vec![6, 7])
        );
        let last = compound.len() - 1;
        compound[last] = 0;
        assert_eq!(
            discord_receiver_report_sequences(&decrypted_rtcp_event(compound), 8, 20, 40),
            None
        );
        let mut short_second_block = receiver_report_packet(&[(40, 6)]);
        short_second_block[0] = 0x82; // The second declared block is absent.
        assert_eq!(
            discord_receiver_report_sequences(&decrypted_rtcp_event(short_second_block), 8, 20, 40),
            None
        );
    }

    #[test]
    fn receiver_report_rejects_truncation_bad_version_and_undecrypted_fallback() {
        let plaintext = receiver_report_packet(&[(40, 8)]);
        for length in 0..plaintext.len() {
            let packet = decrypted_rtcp_event(plaintext[..length].to_vec());
            assert_eq!(discord_receiver_report_sequences(&packet, 8, 20, 40), None);
        }
        let mut packet = decrypted_rtcp_event(plaintext);
        assert_eq!(discord_receiver_report_sequences(&packet, 0, 20, 40), None);
        assert_eq!(
            discord_receiver_report_sequences(&packet, 8, packet.len() + 1, 40),
            None
        );
        packet[0] = 0x41;
        assert_eq!(discord_receiver_report_sequences(&packet, 8, 20, 40), None);
    }

    #[test]
    fn only_a_known_configured_operator_ssrc_is_forwardable() {
        let mut speakers = SpeakerRegistry::new([10, 11]);
        assert!(speakers.unresolved(300));
        assert_eq!(speakers.operator_for(300), None);
        assert!(speakers.take_unresolved_report_slot(300));
        assert!(!speakers.take_unresolved_report_slot(300));
        speakers.observe(300, 99);
        assert_eq!(speakers.operator_for(300), None);
        speakers.observe(300, 10);
        assert_eq!(speakers.operator_for(300), Some(10));
        speakers.remove_user(10);
        assert!(speakers.unresolved(300));
        assert_eq!(speakers.operator_for(300), None);
    }

    #[test]
    fn canonical_transcript_completion_edits_the_source_item_and_is_not_duplicated() {
        let mut ledger = TranscriptLedger::default();
        let item = ledger.partial("native-item-7", "result ");
        assert_eq!(item.status, TranscriptStatus::Partial);
        ledger.bind_discord_message("native-item-7", MessageId::new(55));
        let completed = ledger
            .complete("native-item-7", "assistant", "result: 5")
            .unwrap();
        assert_eq!(completed.discord_message_ids, vec![MessageId::new(55)]);
        assert_eq!(completed.status, TranscriptStatus::Complete);
        assert_eq!(completed.text, "result: 5");
        ledger.mark_posted("native-item-7", transcript_content_pages(&completed));
        assert!(
            ledger
                .complete("native-item-7", "assistant", "result: 5")
                .is_none()
        );
    }

    #[test]
    fn flat_done_keeps_raw_text_partial_until_canonical_completion_promotes_the_record() {
        let mut ledger = TranscriptLedger::default();
        let mut partial_text = String::new();
        for delta in ["이제 ", "나가", "도 돼"] {
            partial_text.push_str(delta);
            let partial = ledger.observe_flat_delta("user", delta).unwrap();
            assert_eq!(partial.source_id, "transcript-event-1");
            assert_eq!(partial.native_source_id, None);
            assert_eq!(partial.role, "user");
            assert_eq!(partial.status, TranscriptStatus::Partial);
            assert_eq!(partial.text, partial_text);
            ledger.bind_discord_message(&partial.source_id, MessageId::new(42));
            let pages = transcript_content_pages(&partial);
            assert_eq!(
                pages,
                vec![format!(
                    "**Discord speaker · partial transcript**\n{partial_text}"
                )]
            );
            ledger.mark_posted(&partial.source_id, pages);
        }
        let raw_text = "이제 나가도 돼.\n  원문 🙂";
        let done = ledger.observe_flat_done("user", raw_text).unwrap();
        assert_eq!(done.source_id, "transcript-event-1");
        assert_eq!(done.native_source_id, None);
        assert_eq!(done.role, "user");
        assert_eq!(done.status, TranscriptStatus::Partial);
        assert_eq!(done.text, raw_text);
        assert_eq!(done.discord_message_ids, vec![MessageId::new(42)]);
        let pages = transcript_content_pages(&done);
        assert_eq!(
            pages,
            vec![format!(
                "**Discord speaker · partial transcript**\n{raw_text}"
            )]
        );
        ledger.mark_posted(&done.source_id, pages.clone());
        assert_eq!(ledger.items.len(), 1);
        assert_eq!(ledger.items[&done.source_id].posted_content, Some(pages));

        let mut ended_without_canonical = ledger.clone();
        let incomplete = ended_without_canonical.mark_open_items_incomplete();
        assert_eq!(incomplete.len(), 1);
        assert_eq!(incomplete[0].text, raw_text);
        assert_eq!(incomplete[0].discord_message_ids, vec![MessageId::new(42)]);
        assert_eq!(incomplete[0].native_source_id, None);
        assert_eq!(incomplete[0].status, TranscriptStatus::Incomplete);
        assert_eq!(
            transcript_content_pages(&incomplete[0]),
            vec![format!(
                "**Discord speaker · unconfirmed at call end**\n{raw_text}"
            )]
        );

        let canonical = ledger
            .observe_canonical_completion("native-item-9", "user", raw_text)
            .unwrap();
        assert_eq!(canonical.source_id, "native-item-9");
        assert_eq!(canonical.native_source_id.as_deref(), Some("native-item-9"));
        assert_eq!(canonical.role, "user");
        assert_eq!(canonical.status, TranscriptStatus::Complete);
        assert_eq!(canonical.text, raw_text);
        assert_eq!(canonical.discord_message_ids, vec![MessageId::new(42)]);
        assert_eq!(canonical.posted_content, None);
        assert_eq!(ledger.items.len(), 1);
        assert!(!ledger.items.contains_key("transcript-event-1"));
        assert!(ledger.pending_flat_done.is_empty());
        let pages = transcript_content_pages(&canonical);
        assert_eq!(pages, vec![format!("**Discord speaker**\n{raw_text}")]);
        ledger.mark_posted(&canonical.source_id, pages.clone());
        assert_eq!(
            ledger.items[&canonical.source_id].posted_content,
            Some(pages)
        );
        assert!(
            ledger
                .observe_canonical_completion("native-item-9", "user", raw_text)
                .is_none()
        );
        assert!(ledger.observe_flat_done("user", raw_text).is_none());
        assert_eq!(ledger.items.len(), 1);
    }

    #[test]
    fn realtime_start_requests_remaining_transcript_commit_on_the_same_thread() {
        let thread_id = "persistent-native-thread";
        let call_id = Uuid::from_u128(1);
        let params = realtime_start_params(
            thread_id,
            call_id,
            Some("cedar"),
            "voice-offer",
            &realtime_call_start_context(call_id, thread_id, Language::English),
        );

        assert_eq!(params["threadId"], thread_id);
        assert_eq!(params["realtimeSessionId"], call_id.to_string());
        assert_eq!(
            params["transport"],
            json!({"type": "webrtc", "sdp": "voice-offer"})
        );
        assert_eq!(params["voice"], "cedar");
        assert_eq!(params["flushTranscriptTailOnSessionEnd"], true);
        assert!(params.get("model").is_none());
        assert!(params.get("modelProvider").is_none());
        assert!(params.get("modelExecutor").is_none());
        assert!(params.get("clientManagedHandoffs").is_none());
    }

    #[test]
    fn realtime_start_params_route_three_calls_by_unique_session_and_keep_late_item_owner() {
        let thread_id = "persistent-native-thread";
        let calls = Mutex::new(HashMap::new());
        let received = AtomicU64::new(1);
        let mut sessions = Vec::new();
        let mut owners = Vec::new();
        for (index, channel) in [201, 202, 203].into_iter().enumerate() {
            let call_id = Uuid::from_u128(index as u128 + 1);
            let params = realtime_start_params(
                thread_id,
                call_id,
                None,
                "offer",
                &realtime_call_start_context(call_id, thread_id, Language::English),
            );
            let session_id = params["realtimeSessionId"].as_str().unwrap().to_owned();
            assert_eq!(session_id, call_id.to_string());
            assert_ne!(session_id, thread_id);
            assert_eq!(params["threadId"], thread_id);
            assert!(!sessions.contains(&session_id));
            sessions.push(session_id.clone());

            let boundary = index as u64 * 10 + 1;
            received.store(boundary, Ordering::Release);
            let call = TranscriptCall::begin(
                &calls,
                call_id,
                thread_id.into(),
                ChannelId::new(channel),
                &received,
            )
            .unwrap();
            let mut start = json!({"method":"thread/realtime/started", "params": {
                "threadId":thread_id, "realtimeSessionId":session_id
            }});
            crate::native::stamp_state_observation(&mut start["params"], 1, boundary + 1);
            assert_eq!(
                call.native_event_owner("thread/realtime/started", &start)
                    .unwrap()
                    .map(|owner| owner.transcript_thread_id),
                Some(ChannelId::new(channel)),
            );
            let item_id = format!("item-{index}");
            let mut item_start = json!({"method":"thread/realtime/item/started", "params": {
                "threadId":thread_id, "item":{"type":"transcriptSegment", "id":item_id,
                    "realtimeSessionId":session_id, "role":"user"}
            }});
            crate::native::stamp_state_observation(&mut item_start["params"], 1, boundary + 2);
            assert_eq!(
                call.native_event_owner("thread/realtime/item/started", &item_start)
                    .unwrap()
                    .map(|owner| owner.transcript_thread_id),
                Some(ChannelId::new(channel)),
            );
            let delta = json!({"method":"thread/realtime/item/transcript/delta", "params": {
                "threadId":thread_id, "itemId":item_id, "delta":"voice words"
            }});
            assert_eq!(
                call.native_event_owner("thread/realtime/item/transcript/delta", &delta)
                    .unwrap()
                    .map(|owner| owner.transcript_thread_id),
                Some(ChannelId::new(channel)),
            );
            owners.push(call);
        }

        for (index, channel) in [201, 202, 203].into_iter().enumerate() {
            let mut completion = json!({"method":"thread/realtime/item/completed", "params": {
                "threadId":thread_id, "item":{"type":"transcriptSegment", "id":format!("item-{index}"),
                    "realtimeSessionId":sessions[index], "role":"user", "text":"completed"}
            }});
            crate::native::stamp_state_observation(&mut completion["params"], 1, 22 + index as u64);
            assert_eq!(
                owners[2]
                    .native_event_owner("thread/realtime/item/completed", &completion)
                    .unwrap()
                    .map(|owner| owner.transcript_thread_id),
                Some(ChannelId::new(channel)),
            );
        }
        assert_eq!(owners[0].native_thread_id, owners[2].native_thread_id);
        assert_eq!(sessions.len(), 3);
    }

    #[test]
    fn late_native_items_keep_their_original_transcript_across_three_voice_calls() {
        let calls = Mutex::new(HashMap::new());
        let received = AtomicU64::new(1);
        let first = TranscriptCall::begin(
            &calls,
            Uuid::new_v4(),
            "persistent-codex-thread".into(),
            ChannelId::new(201),
            &received,
        )
        .unwrap();
        let mut first_start = json!({
            "method":"thread/realtime/started",
            "params": {"threadId":"persistent-codex-thread", "realtimeSessionId":"session-A"}
        });
        crate::native::stamp_state_observation(&mut first_start["params"], 1, 2);
        assert_eq!(
            first
                .native_event_owner("thread/realtime/started", &first_start)
                .unwrap()
                .map(|owner| owner.transcript_thread_id),
            Some(ChannelId::new(201)),
        );
        first.reader_state.store(2, Ordering::Release);

        received.store(10, Ordering::Release);
        let second = TranscriptCall::begin(
            &calls,
            Uuid::new_v4(),
            "persistent-codex-thread".into(),
            ChannelId::new(202),
            &received,
        )
        .unwrap();
        let mut second_start = json!({
            "method":"thread/realtime/started",
            "params": {"threadId":"persistent-codex-thread", "realtimeSessionId":"session-B"}
        });
        crate::native::stamp_state_observation(&mut second_start["params"], 1, 11);
        assert_eq!(
            second
                .native_event_owner("thread/realtime/started", &second_start)
                .unwrap()
                .map(|owner| owner.transcript_thread_id),
            Some(ChannelId::new(202)),
        );
        second.reader_state.store(2, Ordering::Release);

        received.store(20, Ordering::Release);
        let third = TranscriptCall::begin(
            &calls,
            Uuid::new_v4(),
            "persistent-codex-thread".into(),
            ChannelId::new(203),
            &received,
        )
        .unwrap();
        let mut third_start = json!({
            "method":"thread/realtime/started",
            "params": {"threadId":"persistent-codex-thread", "realtimeSessionId":"session-C"}
        });
        crate::native::stamp_state_observation(&mut third_start["params"], 1, 21);
        assert_eq!(
            third
                .native_event_owner("thread/realtime/started", &third_start)
                .unwrap()
                .map(|owner| owner.transcript_thread_id),
            Some(ChannelId::new(203)),
        );

        let mut late_first = json!({
            "method":"item/completed",
            "params": {
                "threadId":"persistent-codex-thread",
                "item":{"type":"agentMessage", "id":"late-A", "realtimeSessionId":"session-A"}
            }
        });
        crate::native::stamp_state_observation(&mut late_first["params"], 1, 22);
        let mut late_second = json!({
            "method":"item/completed",
            "params": {
                "threadId":"persistent-codex-thread",
                "item":{"type":"agentMessage", "id":"late-B", "realtimeSessionId":"session-B"}
            }
        });
        crate::native::stamp_state_observation(&mut late_second["params"], 1, 23);
        let mut current = json!({
            "method":"item/completed",
            "params": {
                "threadId":"persistent-codex-thread",
                "item":{"type":"agentMessage", "id":"current-C", "realtimeSessionId":"session-C"}
            }
        });
        crate::native::stamp_state_observation(&mut current["params"], 1, 24);
        assert_eq!(
            third
                .native_event_owner("item/completed", &late_first)
                .unwrap()
                .map(|owner| owner.transcript_thread_id),
            Some(ChannelId::new(201)),
        );
        assert_eq!(
            third.native_item_owners.lock().unwrap()
                [&("persistent-codex-thread".into(), "late-A".into())]
                .0,
            first.call_instance_id,
        );
        assert_eq!(
            third
                .native_event_owner("item/completed", &late_second)
                .unwrap()
                .map(|owner| owner.transcript_thread_id),
            Some(ChannelId::new(202)),
        );
        assert_eq!(
            third
                .native_event_owner("item/completed", &current)
                .unwrap()
                .map(|owner| owner.transcript_thread_id),
            Some(ChannelId::new(203)),
        );
        let ordinary_text_work = json!({
            "method":"item/completed",
            "params": {
                "threadId":"persistent-codex-thread",
                "item":{"type":"agentMessage", "id":"ordinary-text-work"}
            }
        });
        assert!(
            third
                .native_event_owner("item/completed", &ordinary_text_work)
                .unwrap()
                .is_none()
        );
        assert_eq!(first.native_thread_id, second.native_thread_id);
        assert_eq!(second.native_thread_id, third.native_thread_id);
    }

    #[test]
    fn canonical_first_repeated_deltas_suppress_flat_mirrors_and_keep_the_record() {
        let mut ledger = TranscriptLedger::default();
        ledger.start_canonical("native-user-1", "user");
        let mut partial_text = String::new();
        for delta in ["spoken ", "words", " 🙂"] {
            partial_text.push_str(delta);
            let partial = ledger.partial("native-user-1", delta);
            assert_eq!(partial.text, partial_text);
            assert_eq!(partial.role, "user");
            assert_eq!(partial.status, TranscriptStatus::Partial);
            ledger.bind_discord_message(&partial.source_id, MessageId::new(55));
            ledger.mark_posted(&partial.source_id, transcript_content_pages(&partial));
            assert!(ledger.observe_flat_delta("user", delta).is_none());
            assert_eq!(ledger.items.len(), 1);
        }
        let raw_text = "spoken words.\n  🙂";
        assert!(ledger.observe_flat_done("user", raw_text).is_none());
        assert_eq!(ledger.items.len(), 1);
        let canonical = ledger
            .observe_canonical_completion("native-user-1", "user", raw_text)
            .unwrap();
        assert_eq!(canonical.source_id, "native-user-1");
        assert_eq!(canonical.native_source_id.as_deref(), Some("native-user-1"));
        assert_eq!(canonical.role, "user");
        assert_eq!(canonical.status, TranscriptStatus::Complete);
        assert_eq!(canonical.text, raw_text);
        assert_eq!(canonical.discord_message_ids, vec![MessageId::new(55)]);
        assert_eq!(ledger.items.len(), 1);
        assert!(ledger.pending_flat_done.is_empty());
        let pages = transcript_content_pages(&canonical);
        assert_eq!(pages, vec![format!("**Discord speaker**\n{raw_text}")]);
        ledger.mark_posted(&canonical.source_id, pages);
        assert!(
            ledger
                .observe_canonical_completion("native-user-1", "user", raw_text)
                .is_none()
        );
        assert!(ledger.observe_flat_done("user", raw_text).is_none());
        assert_eq!(ledger.items.len(), 1);
    }

    #[test]
    fn canonical_start_after_flat_delta_preserves_both_known_discord_ids() {
        let mut ledger = TranscriptLedger::default();
        let partial = ledger.observe_flat_delta("user", "hello").unwrap();
        ledger.bind_discord_message(&partial.source_id, MessageId::new(42));
        ledger.mark_posted(&partial.source_id, transcript_content_pages(&partial));
        ledger.start_canonical("native-item-9", "user");
        let native_partial = ledger.partial("native-item-9", "hello");
        ledger.bind_discord_message(&native_partial.source_id, MessageId::new(55));
        ledger.mark_posted(
            &native_partial.source_id,
            transcript_content_pages(&native_partial),
        );
        let raw_text = "hello!\n  🙂";
        let done = ledger.observe_flat_done("user", raw_text).unwrap();
        assert_eq!(done.status, TranscriptStatus::Partial);
        assert_eq!(done.text, raw_text);
        assert_eq!(done.discord_message_ids, vec![MessageId::new(42)]);
        ledger.mark_posted(&done.source_id, transcript_content_pages(&done));
        let canonical = ledger
            .observe_canonical_completion("native-item-9", "user", raw_text)
            .unwrap();
        assert_eq!(canonical.source_id, "native-item-9");
        assert_eq!(canonical.native_source_id.as_deref(), Some("native-item-9"));
        assert_eq!(canonical.role, "user");
        assert_eq!(canonical.status, TranscriptStatus::Complete);
        assert_eq!(canonical.text, raw_text);
        assert_eq!(
            canonical.discord_message_ids,
            vec![MessageId::new(55), MessageId::new(42)]
        );
        assert_eq!(canonical.posted_content, None);
        assert_eq!(ledger.items.len(), 1);
        assert!(ledger.pending_flat_done.is_empty());
        assert_eq!(
            ledger.items[&canonical.source_id].discord_message_ids,
            canonical.discord_message_ids
        );
        assert_eq!(
            transcript_content_pages(&canonical),
            vec![format!("**Discord speaker**\n{raw_text}")]
        );
        assert!(
            ledger
                .observe_canonical_completion("native-item-9", "user", raw_text)
                .is_some()
        );
        ledger.mark_posted(&canonical.source_id, transcript_content_pages(&canonical));
        assert!(
            ledger
                .observe_canonical_completion("native-item-9", "user", raw_text)
                .is_none()
        );
    }

    #[test]
    fn repeated_native_completion_keeps_the_same_text_next_flat_record_pending() {
        let mut ledger = TranscriptLedger::default();
        let raw_text = "hello\n  🙂";
        let partial = ledger.observe_flat_delta("user", "hello").unwrap();
        ledger.bind_discord_message(&partial.source_id, MessageId::new(42));
        let done = ledger.observe_flat_done("user", raw_text).unwrap();
        ledger.mark_posted(&done.source_id, transcript_content_pages(&done));
        let first = ledger
            .observe_canonical_completion("native-item-9", "user", raw_text)
            .unwrap();
        assert_eq!(first.discord_message_ids, vec![MessageId::new(42)]);
        assert!(
            ledger
                .observe_canonical_completion("native-item-9", "user", raw_text)
                .is_some()
        );
        assert!(ledger.pending_flat_done.is_empty());
        ledger.mark_posted(&first.source_id, transcript_content_pages(&first));
        let next_partial = ledger.observe_flat_delta("user", "hello").unwrap();
        ledger.bind_discord_message(&next_partial.source_id, MessageId::new(43));
        let next_done = ledger.observe_flat_done("user", raw_text).unwrap();
        assert_eq!(next_done.source_id, "transcript-event-2");
        assert_eq!(next_done.native_source_id, None);
        assert_eq!(next_done.role, "user");
        assert_eq!(next_done.status, TranscriptStatus::Partial);
        assert_eq!(next_done.text, raw_text);
        assert_eq!(next_done.discord_message_ids, vec![MessageId::new(43)]);
        let pages = transcript_content_pages(&next_done);
        assert_eq!(
            pages,
            vec![format!(
                "**Discord speaker · partial transcript**\n{raw_text}"
            )]
        );
        ledger.mark_posted(&next_done.source_id, pages.clone());
        assert_eq!(
            ledger.items[&next_done.source_id].posted_content,
            Some(pages)
        );
        assert_eq!(ledger.pending_flat_done.len(), 1);
        assert!(
            ledger
                .observe_canonical_completion("native-item-9", "user", raw_text)
                .is_none()
        );
        assert!(
            ledger
                .canonicalize_flat("transcript-event-2", "native-item-9", "user", raw_text)
                .is_none()
        );
        let pending = &ledger.items["transcript-event-2"];
        assert_eq!(pending.native_source_id, None);
        assert_eq!(pending.role, "user");
        assert_eq!(pending.status, TranscriptStatus::Partial);
        assert_eq!(pending.text, raw_text);
        assert_eq!(pending.discord_message_ids, vec![MessageId::new(43)]);
        assert_eq!(ledger.items.len(), 2);
        assert_eq!(ledger.pending_flat_done.len(), 1);
        assert_eq!(
            ledger.items["native-item-9"].discord_message_ids,
            vec![MessageId::new(42)]
        );

        let second = ledger
            .observe_canonical_completion("native-item-10", "user", raw_text)
            .unwrap();
        assert_eq!(second.source_id, "native-item-10");
        assert_eq!(second.native_source_id.as_deref(), Some("native-item-10"));
        assert_eq!(second.role, "user");
        assert_eq!(second.status, TranscriptStatus::Complete);
        assert_eq!(second.text, raw_text);
        assert_eq!(second.discord_message_ids, vec![MessageId::new(43)]);
        assert_eq!(second.posted_content, None);
        assert_eq!(ledger.items.len(), 2);
        assert!(ledger.pending_flat_done.is_empty());
        assert!(!ledger.items.contains_key("transcript-event-2"));
        assert_eq!(
            ledger.items["native-item-9"].discord_message_ids,
            vec![MessageId::new(42)]
        );
        ledger.mark_posted(&second.source_id, transcript_content_pages(&second));
        assert!(
            ledger
                .observe_canonical_completion("native-item-10", "user", raw_text)
                .is_none()
        );
    }

    #[test]
    fn failed_canonical_delivery_remains_retryable_until_content_is_posted() {
        let mut ledger = TranscriptLedger::default();
        let canonical = ledger
            .complete("item-pending", "assistant", "still here")
            .unwrap();
        assert!(
            ledger
                .complete("item-pending", "assistant", "still here")
                .is_some()
        );
        ledger.mark_posted("item-pending", transcript_content_pages(&canonical));
        assert!(
            ledger
                .complete("item-pending", "assistant", "still here")
                .is_none()
        );
    }

    #[tokio::test]
    async fn superseded_reader_survives_a_lost_start_without_admitting_new_call_items() {
        // Exercise the production receiver with and without real broadcast lag,
        // without relying on the close watch being selected first. The ledger lock suspends this
        // reader without replacing the delivery implementation with a mock.
        for overflow in [false, true] {
            let calls = Mutex::new(HashMap::new());
            let old_id = Uuid::new_v4();
            let old = TranscriptCall::begin(
                &calls,
                old_id,
                "T".into(),
                ChannelId::new(10),
                &AtomicU64::new(1),
            )
            .unwrap();
            let mut held = old.ledger.lock().await;
            held.start_canonical("A-open", "user");
            let original = held
                .complete("A-complete", "user", "old raw\n  🙂")
                .unwrap();
            held.bind_discord_message("A-complete", MessageId::new(50));
            held.mark_posted("A-complete", transcript_content_pages(&original));
            assert!(old.accepts_event(&json!({"itemId":"A-open"})).unwrap());
            assert!(old.accepts_event(&json!({"itemId":"A-complete"})).unwrap());
            let (sender, receiver) = broadcast::channel(256);
            let (_cancel, cancel_rx) = watch::channel(false);
            let (closed, closed_rx) = watch::channel(None::<String>);
            let playback = AudioPipe::new().expect("create test audio pipe");
            let task = tokio::spawn(handle_native_realtime_events(
                receiver,
                old.clone(),
                Arc::new(Http::new("")),
                playback.clone(),
                cancel_rx,
                closed_rx,
            ));
            closed.send(Some("A closed".into())).unwrap();
            let new = TranscriptCall::begin(
                &calls,
                Uuid::new_v4(),
                "T".into(),
                ChannelId::new(20),
                &AtomicU64::new(1),
            )
            .unwrap();
            sender
                .send(json!({"method":"thread/realtime/started","params":{"threadId":"T"}}))
                .unwrap();
            if overflow {
                for _ in 0..300 {
                    sender
                        .send(json!({"method":"unrelated","params":{"threadId":"T"}}))
                        .unwrap();
                }
            }
            for event in [
                json!({"method":"thread/realtime/item/started","params":{"threadId":"T","item":{"type":"transcriptSegment","id":"B1","role":"user"}}}),
                json!({"method":"thread/realtime/item/completed","params":{"threadId":"T","item":{"type":"transcriptSegment","id":"B1","role":"user","text":"B raw"}}}),
                json!({"method":"thread/realtime/transcript/done","params":{"threadId":"T","role":"user","text":"B flat"}}),
                json!({"method":"thread/realtime/error","params":{"threadId":"T","message":"B transport failure"}}),
                json!({"method":"thread/realtime/closed","params":{"threadId":"T"}}),
                json!({"method":"thread/realtime/item/started","params":{"threadId":"T","item":{"type":"transcriptSegment","id":"A-open","role":"assistant"}}}),
                json!({"method":"thread/realtime/item/completed","params":{"threadId":"T","item":{"type":"transcriptSegment","id":"A-complete","role":"user","text":"old raw\n  🙂"}}}),
            ] {
                sender.send(event).unwrap();
            }
            drop(sender);
            drop(held);
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let retained = old.ledger.lock().await;
            assert_eq!(retained.items.len(), 2);
            assert!(!retained.items.contains_key("B1"));
            assert!(retained.pending_flat_done.is_empty());
            assert_eq!(retained.items["A-open"].role, "assistant");
            assert_eq!(
                retained.items["A-open"].status,
                TranscriptStatus::Incomplete
            );
            assert_eq!(retained.items["A-complete"].text, "old raw\n  🙂");
            assert_eq!(
                retained.items["A-complete"].discord_message_ids,
                vec![MessageId::new(50)]
            );
            assert!(new.ledger.lock().await.items.is_empty());
            assert_eq!(old.native_thread_id, new.native_thread_id);
            assert_ne!(old.transcript_thread_id, new.transcript_thread_id);
        }
    }

    #[tokio::test]
    async fn closed_reader_keeps_accepting_old_items_before_another_call_begins() {
        let calls = Mutex::new(HashMap::new());
        let call = TranscriptCall::begin(
            &calls,
            Uuid::new_v4(),
            "T".into(),
            ChannelId::new(10),
            &AtomicU64::new(1),
        )
        .unwrap();
        let (sender, receiver) = broadcast::channel(256);
        let (_cancel, cancel_rx) = watch::channel(false);
        let (_closed, closed_rx) = watch::channel(None::<String>);
        sender
            .send(json!({"method":"thread/realtime/closed","params":{"threadId":"T"}}))
            .unwrap();
        sender.send(json!({"method":"thread/realtime/item/started","params":{"threadId":"T","item":{"type":"transcriptSegment","id":"A-late","role":"user"}}})).unwrap();
        drop(sender);
        handle_native_realtime_events(
            receiver,
            call.clone(),
            Arc::new(Http::new("")),
            AudioPipe::new().expect("create test audio pipe"),
            cancel_rx,
            closed_rx,
        )
        .await
        .unwrap();
        let ledger = call.ledger.lock().await;
        assert_eq!(
            ledger.items["A-late"].native_source_id.as_deref(),
            Some("A-late")
        );
        assert_eq!(ledger.items["A-late"].status, TranscriptStatus::Incomplete);
    }

    #[tokio::test]
    async fn failed_complete_delivery_survives_reader_exit_and_rejoin_with_honest_status() {
        let calls = Mutex::new(HashMap::new());
        let call_id = Uuid::new_v4();
        let call = TranscriptCall::begin(
            &calls,
            call_id,
            "T".into(),
            ChannelId::new(10),
            &AtomicU64::new(1),
        )
        .unwrap();
        {
            let mut ledger = call.ledger.lock().await;
            let partial = ledger.partial("X", "partial");
            ledger.bind_discord_message("X", MessageId::new(55));
            ledger.mark_posted("X", transcript_content_pages(&partial));
            ledger.complete("X", "user", "S\n  🙂").unwrap();
            // Feed an explicit PATCH error into the same production retention
            // boundary that receives the actual HTTP result. No HTTP is claimed.
            call.record_delivery(&ledger, "X", &Err(anyhow!("explicit PATCH failure")))
                .unwrap();
        }
        let (sender, receiver) = broadcast::channel(256);
        let (_cancel, cancel_rx) = watch::channel(false);
        let (_closed, closed_rx) = watch::channel(None::<String>);
        drop(sender);
        let task = tokio::spawn(handle_native_realtime_events(
            receiver,
            call.clone(),
            Arc::new(Http::new("")),
            AudioPipe::new().expect("create test audio pipe"),
            cancel_rx,
            closed_rx,
        ));
        let operation = transcript_task_result(task.await);
        assert_eq!(operation.ok, Some(false));
        let successful = VoiceOperationResult::succeeded();
        assert_eq!(
            VoiceLeaveResult::completion_status(
                &successful,
                &successful,
                &successful,
                &operation,
                &successful,
                &VoiceOperationResult::not_attempted(),
            ),
            "partial"
        );
        drop(call);
        let new = TranscriptCall::begin(
            &calls,
            Uuid::new_v4(),
            "T".into(),
            ChannelId::new(20),
            &AtomicU64::new(1),
        )
        .unwrap();
        let retained = calls.lock().unwrap()[&call_id].clone();
        assert_eq!(retained.call_instance_id, call_id);
        assert_eq!(retained.native_thread_id, "T");
        assert_eq!(retained.transcript_thread_id, ChannelId::new(10));
        let failures = retained.failures.lock().unwrap();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].item.source_id, "X");
        assert_eq!(failures[0].item.native_source_id.as_deref(), Some("X"));
        assert_eq!(failures[0].item.text, "S\n  🙂");
        assert_eq!(failures[0].item.status, TranscriptStatus::Complete);
        assert_eq!(
            failures[0].item.discord_message_ids,
            vec![MessageId::new(55)]
        );
        assert_eq!(failures[0].error, "explicit PATCH failure");
        assert!(!failures[0].resolved);
        drop(failures);
        assert_eq!(retained.ledger.lock().await.items["X"].text, "S\n  🙂");
        assert!(new.ledger.lock().await.items.is_empty());
    }

    #[tokio::test]
    async fn successful_known_id_delivery_resolves_failure_without_erasing_its_original_record() {
        let calls = Mutex::new(HashMap::new());
        let call = TranscriptCall::begin(
            &calls,
            Uuid::new_v4(),
            "T".into(),
            ChannelId::new(10),
            &AtomicU64::new(1),
        )
        .unwrap();
        {
            let mut ledger = call.ledger.lock().await;
            let item = ledger.complete("X", "user", "S").unwrap();
            ledger.bind_discord_message("X", MessageId::new(55));
            call.record_delivery(&ledger, "X", &Err(anyhow!("PATCH failed")))
                .unwrap();
            ledger.mark_posted("X", transcript_content_pages(&item));
            call.record_delivery(&ledger, "X", &Ok(())).unwrap();
        }
        let (sender, receiver) = broadcast::channel(256);
        let (_cancel, cancel_rx) = watch::channel(false);
        let (_closed, closed_rx) = watch::channel(None::<String>);
        drop(sender);
        let task = tokio::spawn(handle_native_realtime_events(
            receiver,
            call.clone(),
            Arc::new(Http::new("")),
            AudioPipe::new().expect("create test audio pipe"),
            cancel_rx,
            closed_rx,
        ));
        let operation = transcript_task_result(task.await);
        assert_eq!(operation.ok, Some(true));
        let successful = VoiceOperationResult::succeeded();
        assert_eq!(
            VoiceLeaveResult::completion_status(
                &successful,
                &successful,
                &successful,
                &operation,
                &successful,
                &VoiceOperationResult::not_attempted(),
            ),
            "ended"
        );
        let failures = call.failures.lock().unwrap();
        assert_eq!(failures.len(), 1);
        assert!(failures[0].resolved);
        assert_eq!(failures[0].item.text, "S");
        assert_eq!(
            failures[0].item.discord_message_ids,
            vec![MessageId::new(55)]
        );
    }

    #[tokio::test]
    async fn canonical_promotion_resolves_only_its_failed_flat_source_and_keeps_original_record() {
        for known_id in [true, false] {
            for unrelated_failure in [false, true] {
                let calls = Mutex::new(HashMap::new());
                let call_id = Uuid::new_v4();
                let call = TranscriptCall::begin(
                    &calls,
                    call_id,
                    "T".into(),
                    ChannelId::new(10),
                    &AtomicU64::new(1),
                )
                .unwrap();
                let mut ledger = call.ledger.lock().await;
                let flat = ledger.observe_flat_delta("user", "partial").unwrap();
                if known_id {
                    ledger.bind_discord_message(&flat.source_id, MessageId::new(55));
                    ledger.mark_posted(&flat.source_id, transcript_content_pages(&flat));
                }
                let raw = "same raw\n  🙂";
                let failed = ledger.observe_flat_done("user", raw).unwrap();
                let original_ids = failed.discord_message_ids.clone();
                let original_error = if known_id {
                    "PATCH failed"
                } else {
                    "query or POST failed"
                };
                call.record_delivery(&ledger, &failed.source_id, &Err(anyhow!(original_error)))
                    .unwrap();
                let unrelated = if unrelated_failure {
                    let next = ledger.observe_flat_delta("user", "partial").unwrap();
                    ledger.bind_discord_message(&next.source_id, MessageId::new(99));
                    let done = ledger.observe_flat_done("user", raw).unwrap();
                    call.record_delivery(
                        &ledger,
                        &done.source_id,
                        &Err(anyhow!("unrelated PATCH failed")),
                    )
                    .unwrap();
                    Some(done)
                } else {
                    None
                };
                let canonical = ledger
                    .observe_canonical_completion("native-X", "user", raw)
                    .unwrap();
                assert_eq!(canonical.source_id, "native-X");
                assert_eq!(canonical.native_source_id.as_deref(), Some("native-X"));
                assert_eq!(canonical.discord_message_ids, original_ids);
                assert!(!ledger.items.contains_key(&failed.source_id));
                assert_eq!(ledger.canonical_sources[&failed.source_id], "native-X");
                if !known_id {
                    // Model the returned ID at the production success boundary;
                    // the original failure had no known ID and stays unchanged.
                    ledger.bind_discord_message("native-X", MessageId::new(77));
                }
                ledger.mark_posted("native-X", transcript_content_pages(&canonical));
                // The actual promotion above supplies lineage. HTTP success is
                // supplied to its production retention boundary, not executed.
                call.record_delivery(&ledger, "native-X", &Ok(())).unwrap();
                let history = call.failures.lock().unwrap();
                assert_eq!(history[0].item, failed);
                assert_eq!(history[0].item.native_source_id, None);
                assert_eq!(history[0].item.text, raw);
                assert_eq!(history[0].item.discord_message_ids, original_ids);
                assert_eq!(history[0].error, original_error);
                assert!(history[0].resolved);
                if let Some(unrelated) = unrelated {
                    assert_ne!(unrelated.source_id, failed.source_id);
                    assert_eq!(unrelated.role, failed.role);
                    assert_eq!(unrelated.text, failed.text);
                    assert_eq!(history[1].item, unrelated);
                    assert!(!history[1].resolved);
                    assert!(!ledger.canonical_sources.contains_key(&unrelated.source_id));
                }
                drop(history);
                let operation = match call.delivery_result() {
                    Ok(()) => VoiceOperationResult::succeeded(),
                    Err(error) => VoiceOperationResult::failed(error),
                };
                assert_eq!(operation.ok, Some(!unrelated_failure));
                assert_eq!(operation.ok == Some(true), !unrelated_failure);
                assert_eq!(call.call_instance_id, call_id);
                assert_eq!(call.native_thread_id, "T");
                assert_eq!(call.transcript_thread_id, ChannelId::new(10));
            }
        }
    }

    #[tokio::test]
    async fn partial_delivery_is_suppressed_and_completed_assistant_is_eligible() {
        let calls = Mutex::new(HashMap::new());
        let call = TranscriptCall::begin(
            &calls,
            Uuid::new_v4(),
            "T".into(),
            ChannelId::new(10),
            &AtomicU64::new(1),
        )
        .unwrap();
        let params = json!({"threadId":"T", "itemId":"user-item"});
        let http = Arc::new(Http::new(""));
        let mut ledger = call.ledger.lock().await;
        let partial = ledger.partial("user-item", "draft words");
        deliver_transcript(&http, &call, &mut ledger, &params, false, partial)
            .await
            .unwrap();
        let stored_partial = &ledger.items["user-item"];
        assert_eq!(stored_partial.status, TranscriptStatus::Partial);
        assert_eq!(stored_partial.text, "draft words");
        assert!(stored_partial.discord_message_ids.is_empty());
        assert_eq!(stored_partial.posted_content, None);
        assert!(ledger.post_attempts.get("user-item").is_none());
        assert!(call.failures.lock().unwrap().is_empty());
        assert!(call.delivery_result().is_ok());

        let completed_event = json!({
            "threadId":"T",
            "item":{"type":"agentMessage", "id":"assistant-item", "text":"spoken answer"}
        });
        let completed =
            observe_native_transcript_item(&mut ledger, "item/completed", &completed_event)
                .unwrap();
        assert_eq!(completed.role, "assistant");
        assert_eq!(completed.status, TranscriptStatus::Complete);
        assert_eq!(completed.text, "spoken answer");
        assert!(should_publish_transcript(&completed));
        assert_eq!(ledger.items.len(), 2);
    }

    #[tokio::test]
    async fn new_reader_keeps_known_old_call_items_out_and_accepts_its_own_native_item() {
        let calls = Mutex::new(HashMap::new());
        let old_id = Uuid::new_v4();
        let old = TranscriptCall::begin(
            &calls,
            old_id,
            "T".into(),
            ChannelId::new(10),
            &AtomicU64::new(1),
        )
        .unwrap();
        let (old_sender, old_receiver) = broadcast::channel(256);
        let (_old_cancel, old_cancel_rx) = watch::channel(false);
        let (_old_closed, old_closed_rx) = watch::channel(None::<String>);
        old_sender.send(json!({
            "method":"thread/realtime/item/started",
            "params":{"threadId":"T","item":{"type":"transcriptSegment","id":"A-known","role":"user"}}
        })).unwrap();
        drop(old_sender);
        handle_native_realtime_events(
            old_receiver,
            old.clone(),
            Arc::new(Http::new("")),
            AudioPipe::new().expect("create test audio pipe"),
            old_cancel_rx,
            old_closed_rx,
        )
        .await
        .unwrap();
        let new = TranscriptCall::begin(
            &calls,
            Uuid::new_v4(),
            "T".into(),
            ChannelId::new(20),
            &AtomicU64::new(1),
        )
        .unwrap();
        let (new_sender, new_receiver) = broadcast::channel(256);
        let (_new_cancel, new_cancel_rx) = watch::channel(false);
        let (_new_closed, new_closed_rx) = watch::channel(None::<String>);
        for item in [
            json!({"type":"transcriptSegment","id":"A-known","role":"user"}),
            json!({"type":"transcriptSegment","id":"B-new","role":"user"}),
        ] {
            new_sender
                .send(json!({
                    "method":"thread/realtime/item/started",
                    "params":{"threadId":"T","item":item}
                }))
                .unwrap();
        }
        drop(new_sender);
        // Started-only items have no received text/IDs, so both production
        // cleanup readers perform no HTTP, including the V4 regression source.
        handle_native_realtime_events(
            new_receiver,
            new.clone(),
            Arc::new(Http::new("")),
            AudioPipe::new().expect("create test audio pipe"),
            new_cancel_rx,
            new_closed_rx,
        )
        .await
        .unwrap();
        let new_ledger = new.ledger.lock().await;
        assert_eq!(new_ledger.items.len(), 1);
        assert!(!new_ledger.items.contains_key("A-known"));
        assert_eq!(
            new_ledger.items["B-new"].native_source_id.as_deref(),
            Some("B-new")
        );
        assert_eq!(new_ledger.items["B-new"].role, "user");
        drop(new_ledger);
        let old_ledger = old.ledger.lock().await;
        assert_eq!(old_ledger.items.len(), 1);
        assert_eq!(
            old_ledger.items["A-known"].native_source_id.as_deref(),
            Some("A-known")
        );
        assert_eq!(old_ledger.items["A-known"].role, "user");
        assert_eq!(old_ledger.items["A-known"].text, "");
        assert!(old_ledger.items["A-known"].discord_message_ids.is_empty());
        assert_eq!(old.call_instance_id, old_id);
        assert_eq!(old.native_thread_id, new.native_thread_id);
        assert_ne!(old.transcript_thread_id, new.transcript_thread_id);
    }

    #[tokio::test]
    async fn queued_preboundary_native_items_stay_with_old_reader_and_out_of_new_reader() {
        let calls = Mutex::new(HashMap::new());
        let received = AtomicU64::new(1);
        let old = TranscriptCall::begin(
            &calls,
            Uuid::new_v4(),
            "T".into(),
            ChannelId::new(10),
            &received,
        )
        .unwrap();
        let held = old.ledger.lock().await;
        let (sender, old_receiver) = broadcast::channel(256);
        let (_old_cancel, old_cancel_rx) = watch::channel(false);
        let (_old_closed, old_closed_rx) = watch::channel(None::<String>);
        let old_task = tokio::spawn(handle_native_realtime_events(
            old_receiver,
            old.clone(),
            Arc::new(Http::new("")),
            AudioPipe::new().expect("create test audio pipe"),
            old_cancel_rx,
            old_closed_rx,
        ));
        let mut queued = json!({
            "method":"thread/realtime/item/started",
            "params":{"threadId":"T","item":{"type":"transcriptSegment","id":"A-queued","role":"user"}}
        });
        crate::native::stamp_state_observation(&mut queued["params"], 1, 10);
        sender.send(queued).unwrap();
        // B subscribes after A-queued was published and will never receive it.
        let new_receiver = sender.subscribe();
        let mut prestart = json!({
            "method":"thread/realtime/item/started",
            "params":{"threadId":"T","item":{"type":"transcriptSegment","id":"A-before-B-start","role":"user"}}
        });
        crate::native::stamp_state_observation(&mut prestart["params"], 1, 11);
        sender.send(prestart).unwrap();
        received.store(12, Ordering::Release);
        let new = TranscriptCall::begin(
            &calls,
            Uuid::new_v4(),
            "T".into(),
            ChannelId::new(20),
            &received,
        )
        .unwrap();
        let (_new_cancel, new_cancel_rx) = watch::channel(false);
        let (_new_closed, new_closed_rx) = watch::channel(None::<String>);
        let new_task = tokio::spawn(handle_native_realtime_events(
            new_receiver,
            new.clone(),
            Arc::new(Http::new("")),
            AudioPipe::new().expect("create test audio pipe"),
            new_cancel_rx,
            new_closed_rx,
        ));
        let mut current = json!({
            "method":"thread/realtime/item/started",
            "params":{"threadId":"T","item":{"type":"transcriptSegment","id":"B-current","role":"user"}}
        });
        crate::native::stamp_state_observation(&mut current["params"], 1, 12);
        sender.send(current).unwrap();
        drop(sender);
        drop(held);
        // Started-only empty placeholders keep this actual receiver/cleanup test zero-HTTP.
        old_task.await.unwrap().unwrap();
        new_task.await.unwrap().unwrap();
        let old_ledger = old.ledger.lock().await;
        assert_eq!(old_ledger.items.len(), 2);
        assert_eq!(
            old_ledger.items["A-queued"].native_source_id.as_deref(),
            Some("A-queued")
        );
        assert_eq!(
            old_ledger.items["A-before-B-start"]
                .native_source_id
                .as_deref(),
            Some("A-before-B-start")
        );
        assert!(!old_ledger.items.contains_key("B-current"));
        drop(old_ledger);
        let new_ledger = new.ledger.lock().await;
        assert_eq!(new_ledger.items.len(), 1);
        assert_eq!(
            new_ledger.items["B-current"].native_source_id.as_deref(),
            Some("B-current")
        );
        assert!(!new_ledger.items.contains_key("A-queued"));
        assert!(!new_ledger.items.contains_key("A-before-B-start"));
        let mut original_flat = json!({"threadId":"T","role":"user","text":"queued flat"});
        crate::native::stamp_state_observation(&mut original_flat, 1, 10);
        assert!(old.accepts_event(&original_flat).unwrap());
        assert!(old.accepts_event(&original_flat).unwrap());
        assert!(!new.accepts_event(&original_flat).unwrap());
        received.store(20, Ordering::Release);
        let third = TranscriptCall::begin(
            &calls,
            Uuid::new_v4(),
            "T".into(),
            ChannelId::new(30),
            &received,
        )
        .unwrap();
        let mut pending_b = json!({"threadId":"T","itemId":"B-queued-before-C"});
        crate::native::stamp_state_observation(&mut pending_b, 1, 15);
        assert!(!old.accepts_event(&pending_b).unwrap());
        assert!(new.accepts_event(&pending_b).unwrap());
        assert!(!third.accepts_event(&pending_b).unwrap());
    }

    #[test]
    fn unknown_partial_post_is_recovered_before_canonical_content() {
        let mut ledger = TranscriptLedger::default();
        ledger.start_canonical("X", "user");
        let partial = ledger.partial("X", "hello");
        let original_body = transcript_content_pages(&partial)[0].clone();
        let attempt = ledger.record_post_attempt("X", 0, &original_body);
        let nonce = ledger.post_attempts["X"][attempt].nonce.clone();
        let canonical = ledger.complete("X", "user", "hello").unwrap();
        let current = transcript_content_pages(&canonical);
        assert_ne!(original_body, current[0]);
        ledger.complete("unrelated", "user", "hello").unwrap();
        ledger.bind_discord_message("unrelated", MessageId::new(99));
        assert!(ledger.needs_discord_lookup(&canonical, &current));
        // This is the production GET-result matcher, without executing HTTP.
        ledger
            .recover_discord_messages(
                &canonical,
                &current,
                vec![
                    (
                        MessageId::new(99),
                        original_body.clone(),
                        UserId::new(42),
                        Some(Nonce::String(nonce.clone())),
                    ),
                    (
                        MessageId::new(55),
                        original_body.clone(),
                        UserId::new(42),
                        Some(Nonce::String(nonce.clone())),
                    ),
                ],
                UserId::new(42),
            )
            .unwrap();
        assert_eq!(
            ledger.items["X"].discord_message_ids,
            vec![MessageId::new(55)]
        );
        assert_eq!(ledger.items["X"].native_source_id.as_deref(), Some("X"));
        assert_eq!(ledger.items["X"].text, "hello");
        assert_eq!(
            ledger.items["unrelated"].discord_message_ids,
            vec![MessageId::new(99)]
        );
        assert_eq!(ledger.post_attempts["X"][attempt].content, original_body);
        assert_eq!(ledger.post_attempts["X"][attempt].page, 0);
        assert_eq!(
            ledger.post_attempts["X"][attempt].message_id,
            Some(MessageId::new(55))
        );
        assert!(!ledger.has_pending_posts("X"));

        let mut shrink = TranscriptLedger::default();
        shrink.start_canonical("long", "user");
        let partial = shrink.partial("long", &"a".repeat(2000));
        let original_pages = transcript_content_pages(&partial);
        assert_eq!(original_pages.len(), 2);
        shrink.bind_discord_message("long", MessageId::new(70));
        let extra = shrink.record_post_attempt("long", 1, &original_pages[1]);
        let nonce = shrink.post_attempts["long"][extra].nonce.clone();
        let canonical = shrink.complete("long", "user", "short").unwrap();
        let current = transcript_content_pages(&canonical);
        assert_eq!(current.len(), 1);
        assert_eq!(canonical.discord_message_ids.len(), current.len());
        assert!(shrink.needs_discord_lookup(&canonical, &current));
        shrink
            .recover_discord_messages(
                &canonical,
                &current,
                vec![(
                    MessageId::new(71),
                    original_pages[1].clone(),
                    UserId::new(42),
                    Some(Nonce::String(nonce)),
                )],
                UserId::new(42),
            )
            .unwrap();
        assert_eq!(
            shrink.items["long"].discord_message_ids,
            vec![MessageId::new(70), MessageId::new(71)]
        );
        assert_eq!(
            shrink.items["long"]
                .discord_message_ids
                .iter()
                .skip(current.len())
                .copied()
                .collect::<Vec<_>>(),
            vec![MessageId::new(71)]
        );
        assert_eq!(shrink.post_attempts["long"][extra].page, 1);
        assert_eq!(
            shrink.post_attempts["long"][extra].content,
            original_pages[1]
        );
        assert!(!shrink.has_pending_posts("long"));

        let mut promotion = TranscriptLedger::default();
        let flat = promotion.observe_flat_delta("user", "hello").unwrap();
        let original = transcript_content_pages(&flat)[0].clone();
        let flat_attempt = promotion.record_post_attempt(&flat.source_id, 0, &original);
        let nonce = promotion.post_attempts[&flat.source_id][flat_attempt]
            .nonce
            .clone();
        promotion.observe_flat_done("user", "hello").unwrap();
        let other = promotion.observe_flat_delta("user", "he").unwrap();
        let other_body = transcript_content_pages(&other)[0].clone();
        let other_attempt = promotion.record_post_attempt(&other.source_id, 0, &other_body);
        promotion.observe_flat_delta("user", "llo").unwrap();
        promotion.observe_flat_done("user", "hello").unwrap();
        let canonical = promotion
            .observe_canonical_completion("native-X", "user", "hello")
            .unwrap();
        let current = transcript_content_pages(&canonical);
        assert_eq!(promotion.canonical_sources[&flat.source_id], "native-X");
        promotion
            .recover_discord_messages(
                &canonical,
                &current,
                vec![(
                    MessageId::new(80),
                    original.clone(),
                    UserId::new(42),
                    Some(Nonce::String(nonce)),
                )],
                UserId::new(42),
            )
            .unwrap();
        assert_eq!(
            promotion.items["native-X"].discord_message_ids,
            vec![MessageId::new(80)]
        );
        assert_eq!(
            promotion.post_attempts[&flat.source_id][flat_attempt].content,
            original
        );
        assert_eq!(
            promotion.post_attempts[&flat.source_id][flat_attempt].message_id,
            Some(MessageId::new(80))
        );
        assert_eq!(
            promotion.post_attempts[&other.source_id][other_attempt].message_id,
            None
        );
        assert!(!promotion.has_pending_posts("native-X"));
        assert!(promotion.has_pending_posts(&other.source_id));
    }

    #[tokio::test]
    async fn prestart_old_transport_events_do_not_abort_current_sdp_wait() {
        for method in ["thread/realtime/closed", "thread/realtime/error"] {
            let (sender, mut events) = broadcast::channel(4);
            let mut stale = json!({
                "method":method,
                "params":{"threadId":"T","reason":"old A closed","message":"old A error"}
            });
            crate::native::stamp_state_observation(&mut stale["params"], 1, 11);
            sender.send(stale).unwrap();
            let mut current = json!({
                "method":"thread/realtime/sdp",
                "params":{"threadId":"T","sdp":"B SDP"}
            });
            crate::native::stamp_state_observation(&mut current["params"], 1, 12);
            sender.send(current).unwrap();
            assert_eq!(
                await_realtime_sdp(&mut events, "T", 12).await.unwrap(),
                "B SDP"
            );
        }
    }

    #[test]
    fn recording_failure_keeps_rich_partial_without_blocking_resource_cleanup() {
        let successful = VoiceOperationResult::succeeded();
        for recording in ["delivery", "footer", "summary"] {
            let transcript = if recording == "delivery" {
                VoiceOperationResult::failed("retained transcript delivery failure")
            } else {
                successful.clone()
            };
            let footer = if recording == "footer" {
                VoiceOperationResult::failed("recording footer failure")
            } else {
                successful.clone()
            };
            let summary_update = if recording == "summary" {
                VoiceOperationResult::failed("transcript summary edit failure")
            } else {
                VoiceOperationResult::not_attempted()
            };
            let result = VoiceLeaveResult {
                status: VoiceLeaveResult::completion_status(
                    &successful,
                    &successful,
                    &successful,
                    &transcript,
                    &footer,
                    &summary_update,
                ),
                voice_call_active: false,
                native_thread_id: Some("T".into()),
                native_realtime_stop: successful.clone(),
                peer_connection_close: successful.clone(),
                discord_voice_leave: successful.clone(),
                transcript_delivery: transcript,
                transcript_summary_update: summary_update,
                transcript_footer: footer,
            };
            assert_eq!(result.status, "partial");
            // This is the actual decision consumed by the plain leave wrapper.
            assert!(
                result.cleanup_ok(),
                "recording-only {recording} failure must not gate independent work"
            );
            for resource in ["native", "peer", "discord"] {
                let mut failed = result.clone();
                match resource {
                    "native" => {
                        failed.native_realtime_stop = VoiceOperationResult::failed("stop failed")
                    }
                    "peer" => {
                        failed.peer_connection_close = VoiceOperationResult::failed("close failed")
                    }
                    _ => failed.discord_voice_leave = VoiceOperationResult::failed("leave failed"),
                }
                assert!(
                    !failed.cleanup_ok(),
                    "actual {resource} cleanup failure must still stop the wrapper"
                );
                for status in ["partial", "ended"] {
                    failed.status = status;
                    match resource {
                        "native" => {
                            failed.native_realtime_stop = VoiceOperationResult::not_attempted()
                        }
                        "peer" => {
                            failed.peer_connection_close = VoiceOperationResult::not_attempted()
                        }
                        _ => failed.discord_voice_leave = VoiceOperationResult::not_attempted(),
                    }
                    assert!(
                        !failed.cleanup_ok(),
                        "unknown {resource} cleanup must not count as successful"
                    );
                }
            }
        }
        let ended = VoiceLeaveResult {
            status: "ended",
            voice_call_active: false,
            native_thread_id: Some("T".into()),
            native_realtime_stop: successful.clone(),
            peer_connection_close: successful.clone(),
            discord_voice_leave: successful.clone(),
            transcript_delivery: successful.clone(),
            transcript_summary_update: successful.clone(),
            transcript_footer: successful,
        };
        assert!(ended.cleanup_ok());
        let inactive = VoiceLeaveResult {
            status: "already_ended",
            voice_call_active: false,
            native_thread_id: None,
            native_realtime_stop: VoiceOperationResult::not_attempted(),
            peer_connection_close: VoiceOperationResult::not_attempted(),
            discord_voice_leave: VoiceOperationResult::not_attempted(),
            transcript_delivery: VoiceOperationResult::not_attempted(),
            transcript_summary_update: VoiceOperationResult::not_attempted(),
            transcript_footer: VoiceOperationResult::not_attempted(),
        };
        assert!(inactive.cleanup_ok());
        let mut inactive_failed = inactive.clone();
        inactive_failed.discord_voice_leave = VoiceOperationResult::failed("leave failed");
        assert!(!inactive_failed.cleanup_ok());
        let mut inactive_unknown = inactive;
        inactive_unknown.discord_voice_leave.attempted = true;
        assert!(!inactive_unknown.cleanup_ok());
    }

    #[test]
    fn ambiguous_unknown_posts_do_not_bind_confirm_or_reach_current_delivery() {
        for completed_first in ["Y", "X"] {
            for candidate_count in [1, 2] {
                let mut ledger = TranscriptLedger::default();
                ledger.start_canonical("X", "user");
                let partial_x = ledger.partial("X", "hello");
                let original = transcript_content_pages(&partial_x)[0].clone();
                let attempt_x = ledger.record_post_attempt("X", 0, &original);
                ledger.start_canonical("Y", "user");
                let partial_y = ledger.partial("Y", "hello");
                assert_eq!(transcript_content_pages(&partial_y)[0], original);
                let attempt_y = ledger.record_post_attempt("Y", 0, &original);
                let canonical = ledger
                    .complete(completed_first, "user", &format!("hello {completed_first}"))
                    .unwrap();
                let current = transcript_content_pages(&canonical);
                let candidates = (0..candidate_count)
                    .map(|index| {
                        (
                            MessageId::new(101 + index),
                            original.clone(),
                            UserId::new(42),
                            None,
                        )
                    })
                    .collect();
                // Production matcher Err returns from upsert before its PATCH/POST loops.
                let recovery = ledger.recover_discord_messages(
                    &canonical,
                    &current,
                    candidates,
                    UserId::new(42),
                );
                assert!(
                    recovery.is_err(),
                    "same-body original sources cannot be assigned by Discord ID order"
                );
                assert!(ledger.items["X"].discord_message_ids.is_empty());
                assert!(ledger.items["Y"].discord_message_ids.is_empty());
                assert!(ledger.has_pending_posts("X"));
                assert!(ledger.has_pending_posts("Y"));
                assert_eq!(ledger.post_attempts["X"][attempt_x].content, original);
                assert_eq!(ledger.post_attempts["Y"][attempt_y].content, original);
                assert_eq!(ledger.post_attempts["X"][attempt_x].message_id, None);
                assert_eq!(ledger.post_attempts["Y"][attempt_y].message_id, None);
                assert_eq!(
                    ledger.items[completed_first].text,
                    format!("hello {completed_first}")
                );
            }
        }
        let mut unique = TranscriptLedger::default();
        unique.start_canonical("only", "user");
        let partial = unique.partial("only", "different");
        let original = transcript_content_pages(&partial)[0].clone();
        let attempt = unique.record_post_attempt("only", 0, &original);
        let nonce = unique.post_attempts["only"][attempt].nonce.clone();
        let canonical = unique
            .complete("only", "user", "different complete")
            .unwrap();
        let current = transcript_content_pages(&canonical);
        assert!(
            unique
                .recover_discord_messages(
                    &canonical,
                    &current,
                    vec![
                        (
                            MessageId::new(201),
                            original.clone(),
                            UserId::new(42),
                            Some(Nonce::String(nonce.clone()))
                        ),
                        (
                            MessageId::new(202),
                            original.clone(),
                            UserId::new(42),
                            Some(Nonce::String(nonce.clone()))
                        ),
                    ],
                    UserId::new(42)
                )
                .is_err(),
            "multiple unknown candidate IDs alone do not establish the original post"
        );
        assert!(unique.items["only"].discord_message_ids.is_empty());
        assert_eq!(unique.post_attempts["only"][attempt].message_id, None);
        unique.complete("known", "user", "different").unwrap();
        unique.bind_discord_message("known", MessageId::new(202));
        unique
            .recover_discord_messages(
                &canonical,
                &current,
                vec![
                    (
                        MessageId::new(201),
                        original.clone(),
                        UserId::new(42),
                        Some(Nonce::String(nonce.clone())),
                    ),
                    (
                        MessageId::new(202),
                        original.clone(),
                        UserId::new(42),
                        Some(Nonce::String(nonce)),
                    ),
                ],
                UserId::new(42),
            )
            .unwrap();
        assert_eq!(
            unique.items["only"].discord_message_ids,
            vec![MessageId::new(201)]
        );
        assert_eq!(
            unique.items["known"].discord_message_ids,
            vec![MessageId::new(202)]
        );
        assert_eq!(
            unique.post_attempts["only"][attempt].message_id,
            Some(MessageId::new(201))
        );
        assert!(!unique.has_pending_posts("only"));
    }

    #[tokio::test]
    async fn ended_reader_late_known_item_routes_to_original_ledger_before_current_call_filter() {
        for end in ["closed", "cancelled", "future_dropped"] {
            for current_thread in ["T", "current-T"] {
                let calls = Mutex::new(HashMap::new());
                let received = AtomicU64::new(1);
                let old_id = Uuid::new_v4();
                let old = TranscriptCall::begin(
                    &calls,
                    old_id,
                    "T".into(),
                    ChannelId::new(10),
                    &received,
                )
                .unwrap();
                let (old_sender, old_receiver) = broadcast::channel(8);
                let (old_cancel, old_cancel_rx) = watch::channel(false);
                let (_old_closed, old_closed_rx) = watch::channel(None::<String>);
                old_sender.send(json!({
                    "method":"thread/realtime/item/started",
                    "params":{"threadId":"T","item":{"type":"transcriptSegment","id":"A-known","role":"user"}}
                })).unwrap();
                let mut old_reader = Box::pin(handle_native_realtime_events(
                    old_receiver,
                    old.clone(),
                    Arc::new(Http::new("")),
                    AudioPipe::new().expect("create test audio pipe"),
                    old_cancel_rx,
                    old_closed_rx,
                ));
                if end == "closed" {
                    drop(old_sender);
                    old_reader.await.unwrap();
                } else {
                    assert!(futures_util::poll!(old_reader.as_mut()).is_pending());
                    if end == "cancelled" {
                        old_cancel.send(true).unwrap();
                        old_reader.await.unwrap();
                    } else {
                        // Dropping the actual reader future exercises abort/drop registration cleanup.
                        drop(old_reader);
                    }
                    drop(old_sender);
                }
                let raw = "original A raw\n  🙂";
                {
                    let mut ledger = old.ledger.lock().await;
                    let completed = ledger.complete("A-known", "user", raw).unwrap();
                    ledger.bind_discord_message("A-known", MessageId::new(55));
                    ledger.mark_posted("A-known", transcript_content_pages(&completed));
                }
                received.store(12, Ordering::Release);
                let current = TranscriptCall::begin(
                    &calls,
                    Uuid::new_v4(),
                    current_thread.into(),
                    ChannelId::new(20),
                    &received,
                )
                .unwrap();
                let held_original = old.ledger.lock().await;
                let (sender, receiver) = broadcast::channel(8);
                let (_cancel, cancel_rx) = watch::channel(false);
                let (_closed, closed_rx) = watch::channel(None::<String>);
                let mut late = json!({
                    "method":"thread/realtime/item/completed",
                    "params":{"threadId":"T","item":{"type":"transcriptSegment","id":"A-known","role":"user","text":raw}}
                });
                crate::native::stamp_state_observation(&mut late["params"], 1, 13);
                sender.send(late).unwrap();
                sender.send(json!({
                    "method":"thread/realtime/item/started",
                    "params":{"threadId":current_thread,"item":{"type":"transcriptSegment","id":"B-current","role":"user"}}
                })).unwrap();
                sender.send(json!({
                    "method":"thread/realtime/item/started",
                    "params":{"threadId":"unrelated-T","item":{"type":"transcriptSegment","id":"unknown-other-thread","role":"user"}}
                })).unwrap();
                drop(sender);
                let mut reader = Box::pin(handle_native_realtime_events(
                    receiver,
                    current.clone(),
                    Arc::new(Http::new("")),
                    AudioPipe::new().expect("create test audio pipe"),
                    cancel_rx,
                    closed_rx,
                ));
                // Exact posted canonical duplicate avoids HTTP, but the actual receiver
                // must first wait for A's retained ledger rather than ignore the event.
                assert!(
                    futures_util::poll!(reader.as_mut()).is_pending(),
                    "late known A item must reach its ended owner's ledger"
                );
                drop(held_original);
                reader.await.unwrap();
                let original = old.ledger.lock().await;
                assert_eq!(original.items.len(), 1);
                assert_eq!(
                    original.items["A-known"].native_source_id.as_deref(),
                    Some("A-known")
                );
                assert_eq!(original.items["A-known"].text, raw);
                assert_eq!(original.items["A-known"].status, TranscriptStatus::Complete);
                assert_eq!(
                    original.items["A-known"].discord_message_ids,
                    vec![MessageId::new(55)]
                );
                assert_eq!(old.call_instance_id, old_id);
                assert_eq!(old.native_thread_id, "T");
                assert_eq!(old.transcript_thread_id, ChannelId::new(10));
                drop(original);
                let current_ledger = current.ledger.lock().await;
                assert_eq!(current_ledger.items.len(), 1);
                assert_eq!(
                    current_ledger.items["B-current"]
                        .native_source_id
                        .as_deref(),
                    Some("B-current")
                );
                assert!(!current_ledger.items.contains_key("A-known"));
                assert!(!current_ledger.items.contains_key("unknown-other-thread"));
                assert_eq!(current.transcript_thread_id, ChannelId::new(20));
            }
        }
    }

    #[test]
    fn singleton_unqualified_public_message_does_not_confirm_an_original_post() {
        let calls = Mutex::new(HashMap::new());
        let call_id = Uuid::new_v4();
        let call = TranscriptCall::begin(
            &calls,
            call_id,
            "T".into(),
            ChannelId::new(10),
            &AtomicU64::new(1),
        )
        .unwrap();
        let mut ledger = call.ledger.try_lock().unwrap();
        ledger.start_canonical("X", "user");
        let partial = ledger.partial("X", "hello");
        let original_body = transcript_content_pages(&partial)[0].clone();
        let attempt = ledger.record_post_attempt("X", 0, &original_body);
        let original_nonce = ledger.post_attempts["X"][attempt].nonce.clone();
        assert_eq!(original_nonce.len(), 25);
        assert!(original_nonce.is_ascii());
        // Explicit result injection into the production retention boundary, not HTTP.
        call.record_delivery(&ledger, "X", &Err(anyhow!("original POST outcome unknown")))
            .unwrap();
        let canonical = ledger.complete("X", "user", "hello X").unwrap();
        let current = transcript_content_pages(&canonical);
        for missing in ["nonce", "author", "wrong_nonce", "body"] {
            let author = if missing == "author" {
                UserId::new(43)
            } else {
                UserId::new(42)
            };
            let nonce = match missing {
                "nonce" => None,
                "wrong_nonce" => Some(Nonce::String("different origin".into())),
                _ => Some(Nonce::String(original_nonce.clone())),
            };
            let body = if missing == "body" {
                format!("{original_body} changed")
            } else {
                original_body.clone()
            };
            // This is the actual GET-result matcher with unqualified singleton metadata.
            let recovery = ledger.recover_discord_messages(
                &canonical,
                &current,
                vec![(MessageId::new(301), body, author, nonce)],
                UserId::new(42),
            );
            assert!(
                recovery.is_err(),
                "an unqualified singleton body is not original POST evidence"
            );
            call.record_delivery(&ledger, "X", &recovery).unwrap();
            assert!(ledger.items["X"].discord_message_ids.is_empty());
            assert!(ledger.has_pending_posts("X"));
            assert_eq!(ledger.post_attempts["X"][attempt].message_id, None);
            assert_eq!(ledger.post_attempts["X"][attempt].content, original_body);
            assert_eq!(ledger.post_attempts["X"][attempt].nonce, original_nonce);
            assert_eq!(ledger.items["X"].native_source_id.as_deref(), Some("X"));
            assert_eq!(ledger.items["X"].text, "hello X");
        }
        ledger
            .recover_discord_messages(
                &canonical,
                &current,
                vec![(
                    MessageId::new(301),
                    original_body.clone(),
                    UserId::new(42),
                    Some(Nonce::String(original_nonce.clone())),
                )],
                UserId::new(42),
            )
            .unwrap();
        assert_eq!(
            ledger.items["X"].discord_message_ids,
            vec![MessageId::new(301)]
        );
        assert_eq!(
            ledger.post_attempts["X"][attempt].message_id,
            Some(MessageId::new(301))
        );
        assert_eq!(ledger.post_attempts["X"][attempt].nonce, original_nonce);
        // Origin is proven independently of a later canonical edit failure.
        call.record_delivery(
            &ledger,
            "X",
            &Err(anyhow!("PATCH denied after origin was proven")),
        )
        .unwrap();
        assert!(call.delivery_result().is_err());
        let failures = call.failures.lock().unwrap();
        assert_eq!(failures[0].item.source_id, "X");
        assert_eq!(failures[0].item.native_source_id.as_deref(), Some("X"));
        assert_eq!(failures[0].item.text, "hello");
        assert!(failures[0].item.discord_message_ids.is_empty());
        assert_eq!(failures[0].error, "original POST outcome unknown");
        assert!(!failures[0].resolved);
        let last = failures.last().unwrap();
        assert_eq!(last.item.text, "hello X");
        assert_eq!(last.item.discord_message_ids, vec![MessageId::new(301)]);
        assert_eq!(last.error, "PATCH denied after origin was proven");
        assert!(!last.resolved);
        assert_eq!(call.call_instance_id, call_id);
        assert_eq!(call.native_thread_id, "T");
        assert_eq!(call.transcript_thread_id, ChannelId::new(10));
        drop(failures);
        drop(ledger);

        let mut distinct = TranscriptLedger::default();
        distinct.start_canonical("X", "user");
        let x = distinct.partial("X", "hello");
        let body = transcript_content_pages(&x)[0].clone();
        let x_attempt = distinct.record_post_attempt("X", 0, &body);
        distinct.start_canonical("Y", "user");
        distinct.partial("Y", "hello");
        let y_attempt = distinct.record_post_attempt("Y", 0, &body);
        let x_nonce = distinct.post_attempts["X"][x_attempt].nonce.clone();
        let y_nonce = distinct.post_attempts["Y"][y_attempt].nonce.clone();
        assert_ne!(x_nonce, y_nonce);
        let x = distinct.complete("X", "user", "hello X").unwrap();
        let y = distinct.complete("Y", "user", "hello Y").unwrap();
        let found = vec![
            (
                MessageId::new(401),
                body.clone(),
                UserId::new(42),
                Some(Nonce::String(x_nonce.clone())),
            ),
            (
                MessageId::new(402),
                body.clone(),
                UserId::new(42),
                Some(Nonce::String(y_nonce.clone())),
            ),
        ];
        distinct
            .recover_discord_messages(
                &y,
                &transcript_content_pages(&y),
                found.clone(),
                UserId::new(42),
            )
            .unwrap();
        assert_eq!(
            distinct.items["Y"].discord_message_ids,
            vec![MessageId::new(402)]
        );
        assert!(distinct.items["X"].discord_message_ids.is_empty());
        distinct
            .recover_discord_messages(&x, &transcript_content_pages(&x), found, UserId::new(42))
            .unwrap();
        assert_eq!(
            distinct.items["X"].discord_message_ids,
            vec![MessageId::new(401)]
        );
        assert_eq!(distinct.post_attempts["X"][x_attempt].nonce, x_nonce);
        assert_eq!(distinct.post_attempts["Y"][y_attempt].nonce, y_nonce);

        let mut collision = TranscriptLedger::default();
        collision.start_canonical("X", "user");
        collision.partial("X", "hello");
        let x_attempt = collision.record_post_attempt("X", 0, &body);
        // Supply two original targets with the same request nonce/body as a collision fixture.
        let duplicate_attempt = collision.post_attempts["X"][x_attempt].clone();
        let duplicate_nonce = duplicate_attempt.nonce.clone();
        collision.start_canonical("Y", "user");
        collision.partial("Y", "hello");
        collision
            .post_attempts
            .entry("Y".into())
            .or_default()
            .push(duplicate_attempt);
        let x = collision.complete("X", "user", "hello X").unwrap();
        assert!(
            collision
                .recover_discord_messages(
                    &x,
                    &transcript_content_pages(&x),
                    vec![(
                        MessageId::new(501),
                        body.clone(),
                        UserId::new(42),
                        Some(Nonce::String(duplicate_nonce))
                    ),],
                    UserId::new(42)
                )
                .is_err()
        );
        assert!(collision.items["X"].discord_message_ids.is_empty());
        assert!(collision.items["Y"].discord_message_ids.is_empty());
        assert!(collision.has_pending_posts("X"));
        assert!(collision.has_pending_posts("Y"));

        let mut pages = TranscriptLedger::default();
        pages.start_canonical("two", "user");
        let partial = pages.partial("two", &"a".repeat(2000));
        let original_pages = transcript_content_pages(&partial);
        assert_eq!(original_pages.len(), 2);
        let first = pages.record_post_attempt("two", 0, &original_pages[0]);
        let second = pages.record_post_attempt("two", 1, &original_pages[1]);
        let nonce = pages.post_attempts["two"][first].nonce.clone();
        let current = pages.complete("two", "user", "canonical").unwrap();
        // A valid first page must not mutate the ledger before a missing second
        // target fails the all-target origin preflight.
        assert!(
            pages
                .recover_discord_messages(
                    &current,
                    &transcript_content_pages(&current),
                    vec![(
                        MessageId::new(601),
                        original_pages[0].clone(),
                        UserId::new(42),
                        Some(Nonce::String(nonce))
                    ),],
                    UserId::new(42)
                )
                .is_err()
        );
        assert!(pages.items["two"].discord_message_ids.is_empty());
        assert_eq!(pages.post_attempts["two"][first].message_id, None);
        assert_eq!(pages.post_attempts["two"][second].message_id, None);
    }

    #[test]
    fn only_canonical_completed_transcripts_are_publishable() {
        let mut ledger = TranscriptLedger::default();
        let delta = json!({
            "itemId": "native-item-1",
            "delta": "partial words",
        });
        let partial = observe_native_transcript_item(
            &mut ledger,
            "thread/realtime/item/transcript/delta",
            &delta,
        )
        .unwrap();
        assert_eq!(partial.status, TranscriptStatus::Partial);
        assert_eq!(partial.text, "partial words");
        assert!(!should_publish_transcript(&partial));
        assert!(ledger.items["native-item-1"].discord_message_ids.is_empty());
        assert_eq!(ledger.items["native-item-1"].posted_content, None);

        let completed_event = json!({
            "item": {
                "type": "transcriptSegment",
                "id": "native-item-1",
                "role": "user",
                "text": "canonical words",
            },
        });
        let completed = observe_native_transcript_item(
            &mut ledger,
            "thread/realtime/item/completed",
            &completed_event,
        )
        .unwrap();
        assert_eq!(completed.status, TranscriptStatus::Complete);
        assert_eq!(completed.text, "canonical words");
        assert!(should_publish_transcript(&completed));
        ledger.mark_posted("native-item-1", transcript_content_pages(&completed));
        assert_eq!(
            ledger.items["native-item-1"].posted_content,
            Some(vec!["**Discord speaker**\ncanonical words".to_owned()])
        );

        assert!(
            observe_native_transcript_item(
                &mut ledger,
                "thread/realtime/item/completed",
                &completed_event,
            )
            .is_none()
        );
        assert_eq!(ledger.items.len(), 1);
    }

    #[test]
    fn flat_done_remains_unpublished_and_close_notice_requires_nonempty_partial_text() {
        let mut ledger = TranscriptLedger::default();
        let delta = ledger.observe_flat_delta("user", "unfin").unwrap();
        assert_eq!(delta.status, TranscriptStatus::Partial);
        let done = ledger.observe_flat_done("user", "unfinished").unwrap();
        assert_eq!(done.status, TranscriptStatus::Partial);
        assert!(!should_publish_transcript(&done));

        let incomplete = ledger.mark_open_items_incomplete();
        assert_eq!(incomplete.len(), 1);
        assert_eq!(incomplete[0].status, TranscriptStatus::Incomplete);
        assert!(has_nonempty_incomplete_transcript(&incomplete));

        let mut empty_ledger = TranscriptLedger::default();
        empty_ledger.start_canonical("empty-item", "user");
        let empty = empty_ledger.mark_open_items_incomplete();
        assert_eq!(empty.len(), 1);
        assert_eq!(empty[0].text, "");
        assert!(!has_nonempty_incomplete_transcript(&empty));
    }

    #[test]
    fn incomplete_transcript_remains_identified_when_a_call_ends() {
        let mut ledger = TranscriptLedger::default();
        ledger.partial("item-1", "unfinished");
        let incomplete = ledger.mark_open_items_incomplete();
        assert_eq!(incomplete.len(), 1);
        assert_eq!(incomplete[0].source_id, "item-1");
        assert_eq!(incomplete[0].status, TranscriptStatus::Incomplete);
    }

    #[test]
    fn native_interrupt_event_recognizes_cancelled_output_and_ignores_normal_events() {
        assert!(is_native_audio_interrupt(
            br#"{"type":"response.cancelled"}"#
        ));
        assert!(is_native_audio_interrupt(
            br#"{"type":"response.done","response":{"status":"cancelled"}}"#
        ));
        assert!(!is_native_audio_interrupt(
            br#"{"type":"response.audio.delta"}"#
        ));
        assert!(!is_native_audio_interrupt(b"not json"));
    }

    #[tokio::test]
    async fn sdp_wait_keeps_known_original_items_for_the_actual_recording_receiver() {
        for startup_ok in [true, false] {
            for current_thread in ["T", "current-T"] {
                let calls = Mutex::new(HashMap::new());
                let received = AtomicU64::new(1);
                let old = TranscriptCall::begin(
                    &calls,
                    Uuid::new_v4(),
                    "T".into(),
                    ChannelId::new(10),
                    &received,
                )
                .unwrap();
                let (old_sender, old_events) = broadcast::channel(2);
                old_sender.send(json!({"method":"thread/realtime/item/started","params":{"threadId":"T","item":{"type":"transcriptSegment","id":"A-known","role":"user"}}})).unwrap();
                drop(old_sender);
                let (_cancel, cancelled) = watch::channel(false);
                let (_closed, closed) = watch::channel(None::<String>);
                handle_native_realtime_events(
                    old_events,
                    old.clone(),
                    Arc::new(Http::new("")),
                    AudioPipe::new().expect("create test audio pipe"),
                    cancelled,
                    closed,
                )
                .await
                .unwrap();
                let raw = "original A canonical\n  🙂";
                {
                    let mut ledger = old.ledger.lock().await;
                    let item = ledger.complete("A-known", "user", raw).unwrap();
                    ledger.bind_discord_message("A-known", MessageId::new(55));
                    ledger.mark_posted("A-known", transcript_content_pages(&item));
                }
                received.store(12, Ordering::Release);
                let current = TranscriptCall::begin(
                    &calls,
                    Uuid::new_v4(),
                    current_thread.into(),
                    ChannelId::new(20),
                    &received,
                )
                .unwrap();
                let held_original = old.ledger.lock().await;
                let (sender, mut events) = broadcast::channel(8);
                let recording = StartupTranscriptEvents {
                    events: Some(events.resubscribe()),
                    calls: &calls,
                    committed_call: Some(current.clone()),
                };
                let mut late = json!({"method":"thread/realtime/item/completed","params":{"threadId":"T","item":{"type":"transcriptSegment","id":"A-known","role":"user","text":raw}}});
                crate::native::stamp_state_observation(&mut late["params"], 1, 11);
                sender.send(late).unwrap();
                sender.send(json!({"method":"thread/realtime/item/started","params":{"threadId":current_thread,"item":{"type":"transcriptSegment","id":"B-current","role":"user"}}})).unwrap();
                let mut result = if startup_ok {
                    json!({"method":"thread/realtime/sdp","params":{"threadId":current_thread,"sdp":"B SDP"}})
                } else {
                    json!({"method":"thread/realtime/error","params":{"threadId":current_thread,"message":"genuine B startup error"}})
                };
                crate::native::stamp_state_observation(&mut result["params"], 1, 12);
                sender.send(result).unwrap();
                drop(sender);
                let answer =
                    await_realtime_sdp(&mut events, current_thread, current.start_order).await;
                let http = Arc::new(Http::new(""));
                if startup_ok {
                    assert_eq!(answer.unwrap(), "B SDP");
                    let (_cancel, cancelled) = watch::channel(false);
                    let (_closed, closed) = watch::channel(None::<String>);
                    let mut reader = Box::pin(handle_native_realtime_events(
                        recording.into_receiver(),
                        current.clone(),
                        http,
                        AudioPipe::new().expect("create test audio pipe"),
                        cancelled,
                        closed,
                    ));
                    // Exact posted duplicate avoids HTTP. The production recording
                    // receiver must reach A's original ledger despite SDP/prestart filtering.
                    assert!(
                        futures_util::poll!(reader.as_mut()).is_pending(),
                        "SDP wait must preserve the original known owner event"
                    );
                    drop(held_original);
                    reader.await.unwrap();
                    let ledger = current.ledger.lock().await;
                    assert_eq!(ledger.items.len(), 1);
                    assert!(ledger.items.contains_key("B-current"));
                    assert!(!ledger.items.contains_key("A-known"));
                } else {
                    assert!(
                        format!("{:#}", answer.unwrap_err()).contains("genuine B startup error")
                    );
                    drop(recording);
                    assert_eq!(current.reader_state.load(Ordering::Acquire), 2);
                    let queued = old
                        .pending_native_events
                        .lock()
                        .unwrap()
                        .front()
                        .cloned()
                        .unwrap();
                    assert_eq!(queued["params"]["item"]["text"], raw);
                    assert_eq!(
                        crate::native::state_observation_order(&queued["params"]),
                        Some((1, 11))
                    );
                    let mut recovery = Box::pin(drain_retained_startup_events(&calls, &http));
                    assert!(futures_util::poll!(recovery.as_mut()).is_pending());
                    // Cancelling the actual drain while its original ledger is
                    // held must retain the front; another owned caller can resume it.
                    drop(recovery);
                    assert_eq!(old.pending_native_events.lock().unwrap().len(), 1);
                    drop(held_original);
                    drain_retained_startup_events(&calls, &http).await.unwrap();
                    assert!(old.pending_native_events.lock().unwrap().is_empty());
                    assert!(current.ledger.lock().await.items.is_empty());
                }
                let ledger = old.ledger.lock().await;
                assert_eq!(ledger.items["A-known"].text, raw);
                assert_eq!(
                    ledger.items["A-known"].discord_message_ids,
                    vec![MessageId::new(55)]
                );
                assert_eq!(old.transcript_thread_id, ChannelId::new(10));
            }
        }
        // The exact production parser appends a stamped delta once, not once
        // per broadcast subscriber; a distinct lower order is not a highwater casualty.
        let mut ledger = TranscriptLedger::default();
        ledger.start_canonical("delta-X", "user");
        let mut delta = json!({"threadId":"T","itemId":"delta-X","delta":"ab"});
        crate::native::stamp_state_observation(&mut delta, 1, 20);
        observe_native_transcript_item(
            &mut ledger,
            "thread/realtime/item/transcript/delta",
            &delta,
        )
        .unwrap();
        observe_native_transcript_item(
            &mut ledger,
            "thread/realtime/item/transcript/delta",
            &delta,
        )
        .unwrap();
        assert_eq!(ledger.items["delta-X"].text, "ab");
        let mut earlier_distinct = json!({"threadId":"T","itemId":"delta-X","delta":"c"});
        crate::native::stamp_state_observation(&mut earlier_distinct, 1, 19);
        observe_native_transcript_item(
            &mut ledger,
            "thread/realtime/item/transcript/delta",
            &earlier_distinct,
        )
        .unwrap();
        assert_eq!(ledger.items["delta-X"].text, "abc");
        assert_eq!(ledger.processed_native_observations.len(), 2);
    }

    #[tokio::test]
    async fn blocked_native_writer_does_not_begin_a_call_before_its_start_frame() {
        struct GatedWriter {
            ready: Arc<AtomicBool>,
            flushed: Arc<AtomicBool>,
            sent: Arc<AtomicBool>,
            early_events: broadcast::Sender<Value>,
        }
        impl futures_util::Sink<tokio_tungstenite::tungstenite::Message> for GatedWriter {
            type Error = std::convert::Infallible;
            fn poll_ready(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Self::Error>> {
                if self.ready.load(Ordering::Acquire) {
                    std::task::Poll::Ready(Ok(()))
                } else {
                    std::task::Poll::Pending
                }
            }
            fn start_send(
                self: std::pin::Pin<&mut Self>,
                _: tokio_tungstenite::tungstenite::Message,
            ) -> Result<(), Self::Error> {
                self.sent.store(true, Ordering::Release);
                let mut sdp = json!({"method":"thread/realtime/sdp","params":{"threadId":"T","sdp":"early B SDP"}});
                crate::native::stamp_state_observation(&mut sdp["params"], 2, 12);
                self.early_events.send(sdp).unwrap();
                Ok(())
            }
            fn poll_flush(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Self::Error>> {
                if self.flushed.load(Ordering::Acquire) {
                    std::task::Poll::Ready(Ok(()))
                } else {
                    std::task::Poll::Pending
                }
            }
            fn poll_close(
                self: std::pin::Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<Result<(), Self::Error>> {
                std::task::Poll::Ready(Ok(()))
            }
        }
        let calls = Mutex::new(HashMap::new());
        let received = AtomicU64::new(1);
        let old = TranscriptCall::begin(
            &calls,
            Uuid::new_v4(),
            "T".into(),
            ChannelId::new(10),
            &received,
        )
        .unwrap();
        let (sender, mut events) = broadcast::channel(8);
        let ready = Arc::new(AtomicBool::new(false));
        let flushed = Arc::new(AtomicBool::new(false));
        let sent = Arc::new(AtomicBool::new(false));
        let writer = AsyncMutex::new(Some(GatedWriter {
            ready: ready.clone(),
            flushed: flushed.clone(),
            sent: sent.clone(),
            early_events: sender.clone(),
        }));
        let held_writer = writer.lock().await;
        let current_id = Uuid::new_v4();
        let mut send = Box::pin(crate::native::send_json_with_commit(
            &writer,
            json!({"method":"thread/realtime/start"}),
            &received,
            |fence| {
                TranscriptCall::begin_with_generation(
                    &calls,
                    current_id,
                    "T".into(),
                    ChannelId::new(20),
                    fence,
                    Some(2),
                )
            },
        ));
        assert!(futures_util::poll!(send.as_mut()).is_pending());
        assert_eq!(
            calls.lock().unwrap().len(),
            1,
            "a blocked writer must not create B or supersede A"
        );
        assert!(!old.superseded.load(Ordering::Acquire));
        drop(held_writer);
        assert!(futures_util::poll!(send.as_mut()).is_pending());
        assert_eq!(
            calls.lock().unwrap().len(),
            1,
            "sink readiness must precede the boundary too"
        );
        let mut stale = json!({"method":"thread/realtime/closed","params":{"threadId":"T","reason":"A ended before B frame"}});
        crate::native::stamp_state_observation(&mut stale["params"], 1, 11);
        sender.send(stale).unwrap();
        for method in [
            "thread/realtime/error",
            "thread/realtime/closed",
            "thread/realtime/sdp",
        ] {
            let mut old_generation = json!({"method":method,"params":{"threadId":"T","message":"old generation error","reason":"old generation close","sdp":"wrong old SDP"}});
            crate::native::stamp_state_observation(&mut old_generation["params"], 1, 99);
            sender.send(old_generation).unwrap();
        }
        received.store(12, Ordering::Release);
        ready.store(true, Ordering::Release);
        assert!(futures_util::poll!(send.as_mut()).is_pending());
        assert!(sent.load(Ordering::Acquire));
        let current = calls.lock().unwrap()[&current_id].clone();
        assert_eq!(current.start_order, 12);
        assert_eq!(current.start_generation, Some(2));
        assert!(old.superseded.load(Ordering::Acquire));
        // The sink emits B's SDP before flush completes; subscribing before the
        // commit preserves it while excluding old-generation higher-order events.
        assert_eq!(
            await_realtime_sdp_for_connection(
                &mut events,
                "T",
                current.start_order,
                current.start_generation
            )
            .await
            .unwrap(),
            "early B SDP"
        );
        flushed.store(true, Ordering::Release);
        assert!(Arc::ptr_eq(&send.await.unwrap(), &current));
        let mut genuine = json!({"method":"thread/realtime/error","params":{"threadId":"T","message":"genuine current error"}});
        crate::native::stamp_state_observation(&mut genuine["params"], 2, 13);
        sender.send(genuine).unwrap();
        assert!(
            format!(
                "{:#}",
                await_realtime_sdp_for_connection(
                    &mut events,
                    "T",
                    current.start_order,
                    current.start_generation
                )
                .await
                .unwrap_err()
            )
            .contains("genuine current error")
        );
    }

    #[tokio::test]
    async fn dropped_recording_receiver_keeps_new_original_canonical_before_ledger_wait() {
        let calls = Mutex::new(HashMap::new());
        let received = AtomicU64::new(1);
        let old_id = Uuid::new_v4();
        let old = TranscriptCall::begin(&calls, old_id, "T".into(), ChannelId::new(10), &received)
            .unwrap();
        let (old_sender, old_events) = broadcast::channel(2);
        old_sender.send(json!({"method":"thread/realtime/item/started","params":{"threadId":"T","item":{"type":"transcriptSegment","id":"A-known","role":"user"}}})).unwrap();
        drop(old_sender);
        let (_cancel, cancelled) = watch::channel(false);
        let (_closed, closed) = watch::channel(None::<String>);
        handle_native_realtime_events(
            old_events,
            old.clone(),
            Arc::new(Http::new("")),
            AudioPipe::new().expect("create test audio pipe"),
            cancelled,
            closed,
        )
        .await
        .unwrap();
        {
            let mut ledger = old.ledger.lock().await;
            ledger.items.get_mut("A-known").unwrap().status = TranscriptStatus::Partial;
            ledger.partial("A-known", "original partial P");
            ledger.bind_discord_message("A-known", MessageId::new(55));
            old.record_delivery(&ledger, "A-known", &Err(anyhow!("original PATCH denied")))
                .unwrap();
        }
        received.store(12, Ordering::Release);
        let current = TranscriptCall::begin(
            &calls,
            Uuid::new_v4(),
            "T".into(),
            ChannelId::new(20),
            &received,
        )
        .unwrap();
        let held_original = old.ledger.lock().await;
        let (sender, events) = broadcast::channel(2);
        let raw = "NEW canonical A S\n  🙂";
        let mut late = json!({"method":"thread/realtime/item/completed","params":{"threadId":"T","item":{"type":"transcriptSegment","id":"A-known","role":"user","text":raw}}});
        crate::native::stamp_state_observation(&mut late["params"], 1, 13);
        sender.send(late.clone()).unwrap();
        let (_cancel, cancelled) = watch::channel(false);
        let (_closed, closed) = watch::channel(None::<String>);
        let mut reader = Box::pin(handle_native_realtime_events(
            events,
            current.clone(),
            Arc::new(Http::new("")),
            AudioPipe::new().expect("create test audio pipe"),
            cancelled,
            closed,
        ));
        assert!(futures_util::poll!(reader.as_mut()).is_pending());
        drop(reader);
        assert_eq!(
            old.pending_native_events.lock().unwrap().front().cloned(),
            Some(late),
            "received new canonical must survive B drop before A's ledger is acquired"
        );
        assert_eq!(held_original.items["A-known"].text, "original partial P");
        assert_eq!(
            held_original.items["A-known"].discord_message_ids,
            vec![MessageId::new(55)]
        );
        let failures = old.failures.lock().unwrap();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].item.text, "original partial P");
        assert_eq!(
            failures[0].item.discord_message_ids,
            vec![MessageId::new(55)]
        );
        assert!(failures[0].error.contains("original PATCH denied"));
        assert!(!failures[0].resolved);
        assert_eq!(old.call_instance_id, old_id);
        assert_eq!(old.transcript_thread_id, ChannelId::new(10));
        drop(failures);
        drop(held_original);
        assert!(current.ledger.lock().await.items.is_empty());
        // Do not resume a changed-body canonical here: that would issue real HTTP.
    }

    #[tokio::test]
    async fn explicit_original_session_keeps_unseen_canonical_out_of_current_call() {
        for evidence in ["started", "canonical_control"] {
            let calls = Mutex::new(HashMap::new());
            let received = AtomicU64::new(1);
            let old_id = Uuid::new_v4();
            let old =
                TranscriptCall::begin(&calls, old_id, "T".into(), ChannelId::new(10), &received)
                    .unwrap();
            let (old_sender, old_events) = broadcast::channel(4);
            let control = if evidence == "started" {
                json!({"method":"thread/realtime/started","params":{"threadId":"T","realtimeSessionId":"SA"}})
            } else {
                json!({"method":"thread/realtime/item/started","params":{"threadId":"T","item":{"type":"realtimeSessionStarted","id":"A-session-control","realtimeSessionId":"SA"}}})
            };
            old_sender.send(control).unwrap();
            drop(old_sender);
            let (_cancel, cancelled) = watch::channel(false);
            let (_closed, closed) = watch::channel(None::<String>);
            handle_native_realtime_events(
                old_events,
                old.clone(),
                Arc::new(Http::new("")),
                AudioPipe::new().expect("create test audio pipe"),
                cancelled,
                closed,
            )
            .await
            .unwrap();
            received.store(12, Ordering::Release);
            let current = TranscriptCall::begin(
                &calls,
                Uuid::new_v4(),
                "T".into(),
                ChannelId::new(20),
                &received,
            )
            .unwrap();
            let (sender, events) = broadcast::channel(4);
            sender.send(json!({"method":"thread/realtime/started","params":{"threadId":"T","realtimeSessionId":"SB"}})).unwrap();
            sender.send(json!({"method":"thread/realtime/item/started","params":{"threadId":"T","item":{"type":"transcriptSegment","id":"B-current","realtimeSessionId":"SB","role":"user"}}})).unwrap();
            let (_cancel, cancelled) = watch::channel(false);
            let (_closed, closed) = watch::channel(None::<String>);
            let mut reader = Box::pin(handle_native_realtime_events(
                events,
                current.clone(),
                Arc::new(Http::new("")),
                AudioPipe::new().expect("create test audio pipe"),
                cancelled,
                closed,
            ));
            assert!(futures_util::poll!(reader.as_mut()).is_pending());
            let held_original = old.ledger.lock().await;
            let held_current = current.ledger.lock().await;
            let raw = "previous SA new canonical X\n  🙂";
            let mut late = json!({"method":"thread/realtime/item/completed","params":{"threadId":"T","item":{"type":"transcriptSegment","id":"A-unseen","realtimeSessionId":"SA","role":"user","text":raw}}});
            crate::native::stamp_state_observation(&mut late["params"], 1, 13);
            sender.send(late.clone()).unwrap();
            assert!(futures_util::poll!(reader.as_mut()).is_pending());
            assert_eq!(
                old.native_item_owners.lock().unwrap()[&("T".into(), "A-unseen".into())].0,
                old_id,
                "explicit observed SA must not let B claim the unseen A item"
            );
            drop(reader);
            assert_eq!(
                old.pending_native_events.lock().unwrap().front().cloned(),
                Some(late)
            );
            assert_eq!(held_current.items.len(), 1);
            assert!(held_current.items.contains_key("B-current"));
            assert!(!held_current.items.contains_key("A-unseen"));
            assert!(!held_original.items.contains_key("A-unseen"));
            assert_eq!(old.transcript_thread_id, ChannelId::new(10));
            assert_eq!(current.transcript_thread_id, ChannelId::new(20));
            // The exact queued new raw is preserved; known-origin POST/PATCH is unexecuted.
        }
        let calls = Mutex::new(HashMap::new());
        let received = AtomicU64::new(12);
        let current_id = Uuid::new_v4();
        let current = TranscriptCall::begin(
            &calls,
            current_id,
            "T".into(),
            ChannelId::new(20),
            &received,
        )
        .unwrap();
        let (sender, events) = broadcast::channel(4);
        let (_cancel, cancelled) = watch::channel(false);
        let (_closed, closed) = watch::channel(None::<String>);
        let http = Arc::new(Http::new(""));
        let mut reader = Box::pin(handle_native_realtime_events(
            events,
            current.clone(),
            http,
            AudioPipe::new().expect("create test audio pipe"),
            cancelled,
            closed,
        ));
        let mut unknown = json!({"method":"thread/realtime/item/completed","params":{"threadId":"T","item":{"type":"transcriptSegment","id":"unknown-X","realtimeSessionId":"SU","role":"user","text":"unassigned raw S\n  🙂"}}});
        crate::native::stamp_state_observation(&mut unknown["params"], 1, 13);
        sender.send(unknown.clone()).unwrap();
        sender.send(unknown.clone()).unwrap();
        assert!(futures_util::poll!(reader.as_mut()).is_pending());
        assert!(current.delivery_result().is_err());
        assert!(current.ledger.lock().await.items.is_empty());
        {
            let history = current.native_session_history.lock().unwrap();
            assert_eq!(
                history.unassigned.len(),
                1,
                "one stamped observation has one retained unknown record"
            );
            assert_eq!(history.unassigned[0].event, unknown);
            assert_eq!(history.unassigned[0].observed_by, current_id);
            assert_eq!(
                history.unassigned[0].observed_transcript_thread_id,
                ChannelId::new(20)
            );
            assert_eq!(history.unassigned[0].assigned_owner, None);
        }
        let held_current = current.ledger.lock().await;
        let mut control = json!({"method":"thread/realtime/item/started","params":{"threadId":"T","item":{"type":"realtimeSessionStarted","id":"SU-control","realtimeSessionId":"SU"}}});
        crate::native::stamp_state_observation(&mut control["params"], 1, 14);
        sender.send(control).unwrap();
        assert!(futures_util::poll!(reader.as_mut()).is_pending());
        drop(reader);
        let history = current.native_session_history.lock().unwrap();
        assert_eq!(history.unassigned.len(), 1);
        assert_eq!(
            history.unassigned[0].event, unknown,
            "assignment never rewrites the observed original raw"
        );
        assert_eq!(history.unassigned[0].assigned_owner, Some(current_id));
        assert_eq!(
            history.unassigned[0].observed_transcript_thread_id,
            ChannelId::new(20)
        );
        drop(history);
        assert_eq!(
            current
                .pending_native_events
                .lock()
                .unwrap()
                .front()
                .cloned(),
            Some(unknown)
        );
        assert_eq!(
            current.native_item_owners.lock().unwrap()[&("T".into(), "unknown-X".into())].0,
            current_id
        );
        assert!(
            current.delivery_result().is_err(),
            "assigned metadata is not completed HTTP delivery"
        );
        assert!(held_current.items.is_empty());
        // Actual changed-body POST/PATCH is deliberately unexecuted; the retained
        // record/queue and truthful pending result are the production seams here.
    }

    #[tokio::test]
    async fn unassigned_canonical_keeps_id_only_delta_for_delayed_original_session_control() {
        let calls = Mutex::new(HashMap::new());
        let received = AtomicU64::new(1);
        let old_id = Uuid::new_v4();
        let old = TranscriptCall::begin(&calls, old_id, "T".into(), ChannelId::new(10), &received)
            .unwrap();
        let (old_sender, old_events) = broadcast::channel(4);
        let (_old_cancel, old_cancelled) = watch::channel(false);
        let (_old_closed, old_closed) = watch::channel(None::<String>);
        let mut old_reader = Box::pin(handle_native_realtime_events(
            old_events,
            old.clone(),
            Arc::new(Http::new("")),
            AudioPipe::new().expect("create test audio pipe"),
            old_cancelled,
            old_closed,
        ));
        assert!(futures_util::poll!(old_reader.as_mut()).is_pending());
        // This A control was received before B's frame, but A has not processed it.
        let mut control = json!({"method":"thread/realtime/item/started","params":{"threadId":"T","item":{"type":"realtimeSessionStarted","id":"SA-control","realtimeSessionId":"SA"}}});
        crate::native::stamp_state_observation(&mut control["params"], 1, 10);
        old_sender.send(control).unwrap();
        received.store(12, Ordering::Release);
        let current_id = Uuid::new_v4();
        let current = TranscriptCall::begin(
            &calls,
            current_id,
            "T".into(),
            ChannelId::new(20),
            &received,
        )
        .unwrap();
        let (sender, events) = broadcast::channel(8);
        let mut current_control = json!({"method":"thread/realtime/started","params":{"threadId":"T","realtimeSessionId":"SB"}});
        crate::native::stamp_state_observation(&mut current_control["params"], 1, 12);
        sender.send(current_control).unwrap();
        let mut current_item = json!({"method":"thread/realtime/item/started","params":{"threadId":"T","item":{"type":"transcriptSegment","id":"B-current","realtimeSessionId":"SB","role":"user"}}});
        crate::native::stamp_state_observation(&mut current_item["params"], 1, 13);
        sender.send(current_item).unwrap();
        let (_cancel, cancelled) = watch::channel(false);
        let (_closed, closed) = watch::channel(None::<String>);
        let mut reader = Box::pin(handle_native_realtime_events(
            events,
            current.clone(),
            Arc::new(Http::new("")),
            AudioPipe::new().expect("create test audio pipe"),
            cancelled,
            closed,
        ));
        assert!(futures_util::poll!(reader.as_mut()).is_pending());
        let held_original = old.ledger.lock().await;
        let held_current = current.ledger.lock().await;
        let mut canonical = json!({"method":"thread/realtime/item/started","params":{"threadId":"T","item":{"type":"transcriptSegment","id":"A-unassigned-X","realtimeSessionId":"SA","role":"user","text":""}}});
        crate::native::stamp_state_observation(&mut canonical["params"], 1, 14);
        let raw = "A ID-only raw delta\n  🙂";
        let mut delta = json!({"method":"thread/realtime/item/transcript/delta","params":{"threadId":"T","itemId":"A-unassigned-X","delta":raw}});
        crate::native::stamp_state_observation(&mut delta["params"], 1, 15);
        sender.send(canonical.clone()).unwrap();
        sender.send(delta.clone()).unwrap();
        assert!(futures_util::poll!(reader.as_mut()).is_pending());
        assert!(
            !current
                .native_item_owners
                .lock()
                .unwrap()
                .contains_key(&("T".into(), "A-unassigned-X".into())),
            "ID-only delta must not claim an already unassigned original item for B"
        );
        {
            let history = current.native_session_history.lock().unwrap();
            assert_eq!(history.unassigned.len(), 2);
            assert_eq!(history.unassigned[0].event, canonical);
            assert_eq!(history.unassigned[1].event, delta);
            for original in &history.unassigned {
                assert_eq!(original.observed_by, current_id);
                assert_eq!(original.observed_transcript_thread_id, ChannelId::new(20));
                assert_eq!(
                    original.observed_calls.get(&current_id),
                    Some(&ChannelId::new(20))
                );
                assert_eq!(original.assigned_owner, None);
            }
        }
        assert!(current.delivery_result().is_err());
        assert!(futures_util::poll!(old_reader.as_mut()).is_pending());
        assert_eq!(
            current.native_item_owners.lock().unwrap()[&("T".into(), "A-unassigned-X".into())].0,
            old_id
        );
        {
            let history = current.native_session_history.lock().unwrap();
            assert_eq!(history.unassigned[0].event, canonical);
            assert_eq!(
                history.unassigned[1].event, delta,
                "session assignment must not inject metadata or rewrite the raw delta"
            );
            assert!(
                history
                    .unassigned
                    .iter()
                    .all(|original| original.assigned_owner == Some(old_id))
            );
        }
        drop(reader);
        drop(old_reader);
        assert_eq!(
            old.pending_native_events
                .lock()
                .unwrap()
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec![canonical, delta]
        );
        assert!(
            held_original.items.is_empty(),
            "held A ledger prevents delivery HTTP in this regression"
        );
        assert_eq!(held_current.items.len(), 1);
        assert!(held_current.items.contains_key("B-current"));
        assert!(!held_current.items.contains_key("A-unassigned-X"));
        assert_eq!(old.transcript_thread_id, ChannelId::new(10));
        assert_eq!(current.transcript_thread_id, ChannelId::new(20));
        assert!(
            old.delivery_result().is_err(),
            "assigned raw waiting for its original ledger is still pending delivery"
        );
        // This is actual receiver/admission/queue state, not a POST/PATCH result.
    }

    #[test]
    fn incomplete_native_item_keeps_new_delta_once_without_overwriting_canonical() {
        let calls = Mutex::new(HashMap::new());
        let call = TranscriptCall::begin(
            &calls,
            Uuid::new_v4(),
            "T".into(),
            ChannelId::new(10),
            &AtomicU64::new(1),
        )
        .unwrap();
        let mut ledger = TranscriptLedger::default();
        let started = json!({"threadId":"T","item":{"type":"transcriptSegment","id":"A-native-X","realtimeSessionId":"SA","role":"user"}});
        assert!(
            observe_native_transcript_item(&mut ledger, "thread/realtime/item/started", &started)
                .is_none()
        );
        let original_raw = "원문 P\n  🙂".repeat(220);
        let mut initial = json!({"threadId":"T","itemId":"A-native-X","delta":original_raw});
        crate::native::stamp_state_observation(&mut initial, 1, 2);
        let partial = observe_native_transcript_item(
            &mut ledger,
            "thread/realtime/item/transcript/delta",
            &initial,
        )
        .unwrap();
        assert_eq!(partial.text, original_raw);
        assert_eq!(partial.status, TranscriptStatus::Partial);
        let original_pages = transcript_content_pages(&partial);
        assert_eq!(original_pages.len(), 2);
        ledger.bind_discord_message("A-native-X", MessageId::new(55));
        let attempt = ledger.record_post_attempt("A-native-X", 1, &original_pages[1]);
        let original_nonce = ledger.post_attempts["A-native-X"][attempt].nonce.clone();
        let original_snapshot = ledger.items["A-native-X"].clone();
        call.record_delivery(
            &ledger,
            "A-native-X",
            &Err(anyhow!("original PATCH denied")),
        )
        .unwrap();
        let ended = ledger.mark_open_items_incomplete();
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].status, TranscriptStatus::Incomplete);

        let raw_delta = "\n  새 raw delta 🙂";
        let mut late = json!({"threadId":"T","itemId":"A-native-X","delta":raw_delta});
        crate::native::stamp_state_observation(&mut late, 1, 13);
        let updated = observe_native_transcript_item(
            &mut ledger,
            "thread/realtime/item/transcript/delta",
            &late,
        )
        .unwrap();
        assert_eq!(
            updated.text,
            format!("{original_raw}{raw_delta}"),
            "fresh late delta must remain in its original incomplete item"
        );
        assert_eq!(updated.status, TranscriptStatus::Incomplete);
        assert_eq!(updated.role, "user");
        assert_eq!(updated.native_source_id.as_deref(), Some("A-native-X"));
        assert_eq!(updated.discord_message_ids, vec![MessageId::new(55)]);
        observe_native_transcript_item(&mut ledger, "thread/realtime/item/transcript/delta", &late);
        assert_eq!(
            ledger.items["A-native-X"].text, updated.text,
            "same stamped observation must not append twice"
        );
        assert_eq!(
            ledger.items["A-native-X"].status,
            TranscriptStatus::Incomplete
        );

        let canonical_raw = "exact canonical S\n  🙂";
        let mut completed = json!({"threadId":"T","item":{"type":"transcriptSegment","id":"A-native-X","realtimeSessionId":"SA","role":"user","text":canonical_raw}});
        crate::native::stamp_state_observation(&mut completed, 1, 14);
        let canonical = observe_native_transcript_item(
            &mut ledger,
            "thread/realtime/item/completed",
            &completed,
        )
        .unwrap();
        assert_eq!(canonical.text, canonical_raw);
        assert_eq!(canonical.status, TranscriptStatus::Complete);
        let mut after_canonical =
            json!({"threadId":"T","itemId":"A-native-X","delta":"must not overwrite canonical"});
        crate::native::stamp_state_observation(&mut after_canonical, 1, 15);
        observe_native_transcript_item(
            &mut ledger,
            "thread/realtime/item/transcript/delta",
            &after_canonical,
        );
        let current = &ledger.items["A-native-X"];
        assert_eq!(current.text, canonical_raw);
        assert_eq!(current.status, TranscriptStatus::Complete);
        assert_eq!(current.role, "user");
        assert_eq!(current.native_source_id.as_deref(), Some("A-native-X"));
        assert_eq!(current.discord_message_ids, vec![MessageId::new(55)]);
        assert_eq!(ledger.post_attempts["A-native-X"].len(), 1);
        let original_attempt = &ledger.post_attempts["A-native-X"][attempt];
        assert_eq!(original_attempt.page, 1);
        assert_eq!(original_attempt.content, original_pages[1]);
        assert_eq!(original_attempt.nonce, original_nonce);
        assert_eq!(original_attempt.message_id, None);
        assert!(ledger.has_pending_posts("A-native-X"));
        let failures = call.failures.lock().unwrap();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].item, original_snapshot);
        assert_eq!(failures[0].error, "original PATCH denied");
        assert!(!failures[0].resolved);
        // Actual parser/cleanup ledger and original snapshots only; no HTTP.
    }

    #[tokio::test]
    async fn delta_first_realtime_item_waits_for_session_proof_and_keeps_raw_order() {
        for delayed_control in [false, true] {
            let calls = Mutex::new(HashMap::new());
            let received = AtomicU64::new(1);
            let old_id = Uuid::new_v4();
            let old =
                TranscriptCall::begin(&calls, old_id, "T".into(), ChannelId::new(10), &received)
                    .unwrap();
            let first_call = json!({"method":"thread/realtime/item/transcript/delta","params":{"threadId":"T","itemId":"first-call-control","delta":"first call raw"}});
            assert!(
                old.native_event_owner("thread/realtime/item/transcript/delta", &first_call)
                    .unwrap()
                    .is_none()
            );
            assert!(
                old.accepts_event(&first_call["params"]).unwrap(),
                "first call fallback remains available"
            );
            let (old_sender, old_events) = broadcast::channel(4);
            let (_old_cancel, old_cancelled) = watch::channel(false);
            let (_old_closed, old_closed) = watch::channel(None::<String>);
            let mut old_reader = Some(Box::pin(handle_native_realtime_events(
                old_events,
                old.clone(),
                Arc::new(Http::new("")),
                AudioPipe::new().expect("create test audio pipe"),
                old_cancelled,
                old_closed,
            )));
            assert!(futures_util::poll!(old_reader.as_mut().unwrap().as_mut()).is_pending());
            let mut control = json!({"method":"thread/realtime/item/started","params":{"threadId":"T","item":{"type":"realtimeSessionStarted","id":"SA-control","realtimeSessionId":"SA"}}});
            crate::native::stamp_state_observation(&mut control["params"], 1, 5);
            old_sender.send(control).unwrap();
            if !delayed_control {
                drop(old_sender);
                old_reader.take().unwrap().await.unwrap();
            }
            received.store(12, Ordering::Release);
            let current_id = Uuid::new_v4();
            let current = TranscriptCall::begin(
                &calls,
                current_id,
                "T".into(),
                ChannelId::new(20),
                &received,
            )
            .unwrap();
            let (sender, events) = broadcast::channel(8);
            sender.send(json!({"method":"thread/realtime/started","params":{"threadId":"T","realtimeSessionId":"SB"}})).unwrap();
            sender.send(json!({"method":"thread/realtime/item/started","params":{"threadId":"T","item":{"type":"transcriptSegment","id":"B-normal","role":"user"}}})).unwrap();
            let (_cancel, cancelled) = watch::channel(false);
            let (_closed, closed) = watch::channel(None::<String>);
            let mut reader = Box::pin(handle_native_realtime_events(
                events,
                current.clone(),
                Arc::new(Http::new("")),
                AudioPipe::new().expect("create test audio pipe"),
                cancelled,
                closed,
            ));
            assert!(futures_util::poll!(reader.as_mut()).is_pending());
            let backing = json!({"method":"item/agentMessage/delta","params":{"threadId":"T","itemId":"backing-control","delta":"backing raw"}});
            assert!(
                current
                    .native_event_owner("item/agentMessage/delta", &backing)
                    .unwrap()
                    .is_none()
            );
            assert!(
                current.accepts_event(&backing["params"]).unwrap(),
                "backing agent delta is not blanket-held"
            );
            let held_old = old.ledger.lock().await;
            let held_current = current.ledger.lock().await;
            let mut partial = json!({"method":"thread/realtime/item/transcript/delta","params":{"threadId":"T","itemId":"unseen-X","delta":"original A P\n  🙂"}});
            crate::native::stamp_state_observation(&mut partial["params"], 1, 350);
            sender.send(partial.clone()).unwrap();
            sender.send(partial.clone()).unwrap();
            assert!(futures_util::poll!(reader.as_mut()).is_pending());
            assert!(
                !current
                    .native_item_owners
                    .lock()
                    .unwrap()
                    .contains_key(&("T".into(), "unseen-X".into())),
                "delta-first item must wait for session proof instead of claiming the current call"
            );
            {
                let history = current.native_session_history.lock().unwrap();
                assert_eq!(history.unassigned.len(), 1);
                assert_eq!(history.unassigned[0].event, partial);
                assert_eq!(history.unassigned[0].observed_by, current_id);
                assert_eq!(
                    history.unassigned[0].observed_transcript_thread_id,
                    ChannelId::new(20)
                );
                assert_eq!(history.unassigned[0].assigned_owner, None);
            }
            assert!(current.delivery_result().is_err());
            let mut canonical = json!({"method":"thread/realtime/item/completed","params":{"threadId":"T","item":{"type":"transcriptSegment","id":"unseen-X","realtimeSessionId":"SA","role":"user","text":"exact A canonical S\n  🙂"}}});
            crate::native::stamp_state_observation(&mut canonical["params"], 1, 351);
            sender.send(canonical.clone()).unwrap();
            assert!(futures_util::poll!(reader.as_mut()).is_pending());
            if delayed_control {
                assert!(futures_util::poll!(old_reader.as_mut().unwrap().as_mut()).is_pending());
            }
            assert!(!current.accepts_event(&canonical["params"]).unwrap());
            assert!(old.accepts_event(&canonical["params"]).unwrap());
            assert_eq!(
                current.native_item_owners.lock().unwrap()[&("T".into(), "unseen-X".into())].0,
                old_id
            );
            {
                let history = current.native_session_history.lock().unwrap();
                assert_eq!(
                    history.unassigned.len(),
                    if delayed_control { 2 } else { 1 }
                );
                assert_eq!(history.unassigned[0].event, partial);
                if delayed_control {
                    assert_eq!(history.unassigned[1].event, canonical);
                }
                assert!(
                    history
                        .unassigned
                        .iter()
                        .all(|original| original.assigned_owner == Some(old_id))
                );
            }
            drop(reader);
            drop(old_reader);
            assert_eq!(
                old.pending_native_events
                    .lock()
                    .unwrap()
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>(),
                vec![partial, canonical],
                "retained partial must precede canonical in original receive order"
            );
            assert!(held_old.items.is_empty());
            assert_eq!(held_current.items.len(), 1);
            assert!(held_current.items.contains_key("B-normal"));
            assert!(!held_current.items.contains_key("unseen-X"));
            assert!(
                old.delivery_result().is_err(),
                "queued raw is not HTTP delivery success"
            );
            // Actual receiver/ownership/queue seams; ledger holds prevent HTTP.
        }
    }

    #[tokio::test]
    async fn explicit_session_corrects_sessionless_claim_without_erasing_delivery_history() {
        let calls = Mutex::new(HashMap::new());
        let received = AtomicU64::new(1);
        let old_id = Uuid::new_v4();
        let old = TranscriptCall::begin(&calls, old_id, "T".into(), ChannelId::new(10), &received)
            .unwrap();
        let (old_sender, old_events) = broadcast::channel(2);
        old_sender.send(json!({"method":"thread/realtime/started","params":{"threadId":"T","realtimeSessionId":"SA"}})).unwrap();
        drop(old_sender);
        let (_old_cancel, old_cancelled) = watch::channel(false);
        let (_old_closed, old_closed) = watch::channel(None::<String>);
        handle_native_realtime_events(
            old_events,
            old.clone(),
            Arc::new(Http::new("")),
            AudioPipe::new().expect("create test audio pipe"),
            old_cancelled,
            old_closed,
        )
        .await
        .unwrap();
        received.store(12, Ordering::Release);
        let current_id = Uuid::new_v4();
        let current = TranscriptCall::begin(
            &calls,
            current_id,
            "T".into(),
            ChannelId::new(20),
            &received,
        )
        .unwrap();
        let (sender, events) = broadcast::channel(4);
        sender.send(json!({"method":"thread/realtime/started","params":{"threadId":"T","realtimeSessionId":"SB"}})).unwrap();
        for item_id in ["conflicted-X", "B-normal"] {
            sender.send(json!({"method":"thread/realtime/item/started","params":{"threadId":"T","item":{"type":"transcriptSegment","id":item_id,"role":"user"}}})).unwrap();
        }
        let (_cancel, cancelled) = watch::channel(false);
        let (_closed, closed) = watch::channel(None::<String>);
        let mut reader = Box::pin(handle_native_realtime_events(
            events,
            current.clone(),
            Arc::new(Http::new("")),
            AudioPipe::new().expect("create test audio pipe"),
            cancelled,
            closed,
        ));
        assert!(futures_util::poll!(reader.as_mut()).is_pending());
        assert_eq!(
            current.native_item_owners.lock().unwrap()[&("T".into(), "conflicted-X".into())].0,
            current_id
        );
        let prior_snapshot;
        let original_body;
        let original_nonce;
        {
            let mut ledger = current.ledger.lock().await;
            let mut prior_delta = json!({"threadId":"T","itemId":"conflicted-X","delta":"prior B raw P\n  🙂".repeat(160)});
            crate::native::stamp_state_observation(&mut prior_delta, 1, 13);
            let partial = observe_native_transcript_item(
                &mut ledger,
                "thread/realtime/item/transcript/delta",
                &prior_delta,
            )
            .unwrap();
            let pages = transcript_content_pages(&partial);
            assert_eq!(pages.len(), 2);
            ledger.bind_discord_message("conflicted-X", MessageId::new(77));
            original_body = pages[1].clone();
            let attempt = ledger.record_post_attempt("conflicted-X", 1, &original_body);
            original_nonce = ledger.post_attempts["conflicted-X"][attempt].nonce.clone();
            prior_snapshot = ledger.items["conflicted-X"].clone();
            current
                .record_delivery(
                    &ledger,
                    "conflicted-X",
                    &Err(anyhow!("prior B PATCH denied")),
                )
                .unwrap();
        }
        let held_old = old.ledger.lock().await;
        let held_current = current.ledger.lock().await;
        let mut queued_partial = json!({"method":"thread/realtime/item/transcript/delta","params":{"threadId":"T","itemId":"conflicted-X","delta":"undelivered original P\n  🙂"}});
        crate::native::stamp_state_observation(&mut queued_partial["params"], 1, 14);
        let mut unrelated = json!({"method":"thread/realtime/item/started","params":{"threadId":"T","item":{"type":"transcriptSegment","id":"B-normal","role":"user"}}});
        crate::native::stamp_state_observation(&mut unrelated["params"], 1, 15);
        let mut other_session = json!({"method":"thread/realtime/item/started","params":{"threadId":"T","item":{"type":"realtimeSessionStarted","id":"conflicted-X","realtimeSessionId":"SB"}}});
        crate::native::stamp_state_observation(&mut other_session["params"], 1, 16);
        current.pending_native_events.lock().unwrap().extend([
            queued_partial.clone(),
            unrelated.clone(),
            other_session.clone(),
        ]);
        let captured_old_front = current
            .pending_native_events
            .lock()
            .unwrap()
            .front()
            .cloned()
            .unwrap();
        let mut canonical = json!({"method":"thread/realtime/item/completed","params":{"threadId":"T","item":{"type":"transcriptSegment","id":"conflicted-X","realtimeSessionId":"SA","role":"user","text":"exact original A canonical S\n  🙂"}}});
        crate::native::stamp_state_observation(&mut canonical["params"], 1, 17);
        assert!(
            !current.accepts_event(&canonical["params"]).unwrap(),
            "explicit original session must reject an earlier current-call claim"
        );
        assert!(old.accepts_event(&canonical["params"]).unwrap());
        let normal_b = json!({"threadId":"T","item":{"type":"transcriptSegment","id":"B-normal","realtimeSessionId":"SB","role":"user","text":"normal B canonical"}});
        assert!(current.accepts_event(&normal_b).unwrap());
        assert!(!old.accepts_event(&normal_b).unwrap());
        sender.send(canonical.clone()).unwrap();
        assert!(futures_util::poll!(reader.as_mut()).is_pending());
        drop(reader);
        assert_eq!(
            current.native_item_owners.lock().unwrap()[&("T".into(), "conflicted-X".into())].0,
            old_id
        );
        assert_eq!(
            old.pending_native_events
                .lock()
                .unwrap()
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec![queued_partial, canonical]
        );
        assert_eq!(
            current
                .pending_native_events
                .lock()
                .unwrap()
                .iter()
                .cloned()
                .collect::<Vec<_>>(),
            vec![unrelated, other_session],
            "correction must retain unrelated and explicit other-session queued originals"
        );
        assert_ne!(
            current.pending_native_events.lock().unwrap().front(),
            Some(&captured_old_front),
            "an old captured front cannot consume its unrelated successor"
        );
        assert!(held_old.items.is_empty());
        assert_eq!(
            held_current.items["conflicted-X"], prior_snapshot,
            "prior raw/native/known Discord IDs must not be migrated or erased"
        );
        assert!(held_current.items.contains_key("B-normal"));
        let attempts = &held_current.post_attempts["conflicted-X"];
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].page, 1);
        assert_eq!(attempts[0].content, original_body);
        assert_eq!(attempts[0].nonce, original_nonce);
        assert_eq!(attempts[0].message_id, None);
        assert!(held_current.has_pending_posts("conflicted-X"));
        let failures = current.failures.lock().unwrap();
        assert_eq!(failures.len(), 1);
        assert_eq!(failures[0].item, prior_snapshot);
        assert_eq!(failures[0].error, "prior B PATCH denied");
        assert!(!failures[0].resolved);
        assert_eq!(current.transcript_thread_id, ChannelId::new(20));
        assert_eq!(old.transcript_thread_id, ChannelId::new(10));
        drop(failures);
        drop(held_current);
        drain_pending_native_events(&current, &Arc::new(Http::new("")))
            .await
            .unwrap();
        assert!(current.pending_native_events.lock().unwrap().is_empty());
        // Remaining packets are started-only controls, so this drain uses no HTTP.
        // The captured-front/actual HTTP-await interleaving is a source-only limit.
        // Known ID/error are state fixtures; no old-public-post correction is executed.
    }

    #[tokio::test]
    async fn generic_turn_error_does_not_abort_realtime_sdp_wait() {
        let (sender, mut events) = broadcast::channel(2);
        sender
            .send(json!({
                "method": "error",
                "params": {
                    "threadId": "voice-thread",
                    "error": {"message": "unsupported voice"}
                }
            }))
            .unwrap();
        sender
            .send(json!({
                "method": "thread/realtime/sdp",
                "params": {"threadId": "voice-thread", "sdp": "v=0\r\n"}
            }))
            .unwrap();

        let answer = await_realtime_sdp(&mut events, "voice-thread", 1)
            .await
            .unwrap();

        assert_eq!(answer, "v=0\r\n");
    }

    #[tokio::test]
    async fn native_realtime_startup_error_is_returned_instead_of_waiting_for_sdp() {
        let (sender, mut events) = broadcast::channel(1);
        sender
            .send(json!({
                "method": "thread/realtime/error",
                "params": {
                    "threadId": "voice-thread",
                    "error": {"message": "unsupported voice"}
                }
            }))
            .unwrap();

        let error = await_realtime_sdp(&mut events, "voice-thread", 1)
            .await
            .unwrap_err();

        assert!(format!("{error:#}").contains("unsupported voice"));
    }

    #[test]
    fn operator_input_combines_partial_frames_and_expires_stale_audio() {
        let input = OperatorAudioBuffer::default();
        let now = Instant::now();
        input.push(vec![1; OPUS_FRAME_SAMPLES], now);
        assert!(input.take_output(now).is_none());
        input.push(vec![2; OPUS_FRAME_SAMPLES], now);
        let Some(OperatorAudioOutput::Pcm(output)) = input.take_output(now) else {
            panic!("expected PCM frame")
        };
        assert_eq!(&output[..OPUS_FRAME_SAMPLES], vec![1; OPUS_FRAME_SAMPLES]);
        assert_eq!(&output[OPUS_FRAME_SAMPLES..], vec![2; OPUS_FRAME_SAMPLES]);
        input.push(vec![3; OPUS_FRAME_SAMPLES * 2], now);
        assert!(input.take_output(now + OPERATOR_AUDIO_MAX_AGE).is_none());
        assert!(input.0.lock().unwrap().is_empty());
    }

    #[test]
    fn operator_input_overflow_keeps_recent_audio_within_latency_budget() {
        let input = OperatorAudioBuffer::default();
        let now = Instant::now();
        for sample in 0..12 {
            input.push(vec![sample; OPUS_FRAME_SAMPLES * 2], now);
        }
        for sample in 2..12 {
            let Some(OperatorAudioOutput::Pcm(output)) = input.take_output(now) else {
                panic!("expected PCM frame")
            };
            assert_eq!(output, vec![sample; OPUS_FRAME_SAMPLES * 2]);
        }
        assert!(input.take_output(now).is_none());
    }

    #[test]
    fn operator_input_preserves_encoded_payload_and_sample_duration() {
        let input = OperatorAudioBuffer::default();
        let now = Instant::now();
        let payload = vec![7, 8, 9];
        assert!(input.push_opus(payload.clone(), OPUS_FRAME_SAMPLES, now));
        let Some(OperatorAudioOutput::Opus {
            data,
            duration,
            samples,
        }) = input.take_output(now)
        else {
            panic!("expected encoded Opus packet")
        };
        assert_eq!(data, payload);
        assert_eq!(duration, OPUS_FRAME);
        assert_eq!(samples, OPUS_FRAME_SAMPLES * 2);
    }

    fn encoded_opus_frame(samples_per_channel: usize) -> Vec<u8> {
        let mut encoder = Encoder::new(MEDIA_SAMPLE_RATE, Channels::Stereo, Application::Voip)
            .expect("create Opus encoder");
        let pcm = vec![321_i16; samples_per_channel * 2];
        let mut output = vec![0_u8; OPUS_OUTPUT_CAPACITY];
        let size = encoder
            .encode(&pcm, &mut output)
            .expect("encode Opus frame");
        output.truncate(size);
        output
    }

    #[test]
    fn native_packet_before_songbird_input_installation_is_retained() {
        use songbird::input::{Input as SongbirdInput, LiveInput};

        let pipe = AudioPipe::new().unwrap();
        let payload = encoded_opus_frame(OPUS_FRAME_SAMPLES);
        pipe.push_opus(
            7,
            4800,
            &payload,
            Instant::now() - Duration::from_millis(60),
        )
        .unwrap();
        let input = pipe
            .install_opus_input(Arc::new(AudioStats::default()))
            .unwrap();
        let SongbirdInput::Live(LiveInput::Parsed(mut parsed), None) = input else {
            panic!("expected direct live Opus input");
        };
        let packet = parsed.format.next_packet().unwrap();
        assert_eq!(packet.buf(), payload);
        assert_eq!(packet.dur, OPUS_FRAME_SAMPLES as u64);
    }

    #[test]
    fn clear_before_songbird_input_installation_preserves_new_generation() {
        use songbird::input::{Input as SongbirdInput, LiveInput};

        let pipe = AudioPipe::new().unwrap();
        let old_payload = encoded_opus_frame(OPUS_FRAME_SAMPLES);
        let new_payload = encoded_opus_frame(OPUS_FRAME_SAMPLES / 2);
        let arrived = Instant::now() - Duration::from_millis(60);
        pipe.push_opus(7, 4800, &old_payload, arrived).unwrap();
        pipe.clear();
        pipe.push_opus(8, 5760, &new_payload, arrived).unwrap();
        let input = pipe
            .install_opus_input(Arc::new(AudioStats::default()))
            .unwrap();
        let SongbirdInput::Live(LiveInput::Parsed(mut parsed), None) = input else {
            panic!("expected direct live Opus input");
        };
        let packet = parsed.format.next_packet().unwrap();
        assert_eq!(packet.buf(), new_payload);
        assert_eq!(packet.dur, (OPUS_FRAME_SAMPLES / 2) as u64);
    }

    #[test]
    fn direct_opus_input_trims_dave_bounds_and_rejects_other_durations() {
        let encoded = encoded_opus_frame(OPUS_FRAME_SAMPLES);
        let mut bounded = vec![0xA5; 4];
        bounded.extend_from_slice(&encoded);
        bounded.extend_from_slice(&[0x5A; 7]);

        let (forwarded, samples) = forwardable_opus_payload(&bounded, 4, 7).unwrap();
        assert_eq!(forwarded, encoded);
        assert_eq!(samples, OPUS_FRAME_SAMPLES);

        let ten_ms = encoded_opus_frame(OPUS_FRAME_SAMPLES / 2);
        assert!(forwardable_opus_payload(&ten_ms, 0, 0).is_err());
    }

    #[test]
    fn decoder_is_reset_after_opus_passthrough_before_pcm_fallback() {
        let mut received = ReceivedAudioState::default();
        let _ = received.decoder(55);
        assert!(received.decoders.contains_key(&55));

        received.mark_passthrough(55);
        assert!(!received.decoders.contains_key(&55));
        assert!(received.passthrough_ssrcs.contains(&55));

        let _ = received.decoder(55);
        assert!(received.decoders.contains_key(&55));
        assert!(!received.passthrough_ssrcs.contains(&55));
    }

    #[tokio::test]
    async fn peer_readiness_waits_for_connected_and_rejects_failed_transport() {
        let (states, mut receiver) = watch::channel(RTCPeerConnectionState::New);
        let mut ready = Box::pin(await_peer_connected(&mut receiver, Language::English));
        assert!(futures_util::poll!(&mut ready).is_pending());
        states.send_replace(RTCPeerConnectionState::Connecting);
        assert!(futures_util::poll!(&mut ready).is_pending());
        states.send_replace(RTCPeerConnectionState::Disconnected);
        assert!(futures_util::poll!(&mut ready).is_pending());
        states.send_replace(RTCPeerConnectionState::Connected);
        ready.await.unwrap();
        await_peer_connected(&mut receiver, Language::English)
            .await
            .unwrap();
        states.send_replace(RTCPeerConnectionState::Failed);
        assert!(
            await_peer_connected(&mut receiver, Language::English)
                .await
                .is_err()
        );
    }

    #[test]
    fn opus_decode_respects_extension_already_removed_by_songbird_offset() {
        let input = vec![900_i16; OPUS_FRAME_SAMPLES * 2];
        let mut encoder =
            Encoder::new(MEDIA_SAMPLE_RATE, Channels::Stereo, Application::Voip).unwrap();
        let mut encoded = vec![0_u8; OPUS_OUTPUT_CAPACITY];
        let length = encoder.encode(&input, &mut encoded).unwrap();
        encoded.truncate(length);
        let mut payload = vec![0xbe, 0xde, 0x00, 0x01, 0x10, 0x7f, 0x00, 0x00];
        payload.extend_from_slice(&encoded);
        payload.extend_from_slice(&[0xa5; 11]);
        let mut reference = Decoder::new(MEDIA_SAMPLE_RATE, Channels::Stereo).unwrap();
        let expected = decode_opus_payload(&mut reference, &encoded, 0, 0).unwrap();
        let mut decoder = Decoder::new(MEDIA_SAMPLE_RATE, Channels::Stereo).unwrap();
        let actual = decode_opus_payload(&mut decoder, &payload, 8, 11).unwrap();
        assert_eq!(actual, expected);
    }

    #[test]
    fn opus_decode_uses_public_rtp_end_padding_for_dave_trailer() {
        let input = vec![17_i16; OPUS_FRAME_SAMPLES * 2];
        let mut encoder = Encoder::new(MEDIA_SAMPLE_RATE, Channels::Stereo, Application::Voip)
            .expect("create test Opus encoder");
        let mut encoded = vec![0_u8; OPUS_OUTPUT_CAPACITY];
        let encoded_len = encoder
            .encode(&input, &mut encoded)
            .expect("encode test Opus frame");
        encoded.truncate(encoded_len);

        // DAVE leaves an authentication tag, supplemental-data length, and magic marker
        // in the backing RTP buffer. The public RtpData payload_end_pad identifies all of
        // those bytes so the Opus decoder receives only the encoded frame. RtpData itself
        // is non-exhaustive and has no public constructor, so this test exercises the same
        // payload slicing helper with the bounds delivered by that public event.
        let mut with_dave_trailer = encoded.clone();
        with_dave_trailer.extend([0xa5; 8]);
        with_dave_trailer.extend([0x01, 0xfa, 0xfa]);
        let mut payload_with_prefix = vec![0x42];
        payload_with_prefix.extend(&with_dave_trailer);

        let mut reference_decoder = Decoder::new(MEDIA_SAMPLE_RATE, Channels::Stereo)
            .expect("create reference Opus decoder");
        let reference = decode_opus_payload(&mut reference_decoder, &encoded, 0, 0)
            .expect("decode exact encoded frame");
        let mut bounded_decoder =
            Decoder::new(MEDIA_SAMPLE_RATE, Channels::Stereo).expect("create bounded Opus decoder");
        let bounded = decode_opus_payload(&mut bounded_decoder, &payload_with_prefix, 1, 11)
            .expect("decode frame after removing DAVE trailer");

        assert_eq!(bounded, reference);
        assert_eq!(bounded.len(), OPUS_FRAME_SAMPLES * 2);
        assert!(bounded.iter().any(|sample| *sample != 0));

        let mut untrimmed_decoder = Decoder::new(MEDIA_SAMPLE_RATE, Channels::Stereo)
            .expect("create untrimmed Opus decoder");
        let untrimmed = decode_opus_payload(&mut untrimmed_decoder, &payload_with_prefix, 1, 0);
        match untrimmed {
            Err(_) => {}
            Ok(untrimmed) => assert_ne!(untrimmed, reference),
        }
    }

    #[test]
    fn opus_packet_loss_concealment_keeps_a_stereo_tick() {
        let mut decoder =
            Decoder::new(MEDIA_SAMPLE_RATE, Channels::Stereo).expect("create Opus PLC decoder");
        let concealed = decode_opus_plc(&mut decoder).expect("decode Opus PLC frame");
        assert_eq!(concealed.len(), OPUS_FRAME_SAMPLES * 2);
    }

    #[test]
    fn transcript_headers_use_resolved_agent_and_all_actual_speakers() {
        let assistant = TranscriptItem {
            source_id: "assistant".into(),
            native_source_id: Some("assistant".into()),
            role: "assistant".into(),
            text: "hello".into(),
            status: TranscriptStatus::Complete,
            discord_message_ids: Vec::new(),
            speaker_ids: Vec::new(),
            agent_label: "Example Agent".into(),
            speaker_labels: Vec::new(),
            posted_content: None,
        };
        assert_eq!(
            transcript_content_pages(&assistant)[0],
            "**Example Agent**\nhello"
        );
        let mut user = assistant.clone();
        user.role = "user".into();
        user.speaker_labels = vec!["Example Speaker A".into(), "Example Speaker B".into()];
        assert_eq!(
            transcript_content_pages(&user)[0],
            "**Example Speaker A \\+ Example Speaker B**\nhello"
        );
    }

    #[test]
    fn voice_transcript_file_batches_clear_unused_assistant_pages_and_never_upload_user_files() {
        let assistant = TranscriptItem {
            source_id: "assistant-file-item".into(),
            native_source_id: Some("assistant-file-item".into()),
            role: "assistant".into(),
            text: "See [the report](file:///tmp/report.md)".into(),
            status: TranscriptStatus::Complete,
            discord_message_ids: Vec::new(),
            speaker_ids: Vec::new(),
            agent_label: "Codex".into(),
            speaker_labels: Vec::new(),
            posted_content: None,
        };
        let prepared = PreparedReply {
            content: "See the report".into(),
            files: vec![ReplyFile {
                filename: "report.md".into(),
                data: b"report".to_vec(),
            }],
        };
        let rendered = render_transcript_reply(&assistant, &prepared, Language::English);
        assert_eq!(rendered.contents[0], "**Codex**\nSee the report");
        assert!(!rendered.contents[0].contains("file://"));
        assert_eq!(
            transcript_files_for_page(&assistant, &prepared, 0),
            Some(prepared.files.as_slice())
        );
        assert_eq!(
            transcript_files_for_page(&assistant, &prepared, 1),
            Some(&prepared.files[prepared.files.len()..])
        );

        let mut user = assistant.clone();
        user.role = "user".into();
        let user_rendered = render_transcript_reply(&user, &prepared, Language::English);
        assert!(user_rendered.contents[0].contains("file:///tmp/report.md"));
        assert_eq!(transcript_files_for_page(&user, &prepared, 0), None);
    }

    #[test]
    fn voice_transcript_batches_unbounded_reply_files_across_pages() {
        let assistant = TranscriptItem {
            source_id: "many-files".into(),
            native_source_id: Some("many-files".into()),
            role: "assistant".into(),
            text: "Here are the files".into(),
            status: TranscriptStatus::Complete,
            discord_message_ids: Vec::new(),
            speaker_ids: Vec::new(),
            agent_label: "Codex".into(),
            speaker_labels: Vec::new(),
            posted_content: None,
        };
        let prepared = PreparedReply {
            content: "Here are the files".into(),
            files: (0..21)
                .map(|index| ReplyFile {
                    filename: format!("file-{index}.txt"),
                    data: format!("contents-{index}").into_bytes(),
                })
                .collect(),
        };
        let rendered = render_transcript_reply(&assistant, &prepared, Language::English);
        assert_eq!(rendered.contents.len(), 3);
        assert_eq!(
            transcript_files_for_page(&assistant, &prepared, 0)
                .unwrap()
                .len(),
            10
        );
        assert_eq!(
            transcript_files_for_page(&assistant, &prepared, 1)
                .unwrap()
                .len(),
            10
        );
        assert_eq!(
            transcript_files_for_page(&assistant, &prepared, 2)
                .unwrap()
                .len(),
            1
        );
        assert!(rendered.contents[1].contains("Attachments continued"));
        assert!(rendered.contents[2].contains("Attachments continued"));
        assert_eq!(
            transcript_files_for_page(&assistant, &prepared, 3)
                .unwrap()
                .len(),
            0
        );

        let mut ledger = TranscriptLedger::default();
        let page_zero = PostedReplyFile {
            page: 0,
            id: AttachmentId::new(71),
            filename: "file-0.txt".into(),
            digest: reply_file_digest(&prepared.files[0]),
        };
        let page_one = PostedReplyFile {
            page: 1,
            id: AttachmentId::new(72),
            filename: "file-10.txt".into(),
            digest: reply_file_digest(&prepared.files[10]),
        };
        ledger.mark_posted_reply_files("many-files", 0, vec![page_zero.clone()]);
        ledger.mark_posted_reply_files("many-files", 1, vec![page_one]);
        ledger.mark_posted_reply_files("many-files", 1, Vec::new());
        assert_eq!(ledger.posted_reply_files["many-files"], vec![page_zero]);

        let mut user = assistant.clone();
        user.role = "user".into();
        let user_rendered = render_transcript_reply(&user, &prepared, Language::English);
        assert_eq!(user_rendered.contents.len(), 1);
        assert!(transcript_files_for_page(&user, &prepared, 0).is_none());
    }

    #[test]
    fn voice_transcript_edit_keeps_matching_uploads_and_replaces_changed_files() {
        let same = ReplyFile {
            filename: "report.md".into(),
            data: b"same report".to_vec(),
        };
        let changed = ReplyFile {
            filename: "new.md".into(),
            data: b"new report".to_vec(),
        };
        let existing = vec![
            PostedReplyFile {
                page: 0,
                id: AttachmentId::new(41),
                filename: same.filename.clone(),
                digest: reply_file_digest(&same),
            },
            PostedReplyFile {
                page: 0,
                id: AttachmentId::new(42),
                filename: "retired.md".into(),
                digest: reply_file_digest(&ReplyFile {
                    filename: "retired.md".into(),
                    data: b"retired report".to_vec(),
                }),
            },
        ];
        assert_eq!(
            reply_file_edit_plan(&[same, changed], &existing),
            vec![
                ReplyFileEdit::Keep(AttachmentId::new(41)),
                ReplyFileEdit::Upload(1)
            ]
        );
    }

    #[test]
    fn voice_reply_attachment_ids_match_unique_name_and_size_not_response_order() {
        let first = ReplyFile {
            filename: "first.md".into(),
            data: b"first content".to_vec(),
        };
        let second = ReplyFile {
            filename: "second.pdf".into(),
            data: b"second".to_vec(),
        };
        let returned = vec![
            (AttachmentId::new(52), "second.pdf".into(), 6),
            (AttachmentId::new(51), "first.md".into(), 13),
        ];
        assert_eq!(
            match_reply_attachment_ids(&[first.clone(), second.clone()], &returned).unwrap(),
            vec![Some(AttachmentId::new(51)), Some(AttachmentId::new(52))]
        );

        let ambiguous = vec![
            (AttachmentId::new(61), "first.md".into(), 13),
            (AttachmentId::new(62), "first.md".into(), 13),
        ];
        assert!(match_reply_attachment_ids(&[first.clone()], &ambiguous).is_err());
        let same_name_and_size = ReplyFile {
            filename: first.filename.clone(),
            data: b"other content".to_vec(),
        };
        assert_eq!(
            match_reply_attachment_ids(&[first, same_name_and_size], &ambiguous).unwrap(),
            vec![None, None]
        );
    }

    #[test]
    fn flat_transcript_promotion_migrates_prepared_reply_and_attachment_ids() {
        let mut ledger = TranscriptLedger::default();
        ledger.partial_flat("flat-source", "assistant", "report");
        ledger.prepared_replies.insert(
            "flat-source".into(),
            (
                "[report](file:///tmp/report.md)".into(),
                PreparedReply {
                    content: "report".into(),
                    files: vec![ReplyFile {
                        filename: "report.md".into(),
                        data: b"report".to_vec(),
                    }],
                },
            ),
        );
        let sent = vec![PostedReplyFile {
            page: 0,
            id: AttachmentId::new(43),
            filename: "report.md".into(),
            digest: reply_file_digest(&ReplyFile {
                filename: "report.md".into(),
                data: b"report".to_vec(),
            }),
        }];
        ledger
            .posted_reply_files
            .insert("flat-source".into(), sent.clone());

        assert!(
            ledger
                .canonicalize_flat(
                    "flat-source",
                    "native-source",
                    "assistant",
                    "[report](file:///tmp/report.md)",
                )
                .is_some()
        );
        assert!(ledger.prepared_replies.contains_key("native-source"));
        assert!(!ledger.prepared_replies.contains_key("flat-source"));
        assert_eq!(ledger.posted_reply_files.get("native-source"), Some(&sent));
        assert!(!ledger.posted_reply_files.contains_key("flat-source"));
    }

    #[tokio::test]
    async fn voice_prepared_reply_cache_reuses_files_for_same_native_text() {
        let mut ledger = TranscriptLedger::default();
        let reads = AtomicU64::new(0);
        let prepared = PreparedReply {
            content: "Download the report".into(),
            files: vec![ReplyFile {
                filename: "report.md".into(),
                data: b"report".to_vec(),
            }],
        };
        let first = ledger
            .prepared_reply_for("native-item", "[report](file:///tmp/report.md)", || {
                reads.fetch_add(1, Ordering::Relaxed);
                async { prepared.clone() }
            })
            .await;
        let repeated = ledger
            .prepared_reply_for("native-item", "[report](file:///tmp/report.md)", || {
                reads.fetch_add(1, Ordering::Relaxed);
                async { prepared.clone() }
            })
            .await;
        assert_eq!(first, repeated);
        assert_eq!(reads.load(Ordering::Relaxed), 1);

        let changed = ledger
            .prepared_reply_for("native-item", "[new](file:///tmp/new.md)", || {
                reads.fetch_add(1, Ordering::Relaxed);
                async {
                    PreparedReply {
                        content: "Download the new report".into(),
                        files: vec![ReplyFile {
                            filename: "new.md".into(),
                            data: b"new report".to_vec(),
                        }],
                    }
                }
            })
            .await;
        assert_eq!(changed.files[0].filename, "new.md");
        assert_eq!(reads.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn repeated_voice_completion_uses_source_text_after_rendered_link_removal() {
        let mut ledger = TranscriptLedger::default();
        let source = "[report](file:///tmp/report.md)";
        assert!(
            ledger
                .complete("native-item", "assistant", source)
                .is_some()
        );
        ledger.mark_posted_source_text(
            "native-item",
            source,
            vec!["**Codex**\nDownload the report".into()],
        );
        assert!(
            ledger
                .complete("native-item", "assistant", source)
                .is_none()
        );
    }

    #[test]
    fn discord_member_name_falls_back_from_nickname_to_global_name_to_username() {
        assert_eq!(
            discord_member_display_name(Some("Server Nick"), Some("Global"), "user"),
            "Server Nick"
        );
        assert_eq!(
            discord_member_display_name(None, Some("Global"), "user"),
            "Global"
        );
        assert_eq!(
            discord_member_display_name(Some("  "), None, "user"),
            "user"
        );
        assert_eq!(
            escape_markdown_header("@everyone *nick*"),
            "@everyone \\*nick\\*"
        );
    }

    #[test]
    fn transcript_attribution_keeps_user_contributors_across_item_boundaries() {
        let stats = AudioStats::default();
        let ledger = TranscriptLedger::default();
        stats.note_transcript_speakers([20]);

        let assistant_started =
            json!({"item":{"type":"transcriptSegment","id":"assistant-item","role":"assistant"}});
        assert_eq!(
            transcript_attribution_item_id(
                "thread/realtime/item/started",
                &assistant_started,
                &ledger
            ),
            None
        );
        assert_eq!(stats.start_transcript_item("user-item"), vec![20]);
        stats.note_transcript_speakers([10, 30]);
        assert_eq!(stats.ensure_transcript_item("user-item"), vec![10, 20, 30]);

        let assistant_completed =
            json!({"item":{"type":"agentMessage","id":"assistant-item","text":"reply"}});
        assert_eq!(
            transcript_attribution_item_id("item/completed", &assistant_completed, &ledger),
            None
        );
        assert_eq!(stats.ensure_transcript_item("user-item"), vec![10, 20, 30]);

        let user_completed = json!({"item":{"type":"transcriptSegment","id":"user-item","role":"user","text":"spoken"}});
        assert_eq!(
            transcript_attribution_item_id(
                "thread/realtime/item/completed",
                &user_completed,
                &ledger
            ),
            Some("user-item")
        );
        assert_eq!(stats.finish_transcript_item("user-item"), vec![10, 20, 30]);

        stats.note_transcript_speakers([40]);
        let completion_only = json!({"item":{"type":"transcriptSegment","id":"completion-only","role":"user","text":"late completion"}});
        let source_id = transcript_attribution_item_id(
            "thread/realtime/item/completed",
            &completion_only,
            &ledger,
        )
        .unwrap();
        assert_eq!(stats.finish_transcript_item(source_id), vec![40]);
    }

    #[test]
    fn long_canonical_transcript_is_split_without_losing_unicode_text() {
        let text = "음성 transcript 🙂".repeat(300);
        let item = TranscriptItem {
            source_id: "long-item".into(),
            native_source_id: Some("long-item".into()),
            role: "assistant".into(),
            text: text.clone(),
            status: TranscriptStatus::Complete,
            discord_message_ids: vec![],
            speaker_ids: Vec::new(),
            agent_label: "Example Agent".into(),
            speaker_labels: Vec::new(),
            posted_content: None,
        };
        let pages = transcript_content_pages(&item);
        assert!(pages.len() > 1);
        assert!(pages.iter().all(|page| page.chars().count() <= 2000));
        let recovered: String = pages
            .iter()
            .map(|page| page.split_once('\n').unwrap().1)
            .collect();
        assert_eq!(recovered, text);
    }

    #[test]
    fn join_planning_is_idempotent_and_does_not_move_an_active_call() {
        let channel = ChannelId::new(40);
        assert_eq!(
            plan_voice_join(None, channel, Language::English).unwrap(),
            VoiceJoinAction::Start
        );
        assert_eq!(
            plan_voice_join(Some(channel), channel, Language::English).unwrap(),
            VoiceJoinAction::AlreadyConnected
        );
        assert!(plan_voice_join(Some(channel), ChannelId::new(41), Language::English).is_err());
    }

    #[test]
    fn voice_join_is_limited_by_operator_server_and_channel_ids() {
        let config = Config {
            language: Language::English,
            profile: "test".into(),
            name: "test".into(),
            token_env: "TOKEN".into(),
            credentials_file: None,
            guild_id: 20,
            home_channel_id: 21,
            transcript_channel_id: 22,
            operator_ids: vec![30],
            message_access: None,
            voice_channel_ids: vec![40],
            codex_path: "codex".into(),
            codex_home: None,
            project_dir: ".".into(),
            state_dir: "state".into(),
            model: None,
            effort: None,
            service_tier: None,
            memory_enabled: None,
            permissions: None,
            voice: None,
            worker_path: None,
            input_mode: Default::default(),
        };
        assert!(authorize_voice_channel(&config, GuildId::new(20), ChannelId::new(40)).is_ok());
        assert!(authorize_voice_channel(&config, GuildId::new(23), ChannelId::new(40)).is_err());
        assert!(authorize_voice_channel(&config, GuildId::new(20), ChannelId::new(41)).is_err());
        assert!(
            authorize_voice_join(
                &config,
                GuildId::new(20),
                ChannelId::new(40),
                UserId::new(30)
            )
            .is_ok()
        );
        assert!(
            authorize_voice_join(
                &config,
                GuildId::new(20),
                ChannelId::new(41),
                UserId::new(30)
            )
            .is_err()
        );
        assert!(
            authorize_voice_join(
                &config,
                GuildId::new(20),
                ChannelId::new(40),
                UserId::new(31)
            )
            .is_err()
        );
        assert!(
            authorize_voice_join(
                &config,
                GuildId::new(23),
                ChannelId::new(40),
                UserId::new(30)
            )
            .is_err()
        );
    }
}
