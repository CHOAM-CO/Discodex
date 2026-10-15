use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};
use serenity::all::{
    ChannelId, ChannelType, CreateAllowedMentions, CreateMessage, GetMessages, GuildChannel,
    GuildId, Http, Member, Message, MessageId, Permissions, UserId,
};

use crate::config::Config;

const MAX_SKIP: usize = 1000;
const CHOICE_TTL: std::time::Duration = std::time::Duration::from_secs(1800);
const SCAN_MAX: usize = 1000;

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
struct SelectionMap(HashMap<String, (Pick, std::time::Instant)>);

impl SelectionMap {
    fn insert(&mut self, pick: Pick, now: std::time::Instant) -> Result<String> {
        self.0
            .retain(|_, (_, seen)| now.duration_since(*seen) < CHOICE_TTL);
        for (key, (existing, seen)) in &mut self.0 {
            if *existing == pick {
                *seen = now;
                return Ok(key.clone());
            }
        }
        if self.0.len() >= 10_000 {
            bail!("too many active name choices; unused choices expire after 30 minutes");
        }
        let key = format!("choice_{}", uuid::Uuid::new_v4().simple());
        self.0.insert(key.clone(), (pick, now));
        Ok(key)
    }

    fn lookup(&self, key: &str, now: std::time::Instant) -> Option<Pick> {
        self.0
            .get(key)
            .filter(|(_, seen)| now.duration_since(*seen) < CHOICE_TTL)
            .map(|(pick, _)| pick.clone())
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
        map.insert(pick, std::time::Instant::now())
    }

    fn get_pick(&self, key: &str) -> Result<Pick> {
        self.selections
            .lock()
            .map_err(|_| anyhow::anyhow!("selection state lock poisoned"))?
            .lookup(key, std::time::Instant::now())
            .ok_or_else(|| {
                anyhow::anyhow!("selection expired; list current channels or users again")
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
        for channel in all
            .iter()
            .filter(|c| c.kind != ChannelType::Category && c.name == leaf)
        {
            let channel = channel.clone();
            if category.is_some_and(|cat| {
                channel
                    .parent_id
                    .and_then(|id| categories.get(&id))
                    .is_none_or(|n| n != cat)
            }) {
                continue;
            }
            if !permissions_in(&guild, &bot_member, &channel, &all)?
                .contains(Permissions::VIEW_CHANNEL)
            {
                continue;
            }
            matches.push(channel);
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
            out.push(json!({"name":name,"channel":c.name,"category":cat,"home":c.id.get() == self.config.home_channel_id,"type":format!("{:?}",c.kind),"choice":self.save_pick(Pick::Channel{id:c.id,name:c.name.clone(),parent:c.parent_id})?}));
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
        let limit = bounded(args, "limit", 50, 1, 100)?;
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
        validate_keys(args, &["channel", "channel_choice", "content", "mentions"])?;
        validate_channel_selection(args)?;
        let raw_content = required(args, "content")?;
        if raw_content.chars().count() > 2000 {
            bail!("message content exceeds Discord's 2000-character limit")
        }
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
        let mut text = raw_content.to_owned();
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
        text = append_mentions(text, &users);
        if text.chars().count() > 2000 {
            bail!("message plus explicit mentions exceeds Discord's 2000-character limit")
        }
        let mentions = CreateAllowedMentions::new()
            .everyone(false)
            .all_roles(false)
            .all_users(false)
            .empty_roles()
            .users(users);
        let server = self.guild().await?.name;
        let sent = channel
            .send_message(
                &*self.http,
                CreateMessage::new()
                    .content(text)
                    .allowed_mentions(mentions),
            )
            .await
            .context("send Discord message")?;
        Ok(
            json!({"status":"sent","server":server,"channel":channel.name,"message_link":message_link(self.config.guild_id, channel.id.get(), sent.id.get())}),
        )
    }

    async fn get_messages(&self, args: &Value) -> Result<Value> {
        validate_keys(
            args,
            &[
                "channel",
                "channel_choice",
                "limit",
                "before",
                "after",
                "around",
                "offset",
            ],
        )?;
        validate_channel_selection(args)?;
        let limit = bounded(args, "limit", 50, 1, 100)? as u8;
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
        let offset = bounded(args, "offset", 0, 0, MAX_SKIP as u64)? as usize;
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
        let messages = if offset > 0 && after.is_none() && around.is_none() {
            while offset > 0 {
                let n = offset.min(100) as u8;
                let mut req = GetMessages::new().limit(n);
                if let Some(id) = cursor {
                    req = req.before(id)
                }
                let page = c
                    .messages(&*self.http, req)
                    .await
                    .context("skip recent messages")?;
                if page.is_empty() {
                    return Ok(
                        json!({"server":self.guild().await?.name,"channel":c.name,"messages":[],"next_before":null,"next_after":null,"next_cursor":null,"history_exhausted":true}),
                    );
                }
                offset = offset.saturating_sub(page.len());
                cursor = page.last().map(|m| m.id);
            }
            let mut req = GetMessages::new().limit(limit);
            if let Some(id) = cursor {
                req = req.before(id)
            }
            c.messages(&*self.http, req)
                .await
                .context("read Discord messages")?
        } else {
            let mut req = GetMessages::new().limit(limit);
            if let Some(id) = before {
                req = req.before(id)
            }
            if let Some(id) = after {
                req = req.after(id)
            }
            if let Some(id) = around {
                req = req.around(id)
            }
            c.messages(&*self.http, req)
                .await
                .context("read Discord messages")?
        };
        let mut result = message_cursors(
            messages.iter().map(|m| m.id),
            after.is_some(),
            self.config.guild_id,
            c.id.get(),
        );
        result["server"] = json!(self.guild().await?.name);
        result["channel"] = json!(c.name);
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
            &["channel", "channel_choice", "query", "max_scan", "limit"],
        )?;
        validate_channel_selection(args)?;
        let max = bounded(args, "max_scan", 100, 1, SCAN_MAX as u64)? as usize;
        let result_limit = bounded(args, "limit", 1000, 1, SCAN_MAX as u64)? as usize;
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
        Ok(
            json!({"server":self.guild().await?.name,"channel":c.name,"query":query,"messages":found,"scanned":scanned,"max_scan":max,"requested_scope_complete":true,"history_exhausted":history_exhausted,"matching_count":matching_count,"truncated_matches":matching_count.saturating_sub(result_limit)}),
        )
    }

    async fn download_attachment(&self, args: &Value) -> Result<Value> {
        validate_keys(
            args,
            &[
                "channel",
                "channel_choice",
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
        Ok(serde_json::to_value(result)?)
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
    match (v.get("channel"), v.get("channel_choice")) {
        (Some(name), None) if name.as_str().is_some_and(|s| !s.trim().is_empty()) => Ok(()),
        (None, Some(choice)) if choice.as_str().is_some_and(is_choice) => Ok(()),
        (Some(_), Some(_)) => bail!("provide exactly one of channel or channel_choice"),
        (Some(_), None) => bail!("channel must be a non-empty name string"),
        (None, Some(_)) => bail!("channel_choice must be an opaque choice string"),
        (None, None) => bail!("channel or channel_choice is required"),
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
fn append_mentions(mut content: String, users: &[UserId]) -> String {
    if !users.is_empty() {
        let tags = users
            .iter()
            .map(|id| format!("<@{}>", id.get()))
            .collect::<Vec<_>>()
            .join(" ");
        if !content.is_empty() {
            content.push(' ')
        }
        content.push_str(&tags)
    }
    content
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
    json!({"link":message_link(guild_id,channel_id,m.id.get()),"author":author,"content":content,"timestamp":m.timestamp.to_string(),"attachments":attachments})
}

pub fn tool_definitions(language: crate::i18n::Language) -> Vec<Value> {
    vec![
        def(
            "discord_list_channels",
            language.text(
                "List visible channels and active threads by human-readable server/channel name. When names collide, use the returned opaque choice. Archived threads are not included.",
                "사람이 읽을 수 있는 서버/채널 이름으로 현재 볼 수 있는 채널과 활성 스레드를 나열합니다. 이름이 겹치면 반환된 선택 토큰을 사용하세요. 보관된 스레드는 포함하지 않습니다.",
                "按服务器/频道名称列出当前可见的频道和活跃线程。名称重复时请使用返回的不透明选择值。不包括已归档线程。",
                "人が読めるサーバー/チャンネル名で、現在表示できるチャンネルとアクティブなスレッドを一覧表示します。名前が重複する場合は返された選択値を使ってください。アーカイブ済みスレッドは含みません。"
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
            json!({"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":100,"description":language.text("Users per page", "페이지당 사용자 수", "每页用户数", "ページあたりのユーザー数")},"page":{"type":"string","description":language.text("Opaque next-page token returned by the previous result", "이전 결과가 반환한 불투명 다음 페이지 토큰", "上次结果返回的不透明下一页令牌", "前の結果で返された不透明な次ページトークン")}},"additionalProperties":false}),
        ),
        def(
            "discord_send_message",
            language.text(
                "For explicitly requested sends to another channel; normal replies are posted automatically in the worker's home channel. Provide a channel name (optionally prefixed with #) or a channel_choice from discord_list_channels. Optional exact user names (optionally prefixed with @) for explicit mentions.",
                "다른 채널로 명시적으로 요청된 메시지를 보냅니다. 일반 답변은 워커의 홈 채널에 자동 게시됩니다. 채널 이름(# 접두어 선택 사항) 또는 discord_list_channels가 반환한 channel_choice를 지정하세요. 명시적 멘션은 정확한 사용자 이름(@ 접두어 선택 사항)을 지정할 수 있습니다.",
                "用于按明确要求向其他频道发送消息；普通回复会自动发布在 worker 的主频道。提供频道名称（可选以 # 开头）或 discord_list_channels 返回的 channel_choice。若要明确提及用户，可提供完全匹配的用户名（可选以 @ 开头）。",
                "別チャンネルへの明示的な送信依頼に使います。通常の返答はworkerのホームチャンネルに自動投稿されます。チャンネル名（先頭に # を付けても可）または discord_list_channels が返した channel_choice を指定してください。明示的なメンションには完全一致のユーザー名（先頭に @ を付けても可）を指定できます。"
            ),
            json!({"type":"object","properties":{"channel":{"type":"string","description":language.text("Channel name; optional when channel_choice is provided", "채널 이름; channel_choice가 있으면 생략할 수 있습니다", "频道名称；提供 channel_choice 时可省略", "チャンネル名。channel_choice を指定する場合は省略可")},"channel_choice":{"type":"string","description":language.text("Opaque channel choice returned by discord_list_channels", "discord_list_channels가 반환한 불투명 채널 선택 토큰", "discord_list_channels 返回的不透明频道选择值", "discord_list_channels が返した不透明なチャンネル選択値")},"content":{"type":"string","description":language.text("Message body", "메시지 내용", "消息正文", "メッセージ本文")},"mentions":{"type":"array","items":{"type":"string"},"description":language.text("Exact current guild-member names to mention explicitly", "명시적으로 멘션할 현재 서버 구성원의 정확한 이름", "要明确提及的服务器当前成员的准确姓名", "明示的にメンションする現在のサーバーメンバーの正確な名前")}},"required":["content"],"additionalProperties":false}),
        ),
        def(
            "discord_get_messages",
            language.text(
                "Read messages from a channel name or channel_choice. Use a Discord message link for before, after, or around cursors; latest-message offset is optional.",
                "채널 이름 또는 channel_choice에서 메시지를 읽습니다. before, after, around 커서에는 Discord 메시지 링크를 사용하세요. 최신 메시지 기준 offset은 선택 사항입니다.",
                "从频道名称或 channel_choice 读取消息。before、after 或 around 游标使用 Discord 消息链接；也可以选择提供相对于最新消息的 offset。",
                "チャンネル名または channel_choice からメッセージを読みます。before、after、around のカーソルにはDiscordメッセージリンクを使ってください。最新メッセージからの offset は任意です。"
            ),
            json!({"type":"object","properties":{"channel":{"type":"string","description":language.text("Channel name; optional when channel_choice is provided", "채널 이름; channel_choice가 있으면 생략할 수 있습니다", "频道名称；提供 channel_choice 时可省略", "チャンネル名。channel_choice を指定する場合は省略可")},"channel_choice":{"type":"string","description":language.text("Opaque channel choice returned by discord_list_channels", "discord_list_channels가 반환한 불투명 채널 선택 토큰", "discord_list_channels 返回的不透明频道选择值", "discord_list_channels が返した不透明なチャンネル選択値")},"limit":{"type":"integer","minimum":1,"maximum":100,"description":language.text("Maximum number of messages to return", "반환할 최대 메시지 수", "最多返回的消息数", "返すメッセージの最大数")},"before":{"type":"string","description":language.text("Discord message link; return messages before this message", "Discord 메시지 링크; 이 메시지보다 이전 메시지를 반환합니다", "Discord 消息链接；返回此消息之前的消息", "Discordメッセージリンク。このメッセージより前を返します")},"after":{"type":"string","description":language.text("Discord message link; return messages after this message", "Discord 메시지 링크; 이 메시지보다 이후 메시지를 반환합니다", "Discord 消息链接；返回此消息之后的消息", "Discordメッセージリンク。このメッセージより後を返します")},"around":{"type":"string","description":language.text("Discord message link; return messages around this message", "Discord 메시지 링크; 이 메시지 주변의 메시지를 반환합니다", "Discord 消息链接；返回此消息附近的消息", "Discordメッセージリンク。このメッセージの前後を返します")},"offset":{"type":"integer","minimum":0,"maximum":1000,"description":language.text("Optional number of recent messages to skip", "선택 사항: 최근 메시지 중 건너뛸 수", "可选：跳过最近的消息数", "任意指定：直近のメッセージからスキップする件数")}},"additionalProperties":false}),
        ),
        def(
            "discord_search_messages",
            language.text(
                "Search bounded recent history in the required channel name or channel_choice using literal substring matching.",
                "필수 채널 이름 또는 channel_choice에서 제한된 최근 기록을 대상으로 문자 그대로의 부분 문자열을 검색합니다.",
                "在指定频道名称或 channel_choice 的有限近期历史记录中进行字面子串搜索。",
                "必須のチャンネル名または channel_choice の、範囲を限定した最近の履歴から、文字どおりの部分文字列を検索します。"
            ),
            json!({"type":"object","properties":{"channel":{"type":"string","description":language.text("Channel name; optional when channel_choice is provided", "채널 이름; channel_choice가 있으면 생략할 수 있습니다", "频道名称；提供 channel_choice 时可省略", "チャンネル名。channel_choice を指定する場合は省略可")},"channel_choice":{"type":"string","description":language.text("Opaque channel choice returned by discord_list_channels", "discord_list_channels가 반환한 불투명 채널 선택 토큰", "discord_list_channels 返回的不透明频道选择值", "discord_list_channels が返した不透明なチャンネル選択値")},"query":{"type":"string","description":language.text("Literal substring to search for", "문자 그대로 검색할 부분 문자열", "要搜索的字面子串", "そのまま検索する部分文字列")},"max_scan":{"type":"integer","minimum":1,"maximum":1000,"description":language.text("Maximum number of recent messages to inspect", "확인할 최근 메시지의 최대 수", "最多检查的近期消息数", "確認する直近メッセージの最大数")},"limit":{"type":"integer","minimum":1,"maximum":1000,"description":language.text("Maximum number of matches to return", "반환할 최대 일치 수", "最多返回的匹配数", "返す一致件数の最大数")}},"required":["query"],"additionalProperties":false}),
        ),
        def(
            "discord_download_attachment",
            language.text(
                "Download an attachment by filename from a specific Discord message link. If that message has duplicate filenames, use a returned attachment_choice. The current message and attachment are revalidated; arbitrary URLs are not accepted.",
                "특정 Discord 메시지 링크에서 파일 이름으로 첨부 파일을 다운로드합니다. 파일 이름이 중복되면 반환된 attachment_choice를 사용하세요. 메시지와 첨부 파일은 다시 확인하며 임의 URL은 허용하지 않습니다.",
                "根据特定 Discord 消息链接中的文件名下载附件。如果文件名重复，请使用返回的 attachment_choice。会重新验证当前消息和附件；不接受任意 URL。",
                "特定のDiscordメッセージリンクからファイル名で添付ファイルをダウンロードします。同じ名前が複数ある場合は、返された attachment_choice を使ってください。現在のメッセージと添付を再検証し、任意のURLは受け付けません。"
            ),
            json!({"type":"object","properties":{"channel":{"type":"string","description":language.text("Channel name; optional when channel_choice is provided", "채널 이름; channel_choice가 있으면 생략할 수 있습니다", "频道名称；提供 channel_choice 时可省略", "チャンネル名。channel_choice を指定する場合は省略可")},"channel_choice":{"type":"string","description":language.text("Opaque channel choice returned by discord_list_channels", "discord_list_channels가 반환한 불투명 채널 선택 토큰", "discord_list_channels 返回的不透明频道选择值", "discord_list_channels が返した不透明なチャンネル選択値")},"message_link":{"type":"string","description":language.text("Discord message link returned by history or search", "메시지 기록 또는 검색에서 반환된 Discord 메시지 링크", "消息历史或搜索结果返回的 Discord 消息链接", "履歴または検索結果で返されたDiscordメッセージリンク")},"filename":{"type":"string","description":language.text("Attachment filename; optional when attachment_choice is provided", "첨부 파일 이름; attachment_choice가 있으면 생략할 수 있습니다", "附件文件名；提供 attachment_choice 时可省略", "添付ファイル名。attachment_choice を指定する場合は省略可")},"attachment_choice":{"type":"string","description":language.text("Opaque choice returned when attachment filenames collide", "첨부 파일 이름이 중복될 때 반환되는 불투명 선택 토큰", "附件文件名重复时返回的不透明选择值", "添付ファイル名が重複した場合に返される不透明な選択値")}},"required":["message_link"],"additionalProperties":false}),
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
    fn mention_content_appends_explicit_ids_and_preserves_body() {
        let ids = [UserId::new(7), UserId::new(9)];
        assert_eq!(append_mentions("hello".into(), &ids), "hello <@7> <@9>");
        assert_eq!(append_mentions(String::new(), &[]), "");
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
    fn repeated_choices_reuse_identity_and_expired_choices_are_removed() {
        let now = std::time::Instant::now();
        let mut choices = SelectionMap::default();
        let pick = Pick::User {
            id: UserId::new(7),
            label: "Nick".to_owned(),
        };
        let first = choices.insert(pick.clone(), now).unwrap();
        assert_eq!(choices.insert(pick.clone(), now).unwrap(), first);
        assert!(choices.lookup(&first, now).is_some());
        assert_eq!(choices.0.len(), 1);
        let later = now + CHOICE_TTL;
        assert!(choices.lookup(&first, later).is_none());
        let second = choices.insert(pick, later).unwrap();
        assert_ne!(first, second);
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
