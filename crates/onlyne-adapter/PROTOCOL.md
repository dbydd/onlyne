# Onlyne adapter protocol

This document is the contract for external TypeScript plugins that talk to a host over the socket. The transport is the length-prefixed JSON frame from `onlyne-frame`: a four-byte big-endian `u32` body length, then one UTF-8 JSON object. No other header sits in front of it. Every example below shows the JSON body.

A request carries `id`; its response carries `reply_to`. Operation payloads read `{\"op\":...,\"args\":...}`. A host notification drops `id`. A plugin drops `id` too when it wants no answer.

## Frame table

| Direction | Frame | JSON example |
| --- | --- | --- |
| plugin → host | `hello` | `{"id":1,"op":"hello","args":{"protocol":1,"plugin":"onlyne-agent-pi","version":"1.0.0","kind":"agent","capabilities":["register","report","inject","recycle"],"mount":{"role":"planner","session":"8b1c..."}}}` |
| host → plugin | `welcome` | `{"reply_to":1,"op":"welcome","args":{"protocol":1,"role":"planner","session_id":"s1","generation":1,"prose":"Read the incoming task","server":{"connected":true,"cluster":"local","name":"server"},"host_capabilities":["inject"]}}` |
| plugin → host | `report.ready` | `{"id":2,"op":"report","args":{"kind":"ready","data":{"task_id":"task-1","session_id":"s1","generation":1,"seq":1}}}` |
| plugin → host | `report.heartbeat` | `{"id":3,"op":"report","args":{"kind":"heartbeat","data":{"task_id":"task-1","generation":1,"seq":1002,"observed":{"version":{"generation":1,"seq":1002},"generation_live":true,"isolate_after":1,"terminate_after":3,"mismatch_count":0,"agent":"running","delivery":"none","resource":"attached","recovery":"none","outcome":"pending","public":"working","host":{"orca":{"pane_key":"45e603f7-0772-48aa-bcf6-832272747713:b6d067b6-9255-4f5c-a13f-24f194ea0560"}}}}}}` |
| plugin → host | `report.complete` | `{"id":4,"op":"report","args":{"kind":"complete","data":{"task_id":"task-1","outcome":"done","head":"finished"}}}` |
| plugin → host | `report.fault` | `{"id":5,"op":"report","args":{"kind":"fault","data":{"task_id":"task-1","session_id":"s1","generation":1,"seq":3,"kind":"runtime","reason":"failed","desired":null,"observed":null}}}` |
| plugin → host | `session_register` | `{"id":6,"op":"session_register","args":{"session_id":"s1","pid":4212,"generation":1,"title":"swarm:planner:s1","task_id":"task-1"}}` |
| plugin → host | `assign_ack` | `{"id":7,"op":"assign_ack","args":{"task_id":"task-1","accepted":true,"reason":null}}` |
| plugin → host | `send` | `{"id":8,"op":"send","args":{"protocol":1,"id":"3f2504e0-4f89-41d3-9a0c-0305e82c3301","op_id":"o-3f2504e0-4f89-41d3-9a0c-0305e82c3302","kind":"task","from":{"role":{"role":"planner"}},"to":{"role":{"role":"builder"}},"causality":{"task":"3f2504e0-4f89-41d3-9a0c-0305e82c3303","hop":0,"attempt":0},"body":{"text":"build it"},"ts":"2026-01-01T00:00:00Z","admin":false}}` |
| host → plugin | `assign` | `{"op":"assign","args":{"envelope":{"protocol":1,"id":"3f2504e0-4f89-41d3-9a0c-0305e82c3301","op_id":"o-3f2504e0-4f89-41d3-9a0c-0305e82c3302","kind":"task","from":{"role":{"role":"planner"}},"to":{"role":{"role":"builder"}},"causality":{"task":"3f2504e0-4f89-41d3-9a0c-0305e82c3303","hop":0,"attempt":0},"body":{"text":"build it"},"ts":"2026-01-01T00:00:00Z","admin":false},"prose":"Read the incoming task","task_id":"task-1","generation":1,"parent":null}}` |
| host → plugin | `probe` | `{"op":"probe","args":{"task_id":"task-1"}}` |
| host → plugin | `recycle` | `{"op":"recycle","args":{"task_id":"task-1","reason":"operator","outcome":"cancelled"}}` |
| host → plugin | `config_get` | `{"op":"config_get","args":{"key":"model.name"}}` |
| plugin → host | `deliver` (gateway) | `{"id":9,"op":"deliver","args":{"msg_id":"m1","envelope":{"protocol":1,"id":"3f2504e0-4f89-41d3-9a0c-0305e82c3304","kind":"note","from":{"gateway":{"gateway":"fg1","channel":"fake","conversation":"c1"}},"to":{"role":{"role":"planner"}},"body":{"text":"hello"},"ts":"2026-01-01T00:00:00Z","admin":false}}}` |
| plugin → host | `register_channel` (gateway) | `{"id":10,"op":"register_channel","args":{"platform":"fake","channel":"fg1","conversations":null}}` |
| plugin → host | `health` (gateway) | `{"id":11,"op":"health","args":{"state":"online","detail":null,"uptime_s":3}}` |
| plugin → host | `typing` (gateway) | `{"id":12,"op":"typing","args":{"conversation":"c1","on":true}}` |
| host → plugin | `render_send` (gateway) | `{"op":"render_send","args":{"envelope":{"protocol":1,"id":"3f2504e0-4f89-41d3-9a0c-0305e82c3305","kind":"note","from":{"role":{"role":"planner"}},"to":{"gateway":{"gateway":"fg1","channel":"fake","conversation":"c1"}},"body":{"text":"hello"},"ts":"2026-01-01T00:00:00Z","admin":false},"conversation":"c1","gateway_ref":"r1"}}` |
| plugin → host | `detach` | `{"id":13,"op":"detach","args":{"reason":"operator"}}` |
| host → plugin | `bye` | `{"op":"bye","args":{"reason":"shutdown"}}` |
| either | response success | `{"reply_to":8,"ok":true,"data":{"msg_id":"..."}}` |
| either | response error | `{"reply_to":8,"ok":false,"error":{"code":"invalid","message":"body requires text or image","field":"body"}}` |

`AdapterMsg` also allows a response with no `reply_to`, for host-side dispatchers that fire a notification and want no answer. Keep request ids whenever the caller supplies them.

## Mounts and capabilities

Pick the mount by `kind`. Agent plugins send `kind: agent` with `mount.role` and an optional `mount.session`. Gateways send `kind: gateway` with `mount.gateway` and `mount.platform`. A connection that speaks for a sub-cluster carries `mount.cluster` and `mount.role`. Admin tooling sends `kind: admin` with `mount` null.

`kind` names the connection class. The mount payload is flat and untagged. `kind` sits beside it inside `hello.args`, not inside a wrapper: `{"role":"planner","session":"8b1c..."}` for an agent, `{"gateway":"gw1","platform":"telegram"}` for a gateway, `{"cluster":"cluster-b","role":"cluster-b"}` for a cluster.

The host matches the payload's field set against the variants in declaration order — agent, gateway, cluster, admin. Every payload denies unknown fields. So adding a field to one payload changes which variant answers, and the new field belongs to the earliest variant that owns it.

An agent connection may send `report`, `session_register`, `assign_ack`, `send`, and `detach`. A gateway connection may send `deliver`, `register_channel`, `health`, `typing`, and `detach`. A forbidden operation returns `forbidden` with an `op` field and a message naming the operation and the mount kind.

`register` binds the process to a task. `report` pushes lifecycle facts. `inject` declares support for `assign`. `recycle` declares that the plugin tears down its process when asked. `probe` declares that the host may ask for fresh resource observations. `typing` and `conversations` describe gateway features.

With no `recycle`, the host must judge resource loss through `probe`. With no `report`, the affected session moves to `idle_fault` and the host records a fault. With no `inject`, the host sends the payload through process stdin or argv, then reads the terminal state from the exit code and the last output line.

On this socket that delivery is a `config_get` frame whose only key is `stdin:{task text}`. So a plugin without `inject` must read an unrecognised `config_get` key as a task body, not as a configuration read. The plan lists `config_get{key}` host-to-plugin at §7 line 308 and names no frame for the stdin route at line 310. This document writes the overload down for that reason, instead of leaving it as folklore.

## Handshake and errors

The first frame must be `hello`, and the server waits exactly five seconds for it. Any other first frame gets `invalid` with the exact message `hello required first` and field `op`; then the connection closes. A `hello` that arrives after the window closes the connection silently and logs a `tracing::warn!`. The log carries the local peer pid when one is available.

Platform-owned data stays on the gateway side. A gateway plugin that receives `RenderSendArgs` gets the envelope plus rendered text and image, nothing more. An agent that receives `AssignArgs` gets the envelope, the prose, and the intent, nothing more. Neither serialized payload carries `platform_metadata`, `raw`, or `channel_id`. The SDK states this rule, and the conformance suite inspects the delivered bytes. The wire types carry no raw metadata field at all, so a plugin cannot smuggle platform data into an agent payload through these frames.

## Report sequencing and the ready barrier

Every `report` carries `(generation, seq)`. The host reduces reports and its own lifecycle events for one session against a single watermark. A report whose `seq` is at or below the watermark for its generation is dropped as stale or duplicate. The host's own events for a session take the low single digits: `created` is 1, `resource_attach` 2, `ready` 3. Start your counter above that range and keep it strictly increasing for the life of the generation. This document recommends 1000 as the baseline: it clears the host's range and leaves the host room to interleave its own events.

Keep the counter monotonic across a reconnect inside one generation. A reconnect that restarts the count at the baseline puts every later report below the watermark, and the host ignores all of them, completions included.

`report.ready` is the barrier the payload waits behind, and it passes the same gate. A `report.heartbeat` that carries a lower `(generation, seq)` than an earlier report is dropped, even when it is the frame that carries a state change.

`observed` on `report.heartbeat` is a complete `Observation` tuple. A status string such as `{"state":"running"}` is not valid input. The keys are `version{generation,seq}`, `generation_live`, `isolate_after`, `terminate_after`, `mismatch_count`, `agent`, `delivery`, `resource`, `recovery`, `outcome`, and `public`; `public` projects from the other dimensions. The host rejects an illegal tuple and keeps the previous state.
`observed` may carry one key beside the tuple: `host`, where this process runs. `host` is placement, not a state dimension. `public` and the legality check never read it, and a lone `host` can never make an illegal tuple legal or decide a transition. Its shape is `{"orca": {"pane_key": "<tab_id>:<leaf_id>", "tab_id": …, "leaf_id": …, "handle": …}}`; only `pane_key` is required, and the rest are omitted when the environment did not name them. A process outside an Orca pane omits `host` entirely rather than sending null, so absence means "not in a pane" and never "unknown".

The host stores the tuple as the row's observation instead of wrapping it. A reader of the admin surface therefore finds the binding at `projection.observed.host`, whichever client path wrote the row. The relay's own `cluster_ref` joins it as a sibling key under the same rule.

The comparison tuple includes `host`, so a heartbeat that changes only the binding is news rather than a no-op replay: it advances the watermark and is published as a new version. A process that learns its pane after `report.ready` therefore sends one heartbeat to state it. From that point on, a supervisor reading the session axis can scope its view of the panes.

`report.complete` is terminal for one task. The host answers it by writing the terminal tuple: `delivery: accepted` and `outcome: done|failed|cancelled` at the next version. No heartbeat for that task may follow. `observed` replaces the whole snapshot, and a later one carrying `outcome: pending` is legal input, so it puts the session back to `working` or `idle` after the host has published `exited`. A plugin keeps its counter and stops reporting for the task once it has sent the completion. The host closes the resource on the completion receipt.

Placement outlives the report that ends a run. `report.complete` replaces the snapshot but carries `host` forward from the row's last observation, so a finished session still says which pane it ran in and a supervisor can look at the output where it was produced. Nothing else of the earlier tuple survives the completion.

## AgentSurface

The optional external coding-agent face has these members. Each one returns `Result<(), SurfaceGap>` and defaults to `SurfaceGap::Unsupported(name)`:

`config_path`, `register_tool`, `register_command`, `wake_user(WakeUser { text, deliver_as })`, `send_custom_entry`, `on_turn_lifecycle`, `exit`, `wrap_result`, `set_active_tools`, `set_status`, `set_title`, `set_model`, `set_thinking_level`.

`wake_user` represents both `sendUserMessage(text, {deliverAs:"followUp"})` and `followup(createUserMessage(text))`. A host can collect `SurfaceGaps::report()` to log exactly one line per unsupported member.

## In-process plugin trait

A gateway binary implements `GatewayPlugin` from `onlyne-adapter`. The trait keeps platform SDK dependencies at the plugin boundary. The plugin receives finished text plus optional PNG bytes through `Outbound`; rendering stays in the gateway binary. A plugin never links `resvg`, `pulldown-cmark`, or the test kit.
The internal host-side multiplexer buffers frames through `QueuedFrame`. The plugin-side send payload carries `Outbound`.
The gateway binary converts an inbound or outbound task into an `Outbound` envelope before it hands the task to a plugin.

The fixed signatures are:

```rust
#[async_trait]
pub trait GatewayPlugin: Send {
    fn platform(&self) -> &'static str;
    fn capabilities(&self) -> Vec<Capability>;
    async fn start(&mut self, host: &mut dyn GatewayHost) -> Result<(), AdapterError>;
    async fn send(&mut self, msg: &Outbound) -> Result<SendReceipt, AdapterError>;
    async fn probe(&mut self) -> Result<AdapterHealth, AdapterError>;
    async fn stop(&mut self, reason: &str) -> Result<(), AdapterError>;
    fn onboarding(&mut self) -> Result<Option<OnboardingPrompt>, AdapterError> { Ok(None) }
    async fn list_conversations(&mut self) -> Result<Vec<ConversationInfo>, AdapterError> { Ok(vec![]) }
}

#[async_trait]
pub trait GatewayHost: Send {
    async fn deliver_inbound(&mut self, envelope: &Envelope) -> Result<(), AdapterError>;
    async fn report_health(&mut self, health: &HealthArgs) -> Result<(), AdapterError>;
    async fn register_channel(&mut self, args: &RegisterChannelArgs) -> Result<(), AdapterError>;
    async fn typing(&mut self, args: &TypingArgs) -> Result<(), AdapterError>;
}
```

`onboarding` and `list_conversations` are the two defaulted members. `OnboardingPrompt` has `Qr` and `ManualCode` kinds. An empty conversation list from `list_conversations` means the capability is absent; the `status` view reports it as `unsupported`. `AdapterError` carries an `ErrorCode` and a message, so plugin failures map to socket errors without a server dependency.
