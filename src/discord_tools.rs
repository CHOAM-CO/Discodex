use crate::i18n::{Language, MessageKey};
use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};
use serenity::all::{
    Channel, ChannelId, ChannelType, CreateAllowedMentions, CreateMessage, GetMessages,
    GuildChannel, GuildId, Http, Member, Message, MessageId, Permissions, PrivateChannel, UserId,
};

use crate::config::Config;

#[derive(Clone, PartialEq, Eq)]
enum Pick {
    Channel {
        id: ChannelId,
        name: String,
        parent: Option<ChannelId>,
    },
    User {
        id: UserId,
        label: String,
    },
    Attachment {
        message_id: MessageId,
        attachment_id: u64,
        filename: String,
    },
    After(UserId),
}
#[derive(Default)]
struct SelectionMap(HashMap<String, Pick>);

enum MessageChannel {
    Guild(GuildChannel),
    Dm(PrivateChannel),
}

impl MessageChannel {
    fn id(&self) -> ChannelId {
        match self {
            Self::Guild(channel) => channel.id,
            Self::Dm(channel) => channel.id,
        }
    }

    fn guild_id(&self) -> u64 {
        match self {
            Self::Guild(channel) => channel.guild_id.get(),
            Self::Dm(_) => 0,
        }
    }
}

impl SelectionMap {
    fn insert(&mut self, pick: Pick) -> Result<String> {
        for (key, existing) in &self.0 {
            if *existing == pick {
                return Ok(key.clone());
            }
        }
        let key = format!("choice_{}", uuid::Uuid::new_v4().simple());
        self.0.insert(key.clone(), pick);
        Ok(key)
    }

    fn lookup(&self, key: &str) -> Option<Pick> {
        self.0.get(key).cloned()
    }
}

#[derive(Clone)]
pub struct DiscordTools {
    config: Arc<Config>,
    http: Arc<Http>,
    selections: Arc<Mutex<SelectionMap>>,
}

impl DiscordTools {
    pub fn new(config: Arc<Config>, http: Arc<Http>) -> Self {
        Self {
            config,
            http,
            selections: Arc::new(Mutex::new(SelectionMap::default())),
        }
    }

    pub async fn invoke(&self, name: &str, args: Value) -> Result<Value> {
        let result: Result<Value> = async {
            match name {
                "discord_list_channels" => {
                    validate_keys(&args, &[], self.config.language)?;
                    self.list_channels().await
                }
                "discord_list_users" => self.list_users(&args).await,
                "discord_send_message" => self.send_message(&args).await,
                "discord_get_messages" => self.get_messages(&args).await,
                "discord_search_messages" => self.search_messages(&args).await,
                "discord_download_attachment" => self.download_attachment(&args).await,
                _ => bail!(
                    "{}",
                    localized_template(
                        self.config.language,
                        MessageKey::DiscordToolsUnknownTool,
                        &[("tool", name.to_owned())]
                    )
                ),
            }
        }
        .await;
        result.map_err(|error| {
            anyhow::anyhow!(crate::discord::sanitize_discord_error_chain(
                &self.config,
                &error
            ))
        })
    }

    async fn guild(&self) -> Result<serenity::all::PartialGuild> {
        GuildId::new(self.config.guild_id)
            .to_partial_guild(&*self.http)
            .await
            .context("fetch configured guild")
    }

    async fn current_bot_member(&self) -> Result<Member> {
        let bot_id = self
            .http
            .get_current_user()
            .await
            .context("fetch current bot user")?
            .id;
        GuildId::new(self.config.guild_id)
            .member(&*self.http, bot_id)
            .await
            .context("fetch current bot guild member")
    }

    async fn fresh_channels(
        &self,
        guild: &serenity::all::PartialGuild,
    ) -> Result<Vec<GuildChannel>> {
        let mut channels = self
            .http
            .get_channels(GuildId::new(self.config.guild_id))
            .await
            .context("list configured-guild channels")?;
        channels.extend(
            guild
                .get_active_threads(&*self.http)
                .await
                .context("list active guild threads")?
                .threads,
        );
        Ok(channels)
    }

    fn save_pick(&self, pick: Pick) -> Result<String> {
        let mut map = self.selections.lock().map_err(|_| {
            anyhow::anyhow!(
                "{}",
                self.config
                    .language
                    .message(MessageKey::DiscordToolsSelectionStateUnavailable)
            )
        })?;
        map.insert(pick)
    }

    fn get_pick(&self, key: &str) -> Result<Pick> {
        self.selections
            .lock()
            .map_err(|_| {
                anyhow::anyhow!(
                    "{}",
                    self.config
                        .language
                        .message(MessageKey::DiscordToolsSelectionStateUnavailable)
                )
            })?
            .lookup(key)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{}",
                    self.config
                        .language
                        .message(MessageKey::DiscordToolsUnknownSelection)
                )
            })
    }

    async fn permissions(&self, channel: &GuildChannel) -> Result<Permissions> {
        if channel.guild_id.get() != self.config.guild_id {
            bail!(
                "{}",
                self.config
                    .language
                    .message(MessageKey::DiscordToolsChannelOutsideGuild)
            );
        }
        let guild = self.guild().await?;
        let bot_member = self
            .current_bot_member()
            .await
            .context("fetch current bot guild member")?;
        let channels = self.fresh_channels(&guild).await?;
        let permissions = permissions_in(
            &guild,
            &bot_member,
            channel,
            &channels,
            self.config.language,
        )?;
        Ok(permissions)
    }

    async fn resolve_channel(&self, args: &Value) -> Result<GuildChannel> {
        let guild = self.guild().await?;
        let all = self.fresh_channels(&guild).await?;
        let bot_member = self
            .current_bot_member()
            .await
            .context("fetch current bot member for channel resolution")?;
        if let Some(raw_thread_id) = args.get("thread_id").and_then(Value::as_str) {
            let thread_id = raw_thread_id
                .parse::<u64>()
                .ok()
                .filter(|id| *id != 0)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "{}",
                        self.config
                            .language
                            .message(MessageKey::DiscordToolsThreadIdPositive)
                    )
                })?;
            let channel = self
                .http
                .get_channel(ChannelId::new(thread_id))
                .await
                .context("fetch selected Discord thread")?
                .guild()
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "{}",
                        self.config
                            .language
                            .message(MessageKey::DiscordToolsThreadIdNotGuildThread)
                    )
                })?;
            if channel.guild_id.get() != self.config.guild_id {
                bail!(
                    "{}",
                    self.config
                        .language
                        .message(MessageKey::DiscordToolsThreadOutsideGuild)
                )
            }
            if !thread(&channel) {
                bail!(
                    "{}",
                    self.config
                        .language
                        .message(MessageKey::DiscordToolsThreadNotThread)
                )
            }
            if !permissions_in(&guild, &bot_member, &channel, &all, self.config.language)?
                .contains(Permissions::VIEW_CHANNEL)
            {
                bail!(
                    "{}",
                    self.config
                        .language
                        .message(MessageKey::DiscordToolsThreadNotViewable)
                )
            }
            return Ok(channel);
        }
        if let Some(key) = args.get("channel_choice").and_then(Value::as_str) {
            let Pick::Channel { id, name, parent } = self.get_pick(key)? else {
                bail!(
                    "{}",
                    self.config
                        .language
                        .message(MessageKey::DiscordToolsChoiceNotChannel)
                )
            };
            let channel = all.iter().find(|c| c.id == id).cloned().ok_or_else(|| {
                anyhow::anyhow!(
                    "{}",
                    self.config
                        .language
                        .message(MessageKey::DiscordToolsChannelChoiceStale)
                )
            })?;
            if channel.name != name || channel.parent_id != parent {
                bail!(
                    "{}",
                    self.config
                        .language
                        .message(MessageKey::DiscordToolsChannelRenamedOrMoved)
                );
            }
            if !permissions_in(&guild, &bot_member, &channel, &all, self.config.language)?
                .contains(Permissions::VIEW_CHANNEL)
            {
                bail!(
                    "{}",
                    self.config
                        .language
                        .message(MessageKey::DiscordToolsChannelNotViewable)
                );
            }
            return Ok(channel);
        }
        let query = channel_name_query(required(args, "channel", self.config.language)?);
        if query.trim().is_empty() {
            bail!(
                "{}",
                self.config
                    .language
                    .message(MessageKey::DiscordToolsChannelNameRequired)
            )
        }
        let (category, leaf) = query
            .split_once('/')
            .map(|(a, b)| (Some(a), b))
            .unwrap_or((None, query));
        let categories: HashMap<ChannelId, String> = all
            .iter()
            .filter(|c| c.kind == ChannelType::Category)
            .map(|c| (c.id, c.name.clone()))
            .collect();
        let mut matches = Vec::new();
        for channel in matching_named_channels(&all, &categories, category, leaf, None) {
            if !permissions_in(&guild, &bot_member, channel, &all, self.config.language)?
                .contains(Permissions::VIEW_CHANNEL)
            {
                continue;
            }
            matches.push(channel.clone());
        }
        match matches.as_slice() {
            [] => bail!(
                "{}",
                localized_template(
                    self.config.language,
                    MessageKey::DiscordToolsNoChannelMatches,
                    &[("query", format!("{query:?}"))]
                )
            ),
            [one] => Ok(one.clone()),
            many => {
                let mut candidates = Vec::new();
                for c in many {
                    let cat = c.parent_id.and_then(|id| categories.get(&id)).cloned();
                    candidates.push(json!({"name":cat.as_ref().map(|x|format!("{x}/{}",c.name)).unwrap_or_else(||c.name.clone()),"type":format!("{:?}",c.kind),"choice":self.save_pick(Pick::Channel{id:c.id,name:c.name.clone(),parent:c.parent_id})?}));
                }
                Ok(json!({"error":self.config.language.message(MessageKey::DiscordToolsAmbiguousChannelName),"candidates":candidates}))
                    .and_then(|v| Err(anyhow::anyhow!("{v}")))
            }
        }
    }

    async fn resolve_message_channel(&self, args: &Value) -> Result<MessageChannel> {
        if let Some(dm_id) = args.get("dm_id") {
            let id = selected_channel_id(dm_id, "dm_id", self.config.language)?;
            let channel = self
                .http
                .get_channel(id)
                .await
                .context("fetch selected DM")?;
            return match channel {
                Channel::Private(channel) => Ok(MessageChannel::Dm(channel)),
                _ => bail!(
                    "{}",
                    self.config
                        .language
                        .message(MessageKey::DiscordToolsDmIdNotDm)
                ),
            };
        }
        self.resolve_channel(args).await.map(MessageChannel::Guild)
    }

    async fn message_channel_permissions(
        &self,
        channel: &MessageChannel,
    ) -> Result<Option<Permissions>> {
        match channel {
            MessageChannel::Guild(channel) => self.permissions(channel).await.map(Some),
            MessageChannel::Dm(_) => Ok(None),
        }
    }

    async fn require_message_read_permissions(&self, channel: &MessageChannel) -> Result<()> {
        if let MessageChannel::Guild(channel) = channel {
            require_read_permissions(
                channel.kind,
                self.permissions(channel).await?,
                self.config.language,
            )?;
        }
        Ok(())
    }

    async fn message_channel_context(&self, channel: &MessageChannel) -> Result<Value> {
        match channel {
            MessageChannel::Guild(channel) => {
                let mut context = json!({"server":self.guild().await?.name,"channel":channel.name});
                if thread(channel) {
                    context["thread"] = thread_context(channel);
                }
                Ok(context)
            }
            MessageChannel::Dm(channel) => {
                Ok(json!({"channel":"DM","dm_id":channel.id.get().to_string()}))
            }
        }
    }

    fn resolve_dm_mention(&self, value: &Value, channel: &PrivateChannel) -> Result<UserId> {
        let query = user_name_query(value.as_str().ok_or_else(|| {
            anyhow::anyhow!(
                "{}",
                self.config
                    .language
                    .message(MessageKey::DiscordToolsUserNamesMustBeStrings)
            )
        })?);
        let recipient = &channel.recipient;
        if is_choice(query) {
            if let Pick::User { id, .. } = self.get_pick(query)? {
                if id == recipient.id {
                    return Ok(id);
                }
            }
        } else if name_matches(
            &recipient.name,
            recipient.global_name.as_deref(),
            None,
            query,
        ) {
            return Ok(recipient.id);
        }
        bail!(
            "{}",
            localized_template(
                self.config.language,
                MessageKey::DiscordToolsNoDmRecipientMatch,
                &[("query", format!("{query:?}"))]
            )
        )
    }

    pub(crate) async fn resolve_voice_channel(&self, args: &Value) -> Result<GuildChannel> {
        validate_voice_channel_selection(args, self.config.language)?;
        let guild = self.guild().await?;
        let all = self.fresh_channels(&guild).await?;
        let bot_member = self
            .current_bot_member()
            .await
            .context("fetch current bot member for voice-channel resolution")?;

        if let Some(key) = args.get("channel_choice").and_then(Value::as_str) {
            let Pick::Channel { id, name, parent } = self.get_pick(key)? else {
                bail!(
                    "{}",
                    self.config
                        .language
                        .message(MessageKey::DiscordToolsChoiceNotChannel)
                )
            };
            let channel = all.iter().find(|channel| channel.id == id).ok_or_else(|| {
                anyhow::anyhow!(
                    "{}",
                    self.config
                        .language
                        .message(MessageKey::DiscordToolsChannelChoiceStale)
                )
            })?;
            if channel.name != name || channel.parent_id != parent {
                bail!(
                    "{}",
                    self.config
                        .language
                        .message(MessageKey::DiscordToolsChannelRenamedOrMoved)
                );
            }
            require_voice_channel(channel, self.config.language)?;
            if !permissions_in(&guild, &bot_member, channel, &all, self.config.language)?
                .contains(Permissions::VIEW_CHANNEL)
            {
                bail!(
                    "{}",
                    self.config
                        .language
                        .message(MessageKey::DiscordToolsVoiceChannelNotViewable)
                );
            }
            return Ok(channel.clone());
        }

        let query = channel_name_query(required(args, "channel", self.config.language)?);
        if query.trim().is_empty() {
            bail!(
                "{}",
                self.config
                    .language
                    .message(MessageKey::DiscordToolsChannelNameRequired)
            );
        }
        let (category, leaf) = query
            .split_once('/')
            .map(|(a, b)| (Some(a), b))
            .unwrap_or((None, query));
        let categories: HashMap<ChannelId, String> = all
            .iter()
            .filter(|channel| channel.kind == ChannelType::Category)
            .map(|channel| (channel.id, channel.name.clone()))
            .collect();
        let mut matches = Vec::new();
        for channel in
            matching_named_channels(&all, &categories, category, leaf, Some(ChannelType::Voice))
        {
            if !permissions_in(&guild, &bot_member, channel, &all, self.config.language)?
                .contains(Permissions::VIEW_CHANNEL)
            {
                continue;
            }
            matches.push(channel.clone());
        }
        match matches.as_slice() {
            [] => bail!(
                "{}",
                localized_template(
                    self.config.language,
                    MessageKey::DiscordToolsNoVoiceChannelMatches,
                    &[("query", format!("{query:?}"))]
                )
            ),
            [one] => Ok(one.clone()),
            many => {
                let mut candidates = Vec::new();
                for channel in many {
                    let category = channel
                        .parent_id
                        .and_then(|id| categories.get(&id))
                        .cloned();
                    candidates.push(json!({
                        "name": category.as_ref().map(|name| format!("{name}/{}", channel.name)).unwrap_or_else(|| channel.name.clone()),
                        "type": format!("{:?}", channel.kind),
                        "choice": self.save_pick(Pick::Channel { id: channel.id, name: channel.name.clone(), parent: channel.parent_id })?
                    }));
                }
                bail!(
                    "{}",
                    json!({"error":self.config.language.message(MessageKey::DiscordToolsAmbiguousVoiceChannelName),"candidates":candidates})
                );
            }
        }
    }

    async fn list_channels(&self) -> Result<Value> {
        let guild = self.guild().await?;
        let bot_member = self
            .current_bot_member()
            .await
            .context("fetch current bot member for channel listing")?;
        let all = self.fresh_channels(&guild).await?;
        let cats: HashMap<ChannelId, String> = all
            .iter()
            .filter(|c| c.kind == ChannelType::Category)
            .map(|c| (c.id, c.name.clone()))
            .collect();
        let mut out = Vec::new();
        for c in all.iter().filter(|c| c.kind != ChannelType::Category) {
            if !permissions_in(&guild, &bot_member, c, &all, self.config.language)?
                .contains(Permissions::VIEW_CHANNEL)
            {
                continue;
            }
            let cat = c.parent_id.and_then(|id| cats.get(&id)).cloned();
            let name = cat
                .as_ref()
                .map(|x| format!("{x}/{}", c.name))
                .unwrap_or_else(|| c.name.clone());
            let mut item = json!({"name":name,"channel":c.name,"category":cat,"home":c.id.get() == self.config.home_channel_id,"type":format!("{:?}",c.kind),"choice":self.save_pick(Pick::Channel{id:c.id,name:c.name.clone(),parent:c.parent_id})?});
            if c.kind == ChannelType::Voice {
                item["voice_join_allowed"] =
                    json!(self.config.voice_channel_ids.contains(&c.id.get()));
            }
            if thread(c) {
                item["thread_id"] = json!(c.id.get().to_string());
            }
            out.push(item);
        }
        Ok(
            json!({"server":guild.name,"channels":out,"scope":"currently visible guild channels and active threads","archived_threads_included":false}),
        )
    }

    async fn list_users(&self, args: &Value) -> Result<Value> {
        validate_keys(args, &["limit", "page"], self.config.language)?;
        if args
            .get("page")
            .is_some_and(|v| v.as_str().is_none_or(|s| !is_choice(s)))
        {
            bail!(
                "{}",
                self.config
                    .language
                    .message(MessageKey::DiscordToolsPageTokenRequired)
            )
        }
        let limit = bounded(args, "limit", 50, 1, 1000, self.config.language)?;
        let after = match args.get("page").and_then(Value::as_str) {
            Some(k) => match self.get_pick(k)? {
                Pick::After(id) => Some(id),
                _ => bail!(
                    "{}",
                    self.config
                        .language
                        .message(MessageKey::DiscordToolsMemberPageInvalid)
                ),
            },
            None => None,
        };
        let guild = self.guild().await?;
        let page = guild
            .members(&*self.http, Some(limit), after)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "{}",
                    localized_template(
                        self.config.language,
                        MessageKey::DiscordToolsMemberListIntent,
                        &[("error", e.to_string())]
                    )
                )
            })?;
        let mut users = Vec::new();
        for m in &page {
            users.push(user_value(
                m,
                self.save_pick(Pick::User {
                    id: m.user.id,
                    label: label(m),
                })?,
            ));
        }
        let next = if page.len() as u64 == limit {
            page.last()
                .map(|m| self.save_pick(Pick::After(m.user.id)))
                .transpose()?
        } else {
            None
        };
        Ok(json!({"server":guild.name,"users":users,"next_page":next,"page_size":limit}))
    }

    async fn list_all_members(&self) -> Result<Vec<Member>> {
        let guild = self.guild().await?;
        let mut after = None;
        let mut all = Vec::new();
        loop {
            let page = guild
                .members(&*self.http, Some(1000), after)
                .await
                .map_err(|e| {
                    anyhow::anyhow!(
                        "{}",
                        localized_template(
                            self.config.language,
                            MessageKey::DiscordToolsMemberResolveIntent,
                            &[("error", e.to_string())]
                        )
                    )
                })?;
            if page.is_empty() {
                break;
            }
            after = page.last().map(|m| m.user.id);
            let done = page.len() < 1000;
            all.extend(page);
            if done {
                break;
            }
        }
        Ok(all)
    }

    fn resolve_user(&self, value: &Value, members: &[Member]) -> Result<Member> {
        let raw_query = value.as_str().ok_or_else(|| {
            anyhow::anyhow!(
                "{}",
                self.config
                    .language
                    .message(MessageKey::DiscordToolsUserNamesMustBeStrings)
            )
        })?;
        let query = user_name_query(raw_query);
        if is_choice(query) {
            if let Pick::User { id, label: old } = self.get_pick(query)? {
                let member = members.iter().find(|m| m.user.id == id).ok_or_else(|| {
                    anyhow::anyhow!(
                        "{}",
                        self.config
                            .language
                            .message(MessageKey::DiscordToolsUserLeftGuild)
                    )
                })?;
                if label(member) != old {
                    bail!(
                        "{}",
                        self.config
                            .language
                            .message(MessageKey::DiscordToolsUserNameChanged)
                    )
                }
                return Ok(member.clone());
            }
            bail!(
                "{}",
                self.config
                    .language
                    .message(MessageKey::DiscordToolsChoiceNotUser)
            )
        }
        let matches: Vec<Member> = members
            .iter()
            .filter(|m| matches_name(m, query))
            .cloned()
            .collect();
        match matches.as_slice() {
            [] => bail!(
                "{}",
                localized_template(
                    self.config.language,
                    MessageKey::DiscordToolsNoMemberMatches,
                    &[("query", format!("{query:?}"))]
                )
            ),
            [one] => Ok(one.clone()),
            many => {
                let mut candidates = Vec::new();
                for m in many {
                    candidates.push(json!({"name":label(m),"username":m.user.name,"nickname":m.nick,"global_name":m.user.global_name,"choice":self.save_pick(Pick::User{id:m.user.id,label:label(m)})?}));
                }
                bail!(
                    "{}",
                    json!({"error":self.config.language.message(MessageKey::DiscordToolsAmbiguousUserName),"candidates":candidates})
                );
            }
        }
    }

    async fn send_message(&self, args: &Value) -> Result<Value> {
        validate_keys(
            args,
            &[
                "channel",
                "channel_choice",
                "thread_id",
                "dm_id",
                "content",
                "mentions",
                "files",
            ],
            self.config.language,
        )?;
        validate_channel_selection(args, self.config.language)?;
        let file_paths = explicit_file_paths(args, self.config.language)?;
        let raw_content = args
            .get("content")
            .map(|value| {
                value.as_str().ok_or_else(|| {
                    anyhow::anyhow!(
                        "{}",
                        self.config
                            .language
                            .message(MessageKey::DiscordToolsContentMustBeString)
                    )
                })
            })
            .transpose()?
            .unwrap_or("");
        let files = crate::reply_files::prepare_explicit_files(&self.config, &file_paths).await?;
        validate_send_payload(raw_content, files.len(), self.config.language)?;
        if args.get("mentions").is_some_and(|v| !v.is_array()) {
            bail!(
                "{}",
                self.config
                    .language
                    .message(MessageKey::DiscordToolsMentionsMustBeArray)
            )
        }
        if let Some(names) = args.get("mentions").and_then(Value::as_array) {
            for name in names {
                if name.as_str().is_none_or(|s| s.trim().is_empty()) {
                    bail!(
                        "{}",
                        self.config
                            .language
                            .message(MessageKey::DiscordToolsMentionItemNonempty)
                    )
                }
            }
        }
        let channel = self.resolve_message_channel(args).await?;
        let perms = self.message_channel_permissions(&channel).await?;
        if let (MessageChannel::Guild(channel), Some(perms)) = (&channel, perms) {
            if !perms.contains(Permissions::VIEW_CHANNEL) {
                bail!(
                    "{}",
                    self.config
                        .language
                        .message(MessageKey::DiscordToolsBotLacksViewChannel)
                )
            }
            let needed = if thread(channel) {
                Permissions::SEND_MESSAGES_IN_THREADS
            } else {
                Permissions::SEND_MESSAGES
            };
            if !perms.contains(needed) {
                bail!(
                    "{}",
                    self.config
                        .language
                        .message(MessageKey::DiscordToolsBotLacksSendPermission)
                )
            }
        }
        let mention_names = args.get("mentions").and_then(Value::as_array);
        let members = if matches!(channel, MessageChannel::Guild(_))
            && mention_names.is_some_and(|names| !names.is_empty())
        {
            self.list_all_members().await?
        } else {
            Vec::new()
        };
        let mut users = Vec::new();
        if let Some(names) = mention_names {
            for name in names {
                let id = match &channel {
                    MessageChannel::Guild(_) => self.resolve_user(name, &members)?.user.id,
                    MessageChannel::Dm(channel) => self.resolve_dm_mention(name, channel)?,
                };
                if !users.contains(&id) {
                    users.push(id);
                }
            }
        }
        if !files.is_empty()
            && perms.is_some_and(|perms| !perms.contains(Permissions::ATTACH_FILES))
        {
            bail!(
                "{}",
                self.config
                    .language
                    .message(MessageKey::DiscordToolsBotLacksAttachPermission)
            )
        }
        let content = prepend_resolved_mentions(raw_content, &users);
        let prepared = if perms.is_none_or(|perms| perms.contains(Permissions::ATTACH_FILES)) {
            crate::table_messages::prepare(&content, files, self.config.language).await
        } else {
            crate::table_messages::prepare_plain(&content, files)
        };
        let context = self.message_channel_context(&channel).await?;
        let message_count = prepared.len().max(1);
        let mut pending_messages = prepared.into_iter();
        let mut sent_messages = Vec::with_capacity(message_count);
        let mut all_attachments = Vec::new();
        let mut send_error = None;
        for index in 0..message_count {
            let Some(prepared) = pending_messages.next() else {
                break;
            };
            let body = prepared.content.clone();
            let mentions = CreateAllowedMentions::new()
                .everyone(false)
                .all_roles(false)
                .all_users(false)
                .empty_roles()
                .users(users.clone());
            let builder = prepared.into_builder(
                CreateMessage::new()
                    .content(body)
                    .allowed_mentions(mentions),
            );
            let sent = match channel
                .id()
                .send_message(&*self.http, builder)
                .await
                .with_context(|| {
                    format!(
                        "send Discord message chunk {} of {message_count}",
                        index + 1
                    )
                }) {
                Ok(sent) => sent,
                Err(error) => {
                    send_error = Some(crate::discord::sanitize_discord_error(
                        &self.config,
                        &format!("{error:#}"),
                    ));
                    break;
                }
            };
            let link = message_link(channel.guild_id(), channel.id().get(), sent.id.get());
            let attachments = sent
                .attachments
                .iter()
                .map(|attachment| {
                    let item = json!({
                        "filename": attachment.filename,
                        "size": attachment.size,
                        "url": attachment.url,
                        "message_link": link,
                    });
                    all_attachments.push(item.clone());
                    item
                })
                .collect::<Vec<_>>();
            sent_messages.push(json!({"message_link":link,"attachments":attachments}));
        }
        let first_link = sent_messages
            .first()
            .and_then(|message| message.get("message_link"))
            .cloned()
            .unwrap_or(Value::Null);
        let status = match (send_error.is_some(), sent_messages.is_empty()) {
            (false, _) => "sent",
            (true, true) => "failed",
            (true, false) => "partial",
        };
        let mut result = json!({"status":status,"message_link":first_link,"message_links":sent_messages.iter().filter_map(|message| message.get("message_link").cloned()).collect::<Vec<_>>(),"messages_sent":sent_messages.len(),"messages_requested":message_count,"messages":sent_messages,"attachments":all_attachments});
        result
            .as_object_mut()
            .expect("message result object")
            .extend(context.as_object().expect("channel context object").clone());
        if let Some(error) = send_error {
            result["error"] = json!(error);
        }
        Ok(result)
    }

    async fn messages_before(
        &self,
        channel: ChannelId,
        mut before: Option<MessageId>,
        count: usize,
    ) -> Result<Vec<Message>> {
        let mut out = Vec::new();
        while out.len() < count {
            let page_limit = (count - out.len()).min(100) as u8;
            let mut request = GetMessages::new().limit(page_limit);
            if let Some(cursor) = before {
                request = request.before(cursor);
            }
            let page = channel
                .messages(&*self.http, request)
                .await
                .context("read Discord message page")?;
            if page.is_empty() {
                break;
            }
            let page_len = page.len();
            before = page.last().map(|message| message.id);
            out.extend(page);
            if page_len < page_limit as usize {
                break;
            }
        }
        Ok(out)
    }

    async fn messages_after(
        &self,
        channel: ChannelId,
        after: MessageId,
        count: usize,
    ) -> Result<Vec<Message>> {
        if count == 0 {
            return Ok(Vec::new());
        }
        let page_limit = count.min(100) as u8;
        let page = channel
            .messages(
                &*self.http,
                GetMessages::new().limit(page_limit).after(after),
            )
            .await
            .context("read Discord messages after cursor")?;
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for message in page.iter() {
            if message.id.get() > after.get() && seen.insert(message.id.get()) {
                out.push(message.clone());
            }
        }
        let mut after_cursor = page
            .iter()
            .map(|message| message.id.get())
            .filter(|id| *id > after.get())
            .max()
            .map(MessageId::new);
        if page.len() < page_limit as usize {
            out.truncate(count);
            return Ok(out);
        }
        while out.len() < count {
            let Some(cursor) = after_cursor else {
                break;
            };
            let n = (count - out.len()).min(100) as u8;
            let page = channel
                .messages(&*self.http, GetMessages::new().limit(n).after(cursor))
                .await
                .context("read next Discord messages after cursor")?;
            if page.is_empty() {
                break;
            }
            let page_len = page.len();
            let ids: Vec<_> = page.iter().map(|message| message.id.get()).collect();
            let progress = after_page_progress(cursor.get(), after.get(), &ids, &mut seen);
            if !progress.advanced {
                break;
            }
            for message in page {
                if progress.accepted_ids.contains(&message.id.get()) {
                    out.push(message);
                    if out.len() == count {
                        break;
                    }
                }
            }
            after_cursor = progress.next_after.map(MessageId::new);
            if page_len < n as usize {
                break;
            }
        }
        out.sort_by_key(|message| std::cmp::Reverse(message.id.get()));
        out.truncate(count);
        Ok(out)
    }

    async fn get_messages(&self, args: &Value) -> Result<Value> {
        validate_keys(
            args,
            &[
                "channel",
                "channel_choice",
                "thread_id",
                "dm_id",
                "limit",
                "before",
                "after",
                "around",
                "offset",
            ],
            self.config.language,
        )?;
        validate_channel_selection(args, self.config.language)?;
        let limit = requested_count(args, "limit", 50, self.config.language)?;
        if limit == 0 {
            bail!(
                "{}",
                self.config
                    .language
                    .message(MessageKey::DiscordToolsLimitAtLeastOne)
            )
        }
        let before_link = link_arg(args, "before", self.config.language)?;
        let after_link = link_arg(args, "after", self.config.language)?;
        let around_link = link_arg(args, "around", self.config.language)?;
        if [before_link, after_link, around_link]
            .iter()
            .filter(|v| v.is_some())
            .count()
            > 1
        {
            bail!(
                "{}",
                self.config
                    .language
                    .message(MessageKey::DiscordToolsOneHistoryCursor)
            )
        }
        let offset = requested_count(args, "offset", 0, self.config.language)?;
        if offset > 0 && (after_link.is_some() || around_link.is_some()) {
            bail!(
                "{}",
                self.config
                    .language
                    .message(MessageKey::DiscordToolsOffsetCursorConflict)
            )
        }
        let c = self.resolve_message_channel(args).await?;
        let before = before_link
            .map(|link| parse_message_link(link, c.guild_id(), c.id().get(), self.config.language))
            .transpose()?;
        let after = after_link
            .map(|link| parse_message_link(link, c.guild_id(), c.id().get(), self.config.language))
            .transpose()?;
        let around = around_link
            .map(|link| parse_message_link(link, c.guild_id(), c.id().get(), self.config.language))
            .transpose()?;
        self.require_message_read_permissions(&c).await?;
        let mut offset = offset;
        let mut cursor = before;
        if offset > 0 && after.is_none() && around.is_none() {
            while offset > 0 {
                let n = offset.min(100) as u8;
                let mut req = GetMessages::new().limit(n);
                if let Some(id) = cursor {
                    req = req.before(id);
                }
                let page = c
                    .id()
                    .messages(&*self.http, req)
                    .await
                    .context("skip recent messages")?;
                if page.is_empty() {
                    let mut result = self.message_channel_context(&c).await?;
                    result["messages"] = json!([]);
                    result["next_before"] = Value::Null;
                    result["next_after"] = Value::Null;
                    result["next_cursor"] = Value::Null;
                    result["history_exhausted"] = json!(true);
                    return Ok(result);
                }
                offset = offset.saturating_sub(page.len());
                cursor = page.last().map(|m| m.id);
            }
        }
        let messages = if let Some(anchor) = around {
            if limit <= 100 {
                c.id()
                    .messages(
                        &*self.http,
                        GetMessages::new().limit(limit as u8).around(anchor),
                    )
                    .await
                    .context("read Discord messages around cursor")?
            } else {
                let seed = c
                    .id()
                    .messages(&*self.http, GetMessages::new().limit(100).around(anchor))
                    .await
                    .context("read Discord messages around cursor")?;
                if seed.is_empty() {
                    Vec::new()
                } else {
                    let extra = limit.saturating_sub(seed.len());
                    let older_count = extra.div_ceil(2);
                    let newer_count = extra / 2;
                    let older = if older_count > 0 {
                        self.messages_before(c.id(), seed.last().map(|m| m.id), older_count)
                            .await?
                    } else {
                        Vec::new()
                    };
                    let newer = if newer_count > 0 {
                        match seed.first().map(|m| m.id) {
                            Some(id) => self.messages_after(c.id(), id, newer_count).await?,
                            None => Vec::new(),
                        }
                    } else {
                        Vec::new()
                    };
                    newer
                        .into_iter()
                        .chain(seed)
                        .chain(older)
                        .take(limit)
                        .collect()
                }
            }
        } else if let Some(anchor) = after {
            self.messages_after(c.id(), anchor, limit).await?
        } else {
            self.messages_before(c.id(), cursor, limit).await?
        };
        let mut result = message_cursors(
            messages.iter().map(|m| m.id),
            after.is_some(),
            c.guild_id(),
            c.id().get(),
        );
        let context = self.message_channel_context(&c).await?;
        result
            .as_object_mut()
            .expect("message cursor object")
            .extend(context.as_object().expect("channel context object").clone());
        result["messages"] = json!(
            messages
                .iter()
                .map(|m| message_json(m, c.guild_id(), c.id().get()))
                .collect::<Vec<_>>()
        );
        Ok(result)
    }

    async fn search_messages(&self, args: &Value) -> Result<Value> {
        validate_keys(
            args,
            &[
                "channel",
                "channel_choice",
                "thread_id",
                "dm_id",
                "query",
                "max_scan",
                "limit",
            ],
            self.config.language,
        )?;
        validate_channel_selection(args, self.config.language)?;
        let max = requested_count(args, "max_scan", 100, self.config.language)?;
        if max == 0 {
            bail!(
                "{}",
                self.config
                    .language
                    .message(MessageKey::DiscordToolsMaxScanAtLeastOne)
            )
        }
        let result_limit = requested_count(args, "limit", 1000, self.config.language)?;
        if result_limit == 0 {
            bail!(
                "{}",
                self.config
                    .language
                    .message(MessageKey::DiscordToolsLimitAtLeastOne)
            )
        }
        let query = required(args, "query", self.config.language)?;
        if query.is_empty() {
            bail!(
                "{}",
                self.config
                    .language
                    .message(MessageKey::DiscordToolsQueryRequired)
            )
        }
        let c = self.resolve_message_channel(args).await?;
        self.require_message_read_permissions(&c).await?;
        let mut scanned = 0usize;
        let mut before = None;
        let mut found = Vec::new();
        let mut matching_count = 0usize;
        let mut history_exhausted = false;
        while scanned < max {
            let n = (max - scanned).min(100) as u8;
            let mut req = GetMessages::new().limit(n);
            if let Some(id) = before {
                req = req.before(id)
            }
            let page = c
                .id()
                .messages(&*self.http, req)
                .await
                .context("search Discord history")?;
            if page.is_empty() {
                history_exhausted = true;
                break;
            }
            let page_len = page.len();
            scanned += page_len;
            before = page.last().map(|m| m.id);
            for m in page {
                if crate::attachments::message_parts(&m).0.contains(query) {
                    matching_count += 1;
                    if found.len() < result_limit {
                        found.push(message_json(&m, c.guild_id(), c.id().get()))
                    }
                }
            }
            if page_len < n as usize {
                history_exhausted = true;
                break;
            }
        }
        let mut result = json!({"query":query,"messages":found,"scanned":scanned,"max_scan":max,"requested_scope_complete":true,"history_exhausted":history_exhausted,"matching_count":matching_count,"truncated_matches":matching_count.saturating_sub(result_limit)});
        let context = self.message_channel_context(&c).await?;
        result
            .as_object_mut()
            .expect("message search object")
            .extend(context.as_object().expect("channel context object").clone());
        Ok(result)
    }

    async fn download_attachment(&self, args: &Value) -> Result<Value> {
        validate_keys(
            args,
            &[
                "channel",
                "channel_choice",
                "thread_id",
                "dm_id",
                "message_link",
                "filename",
                "attachment_choice",
            ],
            self.config.language,
        )?;
        validate_channel_selection(args, self.config.language)?;
        let link = required(args, "message_link", self.config.language)?;
        let filename = match args.get("filename") {
            None => None,
            Some(Value::String(value)) if !value.trim().is_empty() => Some(value.as_str()),
            Some(_) => bail!(
                "{}",
                self.config
                    .language
                    .message(MessageKey::DiscordToolsFilenameNonempty)
            ),
        };
        let choice = match args.get("attachment_choice") {
            None => None,
            Some(Value::String(value)) if is_choice(value) => Some(value.as_str()),
            Some(_) => bail!(
                "{}",
                self.config
                    .language
                    .message(MessageKey::DiscordToolsAttachmentChoiceInvalid)
            ),
        };
        match (filename, choice) {
            (Some(_), None) | (None, Some(_)) => {}
            (Some(_), Some(_)) => bail!(
                "{}",
                self.config
                    .language
                    .message(MessageKey::DiscordToolsFilenameOrChoiceExclusive)
            ),
            (None, None) => bail!(
                "{}",
                self.config
                    .language
                    .message(MessageKey::DiscordToolsFilenameOrChoiceRequired)
            ),
        }
        let c = self.resolve_message_channel(args).await?;
        let msg_id = parse_message_link(link, c.guild_id(), c.id().get(), self.config.language)?;
        self.require_message_read_permissions(&c).await?;
        let msg = c
            .id()
            .message(&*self.http, msg_id)
            .await
            .context("fetch selected message")?;
        let attachments = crate::attachments::message_parts(&msg).1;
        let att = if let Some(key) = choice {
            let Pick::Attachment {
                message_id,
                attachment_id,
                filename,
            } = self.get_pick(key)?
            else {
                bail!(
                    "{}",
                    self.config
                        .language
                        .message(MessageKey::DiscordToolsChoiceNotAttachment)
                )
            };
            if message_id != msg.id {
                bail!(
                    "{}",
                    self.config
                        .language
                        .message(MessageKey::DiscordToolsAttachmentChoiceOtherMessage)
                )
            }
            attachments
                .iter()
                .find(|a| a.id.get() == attachment_id && a.filename == filename)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "{}",
                        self.config
                            .language
                            .message(MessageKey::DiscordToolsAttachmentChanged)
                    )
                })?
        } else {
            let expected = filename.expect("validated filename");
            let mut matches = attachments.iter().filter(|a| a.filename == expected);
            let Some(one) = matches.next() else {
                bail!(
                    "{}",
                    localized_template(
                        self.config.language,
                        MessageKey::DiscordToolsNoAttachmentNamed,
                        &[("filename", format!("{expected:?}"))]
                    )
                )
            };
            if matches.next().is_some() {
                let candidates = attachments.iter().filter(|a| a.filename == expected).map(|a| {
                    Ok(json!({"filename":a.filename,"size":a.size,"choice":self.save_pick(Pick::Attachment{message_id:msg.id,attachment_id:a.id.get(),filename:a.filename.clone()})?}))
                }).collect::<Result<Vec<_>>>()?;
                bail!(
                    "{}",
                    json!({"error":self.config.language.message(MessageKey::DiscordToolsAmbiguousAttachmentFilename),"candidates":candidates})
                );
            }
            one
        };
        let result = crate::attachments::download_attachment(&self.config, att)
            .await
            .context("download selected attachment")?;
        let mut result = serde_json::to_value(result)?;
        match &c {
            MessageChannel::Guild(channel) if thread(channel) => {
                result["thread"] = thread_context(channel)
            }
            MessageChannel::Dm(channel) => result["dm_id"] = json!(channel.id.get().to_string()),
            _ => {}
        }
        Ok(result)
    }
}

fn localized_template(language: Language, key: MessageKey, values: &[(&str, String)]) -> String {
    let mut message = language.message(key).to_owned();
    for (name, value) in values {
        message = message.replace(&format!("{{{name}}}"), value);
    }
    message
}

fn require_read_permissions(
    kind: ChannelType,
    permissions: Permissions,
    language: Language,
) -> Result<()> {
    if !permissions.contains(Permissions::VIEW_CHANNEL | Permissions::READ_MESSAGE_HISTORY) {
        bail!(
            "{}",
            language.message(MessageKey::DiscordToolsNeedMessageReadPermissions)
        );
    }
    if matches!(kind, ChannelType::Voice | ChannelType::Stage)
        && !permissions.contains(Permissions::CONNECT)
    {
        bail!(
            "{}",
            language.message(MessageKey::DiscordToolsNeedVoiceConnect)
        );
    }
    Ok(())
}

fn message_cursors(
    ids: impl Iterator<Item = MessageId>,
    forward: bool,
    guild_id: u64,
    channel_id: u64,
) -> Value {
    let (oldest, newest) = ids.fold((None::<u64>, None::<u64>), |(low, high), id| {
        let id = id.get();
        (
            Some(low.map_or(id, |n| n.min(id))),
            Some(high.map_or(id, |n| n.max(id))),
        )
    });
    let before = oldest.map(|id| message_link(guild_id, channel_id, id));
    let after = newest.map(|id| message_link(guild_id, channel_id, id));
    let next = if forward {
        after.as_ref().map(|id| json!({"after":id}))
    } else {
        before.as_ref().map(|id| json!({"before":id}))
    };
    json!({"next_before":before,"next_after":after,"next_cursor":next})
}

fn validate_send_payload(content: &str, file_count: usize, language: Language) -> Result<()> {
    if content.trim().is_empty() && file_count == 0 {
        bail!(
            "{}",
            language.message(MessageKey::DiscordToolsMessageContentOrFileRequired)
        );
    }
    Ok(())
}

fn explicit_file_paths(args: &Value, language: Language) -> Result<Vec<String>> {
    let Some(files) = args.get("files") else {
        return Ok(Vec::new());
    };
    let files = files.as_array().ok_or_else(|| {
        anyhow::anyhow!(
            "{}",
            language.message(MessageKey::DiscordToolsFilesArrayOfPathsRequired)
        )
    })?;
    files
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .as_str()
                .filter(|path| !path.trim().is_empty())
                .map(str::to_owned)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "{}",
                        localized_template(
                            language,
                            MessageKey::DiscordToolsFilePathNonempty,
                            &[("index", index.to_string())]
                        )
                    )
                })
        })
        .collect()
}

fn validate_keys(v: &Value, allowed: &[&str], language: Language) -> Result<()> {
    let object = v.as_object().ok_or_else(|| {
        anyhow::anyhow!(
            "{}",
            language.message(MessageKey::DiscordToolsArgumentsObjectRequired)
        )
    })?;
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            bail!(
                "{}",
                localized_template(
                    language,
                    MessageKey::DiscordToolsUnknownArgument,
                    &[("argument", format!("{key:?}"))]
                )
            )
        }
    }
    Ok(())
}
fn validate_channel_selection(v: &Value, language: Language) -> Result<()> {
    let selectors = ["channel", "channel_choice", "thread_id", "dm_id"];
    if selectors.iter().filter(|key| v.get(*key).is_some()).count() != 1 {
        bail!(
            "{}",
            language.message(MessageKey::DiscordToolsExactlyOneChannelSelector)
        );
    }
    for key in ["thread_id", "dm_id"] {
        if let Some(id) = v.get(key) {
            selected_channel_id(id, key, language)?;
            return Ok(());
        }
    }
    if let Some(name) = v.get("channel") {
        if !name.as_str().is_some_and(|name| !name.trim().is_empty()) {
            bail!(
                "{}",
                language.message(MessageKey::DiscordToolsChannelNameStringRequired)
            );
        }
    } else if !v
        .get("channel_choice")
        .and_then(Value::as_str)
        .is_some_and(is_choice)
    {
        bail!(
            "{}",
            language.message(MessageKey::DiscordToolsChannelChoiceStringRequired)
        );
    }
    Ok(())
}

fn selected_channel_id(value: &Value, key: &str, language: Language) -> Result<ChannelId> {
    value
        .as_str()
        .and_then(|id| id.parse::<u64>().ok())
        .filter(|id| *id != 0)
        .map(ChannelId::new)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{}",
                localized_template(
                    language,
                    MessageKey::DiscordToolsChannelIdPositive,
                    &[("field", key.to_owned())]
                )
            )
        })
}
fn required<'a>(v: &'a Value, k: &str, language: Language) -> Result<&'a str> {
    v.get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{}",
                localized_template(
                    language,
                    MessageKey::DiscordToolsFieldNonempty,
                    &[("field", k.to_owned())]
                )
            )
        })
}
fn bounded(
    v: &Value,
    k: &str,
    default: u64,
    min: u64,
    max: u64,
    language: Language,
) -> Result<u64> {
    let Some(raw) = v.get(k) else {
        return Ok(default);
    };
    let n = raw.as_u64().ok_or_else(|| {
        anyhow::anyhow!(
            "{}",
            localized_template(
                language,
                MessageKey::DiscordToolsFieldNonnegativeInteger,
                &[("field", k.to_owned())]
            )
        )
    })?;
    if !(min..=max).contains(&n) {
        bail!(
            "{}",
            localized_template(
                language,
                MessageKey::DiscordToolsFieldRange,
                &[
                    ("field", k.to_owned()),
                    ("min", min.to_string()),
                    ("max", max.to_string())
                ]
            )
        )
    }
    Ok(n)
}
fn requested_count(v: &Value, key: &str, default: usize, language: Language) -> Result<usize> {
    let Some(raw) = v.get(key) else {
        return Ok(default);
    };
    let value = raw.as_u64().ok_or_else(|| {
        anyhow::anyhow!(
            "{}",
            localized_template(
                language,
                MessageKey::DiscordToolsFieldNonnegativeInteger,
                &[("field", key.to_owned())]
            )
        )
    })?;
    usize::try_from(value).map_err(|_| {
        anyhow::anyhow!(
            "{}",
            localized_template(
                language,
                MessageKey::DiscordToolsFieldTooLarge,
                &[("field", key.to_owned())]
            )
        )
    })
}

fn link_arg<'a>(v: &'a Value, k: &str, language: Language) -> Result<Option<&'a str>> {
    let Some(raw) = v.get(k) else { return Ok(None) };
    let link = raw
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{}",
                localized_template(
                    language,
                    MessageKey::DiscordToolsDiscordMessageLinkRequired,
                    &[("field", k.to_owned())]
                )
            )
        })?;
    Ok(Some(link))
}
fn parse_message_link(
    link: &str,
    guild_id: u64,
    channel_id: u64,
    language: Language,
) -> Result<MessageId> {
    let path = link
        .strip_prefix("https://discord.com/channels/")
        .or_else(|| link.strip_prefix("https://discordapp.com/channels/"))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{}",
                language.message(MessageKey::DiscordToolsMessageReferenceLinkRequired)
            )
        })?;
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() != 3 || parts.iter().any(|part| part.is_empty()) {
        bail!(
            "{}",
            language.message(MessageKey::DiscordToolsMessageLinkIdentity)
        )
    }
    let linked_guild = if parts[0] == "@me" {
        Some(0)
    } else {
        parts[0].parse::<u64>().ok().filter(|id| *id != 0)
    };
    let linked_channel = parts[1].parse::<u64>().ok();
    let message = parts[2]
        .parse::<u64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| {
            anyhow::anyhow!(
                "{}",
                language.message(MessageKey::DiscordToolsMessageLinkInvalidMessage)
            )
        })?;
    if linked_guild != Some(guild_id) || linked_channel != Some(channel_id) {
        bail!(
            "{}",
            language.message(MessageKey::DiscordToolsMessageLinkWrongScope)
        )
    }
    Ok(MessageId::new(message))
}
fn message_link(guild_id: u64, channel_id: u64, message_id: u64) -> String {
    let scope = if guild_id == 0 {
        "@me".to_owned()
    } else {
        guild_id.to_string()
    };
    format!("https://discord.com/channels/{scope}/{channel_id}/{message_id}")
}
fn validate_voice_channel_selection(v: &Value, language: Language) -> Result<()> {
    validate_keys(v, &["channel", "channel_choice"], language)?;
    match (v.get("channel"), v.get("channel_choice")) {
        (Some(name), None) if name.as_str().is_some_and(|value| !value.trim().is_empty()) => Ok(()),
        (None, Some(choice)) if choice.as_str().is_some_and(is_choice) => Ok(()),
        (Some(_), Some(_)) => bail!(
            "{}",
            language.message(MessageKey::DiscordToolsExactlyOneVoiceSelector)
        ),
        (Some(_), None) => bail!(
            "{}",
            language.message(MessageKey::DiscordToolsChannelNameStringRequired)
        ),
        (None, Some(_)) => bail!(
            "{}",
            language.message(MessageKey::DiscordToolsChannelChoiceStringRequired)
        ),
        (None, None) => bail!(
            "{}",
            language.message(MessageKey::DiscordToolsVoiceSelectorRequired)
        ),
    }
}

fn require_voice_channel(channel: &GuildChannel, language: Language) -> Result<()> {
    if channel.kind != ChannelType::Voice {
        bail!(
            "{}",
            language.message(MessageKey::DiscordToolsNotVoiceChannel)
        );
    }
    Ok(())
}

fn matching_named_channels<'a>(
    channels: &'a [GuildChannel],
    categories: &HashMap<ChannelId, String>,
    category: Option<&str>,
    leaf: &str,
    kind: Option<ChannelType>,
) -> Vec<&'a GuildChannel> {
    channels
        .iter()
        .filter(|channel| {
            channel.kind != ChannelType::Category
                && kind.is_none_or(|kind| channel.kind == kind)
                && channel.name == leaf
                && category.is_none_or(|wanted| {
                    channel
                        .parent_id
                        .and_then(|id| categories.get(&id))
                        .is_some_and(|name| name == wanted)
                })
        })
        .collect()
}

fn channel_name_query(query: &str) -> &str {
    query.strip_prefix('#').unwrap_or(query)
}
fn user_name_query(query: &str) -> &str {
    query.strip_prefix('@').unwrap_or(query)
}
fn thread(c: &GuildChannel) -> bool {
    matches!(
        c.kind,
        ChannelType::PublicThread | ChannelType::PrivateThread | ChannelType::NewsThread
    )
}
fn permissions_in(
    guild: &serenity::all::PartialGuild,
    member: &Member,
    channel: &GuildChannel,
    channels: &[GuildChannel],
    language: Language,
) -> Result<Permissions> {
    if channel.guild_id != guild.id {
        bail!(
            "{}",
            language.message(MessageKey::DiscordToolsChannelOutsideGuild)
        )
    }
    if thread(channel) {
        let parent = channel
            .parent_id
            .and_then(|id| channels.iter().find(|c| c.id == id))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "{}",
                    language.message(MessageKey::DiscordToolsThreadParentUnavailable)
                )
            })?;
        Ok(guild.user_permissions_in(parent, member))
    } else {
        Ok(guild.user_permissions_in(channel, member))
    }
}
fn label(m: &Member) -> String {
    m.display_name().to_owned()
}

struct AfterPageProgress {
    accepted_ids: Vec<u64>,
    next_after: Option<u64>,
    advanced: bool,
}

fn after_page_progress(
    cursor: u64,
    anchor: u64,
    page_ids: &[u64],
    seen: &mut HashSet<u64>,
) -> AfterPageProgress {
    let mut accepted_ids = page_ids
        .iter()
        .copied()
        .filter(|id| *id > anchor && *id > cursor && seen.insert(*id))
        .collect::<Vec<_>>();
    accepted_ids.sort_unstable_by(|left, right| right.cmp(left));
    let next_after = page_ids.iter().copied().filter(|id| *id > cursor).max();
    AfterPageProgress {
        accepted_ids,
        next_after,
        advanced: next_after.is_some_and(|next| next > cursor),
    }
}

fn matches_name(m: &Member, q: &str) -> bool {
    name_matches(
        &m.user.name,
        m.user.global_name.as_deref(),
        m.nick.as_deref(),
        q,
    )
}
fn name_matches(username: &str, global: Option<&str>, nickname: Option<&str>, query: &str) -> bool {
    username.eq_ignore_ascii_case(query)
        || global.is_some_and(|s| s.eq_ignore_ascii_case(query))
        || nickname.is_some_and(|s| s.eq_ignore_ascii_case(query))
}
fn prepend_resolved_mentions(content: &str, users: &[UserId]) -> String {
    let mentions = users
        .iter()
        .map(|user| format!("<@{}>", user.get()))
        .collect::<Vec<_>>()
        .join(" ");
    if mentions.is_empty() {
        content.to_owned()
    } else if content.is_empty() {
        mentions
    } else {
        format!("{mentions}\n\n{content}")
    }
}

fn is_choice(value: &str) -> bool {
    value.strip_prefix("choice_").is_some_and(|suffix| {
        suffix.len() == 32 && suffix.bytes().all(|byte| byte.is_ascii_hexdigit())
    })
}
fn user_value(m: &Member, choice: String) -> Value {
    json!({"name":label(m),"username":m.user.name,"global_name":m.user.global_name,"nickname":m.nick,"choice":choice})
}
fn message_json(m: &Message, guild_id: u64, channel_id: u64) -> Value {
    let author = m
        .member
        .as_ref()
        .and_then(|member| member.nick.as_deref())
        .or(m.author.global_name.as_deref())
        .unwrap_or(&m.author.name);
    let (content, files) = crate::attachments::message_parts(m);
    let attachments = files.iter().map(|a| json!({
        "filename":a.filename,
        "extension":std::path::Path::new(&a.filename).extension().and_then(|x|x.to_str()).unwrap_or(""),
        "size":a.size
    })).collect::<Vec<_>>();
    let mut result = json!({"link":message_link(guild_id,channel_id,m.id.get()),"author":author,"content":content,"timestamp":m.timestamp.to_string(),"attachments":attachments});
    if let Some(thread) = m.thread.as_ref() {
        result["thread"] = thread_context(thread);
    }
    result
}

fn thread_context(channel: &GuildChannel) -> Value {
    json!({"id":channel.id.get().to_string(),"name":channel.name})
}

pub fn tool_definitions(language: crate::i18n::Language) -> Vec<Value> {
    vec![
        def(
            "discord_list_channels",
            language.message(MessageKey::DiscordToolsListVisibleChannelsActiveThreadsHuman),
            json!({"type":"object","properties":{},"additionalProperties":false}),
        ),
        def(
            "discord_list_users",
            language.message(MessageKey::DiscordToolsListConfiguredGuildUsersOpaquePaging),
            json!({"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":1000,"description":language.message(MessageKey::DiscordToolsUsersPerPage)},"page":{"type":"string","description":language.message(MessageKey::DiscordToolsOpaqueNextPageTokenReturnedPrevious)}},"additionalProperties":false}),
        ),
        def(
            "discord_send_message",
            language.message(MessageKey::DiscordToolsUseToolReplyLiveMessageReceived),
            json!({"type":"object","properties":{"channel":{"type":"string","description":language.message(MessageKey::DiscordToolsChannelNameOmitWhenSelectingChannel)},"channel_choice":{"type":"string","description":language.message(MessageKey::DiscordToolsOpaqueChannelChoiceReturnedDiscordList)},"thread_id":{"type":"string","description":language.message(MessageKey::DiscordToolsThreadIdReturnedDiscordListChannels)},"dm_id":{"type":"string","description":language.message(MessageKey::DiscordToolsDirectMessageConversationIdIncomingDm)},"content":{"type":"string","description":language.message(MessageKey::DiscordToolsMessageBodyLongContentSentMultiple)},"mentions":{"type":"array","items":{"type":"string"},"description":language.message(MessageKey::DiscordToolsExactCurrentServerMemberDmRecipient)},"files":{"type":"array","minItems":1,"items":{"type":"string"},"description":language.message(MessageKey::DiscordToolsOptionalLocalFilePathsAttachFiles)}},"anyOf":[{"required":["content"]},{"required":["files"]}],"additionalProperties":false}),
        ),
        def(
            "discord_get_messages",
            language.message(MessageKey::DiscordToolsReadMessagesChannelNameChannelChoice),
            json!({"type":"object","properties":{"channel":{"type":"string","description":language.message(MessageKey::DiscordToolsChannelNameOmitWhenSelectingChannel)},"channel_choice":{"type":"string","description":language.message(MessageKey::DiscordToolsOpaqueChannelChoiceReturnedDiscordList)},"thread_id":{"type":"string","description":language.message(MessageKey::DiscordToolsThreadIdReturnedDiscordListChannels)},"dm_id":{"type":"string","description":language.message(MessageKey::DiscordToolsDirectMessageConversationIdIncomingDm)},"limit":{"type":"integer","minimum":1,"description":language.message(MessageKey::DiscordToolsMaximumNumberMessagesReturnLargerRequests)},"before":{"type":"string","description":language.message(MessageKey::DiscordToolsDiscordMessageLinkReturnMessagesBefore)},"after":{"type":"string","description":language.message(MessageKey::DiscordToolsDiscordMessageLinkReturnMessagesAfter)},"around":{"type":"string","description":language.message(MessageKey::DiscordToolsDiscordMessageLinkReturnMessagesAround)},"offset":{"type":"integer","minimum":0,"description":language.message(MessageKey::DiscordToolsOptionalNumberRecentMessagesSkip)}},"additionalProperties":false}),
        ),
        def(
            "discord_search_messages",
            language.message(MessageKey::DiscordToolsSearchRecentHistoryChannelNameChannel),
            json!({"type":"object","properties":{"channel":{"type":"string","description":language.message(MessageKey::DiscordToolsChannelNameOmitWhenSelectingChannel)},"channel_choice":{"type":"string","description":language.message(MessageKey::DiscordToolsOpaqueChannelChoiceReturnedDiscordList)},"thread_id":{"type":"string","description":language.message(MessageKey::DiscordToolsThreadIdReturnedDiscordListChannels)},"dm_id":{"type":"string","description":language.message(MessageKey::DiscordToolsDirectMessageConversationIdIncomingDm)},"query":{"type":"string","description":language.message(MessageKey::DiscordToolsLiteralSubstringSearch)},"max_scan":{"type":"integer","minimum":1,"description":language.message(MessageKey::DiscordToolsMaximumNumberRecentMessagesInspectRequests)},"limit":{"type":"integer","minimum":1,"description":language.message(MessageKey::DiscordToolsMaximumNumberMatchesReturn)}},"required":["query"],"additionalProperties":false}),
        ),
        def(
            "discord_download_attachment",
            language.message(MessageKey::DiscordToolsDownloadAttachmentShownDiscordGetMessages),
            json!({"type":"object","properties":{"channel":{"type":"string","description":language.message(MessageKey::DiscordToolsChannelNameOmitWhenSelectingChannel)},"channel_choice":{"type":"string","description":language.message(MessageKey::DiscordToolsOpaqueChannelChoiceReturnedDiscordList)},"thread_id":{"type":"string","description":language.message(MessageKey::DiscordToolsThreadIdReturnedDiscordListChannels)},"dm_id":{"type":"string","description":language.message(MessageKey::DiscordToolsDirectMessageConversationIdIncomingDm)},"message_link":{"type":"string","description":language.message(MessageKey::DiscordToolsDiscordMessageLinkReturnedHistorySearch)},"filename":{"type":"string","description":language.message(MessageKey::DiscordToolsAttachmentFilenameShownDiscordGetMessages)},"attachment_choice":{"type":"string","description":language.message(MessageKey::DiscordToolsOpaqueChoiceReturnedWhenAttachmentFilenames)}},"required":["message_link"],"additionalProperties":false}),
        ),
    ]
}
fn def(name: &str, description: &str, input_schema: Value) -> Value {
    json!({"name":name,"description":description,"inputSchema":input_schema})
}

#[cfg(test)]
mod dm_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn localized_templates_substitute_runtime_values() {
        assert_eq!(
            localized_template(
                Language::English,
                MessageKey::DiscordToolsUnknownTool,
                &[("tool", "discord_unknown".to_owned())],
            ),
            "Unknown Discord tool: discord_unknown"
        );
    }

    #[test]
    fn direct_message_selection_uses_a_conversation_id_without_other_selectors() {
        assert!(validate_channel_selection(&json!({"dm_id":"42"}), Language::English).is_ok());
        assert_eq!(
            selected_channel_id(&json!("42"), "dm_id", Language::English).unwrap(),
            ChannelId::new(42)
        );
        for invalid in [
            json!({"dm_id":"0"}),
            json!({"dm_id":42}),
            json!({"dm_id":""}),
            json!({"dm_id":"not-an-id"}),
            json!({"dm_id":"42","thread_id":"42"}),
            json!({"dm_id":"42","channel":"general"}),
            json!({"dm_id":"42","channel_choice":"choice_0123456789abcdef0123456789abcdef"}),
        ] {
            assert!(
                validate_channel_selection(&invalid, Language::English).is_err(),
                "accepted {invalid}"
            );
        }
    }

    #[test]
    fn direct_message_links_and_cursors_preserve_the_private_conversation() {
        let link = message_link(0, 42, 123);
        assert_eq!(link, "https://discord.com/channels/@me/42/123");
        assert_eq!(
            parse_message_link(&link, 0, 42, Language::English).unwrap(),
            MessageId::new(123)
        );
        assert!(parse_message_link(&link, 99, 42, Language::English).is_err());
        assert!(parse_message_link(&link, 0, 43, Language::English).is_err());
        assert!(
            parse_message_link(
                "https://discord.com/channels/99/42/123",
                0,
                42,
                Language::English
            )
            .is_err()
        );
        assert_eq!(
            message_cursors([123, 124].into_iter().map(MessageId::new), false, 0, 42),
            json!({
                "next_before":"https://discord.com/channels/@me/42/123",
                "next_after":"https://discord.com/channels/@me/42/124",
                "next_cursor":{"before":"https://discord.com/channels/@me/42/123"}
            })
        );
        let mut message = Message::default();
        message.id = MessageId::new(123);
        message.content = "private text".to_owned();
        let output = message_json(&message, 0, 42);
        assert_eq!(output["link"], link);
        assert_eq!(output["content"], "private text");
    }
    #[test]
    fn explicit_file_paths_accept_optional_nonempty_local_path_arrays() {
        assert_eq!(
            explicit_file_paths(&json!({}), Language::English).unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            explicit_file_paths(
                &json!({"files":["/tmp/one.txt", "/tmp/two.png"]}),
                Language::English
            )
            .unwrap(),
            vec!["/tmp/one.txt", "/tmp/two.png"]
        );
        assert!(explicit_file_paths(&json!({"files":"/tmp/one.txt"}), Language::English).is_err());
        assert!(
            explicit_file_paths(&json!({"files":["/tmp/one.txt", " "]}), Language::English)
                .is_err()
        );
        assert_eq!(
            explicit_file_paths(&json!({"files":vec!["/tmp/x"; 11]}), Language::English)
                .unwrap()
                .len(),
            11
        );
        assert!(validate_send_payload("", 0, Language::English).is_err());
        assert!(validate_send_payload("", 1, Language::English).is_ok());
        assert!(validate_send_payload("caption", 0, Language::English).is_ok());
        assert!(validate_channel_selection(&json!({"thread_id":"123"}), Language::English).is_ok());
        assert!(validate_channel_selection(&json!({"thread_id":"0"}), Language::English).is_err());
        assert!(
            validate_channel_selection(&json!({"thread_id":"not-an-id"}), Language::English)
                .is_err()
        );
        assert!(
            validate_channel_selection(
                &json!({"thread_id":"123","channel":"general"}),
                Language::English
            )
            .is_err()
        );
    }

    #[test]
    fn send_and_download_schemas_expose_file_paths_and_local_download_results() {
        let definitions = tool_definitions(crate::i18n::Language::English);
        let send = definitions
            .iter()
            .find(|tool| tool["name"] == "discord_send_message")
            .unwrap();
        assert_eq!(send["inputSchema"]["properties"]["files"]["type"], "array");
        assert_eq!(send["inputSchema"]["properties"]["files"]["minItems"], 1);
        assert!(send["inputSchema"]["properties"]["files"]["maxItems"].is_null());
        assert_eq!(
            send["inputSchema"]["properties"]["thread_id"]["type"],
            "string"
        );
        assert_eq!(
            send["inputSchema"]["anyOf"],
            json!([{"required":["content"]},{"required":["files"]}])
        );
        for name in [
            "discord_send_message",
            "discord_get_messages",
            "discord_search_messages",
            "discord_download_attachment",
        ] {
            let tool = definitions
                .iter()
                .find(|tool| tool["name"] == name)
                .unwrap();
            assert_eq!(
                tool["inputSchema"]["properties"]["thread_id"]["type"],
                "string"
            );
            assert_eq!(tool["inputSchema"]["properties"]["dm_id"]["type"], "string");
        }
        let download = definitions
            .iter()
            .find(|tool| tool["name"] == "discord_download_attachment")
            .unwrap();
        assert!(
            download["description"]
                .as_str()
                .unwrap()
                .contains("local saved path")
        );
        let users = definitions
            .iter()
            .find(|tool| tool["name"] == "discord_list_users")
            .unwrap();
        assert_eq!(users["inputSchema"]["properties"]["limit"]["maximum"], 1000);
        let history = definitions
            .iter()
            .find(|tool| tool["name"] == "discord_get_messages")
            .unwrap();
        assert!(history["inputSchema"]["properties"]["limit"]["maximum"].is_null());
        assert!(history["inputSchema"]["properties"]["offset"]["maximum"].is_null());
        let search = definitions
            .iter()
            .find(|tool| tool["name"] == "discord_search_messages")
            .unwrap();
        assert!(search["inputSchema"]["properties"]["max_scan"]["maximum"].is_null());
        assert!(search["inputSchema"]["properties"]["limit"]["maximum"].is_null());
    }

    #[test]
    fn thread_context_returns_only_id_and_name() {
        let mut channel = GuildChannel::default();
        channel.id = ChannelId::new(123);
        channel.name = "project-help".to_owned();
        assert_eq!(
            thread_context(&channel),
            json!({"id":"123","name":"project-help"})
        );
    }

    #[test]
    fn voice_history_requires_connect_but_text_history_does_not() {
        let read = Permissions::VIEW_CHANNEL | Permissions::READ_MESSAGE_HISTORY;
        assert!(require_read_permissions(ChannelType::Text, read, Language::English).is_ok());
        assert!(require_read_permissions(ChannelType::Voice, read, Language::English).is_err());
        assert!(
            require_read_permissions(
                ChannelType::Stage,
                read | Permissions::CONNECT,
                Language::English
            )
            .is_ok()
        );
        assert!(
            require_read_permissions(
                ChannelType::Text,
                Permissions::VIEW_CHANNEL,
                Language::English
            )
            .is_err()
        );
    }
    #[test]
    fn message_cursors_follow_direction_and_handle_empty_pages() {
        let ids = [30, 10, 20];
        assert_eq!(
            message_cursors(ids.into_iter().map(MessageId::new), false, 1, 2),
            json!({"next_before":"https://discord.com/channels/1/2/10","next_after":"https://discord.com/channels/1/2/30","next_cursor":{"before":"https://discord.com/channels/1/2/10"}})
        );
        assert_eq!(
            message_cursors(ids.into_iter().map(MessageId::new), true, 1, 2),
            json!({"next_before":"https://discord.com/channels/1/2/10","next_after":"https://discord.com/channels/1/2/30","next_cursor":{"after":"https://discord.com/channels/1/2/30"}})
        );
        assert_eq!(
            message_cursors(std::iter::empty(), true, 1, 2),
            json!({"next_before":null,"next_after":null,"next_cursor":null})
        );
    }
    #[test]
    fn member_names_match_only_exact_username_global_name_or_nickname() {
        assert!(name_matches(
            "User",
            Some("Display"),
            Some("Server Nick"),
            "user"
        ));
        assert!(name_matches(
            "User",
            Some("Display"),
            Some("Server Nick"),
            "DISPLAY"
        ));
        assert!(name_matches(
            "User",
            Some("Display"),
            Some("Server Nick"),
            "Server Nick"
        ));
        assert!(!name_matches(
            "User",
            Some("Display"),
            Some("Server Nick"),
            "Server"
        ));
        assert!(!is_choice("Server Nick"));
        assert!(is_choice("choice_0123456789abcdef0123456789abcdef"));
        assert!(!is_choice("choice_nickname"));
    }
    #[test]
    fn resolved_mentions_precede_content_with_a_markdown_paragraph_break() {
        assert_eq!(
            prepend_resolved_mentions("| A | B |\n|---|---|", &[UserId::new(7), UserId::new(9)]),
            "<@7> <@9>\n\n| A | B |\n|---|---|"
        );
        assert_eq!(prepend_resolved_mentions("table", &[]), "table");
        assert_eq!(prepend_resolved_mentions("", &[UserId::new(7)]), "<@7>");
    }

    #[test]
    fn after_pagination_advances_to_newer_pages_without_duplicates() {
        let mut seen = HashSet::new();
        for id in (101..=200).rev() {
            seen.insert(id);
        }
        let next_page = vec![205, 204, 203, 202, 201];
        let progress = after_page_progress(200, 100, &next_page, &mut seen);
        assert_eq!(progress.accepted_ids, vec![205, 204, 203, 202, 201]);
        assert_eq!(progress.next_after, Some(205));
        assert!(progress.advanced);

        let overlapping_page = vec![205, 204, 210, 209];
        let progress = after_page_progress(205, 100, &overlapping_page, &mut seen);
        assert_eq!(progress.accepted_ids, vec![210, 209]);
        assert_eq!(progress.next_after, Some(210));
        assert!(progress.advanced);
    }

    #[test]
    fn after_pagination_stops_when_cursor_does_not_advance() {
        let mut seen = HashSet::from([150, 149]);
        let repeated_page = vec![150, 149];
        let progress = after_page_progress(150, 100, &repeated_page, &mut seen);
        assert!(progress.accepted_ids.is_empty());
        assert_eq!(progress.next_after, None);
        assert!(!progress.advanced);
    }

    #[test]
    fn requested_counts_accept_values_larger_than_prior_tool_caps() {
        assert_eq!(
            requested_count(&json!({"offset":500_000}), "offset", 0, Language::English).unwrap(),
            500_000
        );
        assert_eq!(
            requested_count(
                &json!({"max_scan":25_000}),
                "max_scan",
                100,
                Language::English
            )
            .unwrap(),
            25_000
        );
        assert!(requested_count(&json!({"limit":-1}), "limit", 50, Language::English).is_err());
        assert!(requested_count(&json!({"limit":"100"}), "limit", 50, Language::English).is_err());
    }
    #[test]
    fn numeric_bounds_and_message_id_inputs_are_strict() {
        assert_eq!(
            bounded(
                &json!({"limit":100}),
                "limit",
                50,
                1,
                100,
                Language::English
            )
            .unwrap(),
            100
        );
        assert!(
            bounded(
                &json!({"limit":101}),
                "limit",
                50,
                1,
                100,
                Language::English
            )
            .is_err()
        );
        assert!(
            bounded(
                &json!({"limit":"10"}),
                "limit",
                50,
                1,
                100,
                Language::English
            )
            .is_err()
        );
        assert_eq!(
            parse_message_link(
                "https://discord.com/channels/1/2/123",
                1,
                2,
                Language::English
            )
            .unwrap(),
            MessageId::new(123)
        );
        assert!(parse_message_link("123", 1, 2, Language::English).is_err());
        assert!(
            parse_message_link(
                "http://discord.com/channels/1/2/123",
                1,
                2,
                Language::English
            )
            .is_err()
        );
        assert!(
            parse_message_link(
                "https://example.com/channels/1/2/123",
                1,
                2,
                Language::English
            )
            .is_err()
        );
        assert!(
            parse_message_link(
                "https://discord.com/channels/1/2/0",
                1,
                2,
                Language::English
            )
            .is_err()
        );
        assert!(
            parse_message_link(
                "https://discord.com/channels/1/2/123/extra",
                1,
                2,
                Language::English
            )
            .is_err()
        );
        assert!(
            parse_message_link(
                "https://discord.com/channels/9/2/123",
                1,
                2,
                Language::English
            )
            .is_err()
        );
        assert!(
            parse_message_link(
                "https://discord.com/channels/1/3/123",
                1,
                2,
                Language::English
            )
            .is_err()
        );
    }
    #[test]
    fn name_prefixes_preserve_numeric_names_as_literal_names() {
        assert_eq!(channel_name_query("#2026"), "2026");
        assert_eq!(channel_name_query("#games/lounge"), "games/lounge");
        assert_eq!(channel_name_query("2026"), "2026");
        assert_eq!(user_name_query("@123"), "123");
        assert_eq!(user_name_query("123"), "123");
    }

    #[test]
    fn voice_channel_selection_accepts_exactly_name_or_opaque_choice() {
        assert!(
            validate_voice_channel_selection(&json!({"channel":"Stage 1"}), Language::English)
                .is_ok()
        );
        assert!(
            validate_voice_channel_selection(
                &json!({"channel_choice":"choice_0123456789abcdef0123456789abcdef"}),
                Language::English
            )
            .is_ok()
        );
        for args in [
            json!({}),
            json!({"channel":"Stage 1","channel_choice":"choice_0123456789abcdef0123456789abcdef"}),
            json!({"thread_id":"123"}),
            json!({"channel":"Stage 1","thread_id":"123"}),
            json!({"channel_choice":"not-a-choice"}),
            json!({"channel":12}),
            json!({"extra":"ignored"}),
        ] {
            assert!(
                validate_voice_channel_selection(&args, Language::English).is_err(),
                "accepted {args}"
            );
        }
    }

    #[test]
    fn voice_name_lookup_filters_same_named_text_channels_and_respects_category() {
        let mut category_a = GuildChannel::default();
        category_a.id = ChannelId::new(1);
        category_a.name = "Games".into();
        category_a.kind = ChannelType::Category;
        let mut voice_a = GuildChannel::default();
        voice_a.id = ChannelId::new(2);
        voice_a.name = "Lobby".into();
        voice_a.parent_id = Some(ChannelId::new(1));
        voice_a.kind = ChannelType::Voice;
        let mut text_a = GuildChannel::default();
        text_a.id = ChannelId::new(3);
        text_a.name = "Lobby".into();
        text_a.parent_id = Some(ChannelId::new(1));
        text_a.kind = ChannelType::Text;
        let mut voice_b = voice_a.clone();
        voice_b.id = ChannelId::new(4);
        voice_b.parent_id = None;
        let channels = vec![category_a, voice_a.clone(), text_a, voice_b.clone()];
        let categories = HashMap::from([(ChannelId::new(1), "Games".to_owned())]);
        let matches = matching_named_channels(
            &channels,
            &categories,
            Some("Games"),
            "Lobby",
            Some(ChannelType::Voice),
        );
        assert_eq!(
            matches.iter().map(|channel| channel.id).collect::<Vec<_>>(),
            vec![voice_a.id]
        );
        assert!(require_voice_channel(&voice_a, Language::English).is_ok());
        assert!(require_voice_channel(&channels[2], Language::English).is_err());
        let unqualified = matching_named_channels(
            &channels,
            &categories,
            None,
            "Lobby",
            Some(ChannelType::Voice),
        );
        assert_eq!(unqualified.len(), 2);
    }

    #[test]
    fn forwarded_history_output_contains_combined_text_and_snapshot_files() {
        let mut message = Message::default();
        message.id = MessageId::new(303);
        message.author.name = "sender".to_owned();
        message.author.global_name = Some("global display".to_owned());
        message.content = "outer body".to_owned();
        message.message_snapshots = serde_json::from_value(json!([{
            "content":"forwarded body",
            "timestamp":"2024-01-01T00:00:00+00:00",
            "edited_timestamp":null,
            "mentions":[],
            "mention_roles":[],
            "attachments":[{
                "id":"99",
                "filename":"forward.png",
                "description":null,
                "height":10,
                "proxy_url":"https://cdn.discordapp.com/attachments/1/99/forward.png",
                "size":42,
                "url":"https://cdn.discordapp.com/attachments/1/99/forward.png",
                "width":20,
                "content_type":"image/png"
            }],
            "embeds":[],
            "type":0,
            "flags":null
        }]))
        .unwrap();

        let result = message_json(&message, 1, 2);
        assert_eq!(result["link"], "https://discord.com/channels/1/2/303");
        assert_eq!(result["author"], "global display");
        assert_eq!(
            result["content"],
            "outer body\n\nForwarded message 1:\nforwarded body"
        );
        assert_eq!(
            result["attachments"],
            json!([{
                "filename":"forward.png",
                "extension":"png",
                "size":42
            }])
        );
        assert!(result.get("id").is_none());
        assert!(result.get("attachments_revision").is_none());
    }

    #[test]
    fn repeated_choices_remain_usable_without_an_arbitrary_expiration() {
        let mut choices = SelectionMap::default();
        let pick = Pick::User {
            id: UserId::new(7),
            label: "Nick".to_owned(),
        };
        let first = choices.insert(pick.clone()).unwrap();
        assert_eq!(choices.insert(pick.clone()).unwrap(), first);
        assert!(choices.lookup(&first).is_some());
        assert_eq!(choices.0.len(), 1);
        assert!(choices.lookup(&first).is_some());
        assert_eq!(choices.insert(pick).unwrap(), first);
        assert_eq!(choices.0.len(), 1);
    }

    #[test]
    fn tool_definitions_follow_selected_language() {
        for (language, expected) in [
            (crate::i18n::Language::English, "List visible channels"),
            (crate::i18n::Language::Korean, "현재 볼 수 있는 채널"),
            (crate::i18n::Language::Chinese, "当前可见的频道"),
            (crate::i18n::Language::Japanese, "現在表示できるチャンネル"),
        ] {
            let tools = tool_definitions(language);
            assert_eq!(tools[0]["name"], "discord_list_channels");
            assert!(tools[0]["description"].as_str().unwrap().contains(expected));
        }
    }
}
