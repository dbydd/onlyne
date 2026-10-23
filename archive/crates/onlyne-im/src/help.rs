//! Progressive in-band `--help` for the IM gateway
//! (`docs/v2.10-IM-PROGRESSIVE-HELP.md`).
//!
//! A role that does not know which markers this gateway accepts, which chat
//! keys it serves, or what a platform backend can render can ask instead of
//! guessing — or worse, calling the platform's HTTP API directly. The request
//! is a stateless `note` addressed to the gateway role whose first line is
//! `<help>` (or a CLI alias), with an optional topic and subtopic:
//!
//! ```text
//! <help>                     level 0 — the overview
//! <help:markers>             level 1 — one topic
//! <help:markers:action>      level 2 — one subtopic
//! <chat:KEY>
//! <help:feishu>              the same help, with that conversation's metadata
//! ```
//!
//! [`parse_help_query`] decides whether a delivery is such a request, and
//! [`render_help`] writes the answer. Both are pure: the caller (`link`)
//! acks the original delivery and sends the answer back as a note, so no
//! help request ever reaches an external chat platform.

use crate::config::Gateway;

/// One parsed help request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpQuery {
    /// The topic, lowercased and trimmed: `""` for the level-0 overview,
    /// `"markers"` for a level-1 topic, `"markers:action"` for a level-2 one.
    pub topic: String,
    /// The conversation the request asked about, when line 1 named one.
    pub chat_key: Option<String>,
}

/// Whether a delivery body is a help request, and what it asks for.
///
/// Two shapes are accepted, both judged on the trimmed **first** line:
///
/// * a global request — `<help>`, `<help:TOPIC>`, `--help`, `-h`, `help`,
///   `/help`, `--help TOPIC`, `help TOPIC`;
/// * a conversation-scoped request — line 1 is a well-formed `<chat:KEY>`
///   alone, and line 2 is a help form (the CLI aliases included).
///
/// Anything else answers `None` and takes the ordinary delivery path.
pub fn parse_help_query(text: &str) -> Option<HelpQuery> {
    let mut lines = text.lines();
    let first = lines.next()?.trim();
    if let Some(topic) = help_topic(first) {
        return Some(HelpQuery {
            topic,
            chat_key: None,
        });
    }
    let key = chat_key_of(first)?;
    let topic = help_topic(lines.next()?.trim())?;
    Some(HelpQuery {
        topic,
        chat_key: Some(key),
    })
}

/// The topic one help line names, or `None` when the line is not a help form.
///
/// The wrapper is matched case-insensitively and the topic is normalized to
/// lowercase; `all` and an empty name both mean the level-0 overview, so a
/// caller has one spelling of the root.
fn help_topic(line: &str) -> Option<String> {
    let lower = line.trim().to_ascii_lowercase();
    if let Some(inner) = lower
        .strip_prefix('<')
        .and_then(|rest| rest.strip_suffix('>'))
    {
        let rest = inner.strip_prefix("help")?;
        let topic = if rest.is_empty() {
            ""
        } else {
            rest.strip_prefix(':')?
        };
        return Some(normalize_topic(topic));
    }
    let (head, tail) = match lower.split_once(char::is_whitespace) {
        Some((head, tail)) => (head, tail.trim()),
        None => (lower.as_str(), ""),
    };
    matches!(head, "--help" | "-h" | "help" | "/help").then(|| normalize_topic(tail))
}

fn normalize_topic(topic: &str) -> String {
    let topic = topic.trim().to_ascii_lowercase();
    if topic == "all" { String::new() } else { topic }
}

/// The key line 1 names, when it is exactly a well-formed `<chat:KEY>`.
fn chat_key_of(line: &str) -> Option<String> {
    let key = line.strip_prefix("<chat:")?.strip_suffix('>')?;
    crate::chatkey::validate(key).then(|| key.to_string())
}

/// Render the answer to one help request.
///
/// A conversation-scoped request is prefixed with that conversation's live
/// metadata — whether the key is registered, and if so its slug, backend,
/// platform coordinate, and last inbound message id.
pub fn render_help(gateway: &Gateway, query: &HelpQuery) -> String {
    let mut out = String::new();
    if let Some(key) = query.chat_key.as_deref() {
        out.push_str(&conversation_context(gateway, key));
        out.push('\n');
    }
    let topic = normalize_topic(&query.topic);
    match topic.as_str() {
        "" => out.push_str(&overview(gateway)),
        "markers" => out.push_str(MARKERS),
        "markers:format" => out.push_str(MARKERS_FORMAT),
        "markers:action" => out.push_str(MARKERS_ACTION),
        "markers:media" => out.push_str(MARKERS_MEDIA),
        "markers:reply" => out.push_str(MARKERS_REPLY),
        "chats" => out.push_str(&chats(gateway)),
        "chats:adopt" => out.push_str(CHATS_ADOPT),
        "feishu" => out.push_str(FEISHU),
        "feishu:card" => out.push_str(FEISHU_CARD),
        "feishu:media" => out.push_str(FEISHU_MEDIA),
        "feishu:events" => out.push_str(FEISHU_EVENTS),
        "qqbot" => out.push_str(QQBOT),
        "mcp" => out.push_str(MCP),
        "examples" => out.push_str(&examples(gateway)),
        other => {
            out.push_str(&format!("Unknown help topic \"{other}\".\n\n"));
            out.push_str(TOPIC_INDEX);
        }
    }
    out
}

/// The conversation metadata a `<chat:KEY>\n<help>` request asks for.
fn conversation_context(gateway: &Gateway, key: &str) -> String {
    let mut out = String::from("[Conversation Context]\n");
    match gateway.conversation_for_key(key) {
        Some(entry) => {
            out.push_str(&format!(
                "<chat:{key}> is registered in this gateway\n  \
                 slug:         {}\n  \
                 gateway:      {}\n  \
                 conversation: {}\n",
                entry.slug, entry.gateway, entry.conversation
            ));
            match crate::chatlog::last_inbound_id(&entry) {
                Some(id) => out.push_str(&format!("  last inbound: <msg:{id}>\n")),
                None => out.push_str("  last inbound: none recorded\n"),
            }
        }
        None => out.push_str(&format!(
            "<chat:{key}> is not registered in this gateway\n  \
             send <help:chats> to list the keys this gateway serves, or put\n  \
             <chat:GATEWAY:CONVERSATION_ID> on line 1 for outbound first contact\n"
        )),
    }
    out
}

/// Level 0: the golden rule, the minimal delivery, the marker cheatsheet, this
/// gateway's live state, and the topic directory.
fn overview(gateway: &Gateway) -> String {
    let backends = if gateway.backends.is_empty() {
        "none configured".to_string()
    } else {
        gateway
            .backends
            .iter()
            .map(|backend| backend.gateway.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let registered = gateway.conversations().len();
    format!(
        "Onlyne IM gateway help — level 0 (overview)\n\
         \n\
         Golden rule: send chat messages with onlyne_send to the gateway role `{}`.\n\
         Never call a platform HTTP API (curl, an SDK, lark-cli) to post a chat\n\
         message: the gateway renders the markers, uploads the media, and routes\n\
         the message to the conversation the key names.\n\
         \n\
         Minimal delivery — line 1 names the conversation, a blank line ends the\n\
         header, and everything after it is the body:\n\
         \n\
         \x20 <chat:KEY>\n\
         \x20\n\
         \x20 the words to send\n\
         \n\
         Header markers — one line each, directly under <chat:KEY>, any order:\n\
         \x20 <msg:PREFIX>             answer a recorded message by id or unique prefix\n\
         \x20 <format:markdown|plain>  markdown card instead of plain text (default: plain)\n\
         \x20 <action:ID|LABEL|STYLE>  one interactive button (STYLE: primary, danger, default)\n\
         \x20 <image:/abs/path>        upload a local image\n\
         \x20 <file:/abs/path>         upload a local file\n\
         \x20 <sticker:ID>             a platform sticker id\n\
         \n\
         This gateway right now:\n\
         \x20 role:          {}\n\
         \x20 backends:      {}\n\
         \x20 conversations: {} registered\n\
         \n\
         {}",
        gateway.role, gateway.role, backends, registered, TOPIC_INDEX
    )
}

/// The level-0 directory of level-1 topics, reused by the unknown-topic answer.
const TOPIC_INDEX: &str = "\
More help topics (drill down like --help subcommands):\n\
\x20 <help:markers>   all six header markers, ordering rules, and their subtopics\n\
\x20 <help:chats>     the chat keys and platform coordinates this gateway serves\n\
\x20 <help:feishu>    Feishu cards, tables, buttons, and media\n\
\x20 <help:qqbot>     QQ addressing and passive-reply rules\n\
\x20 <help:mcp>       the declarative TOML bridge for MCP-only platforms\n\
\x20 <help:examples>  copy-paste onlyne_send examples\n";

/// Level 1: every marker family, its exact syntax, and the ordering rule.
const MARKERS: &str = r##"Header markers — level 1

Line 1 must be <chat:KEY> alone (1-128 characters from [A-Za-z0-9_:-]), with
no leading or trailing spaces. The marker lines follow it immediately, each on
its own line, in any order. The first line that is not a marker — including a
blank line — ends the header and starts the body; from there on, marker-shaped
text is ordinary words.

  <msg:PREFIX>
      Answer a recorded platform message. Single use: a second <msg:...> line
      is body text. Detail: <help:markers:reply>

  <format:markdown> | <format:plain>
      Choose the body format. Single use; aliases md/card mean markdown,
      text means plain. Detail: <help:markers:format>

  <action:ID|LABEL|STYLE>
      One interactive button; repeatable for several. ID is the callback
      value, LABEL defaults to ID, STYLE is primary/danger/default.
      Detail: <help:markers:action>

  <image:/abs/path> | <file:/abs/path>
      Attach a local file the gateway process can read; repeatable. The file
      name becomes the attachment label. Detail: <help:markers:media>

  <sticker:ID>
      A platform sticker id. Single use; sent as the message's metadata, not
      as words.

Rules and failure modes:
  - Marker lines are not trimmed: no leading or trailing spaces, or the line
    is body text.
  - The key line is not trimmed either: <chat:KEY> must be the whole line.
  - Markers after the body begins are literal text, never silently honoured.
  - An unknown marker (<format:mdx>, <action:>) is not a marker at all; it is
    body text and would be shown to the reader. Check <help:markers:*> first.
"##;

const MARKERS_FORMAT: &str = r##"Marker family: <format:markdown> / <format:plain> — level 2

Syntax: one line, exactly <format:markdown> or <format:plain>.
Aliases: md and card mean markdown; text means plain.
Default: plain. Single use; a second <format:...> line is body text.

What each backend does with markdown:
  Feishu   an interactive card. The first line, when it starts with "# ",
           becomes the blue card header; markdown tables become card tables;
           everything else is one markdown element.
  Telegram the body is sent with parse_mode MarkdownV2.
  Slack    mrkdwn parsing stays on; plain turns it off.
  QQ       text only: markdown arrives as its raw characters.
  Linear   a comment body; markdown is rendered by Linear.
  MCP      {{format}} carries "markdown" or "plain" to the configured tool.

Useful shapes on Feishu:
  # 部署审批

  | 环境 | 版本 |
  |---|---|
  | prod | v2.10.0 |

Failure modes:
  - The heading is only a header when it is the first line; "# " further down
    stays body text.
  - <format:mdx> is not a marker: the whole line is shown to the reader.
  - A format line after the body started is literal text.

See also: <help:markers> for the other markers and their ordering.
"##;

const MARKERS_ACTION: &str = r##"Marker family: <action:ID|LABEL|STYLE> — level 2

Syntax: one line per button, <action:ID|LABEL|STYLE>.
  ID     required, non-empty; the value a press reports back as `action`.
  LABEL  the button text; defaults to ID when omitted.
  STYLE  primary, danger, or default — a hint the backend may honour.
Repeatable: each <action:...> line adds one button. A fourth | field stays
inside STYLE.

Where buttons exist:
  Feishu    an interactive card whose last element is a row of buttons; the
            button type follows STYLE.
  Telegram  an inline keyboard, one button per row; STYLE is ignored.
  Slack, QQ, Linear, MCP declare no action capability and drop the buttons.

A press arrives back at the conversation's trigger as an inbound `action`
event whose `action` is the button's ID (Feishu also sends the card's message
id as the reply anchor). Answer with an ordinary reply; on Feishu the press is
acknowledged immediately with an info toast, because the callback must answer
inside three seconds, before any agent has decided anything.

Example:
  <chat:7f3a9c2e1b04d5a6f8c1e3b7a9d2f4c6>
  <format:markdown>
  <action:approve|批准|primary>
  <action:reject|驳回|danger>

  # 是否批准发布？
  | 服务 | 版本 |
  |---|---|
  | api | v2.10.0 |

Failure modes:
  - An empty ID (<action:|批准>) is not a marker; the line becomes body text.
  - Buttons without <format:markdown> still render as a card on Feishu; the
    body is plain text inside it.

See also: <help:markers> for the other markers and their ordering,
<help:feishu:card> for the card shape.
"##;

const MARKERS_MEDIA: &str = r##"Marker families: <image:/abs/path> and <file:/abs/path> — level 2

Syntax: one absolute path per line, repeatable, any order after the key.
The path is on the host that runs the gateway, not the sender's host. The
gateway reads the bytes; the file name becomes the attachment label.

What each backend does:
  Feishu    uploads the bytes, then sends an image or file message. A path
            already shaped feishu:image_key:<key> or feishu:file_key:<key>
            is sent as that key without uploading.
  Telegram  sendPhoto for images, sendDocument for other files; the first
            image carries the text as its caption.
  Slack     a local file is uploaded; an http(s):// attachment becomes a link
            in the text.
  Linear    a local file is uploaded and linked in the comment.
  QQ        the text render path carries no attachments; send a link instead.

The envelope's inline image (onlyne_send's `image` argument) is a different
channel: one image, at most 2 MiB decoded, written into the conversation's
media directory. <image:/path> names a file that already exists on the
gateway host.

Failure modes:
  - A relative path, a missing file, or a platform size limit fails the send;
    the delivery settles as failed and the failure names the backend.
  - Pointing at the sender's own temporary file does not work: the gateway
    cannot read another host's filesystem.

See also: <help:feishu:media> for inbound download and the media directory.
"##;

const MARKERS_REPLY: &str = r##"Marker family: <msg:PREFIX> — level 2

Syntax: <msg:ID>, where ID is a platform message id or any unique prefix of
one recorded in this conversation's chat.jsonl.

Resolution rules:
  - The prefix is matched against the conversation's recorded window.
  - One match answers that full id; the backend then receives the whole id,
    because a platform will not take a truncation of its own id.
  - An ambiguous prefix is refused with the candidates listed; write a longer
    prefix and resend.
  - An absent prefix is refused while the window has messages. On an empty
    window a string of at least 12 characters is taken as a whole id this
    cluster never recorded, so a first reply into an unrecorded thread works.
  - No <msg:...> marker at all: the gateway answers the conversation's last
    inbound event, when the log has one.

Per platform:
  QQ        `msg_id` is what makes a group or c2c send a passive reply,
            accepted without the proactive-message grant. The passive window
            is 5 minutes and at most 5 replies per message. Displaying the
            quote needs a second, separate REFIDX handle; the gateway attaches
            it when the recorded message carries one.
  Feishu    the reply threads under the message, as a new message.
  Slack     `thread_ts` is the message being answered.
  Telegram  `reply_to_message_id`.
  Linear    the comment is created on the issue the conversation belongs to.

Failure modes:
  - A typo becomes a refusal, not text: a <msg:...> line is never shown to the
    reader and the delivery is recorded as an im_unplaced_delivery fault.
  - Markers after the body started are literal text; the reply silently
    becomes an ordinary message.

See also: <help:markers> for the other markers and their ordering,
<help:qqbot> for the passive window.
"##;

/// Level 1: the live conversation directory.
fn chats(gateway: &Gateway) -> String {
    let entries = gateway.conversations();
    let mut out = format!(
        "Registered conversations — level 1 ({} total)\n\n",
        entries.len()
    );
    if entries.is_empty() {
        out.push_str(
            "This gateway serves no conversation yet.\n\
             \x20 - an inbound platform event adopts its conversation automatically, when\n\
             \x20   the listen policy admits it;\n\
             \x20 - or put <chat:GATEWAY:CONVERSATION_ID> on line 1 to adopt a known\n\
             \x20   coordinate on first outbound contact;\n\
             \x20 - or pre-register one with `onlyne-im generate`.\n",
        );
    } else {
        for entry in &entries {
            out.push_str(&format!(
                "  <chat:{}>  gateway={}  conversation={}  slug={}\n",
                entry.chat_key, entry.gateway, entry.conversation, entry.slug
            ));
        }
    }
    out.push_str(
        "\nOutbound first contact: line 1 may name a platform coordinate instead of a\n\
         key — <chat:GATEWAY:CONVERSATION_ID>, or <chat:CONVERSATION_ID> on a gateway\n\
         root that declares one backend — and the conversation is adopted on send.\n\
         \n\
         How a conversation enters the registry: <help:chats:adopt>\n",
    );
    out
}

const CHATS_ADOPT: &str = r##"How a conversation enters the registry — level 2

1. Inbound auto-adoption. A platform event for a conversation no workspace
   carries mints one: coordinates, a fresh 32-hex chat key, and a media
   directory under conversations/<slug>/. The trigger script sees the event in
   the same run. [im.listen] decides whether it may happen.

2. Startup listing. On start, each backend that can list its chats — Feishu,
   for example — reports the conversations the bot is already in, and those
   are adopted before the first event arrives.

3. Disk hot-reload. A lookup that misses the in-memory registry rescans
   conversations/*/ once, so a workspace written by `onlyne-im generate` while
   the gateway is running is served without a restart.

4. Outbound first contact. Line 1 may be a coordinate:
     <chat:feishu:oc_xxxxxxxxxxxxxxxx>
     <chat:qqbot:c2c:437CD65AC133C7DC>
     <chat:qqbot:group:ABCDEF>
   When the gateway id matches a configured backend and the listen policy
   admits the platform conversation id, the coordinate is adopted and the
   message is rendered through that backend. A gateway root with exactly one
   backend also accepts the bare coordinate, e.g. <chat:c2c:OPENID>.

Why an unknown 32-hex key is refused:
   A bare 32-hex string has the shape of a minted chat key, not a platform
   coordinate. Adopting it would turn a typo into a real conversation and a
   message into the wrong chat. The refusal names the key; ask <help:chats>
   for the keys that actually exist.

Listen policy ([im.listen]):
   allow  the only conversational ids that may be adopted; empty means all.
   deny   ids never adopted; deny wins over allow.
   Both match the platform conversation id exactly as the backend spells it.

See also: <help:chats> for the live key list.
"##;

const FEISHU: &str = r##"Feishu / Lark backend — level 1

Inbound:  im.message.receive_v1, im.chat.member.bot.added_v1,
          card.action.trigger.
Outbound: plain text, an interactive markdown card, card tables, buttons,
          images, and files.

The boundary: chat messages — cards, buttons, images, files — go through
onlyne_send to this gateway. Use lark-cli or the Feishu MCP for non-chat
workspace operations: documents, bitable, calendar, approvals, task lookup,
and reading historical thread context by oc_/om_ ids.

Subtopics:
  <help:feishu:card>    cards, tables, headers, buttons, and their callbacks
  <help:feishu:media>   inbound resource download and outbound upload
  <help:feishu:events>  the events and scopes the app must subscribe to
"##;

const FEISHU_CARD: &str = r##"Feishu cards and buttons — level 2

<format:markdown> turns the body into an interactive card:
  - the first line, when it starts with "# ", becomes the blue card header;
  - a markdown table becomes a card table (grey, bold header row, at most ten
    data rows per page);
  - everything else is a markdown element.

<action:ID|LABEL|STYLE> adds a button row; STYLE maps to Feishu's primary,
danger, and default button types.

A press arrives as a card.action.trigger event, reported to the conversation
as an inbound `action` event whose `action` is the button id. Feishu requires
the websocket callback to answer within three seconds; the backend answers
immediately with a fixed info toast ("Received; the agent is working on it")
and the agent's real answer follows as an ordinary message threaded under the
card.

Example:
  <chat:7f3a...>
  <format:markdown>
  <action:approve|批准|primary>
  <action:reject|驳回|danger>

  # 发布审批
  | 服务 | 版本 |
  |---|---|
  | api | v2.10.0 |

Failure modes:
  - A card with buttons and no text still renders: it is a card of buttons.
  - A plain-text payload is sent as a text message; its "# " line stays a
    literal "# ", not a header.

See also: <help:feishu> for the other subtopics.
"##;

const FEISHU_MEDIA: &str = r##"Feishu media — level 2

Inbound. Image and file resources are downloaded to disk before the event
reaches the trigger, so the script receives a readable local path:
  - FEISHU_MEDIA_DIR names the directory; unset, it is
    <temp>/onlyne-feishu-media.
  - The attachment's `alt` keeps the original file name and the platform key,
    e.g. "report.pdf (feishu:file_key:file_v3_...)".
  - A failed download keeps the raw feishu:image_key:/feishu:file_key: handle
    in `path` instead of dropping the attachment.

Outbound. <image:/abs/path> and <file:/abs/path> are uploaded through the
Open API and sent as image/file messages. A path that already is
feishu:image_key:<key> or feishu:file_key:<key> is sent as that key without
an upload. The envelope's inline image is limited to 2 MiB decoded.

The app needs the im:resource scope for either direction.

See also: <help:feishu> for the other subtopics.
"##;

const FEISHU_EVENTS: &str = r##"Feishu events and scopes — level 2

Subscribe in the developer console:
  im.message.receive_v1          messages the bot receives
  im.chat.member.bot.added_v1    the bot joined a group; the chat is adopted
  card.action.trigger            a button press on an interactive card

Grant the scopes im:message, im:chat:readonly, and im:resource.

FEISHU_PASS_ALL_EVENTS=1 (or true/yes) additionally passes non-IM business
events — calendar, approval, task, reaction, and so on — through as inbound
events carrying `event_type` and the raw payload, with a placeholder text
"[feishu event: <type>]". Leave it unset to keep the gateway to chat events.

See also: <help:feishu> for the other subtopics.
"##;

const QQBOT: &str = r##"QQ Open Platform backend — level 1

Coordinates (the platform conversation id after <chat:qqbot:...>):
  c2c:<openid>       a one-to-one chat
  group:<openid>     a group; the group's openid, not its display number
  channel:<id>       a guild channel
  direct:<id>        a guild direct message (user: and private: also mean c2c)

Addressing:
  <chat:qqbot:c2c:437CD65AC133C7DC>
  <chat:qqbot:group:ABCDEF012345>
  <chat:qqbot:channel:123456789>

Passive replies. QQ accepts a group or c2c send without the proactive-message
grant only as a reply to a recent inbound message: carry <msg:ID>, or let the
gateway answer the last inbound event. The passive window is 5 minutes and at
most 5 replies per message; past it the platform refuses the send.

Quote display. `msg_id` makes the send passive; showing the quote needs the
message's separate REFIDX handle (`message_reference`), which the gateway
takes from the recorded message. Group and c2c sends also carry a growing
`msg_seq`; channel and direct sends omit it.

Limits: the QQ render path sends text only. Buttons, stickers, and
attachments are not rendered — send a link instead. Markdown arrives as plain
characters.

See also: <help:examples>, <help:markers:reply>
"##;

const MCP: &str = r##"onlyne-im-backend-mcp — level 1

A declarative bridge for a platform that has an MCP server but no dedicated
backend. Point one [[im.backend]] row at it:

  [[im.backend]]
  gateway = "myplatform"
  run = ["onlyne-im-backend-mcp", "--spec", "myplatform.toml"]

(ONLYNE_IM_MCP_SPEC names the spec instead of --spec.)

The spec, one section per direction:

  [mcp]
  command = "npx"
  args = ["-y", "@scope/myplatform-mcp"]
  env = { API_TOKEN = "..." }
  env_file = ".env"          # optional; relative to the spec file
  # cwd = "."                # optional; relative to the spec file

  [[inbound.notification]]   # a JSON-RPC notification the server pushes
  method = "message.created" # "*" matches every method
  id = "/id"
  conversation = "/chat/id"
  text = "/text"
  sender_id = "/from/id"
  sender_name = "/from/name"
  reply_parent = "/reply_to"
  event_type = "/type"
  kind = "/kind"             # "action" makes an action event
  action = "/action_id"

  [inbound.poll]             # an MCP tool called on a timer
  tool = "list_messages"
  every_secs = 5
  args = { since = "{{since}}", cursor = "{{cursor}}" }
  cursor_pointer = "/cursor"
  items_pointer = "/items"
  id = "/id"
  conversation = "/chat_id"
  text = "/text"
  sender_id = "/sender/id"

  [outbound.send]            # an MCP tool called for each payload
  tool = "send_message"
  args = { chat = "{{conversation}}", text = "{{text}}", format = "{{format}}",
           reply_to = "{{reply_to}}", attachments = "{{attachments_json}}",
           actions = "{{actions_json}}" }
  message_id_pointer = "/message/id"

  [conversations.list]       # optional: adopted at startup
  tool = "list_chats"
  items_pointer = "/items"
  id = "/id"
  title = "/name"

Selectors are JSON Pointers (/a/b) or dot paths (a.b). Poll arguments may use
{{cursor}} and {{since}}; send arguments may use {{conversation}}, {{text}},
{{format}}, {{reply_to}}, {{attachments_json}}, and {{actions_json}}.

See also: <help:examples>
"##;

/// Level 1: four deliveries that can be pasted as they are.
fn examples(gateway: &Gateway) -> String {
    format!(
        r##"Copy-paste examples — level 1

1. A plain reply in a registered conversation

   onlyne_send{{to: "{role}", kind: "note", text: "<chat:7f3a9c2e1b04d5a6f8c1e3b7a9d2f4c6>

   收到，马上处理。"}}

2. A Feishu approval card with buttons

   onlyne_send{{to: "{role}", kind: "note", text: "<chat:7f3a9c2e1b04d5a6f8c1e3b7a9d2f4c6>
   <format:markdown>
   <action:approve|批准|primary>
   <action:reject|驳回|danger>

   # 发布审批
   | 服务 | 版本 |
   |---|---|
   | api | v2.10.0 |"}}

3. A local image and a file

   onlyne_send{{to: "{role}", kind: "note", text: "<chat:7f3a9c2e1b04d5a6f8c1e3b7a9d2f4c6>
   <image:/abs/path/chart.png>
   <file:/abs/path/report.pdf>

   本轮报告见附件。"}}

4. First contact with a platform coordinate (no key minted yet)

   onlyne_send{{to: "{role}", kind: "note", text: "<chat:feishu:oc_xxxxxxxxxxxxxxxx>

   你好，这是本会话的第一条消息。"}}

Replace the chat keys and paths with this gateway's own — <help:chats> lists
the keys it serves.
"##,
        role = gateway.role
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BackendEntry, Conversation};
    use std::sync::{Arc, RwLock};

    fn key() -> String {
        crate::chatkey::mint()
    }

    fn conversation(dir: &std::path::Path, gateway: &str, conversation: &str) -> Conversation {
        Conversation {
            slug: format!("{gateway}-{conversation}"),
            workspace: dir.to_path_buf(),
            config_path: dir.join(".onlyne/config.toml"),
            gateway: gateway.into(),
            conversation: conversation.into(),
            chat_key: key(),
        }
    }

    fn gateway(dir: &std::path::Path, conversations: Vec<Conversation>) -> Gateway {
        Gateway {
            role: "im".into(),
            root: dir.to_path_buf(),
            server: "127.0.0.1:9443".into(),
            cert_pin: "sha256/0000000000000000000000000000000000000000000=".into(),
            backends: vec![
                BackendEntry {
                    gateway: "feishu".into(),
                    run: vec!["onlyne-im-backend-feishu".into()],
                },
                BackendEntry {
                    gateway: "qqbot".into(),
                    run: vec!["onlyne-im-backend-qqbot".into()],
                },
            ],
            pushes: Vec::new(),
            conversations: Arc::new(RwLock::new(conversations)),
            listen: Default::default(),
        }
    }

    #[test]
    fn the_plain_wrapper_is_the_level_zero_overview() {
        let query = parse_help_query("<help>").expect("a help query");
        assert_eq!(query.topic, "");
        assert_eq!(query.chat_key, None);
        assert_eq!(parse_help_query("<help:all>").unwrap().topic, "");
    }

    #[test]
    fn topics_and_subtopics_are_lowercased_and_trimmed() {
        assert_eq!(parse_help_query("<help:Markers>").unwrap().topic, "markers");
        assert_eq!(
            parse_help_query("  <help:Markers:Action>  ").unwrap().topic,
            "markers:action"
        );
        assert_eq!(
            parse_help_query("<help:chats:adopt>").unwrap().topic,
            "chats:adopt"
        );
    }

    #[test]
    fn cli_aliases_mean_the_same_thing() {
        for alias in ["--help", "-h", "help", "/help", "  --help  "] {
            assert_eq!(parse_help_query(alias).unwrap().topic, "", "{alias}");
        }
        assert_eq!(parse_help_query("--help markers").unwrap().topic, "markers");
        assert_eq!(
            parse_help_query("help feishu:card").unwrap().topic,
            "feishu:card"
        );
        assert_eq!(parse_help_query("HELP Markers").unwrap().topic, "markers");
    }

    #[test]
    fn a_chat_key_on_line_one_scopes_the_request_to_that_conversation() {
        let key = key();
        let query = parse_help_query(&format!("<chat:{key}>\n<help>")).expect("a help query");
        assert_eq!(query.chat_key.as_deref(), Some(key.as_str()));
        assert_eq!(query.topic, "");

        let deeper = parse_help_query(&format!("<chat:{key}>\n<help:feishu:card>")).unwrap();
        assert_eq!(deeper.chat_key.as_deref(), Some(key.as_str()));
        assert_eq!(deeper.topic, "feishu:card");

        let cli = parse_help_query(&format!("<chat:{key}>\n--help")).unwrap();
        assert_eq!(cli.chat_key.as_deref(), Some(key.as_str()));
        assert_eq!(cli.topic, "");
    }

    #[test]
    fn ordinary_deliveries_are_not_help_queries() {
        assert!(parse_help_query("").is_none());
        assert!(parse_help_query("hello").is_none());
        assert!(parse_help_query("hello\n<help>").is_none());
        assert!(parse_help_query("<chat:>\n<help>").is_none());
        assert!(parse_help_query("<chat:not a key!>\n<help>").is_none());
        // A key line followed by words is an ordinary (keyless-body) delivery.
        assert!(parse_help_query(&format!("<chat:{}>\nhello", key())).is_none());
        // A marker-shaped line that is not a help form.
        assert!(parse_help_query("<helper>").is_none());
    }

    #[test]
    fn level_zero_names_the_role_backends_and_topics() {
        let dir = tempfile::tempdir().unwrap();
        let gateway = gateway(dir.path(), Vec::new());
        let text = render_help(
            &gateway,
            &HelpQuery {
                topic: String::new(),
                chat_key: None,
            },
        );
        assert!(text.contains("role:          im"), "{text}");
        assert!(text.contains("backends:      feishu, qqbot"), "{text}");
        assert!(text.contains("conversations: 0 registered"), "{text}");
        assert!(text.contains("<help:markers>"), "{text}");
        assert!(text.contains("<help:examples>"), "{text}");
        assert!(text.contains("onlyne_send"), "{text}");
    }

    #[test]
    fn every_level_one_topic_renders_and_links_its_subtopics() {
        let dir = tempfile::tempdir().unwrap();
        let gateway = gateway(dir.path(), Vec::new());
        for (topic, expected) in [
            ("markers", "<help:markers:action>"),
            ("chats", "<help:chats:adopt>"),
            ("feishu", "<help:feishu:card>"),
            ("qqbot", "c2c:<openid>"),
            ("mcp", "[outbound.send]"),
            ("examples", "onlyne_send"),
        ] {
            let text = render_help(
                &gateway,
                &HelpQuery {
                    topic: topic.into(),
                    chat_key: None,
                },
            );
            assert!(text.contains(expected), "{topic}: {text}");
            assert!(!text.contains("Unknown help topic"), "{topic}: {text}");
        }
    }

    #[test]
    fn every_level_two_topic_renders() {
        let dir = tempfile::tempdir().unwrap();
        let gateway = gateway(dir.path(), Vec::new());
        for topic in [
            "markers:format",
            "markers:action",
            "markers:media",
            "markers:reply",
            "chats:adopt",
            "feishu:card",
            "feishu:media",
            "feishu:events",
        ] {
            let text = render_help(
                &gateway,
                &HelpQuery {
                    topic: topic.into(),
                    chat_key: None,
                },
            );
            assert!(text.len() > 100, "{topic}: {text}");
            assert!(!text.contains("Unknown help topic"), "{topic}: {text}");
        }
    }

    #[test]
    fn the_chat_topic_lists_registered_conversations() {
        let dir = tempfile::tempdir().unwrap();
        let entry = conversation(dir.path(), "feishu", "oc_abc");
        let registered = entry.chat_key.clone();
        let gateway = gateway(dir.path(), vec![entry]);
        let text = render_help(
            &gateway,
            &HelpQuery {
                topic: "chats".into(),
                chat_key: None,
            },
        );
        assert!(text.contains(&format!("<chat:{registered}>")), "{text}");
        assert!(text.contains("gateway=feishu"), "{text}");
        assert!(text.contains("conversation=oc_abc"), "{text}");
        assert!(text.contains("slug=feishu-oc_abc"), "{text}");
        assert!(text.contains("1 total"), "{text}");
    }

    #[test]
    fn the_chat_topic_says_so_when_nothing_is_registered() {
        let dir = tempfile::tempdir().unwrap();
        let gateway = gateway(dir.path(), Vec::new());
        let text = render_help(
            &gateway,
            &HelpQuery {
                topic: "chats".into(),
                chat_key: None,
            },
        );
        assert!(text.contains("serves no conversation yet"), "{text}");
        assert!(text.contains("<chat:GATEWAY:CONVERSATION_ID>"), "{text}");
    }

    #[test]
    fn a_scoped_request_shows_the_conversation_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let entry = conversation(dir.path(), "feishu", "oc_abc");
        let registered = entry.chat_key.clone();
        let gateway = gateway(dir.path(), vec![entry]);
        let text = render_help(
            &gateway,
            &HelpQuery {
                topic: String::new(),
                chat_key: Some(registered.clone()),
            },
        );
        assert!(text.starts_with("[Conversation Context]"), "{text}");
        assert!(text.contains("is registered in this gateway"), "{text}");
        assert!(text.contains("slug:         feishu-oc_abc"), "{text}");
        assert!(text.contains("gateway:      feishu"), "{text}");
        assert!(text.contains("conversation: oc_abc"), "{text}");
        assert!(text.contains("last inbound: none recorded"), "{text}");
        // The requested topic still follows the context block.
        assert!(text.contains("<help:markers>"), "{text}");
    }

    #[test]
    fn a_scoped_request_names_the_last_inbound_message() {
        let dir = tempfile::tempdir().unwrap();
        let entry = conversation(dir.path(), "feishu", "oc_abc");
        crate::chatlog::append_inbound(
            &entry,
            &onlyne_im_backend::payload::ImEvent {
                gateway: "feishu".into(),
                platform: "feishu".into(),
                channel: "feishu".into(),
                conversation: "oc_abc".into(),
                kind: onlyne_im_backend::payload::ImEventKind::Message,
                id: "om_123".into(),
                text: Some("hi".into()),
                at: "2026-10-19T00:00:00.000Z".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let registered = entry.chat_key.clone();
        let gateway = gateway(dir.path(), vec![entry]);
        let text = render_help(
            &gateway,
            &HelpQuery {
                topic: "markers".into(),
                chat_key: Some(registered),
            },
        );
        assert!(text.contains("last inbound: <msg:om_123>"), "{text}");
    }

    #[test]
    fn a_scoped_request_for_an_unknown_key_says_it_is_not_registered() {
        let dir = tempfile::tempdir().unwrap();
        let gateway = gateway(dir.path(), Vec::new());
        let missing = key();
        let text = render_help(
            &gateway,
            &HelpQuery {
                topic: "chats".into(),
                chat_key: Some(missing.clone()),
            },
        );
        assert!(
            text.contains(&format!("<chat:{missing}> is not registered")),
            "{text}"
        );
        assert!(text.contains("<help:chats>"), "{text}");
    }

    #[test]
    fn an_unknown_topic_falls_back_to_the_topic_index() {
        let dir = tempfile::tempdir().unwrap();
        let gateway = gateway(dir.path(), Vec::new());
        let text = render_help(
            &gateway,
            &HelpQuery {
                topic: "nope".into(),
                chat_key: None,
            },
        );
        assert!(text.starts_with("Unknown help topic \"nope\"."), "{text}");
        assert!(text.contains("<help:markers>"), "{text}");
        assert!(text.contains("<help:examples>"), "{text}");
    }
}
