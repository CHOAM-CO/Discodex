use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, Mutex},
};

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};
use serenity::all::{
    ChannelId, ChannelType, CreateAllowedMentions, CreateMessage, GetMessages, GuildChannel,
    GuildId, Http, Member, Message, MessageId, Permissions, UserId,
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
        match name {
            "discord_list_channels" => {
                validate_keys(&args, &[])?;
                self.list_channels().await
            }
            "discord_list_users" => self.list_users(&args).await,
            "discord_send_message" => self.send_message(&args).await,
            "discord_get_messages" => self.get_messages(&args).await,
            "discord_search_messages" => self.search_messages(&args).await,
            "discord_download_attachment" => self.download_attachment(&args).await,
            _ => bail!("unknown Discord tool: {name}"),
        }
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
        let mut map = self
            .selections
            .lock()
            .map_err(|_| anyhow::anyhow!("selection state lock poisoned"))?;
        map.insert(pick)
    }

    fn get_pick(&self, key: &str) -> Result<Pick> {
        self.selections
            .lock()
            .map_err(|_| anyhow::anyhow!("selection state lock poisoned"))?
            .lookup(key)
            .ok_or_else(|| {
                anyhow::anyhow!("unknown selection; list current channels or users again")
            })
    }

    async fn permissions(&self, channel: &GuildChannel) -> Result<Permissions> {
        if channel.guild_id.get() != self.config.guild_id {
            bail!("channel is outside configured guild");
        }
        let guild = self.guild().await?;
        let bot_member = self
            .current_bot_member()
            .await
            .context("fetch current bot guild member")?;
        let channels = self.fresh_channels(&guild).await?;
        let permissions = permissions_in(&guild, &bot_member, channel, &channels)?;
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
                    anyhow::anyhow!("thread_id must be a positive Discord channel ID string")
                })?;
            let channel = self
                .http
                .get_channel(ChannelId::new(thread_id))
                .await
                .context("fetch selected Discord thread")?
                .guild()
                .ok_or_else(|| anyhow::anyhow!("thread_id does not identify a guild thread"))?;
            if channel.guild_id.get() != self.config.guild_id {
                bail!("thread is outside configured guild")
            }
            if !thread(&channel) {
                bail!("thread_id does not identify a thread")
            }
            if !permissions_in(&guild, &bot_member, &channel, &all)?
                .contains(Permissions::VIEW_CHANNEL)
            {
                bail!("bot cannot view selected thread")
            }
            return Ok(channel);
        }
        if let Some(key) = args.get("channel_choice").and_then(Value::as_str) {
            let Pick::Channel { id, name, parent } = self.get_pick(key)? else {
                bail!("choice is not a channel")
            };
            let channel =
                all.iter().find(|c| c.id == id).cloned().ok_or_else(|| {
                    anyhow::anyhow!("channel choice is stale; list channels again")
                })?;
            if channel.name != name || channel.parent_id != parent {
                bail!("channel was renamed or moved; list channels again");
            }
            if !permissions_in(&guild, &bot_member, &channel, &all)?
                .contains(Permissions::VIEW_CHANNEL)
            {
                bail!("bot cannot view selected channel");
            }
            return Ok(channel);
        }
        let query = channel_name_query(required(args, "channel")?);
        if query.trim().is_empty() {
            bail!("channel must be a non-empty name")
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
            if !permissions_in(&guild, &bot_member, channel, &all)?
                .contains(Permissions::VIEW_CHANNEL)
            {
                continue;
            }
            matches.push(channel.clone());
        }
        match matches.as_slice() {
            [] => bail!("no visible current channel matches {query:?} in configured guild"),
            [one] => Ok(one.clone()),
            many => {
                let mut candidates = Vec::new();
                for c in many {
                    let cat = c.parent_id.and_then(|id| categories.get(&id)).cloned();
                    candidates.push(json!({"name":cat.as_ref().map(|x|format!("{x}/{}",c.name)).unwrap_or_else(||c.name.clone()),"type":format!("{:?}",c.kind),"choice":self.save_pick(Pick::Channel{id:c.id,name:c.name.clone(),parent:c.parent_id})?}));
                }
                Ok(json!({"error":"ambiguous channel name","candidates":candidates}))
                    .and_then(|v| Err(anyhow::anyhow!("{v}")))
            }
        }
    }

    pub(crate) async fn resolve_voice_channel(&self, args: &Value) -> Result<GuildChannel> {
        validate_voice_channel_selection(args)?;
        let guild = self.guild().await?;
        let all = self.fresh_channels(&guild).await?;
        let bot_member = self
            .current_bot_member()
            .await
            .context("fetch current bot member for voice-channel resolution")?;

        if let Some(key) = args.get("channel_choice").and_then(Value::as_str) {
            let Pick::Channel { id, name, parent } = self.get_pick(key)? else {
                bail!("choice is not a channel")
            };
            let channel = all
                .iter()
                .find(|channel| channel.id == id)
                .ok_or_else(|| anyhow::anyhow!("channel choice is stale; list channels again"))?;
            if channel.name != name || channel.parent_id != parent {
                bail!("channel was renamed or moved; list channels again");
            }
            require_voice_channel(channel)?;
            if !permissions_in(&guild, &bot_member, channel, &all)?
                .contains(Permissions::VIEW_CHANNEL)
            {
                bail!("bot cannot view selected voice channel");
            }
            return Ok(channel.clone());
        }

        let query = channel_name_query(required(args, "channel")?);
        if query.trim().is_empty() {
            bail!("channel must be a non-empty name");
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
            if !permissions_in(&guild, &bot_member, channel, &all)?
                .contains(Permissions::VIEW_CHANNEL)
            {
                continue;
            }
            matches.push(channel.clone());
        }
        match matches.as_slice() {
            [] => bail!("no visible current voice channel matches {query:?} in configured guild"),
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
                    json!({"error":"ambiguous voice channel name","candidates":candidates})
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
            if !permissions_in(&guild, &bot_member, c, &all)?.contains(Permissions::VIEW_CHANNEL) {
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
        validate_keys(args, &["limit", "page"])?;
        if args
            .get("page")
            .is_some_and(|v| v.as_str().is_none_or(|s| !is_choice(s)))
        {
            bail!("page must be an opaque next_page string")
        }
        let limit = bounded(args, "limit", 50, 1, 1000)?;
        let after = match args.get("page").and_then(Value::as_str) {
            Some(k) => match self.get_pick(k)? {
                Pick::After(id) => Some(id),
                _ => bail!("invalid member page token"),
            },
            None => None,
        };
        let guild = self.guild().await?;
        let page = guild
            .members(&*self.http, Some(limit), after)
            .await
            .map_err(|e| anyhow::anyhow!("Discord could not list guild members ({e}); the application's privileged GUILD_MEMBERS intent may need enabling/approval in Developer Portal. No intent or permission was changed."))?;
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
            let page=guild.members(&*self.http,Some(1000),after).await.map_err(|e|anyhow::anyhow!("Discord could not resolve current guild members ({e}); privileged GUILD_MEMBERS intent may need enabling/approval in Developer Portal. No intent or permission was changed."))?;
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
        let raw_query = value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("user names must be strings"))?;
        let query = user_name_query(raw_query);
        if is_choice(query) {
            if let Pick::User { id, label: old } = self.get_pick(query)? {
                let member = members
                    .iter()
                    .find(|m| m.user.id == id)
                    .ok_or_else(|| anyhow::anyhow!("selected user left the configured guild"))?;
                if label(member) != old {
                    bail!("user name changed; list users again")
                }
                return Ok(member.clone());
            }
            bail!("choice token is not a user selection")
        }
        let matches: Vec<Member> = members
            .iter()
            .filter(|m| matches_name(m, query))
            .cloned()
            .collect();
        match matches.as_slice() {
            [] => bail!("no current guild member matches {query:?}; list users for current names"),
            [one] => Ok(one.clone()),
            many => {
                let mut candidates = Vec::new();
                for m in many {
                    candidates.push(json!({"name":label(m),"username":m.user.name,"nickname":m.nick,"global_name":m.user.global_name,"choice":self.save_pick(Pick::User{id:m.user.id,label:label(m)})?}));
                }
                bail!(
                    "{}",
                    json!({"error":"ambiguous user name","candidates":candidates})
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
                "content",
                "mentions",
                "files",
            ],
        )?;
        validate_channel_selection(args)?;
        let file_paths = explicit_file_paths(args)?;
        let raw_content = args
            .get("content")
            .map(|value| {
                value
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("content must be a string"))
            })
            .transpose()?
            .unwrap_or("");
        let files = crate::reply_files::prepare_explicit_files(&self.config, &file_paths).await?;
        validate_send_payload(raw_content, files.len())?;
        if args.get("mentions").is_some_and(|v| !v.is_array()) {
            bail!("mentions must be an array")
        }
        if let Some(names) = args.get("mentions").and_then(Value::as_array) {
            for name in names {
                if name.as_str().is_none_or(|s| s.trim().is_empty()) {
                    bail!("each mention must be a non-empty current guild-member name or choice")
                }
            }
        }
        let channel = self.resolve_channel(args).await?;
        if channel.guild_id.get() != self.config.guild_id {
            bail!("channel is outside configured guild")
        }
        let perms = self.permissions(&channel).await?;
        if !perms.contains(Permissions::VIEW_CHANNEL) {
            bail!("bot lacks VIEW_CHANNEL")
        }
        let needed = if thread(&channel) {
            Permissions::SEND_MESSAGES_IN_THREADS
        } else {
            Permissions::SEND_MESSAGES
        };
        if !perms.contains(needed) {
            bail!("bot lacks permission to send in selected channel")
        }
        let mention_names = args.get("mentions").and_then(Value::as_array);
        let members = if mention_names.is_some_and(|names| !names.is_empty()) {
            self.list_all_members().await?
        } else {
            Vec::new()
        };
        let mut users = Vec::new();
        if let Some(names) = mention_names {
            for name in names {
                let m = self.resolve_user(name, &members)?;
                if !users.contains(&m.user.id) {
                    users.push(m.user.id);
                }
            }
        }
        if !files.is_empty() && !perms.contains(Permissions::ATTACH_FILES) {
            bail!("bot lacks ATTACH_FILES permission in selected channel")
        }
        let content = prepend_resolved_mentions(raw_content, &users);
        let prepared = if perms.contains(Permissions::ATTACH_FILES) {
            crate::table_messages::prepare(&content, files, self.config.language).await
        } else {
            crate::table_messages::prepare_plain(&content, files)
        };
        let server = self.guild().await?.name;
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
                    send_error = Some(error.to_string());
                    break;
                }
            };
            let link = message_link(self.config.guild_id, channel.id.get(), sent.id.get());
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
        let mut result = json!({"status":status,"server":server,"channel":channel.name,"message_link":first_link,"message_links":sent_messages.iter().filter_map(|message| message.get("message_link").cloned()).collect::<Vec<_>>(),"messages_sent":sent_messages.len(),"messages_requested":message_count,"messages":sent_messages,"attachments":all_attachments});
        if let Some(error) = send_error {
            result["error"] = json!(error);
        }
        if thread(&channel) {
            result["thread"] = thread_context(&channel);
        }
        Ok(result)
    }

    async fn messages_before(
        &self,
        channel: &GuildChannel,
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
        channel: &GuildChannel,
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
                "limit",
                "before",
                "after",
                "around",
                "offset",
            ],
        )?;
        validate_channel_selection(args)?;
        let limit = requested_count(args, "limit", 50)?;
        if limit == 0 {
            bail!("limit must be at least 1")
        }
        let before_link = link_arg(args, "before")?;
        let after_link = link_arg(args, "after")?;
        let around_link = link_arg(args, "around")?;
        if [before_link, after_link, around_link]
            .iter()
            .filter(|v| v.is_some())
            .count()
            > 1
        {
            bail!("choose at most one cursor: before, after, or around")
        }
        let offset = requested_count(args, "offset", 0)?;
        if offset > 0 && (after_link.is_some() || around_link.is_some()) {
            bail!("offset cannot be combined with after or around")
        }
        let c = self.resolve_channel(args).await?;
        let before = before_link
            .map(|link| parse_message_link(link, self.config.guild_id, c.id.get()))
            .transpose()?;
        let after = after_link
            .map(|link| parse_message_link(link, self.config.guild_id, c.id.get()))
            .transpose()?;
        let around = around_link
            .map(|link| parse_message_link(link, self.config.guild_id, c.id.get()))
            .transpose()?;
        let p = self.permissions(&c).await?;
        require_read_permissions(c.kind, p)?;
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
                    .messages(&*self.http, req)
                    .await
                    .context("skip recent messages")?;
                if page.is_empty() {
                    let mut result = json!({"server":self.guild().await?.name,"channel":c.name,"messages":[],"next_before":null,"next_after":null,"next_cursor":null,"history_exhausted":true});
                    if thread(&c) {
                        result["thread"] = thread_context(&c);
                    }
                    return Ok(result);
                }
                offset = offset.saturating_sub(page.len());
                cursor = page.last().map(|m| m.id);
            }
        }
        let messages = if let Some(anchor) = around {
            if limit <= 100 {
                c.messages(
                    &*self.http,
                    GetMessages::new().limit(limit as u8).around(anchor),
                )
                .await
                .context("read Discord messages around cursor")?
            } else {
                let seed = c
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
                        self.messages_before(&c, seed.last().map(|m| m.id), older_count)
                            .await?
                    } else {
                        Vec::new()
                    };
                    let newer = if newer_count > 0 {
                        match seed.first().map(|m| m.id) {
                            Some(id) => self.messages_after(&c, id, newer_count).await?,
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
            self.messages_after(&c, anchor, limit).await?
        } else {
            self.messages_before(&c, cursor, limit).await?
        };
        let mut result = message_cursors(
            messages.iter().map(|m| m.id),
            after.is_some(),
            self.config.guild_id,
            c.id.get(),
        );
        result["server"] = json!(self.guild().await?.name);
        result["channel"] = json!(c.name);
        if thread(&c) {
            result["thread"] = thread_context(&c);
        }
        result["messages"] = json!(
            messages
                .iter()
                .map(|m| message_json(m, self.config.guild_id, c.id.get()))
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
                "query",
                "max_scan",
                "limit",
            ],
        )?;
        validate_channel_selection(args)?;
        let max = requested_count(args, "max_scan", 100)?;
        if max == 0 {
            bail!("max_scan must be at least 1")
        }
        let result_limit = requested_count(args, "limit", 1000)?;
        if result_limit == 0 {
            bail!("limit must be at least 1")
        }
        let query = required(args, "query")?;
        if query.is_empty() {
            bail!("query cannot be empty")
        }
        let c = self.resolve_channel(args).await?;
        let p = self.permissions(&c).await?;
        require_read_permissions(c.kind, p)?;
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
                        found.push(message_json(&m, self.config.guild_id, c.id.get()))
                    }
                }
            }
            if page_len < n as usize {
                history_exhausted = true;
                break;
            }
        }
        let mut result = json!({"server":self.guild().await?.name,"channel":c.name,"query":query,"messages":found,"scanned":scanned,"max_scan":max,"requested_scope_complete":true,"history_exhausted":history_exhausted,"matching_count":matching_count,"truncated_matches":matching_count.saturating_sub(result_limit)});
        if thread(&c) {
            result["thread"] = thread_context(&c);
        }
        Ok(result)
    }

    async fn download_attachment(&self, args: &Value) -> Result<Value> {
        validate_keys(
            args,
            &[
                "channel",
                "channel_choice",
                "thread_id",
                "message_link",
                "filename",
                "attachment_choice",
            ],
        )?;
        validate_channel_selection(args)?;
        let link = required(args, "message_link")?;
        let filename = match args.get("filename") {
            None => None,
            Some(Value::String(value)) if !value.trim().is_empty() => Some(value.as_str()),
            Some(_) => bail!("filename must be a non-empty string"),
        };
        let choice = match args.get("attachment_choice") {
            None => None,
            Some(Value::String(value)) if is_choice(value) => Some(value.as_str()),
            Some(_) => bail!("attachment_choice must be a returned opaque choice"),
        };
        match (filename, choice) {
            (Some(_), None) | (None, Some(_)) => {}
            (Some(_), Some(_)) => bail!("provide a filename or attachment_choice, not both"),
            (None, None) => bail!("filename or attachment_choice is required"),
        }
        let c = self.resolve_channel(args).await?;
        let msg_id = parse_message_link(link, self.config.guild_id, c.id.get())?;
        let p = self.permissions(&c).await?;
        require_read_permissions(c.kind, p)?;
        let msg = c
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
                bail!("choice token is not an attachment selection")
            };
            if message_id != msg.id {
                bail!("attachment choice belongs to a different message")
            }
            attachments
                .iter()
                .find(|a| a.id.get() == attachment_id && a.filename == filename)
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "selected attachment changed or was removed; read the message again"
                    )
                })?
        } else {
            let expected = filename.expect("validated filename");
            let mut matches = attachments.iter().filter(|a| a.filename == expected);
            let Some(one) = matches.next() else {
                bail!("no attachment named {expected:?} in this message")
            };
            if matches.next().is_some() {
                let candidates = attachments.iter().filter(|a| a.filename == expected).map(|a| {
                    Ok(json!({"filename":a.filename,"size":a.size,"choice":self.save_pick(Pick::Attachment{message_id:msg.id,attachment_id:a.id.get(),filename:a.filename.clone()})?}))
                }).collect::<Result<Vec<_>>>()?;
                bail!(
                    "{}",
                    json!({"error":"attachment filename is ambiguous","candidates":candidates})
                );
            }
            one
        };
        let result = crate::attachments::download_attachment(&self.config, att)
            .await
            .context("download selected attachment")?;
        let mut result = serde_json::to_value(result)?;
        if thread(&c) {
            result["thread"] = thread_context(&c);
        }
        Ok(result)
    }
}

fn require_read_permissions(kind: ChannelType, permissions: Permissions) -> Result<()> {
    if !permissions.contains(Permissions::VIEW_CHANNEL | Permissions::READ_MESSAGE_HISTORY) {
        bail!("bot needs VIEW_CHANNEL and READ_MESSAGE_HISTORY");
    }
    if matches!(kind, ChannelType::Voice | ChannelType::Stage)
        && !permissions.contains(Permissions::CONNECT)
    {
        bail!("bot needs CONNECT to read the selected voice channel");
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

fn validate_send_payload(content: &str, file_count: usize) -> Result<()> {
    if content.trim().is_empty() && file_count == 0 {
        bail!("provide non-empty content or at least one file");
    }
    Ok(())
}

fn explicit_file_paths(args: &Value) -> Result<Vec<String>> {
    let Some(files) = args.get("files") else {
        return Ok(Vec::new());
    };
    let files = files
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("files must be an array of local path strings"))?;
    files
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .as_str()
                .filter(|path| !path.trim().is_empty())
                .map(str::to_owned)
                .ok_or_else(|| {
                    anyhow::anyhow!("files[{index}] must be a non-empty local path string")
                })
        })
        .collect()
}

fn validate_keys(v: &Value, allowed: &[&str]) -> Result<()> {
    let object = v
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("arguments must be a JSON object"))?;
    for key in object.keys() {
        if !allowed.contains(&key.as_str()) {
            bail!("unknown argument {key:?}")
        }
    }
    Ok(())
}
fn validate_channel_selection(v: &Value) -> Result<()> {
    if let Some(thread_id) = v.get("thread_id") {
        if v.get("channel").is_some() || v.get("channel_choice").is_some() {
            bail!("provide exactly one of channel, channel_choice, or thread_id");
        }
        let thread_id = thread_id
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("thread_id must be a positive Discord channel ID string")
            })?;
        if thread_id
            .parse::<u64>()
            .ok()
            .filter(|id| *id != 0)
            .is_none()
        {
            bail!("thread_id must be a positive Discord channel ID string");
        }
        return Ok(());
    }
    match (v.get("channel"), v.get("channel_choice")) {
        (Some(name), None) if name.as_str().is_some_and(|s| !s.trim().is_empty()) => Ok(()),
        (None, Some(choice)) if choice.as_str().is_some_and(is_choice) => Ok(()),
        (Some(_), Some(_)) => bail!("provide exactly one of channel or channel_choice"),
        (Some(_), None) => bail!("channel must be a non-empty name string"),
        (None, Some(_)) => bail!("channel_choice must be an opaque choice string"),
        (None, None) => bail!("channel, channel_choice, or thread_id is required"),
    }
}
fn required<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v.get(k)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("{k} must be a non-empty string"))
}
fn bounded(v: &Value, k: &str, default: u64, min: u64, max: u64) -> Result<u64> {
    let Some(raw) = v.get(k) else {
        return Ok(default);
    };
    let n = raw
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("{k} must be a non-negative integer"))?;
    if !(min..=max).contains(&n) {
        bail!("{k} must be between {min} and {max}")
    }
    Ok(n)
}
fn requested_count(v: &Value, key: &str, default: usize) -> Result<usize> {
    let Some(raw) = v.get(key) else {
        return Ok(default);
    };
    let value = raw
        .as_u64()
        .ok_or_else(|| anyhow::anyhow!("{key} must be a non-negative integer"))?;
    usize::try_from(value).map_err(|_| anyhow::anyhow!("{key} is too large for this platform"))
}

fn link_arg<'a>(v: &'a Value, k: &str) -> Result<Option<&'a str>> {
    let Some(raw) = v.get(k) else { return Ok(None) };
    let link = raw
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("{k} must be a Discord message link"))?;
    Ok(Some(link))
}
fn parse_message_link(link: &str, guild_id: u64, channel_id: u64) -> Result<MessageId> {
    let path = link
        .strip_prefix("https://discord.com/channels/")
        .or_else(|| link.strip_prefix("https://discordapp.com/channels/"))
        .ok_or_else(|| anyhow::anyhow!("message reference must be a Discord message link"))?;
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() != 3 || parts.iter().any(|part| part.is_empty()) {
        bail!("message link must identify one server, channel, and message")
    }
    let linked_guild = parts[0].parse::<u64>().ok();
    let linked_channel = parts[1].parse::<u64>().ok();
    let message = parts[2]
        .parse::<u64>()
        .ok()
        .filter(|id| *id > 0)
        .ok_or_else(|| anyhow::anyhow!("message link does not contain a valid message"))?;
    if linked_guild != Some(guild_id) || linked_channel != Some(channel_id) {
        bail!("message link belongs to a different server or channel")
    }
    Ok(MessageId::new(message))
}
fn message_link(guild_id: u64, channel_id: u64, message_id: u64) -> String {
    format!("https://discord.com/channels/{guild_id}/{channel_id}/{message_id}")
}
fn validate_voice_channel_selection(v: &Value) -> Result<()> {
    validate_keys(v, &["channel", "channel_choice"])?;
    match (v.get("channel"), v.get("channel_choice")) {
        (Some(name), None) if name.as_str().is_some_and(|value| !value.trim().is_empty()) => Ok(()),
        (None, Some(choice)) if choice.as_str().is_some_and(is_choice) => Ok(()),
        (Some(_), Some(_)) => bail!("provide exactly one of channel or channel_choice"),
        (Some(_), None) => bail!("channel must be a non-empty name string"),
        (None, Some(_)) => bail!("channel_choice must be an opaque choice string"),
        (None, None) => bail!("channel or channel_choice is required"),
    }
}

fn require_voice_channel(channel: &GuildChannel) -> Result<()> {
    if channel.kind != ChannelType::Voice {
        bail!("selected channel is not a voice channel");
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
) -> Result<Permissions> {
    if channel.guild_id != guild.id {
        bail!("channel is outside configured guild")
    }
    if thread(channel) {
        let parent = channel
            .parent_id
            .and_then(|id| channels.iter().find(|c| c.id == id))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "thread parent is unavailable; permission check cannot be completed"
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
            language.text(
                "List visible channels and active threads by human-readable name. Active threads include a thread_id for lookup; archived threads can also be queried by an ID obtained from message metadata.",
                "현재 볼 수 있는 채널과 활성 스레드를 이름으로 나열합니다. 활성 스레드에는 조회용 thread_id가 포함됩니다. 메시지 정보에서 얻은 ID로 보관된 스레드도 조회할 수 있습니다.",
                "按可读名称列出当前可见的频道和活跃线程。活跃线程提供用于查询的 thread_id；也可使用消息元数据中的 ID 查询已归档线程。",
                "現在表示できるチャンネルとアクティブなスレッドを名前で一覧表示します。アクティブスレッドには検索用 thread_id を含めます。メッセージ情報から得たIDでアーカイブ済みスレッドも検索できます。"
            ),
            json!({"type":"object","properties":{},"additionalProperties":false}),
        ),
        def(
            "discord_list_users",
            language.text(
                "List configured-guild users with opaque paging. Needs the application's privileged GUILD_MEMBERS intent.",
                "설정된 서버의 사용자를 불투명 페이지 토큰으로 나열합니다. 애플리케이션에 privileged GUILD_MEMBERS intent가 필요합니다.",
                "使用不透明分页令牌列出已配置服务器中的用户。需要应用启用特权 GUILD_MEMBERS intent。",
                "不透明なページトークンを使って設定済みサーバーのユーザーを一覧表示します。アプリケーションに特権 GUILD_MEMBERS intent が必要です。"
            ),
            json!({"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":1000,"description":language.text("Users per page", "페이지당 사용자 수", "每页用户数", "ページあたりのユーザー数")},"page":{"type":"string","description":language.text("Opaque next-page token returned by the previous result", "이전 결과가 반환한 불투명 다음 페이지 토큰", "上次结果返回的不透明下一页令牌", "前の結果で返された不透明な次ページトークン")}},"additionalProperties":false}),
        ),
        def(
            "discord_send_message",
            language.text(
                "Use this tool to reply to a live message received from another channel or thread in that same source channel or thread, or to send a separate message requested by the user. Ordinary replies are posted automatically in the worker's home channel, so they do not reach the other channel. The live source message establishes where to reply; no separate send request is needed. Provide a channel name or channel_choice from discord_list_channels, or a thread_id from its result or message thread metadata. Long content is split into Discord-sized messages. Optional exact user names for explicit mentions. Optional files is an array of local file paths to attach; files are checked before sending.",
                "다른 채널이나 스레드에서 직접 받은 메시지에는 이 도구로 그 메시지의 채널이나 스레드에 답장하세요. 사용자가 요청한 별도 메시지를 보낼 때도 사용합니다. 일반 답변은 홈 채널에 자동 게시되므로 다른 채널에 전달되지 않습니다. 직접 수신한 메시지의 출처로 답장 위치가 정해지며 별도의 발송 요청은 필요하지 않습니다. discord_list_channels의 채널 이름·channel_choice 또는 결과나 메시지 thread 정보의 thread_id를 지정하세요. 명시적 멘션은 정확한 사용자 이름을 지정할 수 있습니다. 선택적 files 배열에는 첨부할 로컬 경로를 지정하며, 전송 전에 크기 제한과 경로를 검사합니다.",
                "使用此工具在原频道或线程回复从其他频道或线程直接收到的消息，也可发送用户另外要求的消息。普通回复会自动发到主频道，不会到达其他频道。直接收到的消息来源已确定回复位置，无需单独提出发送请求。提供 discord_list_channels 的频道名称、channel_choice，或其结果/消息 thread 信息中的 thread_id。可提供要明确提及的准确成员姓名。较长内容会拆分成多条 Discord 消息；files 数组中的本地路径会经过检查，附件按 Discord 消息限制分批发送。",
                "別チャンネルやスレッドから直接受信したメッセージには、このツールで元のチャンネルやスレッドへ返答してください。ユーザーが別途依頼したメッセージの送信にも使います。通常の返答はホームチャンネルに投稿され、他チャンネルには届きません。直接受信したメッセージの送信元が返信先となるため、別途送信を依頼される必要はありません。discord_list_channels のチャンネル名・channel_choice、または一覧やメッセージ thread 情報の thread_id を指定します。明示的なメンションには正確なユーザー名を指定できます。長文は複数のDiscordメッセージに分けます。files 配列のローカルパスを確認し、添付をDiscordメッセージ上限に合わせて分割送信します。"
            ),
            json!({"type":"object","properties":{"channel":{"type":"string","description":language.text("Channel name; optional when channel_choice is provided", "채널 이름; channel_choice가 있으면 생략할 수 있습니다", "频道名称；提供 channel_choice 时可省略", "チャンネル名。channel_choice を指定する場合は省略可")},"channel_choice":{"type":"string","description":language.text("Opaque channel choice returned by discord_list_channels", "discord_list_channels가 반환한 불투명 채널 선택 토큰", "discord_list_channels 返回的不透明频道选择值", "discord_list_channels が返した不透明なチャンネル選択値")},"thread_id":{"type":"string","description":language.text("Thread ID returned by discord_list_channels or a message's thread metadata; archived threads are also supported", "discord_list_channels 또는 메시지의 thread 정보가 반환한 스레드 ID입니다. 보관된 스레드도 지원합니다", "discord_list_channels 或消息 thread 信息返回的线程 ID；也支持已归档线程", "discord_list_channels またはメッセージの thread 情報が返したスレッドID。アーカイブ済みスレッドも指定できます")},"content":{"type":"string","description":language.text("Message body; long content is sent as multiple Discord-sized messages", "메시지 내용; 긴 내용은 Discord 메시지 크기에 맞춰 여러 개로 나눠 보냅니다", "消息正文；较长内容会拆分为多条符合 Discord 限制的消息", "メッセージ本文。長文はDiscordの上限に合わせて複数のメッセージに分けて送ります")},"mentions":{"type":"array","items":{"type":"string"},"description":language.text("Exact current guild-member names to mention explicitly", "명시적으로 멘션할 현재 서버 구성원의 정확한 이름", "要明确提及的服务器当前成员的准确姓名", "明示的にメンションする現在のサーバーメンバーの正確な名前")},"files":{"type":"array","minItems":1,"items":{"type":"string"},"description":language.text("Optional local file paths to attach; files are checked and split into Discord-sized message batches", "선택 사항: 첨부할 로컬 파일 경로입니다. 파일을 검사하고 Discord 메시지 크기에 맞춰 나눠 보냅니다", "可选：要附加的本地文件路径；会检查文件并按 Discord 消息限制分批发送", "任意：添付するローカルファイルのパスです。ファイルを確認し、Discordメッセージ上限に合わせて分割送信します")}},"anyOf":[{"required":["content"]},{"required":["files"]}],"additionalProperties":false}),
        ),
        def(
            "discord_get_messages",
            language.text(
                "Read messages from a channel name, channel_choice, or thread_id returned in channel/message metadata; archived threads are supported by ID. Use Discord message links for before, after, or around cursors.",
                "채널 이름, channel_choice 또는 채널/메시지 정보가 반환한 thread_id에서 메시지를 읽습니다. ID로 보관된 스레드도 조회할 수 있습니다. before, after, around 커서에는 Discord 메시지 링크를 사용하세요.",
                "从频道名称、channel_choice 或频道/消息元数据返回的 thread_id 读取消息；也支持通过 ID 查询已归档线程。before、after 或 around 游标使用 Discord 消息链接。",
                "チャンネル名、channel_choice、またはチャンネル/メッセージ情報で返された thread_id から読み込みます。ID指定でアーカイブ済みスレッドも読めます。before、after、around にはDiscordメッセージリンクを使います。"
            ),
            json!({"type":"object","properties":{"channel":{"type":"string","description":language.text("Channel name; optional when channel_choice is provided", "채널 이름; channel_choice가 있으면 생략할 수 있습니다", "频道名称；提供 channel_choice 时可省略", "チャンネル名。channel_choice を指定する場合は省略可")},"channel_choice":{"type":"string","description":language.text("Opaque channel choice returned by discord_list_channels", "discord_list_channels가 반환한 불투명 채널 선택 토큰", "discord_list_channels 返回的不透明频道选择值", "discord_list_channels が返した不透明なチャンネル選択値")},"thread_id":{"type":"string","description":language.text("Thread ID returned by discord_list_channels or a message's thread metadata; archived threads are also supported", "discord_list_channels 또는 메시지의 thread 정보가 반환한 스레드 ID입니다. 보관된 스레드도 지원합니다", "discord_list_channels 或消息 thread 信息返回的线程 ID；也支持已归档线程", "discord_list_channels またはメッセージの thread 情報が返したスレッドID。アーカイブ済みスレッドも指定できます")},"limit":{"type":"integer","minimum":1,"description":language.text("Maximum number of messages to return; larger requests are paged automatically", "반환할 최대 메시지 수", "最多返回的消息数", "返すメッセージの最大数")},"before":{"type":"string","description":language.text("Discord message link; return messages before this message", "Discord 메시지 링크; 이 메시지보다 이전 메시지를 반환합니다", "Discord 消息链接；返回此消息之前的消息", "Discordメッセージリンク。このメッセージより前を返します")},"after":{"type":"string","description":language.text("Discord message link; return messages after this message", "Discord 메시지 링크; 이 메시지보다 이후 메시지를 반환합니다", "Discord 消息链接；返回此消息之后的消息", "Discordメッセージリンク。このメッセージより後を返します")},"around":{"type":"string","description":language.text("Discord message link; return messages around this message", "Discord 메시지 링크; 이 메시지 주변의 메시지를 반환합니다", "Discord 消息链接；返回此消息附近的消息", "Discordメッセージリンク。このメッセージの前後を返します")},"offset":{"type":"integer","minimum":0,"description":language.text("Optional number of recent messages to skip", "선택 사항: 최근 메시지 중 건너뛸 수", "可选：跳过最近的消息数", "任意指定：直近のメッセージからスキップする件数")}},"additionalProperties":false}),
        ),
        def(
            "discord_search_messages",
            language.text(
                "Search recent history in a channel name, channel_choice, or thread_id using literal substring matching. Requested ranges are paged through Discord history.",
                "채널 이름, channel_choice 또는 thread_id의 제한된 최근 기록에서 문자 그대로의 부분 문자열을 검색합니다.",
                "在频道名称、channel_choice 或 thread_id 的有限近期历史记录中进行字面子串搜索。",
                "チャンネル名、channel_choice、または thread_id の範囲を限定した最近の履歴から、文字どおりの部分文字列を検索します。"
            ),
            json!({"type":"object","properties":{"channel":{"type":"string","description":language.text("Channel name; optional when channel_choice is provided", "채널 이름; channel_choice가 있으면 생략할 수 있습니다", "频道名称；提供 channel_choice 时可省略", "チャンネル名。channel_choice を指定する場合は省略可")},"channel_choice":{"type":"string","description":language.text("Opaque channel choice returned by discord_list_channels", "discord_list_channels가 반환한 불투명 채널 선택 토큰", "discord_list_channels 返回的不透明频道选择值", "discord_list_channels が返した不透明なチャンネル選択値")},"thread_id":{"type":"string","description":language.text("Thread ID returned by discord_list_channels or a message's thread metadata; archived threads are also supported", "discord_list_channels 또는 메시지의 thread 정보가 반환한 스레드 ID입니다. 보관된 스레드도 지원합니다", "discord_list_channels 或消息 thread 信息返回的线程 ID；也支持已归档线程", "discord_list_channels またはメッセージの thread 情報が返したスレッドID。アーカイブ済みスレッドも指定できます")},"query":{"type":"string","description":language.text("Literal substring to search for", "문자 그대로 검색할 부분 문자열", "要搜索的字面子串", "そのまま検索する部分文字列")},"max_scan":{"type":"integer","minimum":1,"description":language.text("Maximum number of recent messages to inspect; requests are paged automatically", "확인할 최근 메시지의 최대 수", "最多检查的近期消息数", "確認する直近メッセージの最大数")},"limit":{"type":"integer","minimum":1,"description":language.text("Maximum number of matches to return", "반환할 최대 일치 수", "最多返回的匹配数", "返す一致件数の最大数")}},"required":["query"],"additionalProperties":false}),
        ),
        def(
            "discord_download_attachment",
            language.text(
                "Download an attachment shown in discord_get_messages or discord_search_messages using its message link and filename. Select its channel by name/channel_choice or use thread_id for an active or archived thread. If a message has duplicate filenames, use a returned attachment_choice. The result includes the local saved path; use it in discord_send_message files to forward the file. The current message and attachment are revalidated; arbitrary URLs are not accepted.",
                "discord_get_messages 또는 discord_search_messages 결과에 표시된 첨부 파일을 메시지 링크와 파일 이름으로 다운로드합니다. 채널 이름/channel_choice 또는 활성·보관 스레드의 thread_id를 지정하세요. 파일 이름이 중복되면 반환된 attachment_choice를 사용하세요. 결과에는 저장된 로컬 경로가 포함되며 discord_send_message의 files에 지정해 전달할 수 있습니다. 메시지와 첨부 파일은 다시 확인하며 임의 URL은 허용하지 않습니다.",
                "使用消息链接和文件名下载 discord_get_messages 或 discord_search_messages 结果中显示的附件。频道可按名称或 channel_choice 选择，也可使用活动或已归档线程的 thread_id。如果消息中存在同名文件，请使用返回的 attachment_choice。结果包含本地保存路径，可将其用于 discord_send_message 的 files 转发文件。会重新验证当前消息和附件；不接受任意 URL。",
                "discord_get_messages または discord_search_messages の結果に表示された添付を、メッセージリンクとファイル名でダウンロードします。チャンネル名/channel_choice、またはアクティブ/アーカイブ済みスレッドの thread_id を指定します。同名ファイルがある場合は返された attachment_choice を使ってください。結果には保存先のローカルパスが含まれ、discord_send_message の files に指定して転送できます。現在のメッセージと添付を再検証し、任意のURLは受け付けません。"
            ),
            json!({"type":"object","properties":{"channel":{"type":"string","description":language.text("Channel name; optional when channel_choice is provided", "채널 이름; channel_choice가 있으면 생략할 수 있습니다", "频道名称；提供 channel_choice 时可省略", "チャンネル名。channel_choice を指定する場合は省略可")},"channel_choice":{"type":"string","description":language.text("Opaque channel choice returned by discord_list_channels", "discord_list_channels가 반환한 불투명 채널 선택 토큰", "discord_list_channels 返回的不透明频道选择值", "discord_list_channels が返した不透明なチャンネル選択値")},"thread_id":{"type":"string","description":language.text("Thread ID returned by discord_list_channels or a message's thread metadata; archived threads are also supported", "discord_list_channels 또는 메시지의 thread 정보가 반환한 스레드 ID입니다. 보관된 스레드도 지원합니다", "discord_list_channels 或消息 thread 信息返回的线程 ID；也支持已归档线程", "discord_list_channels またはメッセージの thread 情報が返したスレッドID。アーカイブ済みスレッドも指定できます")},"message_link":{"type":"string","description":language.text("Discord message link returned by history or search", "메시지 기록 또는 검색에서 반환된 Discord 메시지 링크", "消息历史或搜索结果返回的 Discord 消息链接", "履歴または検索結果で返されたDiscordメッセージリンク")},"filename":{"type":"string","description":language.text("Attachment filename shown by discord_get_messages/search; optional when attachment_choice is provided", "discord_get_messages/search 결과에 표시된 첨부 파일 이름; attachment_choice가 있으면 생략 가능", "discord_get_messages/search 显示的附件文件名；提供 attachment_choice 时可省略", "discord_get_messages/search に表示された添付ファイル名。attachment_choice を指定する場合は省略可")},"attachment_choice":{"type":"string","description":language.text("Opaque choice returned when attachment filenames collide", "첨부 파일 이름이 중복될 때 반환되는 불투명 선택 토큰", "附件文件名重复时返回的不透明选择值", "添付ファイル名が重複した場合に返される不透明な選択値")}},"required":["message_link"],"additionalProperties":false}),
        ),
    ]
}
fn def(name: &str, description: &str, input_schema: Value) -> Value {
    json!({"name":name,"description":description,"inputSchema":input_schema})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_file_paths_accept_optional_nonempty_local_path_arrays() {
        assert_eq!(
            explicit_file_paths(&json!({})).unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(
            explicit_file_paths(&json!({"files":["/tmp/one.txt", "/tmp/two.png"]})).unwrap(),
            vec!["/tmp/one.txt", "/tmp/two.png"]
        );
        assert!(explicit_file_paths(&json!({"files":"/tmp/one.txt"})).is_err());
        assert!(explicit_file_paths(&json!({"files":["/tmp/one.txt", " "]})).is_err());
        assert_eq!(
            explicit_file_paths(&json!({"files":vec!["/tmp/x"; 11]}))
                .unwrap()
                .len(),
            11
        );
        assert!(validate_send_payload("", 0).is_err());
        assert!(validate_send_payload("", 1).is_ok());
        assert!(validate_send_payload("caption", 0).is_ok());
        assert!(validate_channel_selection(&json!({"thread_id":"123"})).is_ok());
        assert!(validate_channel_selection(&json!({"thread_id":"0"})).is_err());
        assert!(validate_channel_selection(&json!({"thread_id":"not-an-id"})).is_err());
        assert!(
            validate_channel_selection(&json!({"thread_id":"123","channel":"general"})).is_err()
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
        assert!(require_read_permissions(ChannelType::Text, read).is_ok());
        assert!(require_read_permissions(ChannelType::Voice, read).is_err());
        assert!(require_read_permissions(ChannelType::Stage, read | Permissions::CONNECT).is_ok());
        assert!(require_read_permissions(ChannelType::Text, Permissions::VIEW_CHANNEL).is_err());
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
            requested_count(&json!({"offset":500_000}), "offset", 0).unwrap(),
            500_000
        );
        assert_eq!(
            requested_count(&json!({"max_scan":25_000}), "max_scan", 100).unwrap(),
            25_000
        );
        assert!(requested_count(&json!({"limit":-1}), "limit", 50).is_err());
        assert!(requested_count(&json!({"limit":"100"}), "limit", 50).is_err());
    }
    #[test]
    fn numeric_bounds_and_message_id_inputs_are_strict() {
        assert_eq!(
            bounded(&json!({"limit":100}), "limit", 50, 1, 100).unwrap(),
            100
        );
        assert!(bounded(&json!({"limit":101}), "limit", 50, 1, 100).is_err());
        assert!(bounded(&json!({"limit":"10"}), "limit", 50, 1, 100).is_err());
        assert_eq!(
            parse_message_link("https://discord.com/channels/1/2/123", 1, 2).unwrap(),
            MessageId::new(123)
        );
        assert!(parse_message_link("123", 1, 2).is_err());
        assert!(parse_message_link("http://discord.com/channels/1/2/123", 1, 2).is_err());
        assert!(parse_message_link("https://example.com/channels/1/2/123", 1, 2).is_err());
        assert!(parse_message_link("https://discord.com/channels/1/2/0", 1, 2).is_err());
        assert!(parse_message_link("https://discord.com/channels/1/2/123/extra", 1, 2).is_err());
        assert!(parse_message_link("https://discord.com/channels/9/2/123", 1, 2).is_err());
        assert!(parse_message_link("https://discord.com/channels/1/3/123", 1, 2).is_err());
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
        assert!(validate_voice_channel_selection(&json!({"channel":"Stage 1"})).is_ok());
        assert!(
            validate_voice_channel_selection(
                &json!({"channel_choice":"choice_0123456789abcdef0123456789abcdef"})
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
                validate_voice_channel_selection(&args).is_err(),
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
        assert!(require_voice_channel(&voice_a).is_ok());
        assert!(require_voice_channel(&channels[2]).is_err());
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
