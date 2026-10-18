# System One Decider / 毫秒级边缘决策反射弧

Onlyne v2.9 integrates **System One decision models** speaking the [TypeSafe System One API](https://docs.typesafe.ai/api) (`POST /v1/systemone`, such as TypeSafe `jev-latest` or self-hosted Clef gateways) at five edge checkpoints.

Instead of generating free-form text or placing orchestration policy inside `onlyne-server`, System-1 models evaluate a structured JSON `state` against batched typed questions—**`noul`** (calibrated yes/no probability in `[0, 1]`), **`choice`** (probability distribution + `confidence` over a closed option set), and **`score`** (weighted rubric rating in `[0, N-1]`)—in milliseconds.

---

## 1. Zero-Configuration Defaults & Environment Variables / 环境变量与默认配置

`onlyne_net::SystemOneClient` requires zero external crates (`reqwest` is not used; it speaks HTTP/1.1 directly over `tokio::net::TcpStream` with dual-stack address fallback):

| Environment Variable | Fallback / Default | Description |
|---|---|---|
| `ONLYNE_DECIDER` | Enabled (disabled under `ONLYNE_BACKEND=fake` unless set to `1`/`true`/`on`) | Set to `0`, `false`, or `off` to disable all System-1 calls; set to `1` to force-enable even when `ONLYNE_BACKEND=fake`. |
| `ONLYNE_DECIDER_URL` | `TYPESAFE_BASE_URL` → `http://127.0.0.1:8080` | Base URL for `POST /v1/systemone`. |
| `ONLYNE_DECIDER_MODEL` | `TYPESAFE_DEFAULT_MODEL` → `jev-latest` | Model identifier sent in the `POST /v1/systemone` body. |
| `ONLYNE_DECIDER_API_KEY` | `TYPESAFE_API_KEY` → none | Optional Bearer token for remote TypeSafe endpoints. |
| `ONLYNE_DECIDER_TIMEOUT_MS` | `1500` (1.5 seconds) | Per-request timeout in milliseconds. Any timeout or connection refusal degrades silently to standard deterministic behavior. |
| `ONLYNE_DECIDER_SPEC` | `<root>/.onlyne/decider.json` → embedded `decider_defaults.json` | Path to a custom JSON spec file or an inline JSON object that deep-merges over the default instructions, rubrics, thresholds, and custom questions. |

---

## 2. The Five Edge Reflexes / 五大边缘神经反射弧

### Reflex 1: Inbound IM Semantic Router & Implicit Thread Linker (`onlyne-im`)
- **Location**: `crates/onlyne-im/src/decider.rs`, wired into `handle_backend_op` (`runloop.rs`) and `PushInput::Trigger` (`trigger.rs`).
- **What it asks in one batched `POST /v1/systemone` call**:
  1. `should_handle` (`noul`): Is this message asking the coding-agent cluster for work/answers, or is it unrelated human chatter in a group chat?
  2. `target_role` (`choice`): Which candidate role (or `"ignore"`) best matches the request?
  3. `reply_to_msg` (`choice`): When the user did not click native reply on the chat platform, which recent message ID in the conversation's history window is this message continuing (or `"none"`)?
- **How push scripts consume it**:
  - `PushInput::Trigger` includes `"decider": { "should_handle", "target_role", "target_confidence", "suggested_reply_to", "reply_confidence" }` in the JSON piped to stdin.
  - The child script environment receives `ONLYNE_DECIDER_SHOULD_HANDLE`, `ONLYNE_DECIDER_TARGET`, `ONLYNE_DECIDER_CONFIDENCE`, and `ONLYNE_DECIDER_REPLY_TO`.

### Reflex 2: Pre-flight `onlyne_complete(done)` Contradiction Guard (`onlyne-client`)
- **Location**: `crates/onlyne-client/src/session/dispatch/decider.rs` (`verify_done_outcome`), wired into `reports.rs`.
- **Problem solved**: Coding agents sometimes call `onlyne_complete(outcome="done", ...)` while their `details` body actually says *"I am blocked waiting for user credentials"* or *"Compilation failed with an unresolved error"*.
- **Behavior**: When a plugin reports `outcome = "done"` with a non-trivial summary/details body (≥ 24 bytes), `verify_done_outcome` evaluates `contradicts_done` (`noul`) and `actual_status` (`choice`: `done` / `blocked` / `failed`). When `contradicts_done >= 0.85` and `actual_status.confidence >= 0.65`, the client automatically downgrades the recorded outcome to `Outcome::Blocked` or `Outcome::Failed` and logs a warning naming the contradiction.

### Reflex 3: Adaptive Turn-End Nudge (`onlyne-client`)
- **Location**: `crates/onlyne-client/src/session/dispatch/decider.rs` (`diagnose_turn_end_suffix`), wired into `turn_end.rs`.
- **Behavior**: When an agent ends its turn without calling `onlyne_complete`, `diagnose_turn_end_suffix` classifies the closing output (`forgot_complete`, `blocked`, or `error`) and appends a targeted instruction to the standard nudge text telling the model which `outcome` variant (`"done"`, `"blocked"`, or `"failed"`) to file.

### Reflex 4: Heartbeat Thrashing Detector & Live Phase Tag (`onlyne-client` + `onlyne tui`)
- **Location**: `crates/onlyne-client/src/session/dispatch/decider.rs` (`analyze_heartbeat_json`), wired into `reports.rs` and rendered in `crates/onlyne-cli/src/tui/cluster.rs`.
- **Behavior**: When a session heartbeat carries descriptive observation fields (`note`, `text`, `tool`, `output`), one batched System-1 call evaluates:
  - `is_thrashing` (`noul`): Whether the agent is stuck repeating the same failing operation.
  - `progress` (`score`: `0..3` rubric).
  - `phase` (`choice`: `reading`, `planning`, `coding`, `testing`, `debugging`, `waiting`, `wrapping_up`).
- **TUI display**: The `session` column on `onlyne tui` Page 1 automatically renders `busy · coding` (or `busy · debugging !thrash` when `thrashing >= 0.88`).

### Reflex 5: Autonomic Fault Triage (`onlyne triage`)
- **Location**: `crates/onlyne-cli/src/triage.rs`, exposed as `onlyne triage` and `onlyne server triage`.
- **Usage**:
  ```bash
  # Evaluate open faults on the cluster and print recommended actions + severity scores
  onlyne --server-root ./cluster triage

  # Automatically execute high-confidence retry / recycle / fail actions
  onlyne --server-root ./cluster triage --apply --min-confidence 0.65 [--spec ./custom-decider.json]
  ```
- **Mounting as an automatic `[[hook]]` in `spec.toml`**:
  ```toml
  [[hook]]
  on = ["delivery_blocked", "turn_end_without_complete", "fault"]
  run = ["onlyne", "--server-root", "/srv/onlyne", "triage", "--from-hook", "--apply"]
  timeout = "10s"
  ```

---

## 3. Meta-Programming Instructions, Rubrics & Custom Questions (`DeciderSpec`) / 可元编程指令与判据定制

No System-1 prompt text, option rubric, threshold, or nudge template is hardcoded in procedural Rust logic. Every reflex is driven by **`onlyne_net::DeciderSpec`** (baseline in `crates/onlyne-net/src/decider_defaults.json`).

### 3.1 Exporting the Baseline Spec

```bash
# Print the complete baseline DeciderSpec to customize
onlyne schema decider --pretty > ./cluster/.onlyne/decider.json
```

### 3.2 Deep-Merge Overrides, `{var}` Interpolation, and Extra Questions

You only need to specify the fields you want to override in `<root>/.onlyne/decider.json` or `ONLYNE_DECIDER_SPEC`. Object maps merge recursively:

- **`{var}` state interpolation**: Any string inside `instructions`, `criteria`, or `templates` can reference top-level fields of the reflex's `state` (such as `{task_id}`, `{role}`, `{platform}`, `{conversation}`, `{head}`, `{closing}`).
- **Structured JSON instructions/criteria**: Because `Question.instructions` and `criteria` are `serde_json::Value`, you can pass structured JSON objects or arrays directly to System One (`https://docs.typesafe.ai/primitives/advanced`).
- **Speculative fan-out with custom questions**: Any extra question key you add to a reflex's `questions` map is evaluated in the same single HTTP round-trip and returned in `extra` (`DeciderRouting.extra`, `TriageDecision.extra`, or `observed.decider.extra`).

Example `.onlyne/decider.json` override:

```json
{
  "im_inbound": {
    "questions": {
      "should_handle": {
        "instructions": "Platform {platform} chat {conversation}: is this message directed at our Rust engineering agents?"
      },
      "is_urgent_prod": {
        "type": "noul",
        "instructions": "Does this message report a live production outage?"
      }
    }
  },
  "turn_end": {
    "thresholds": {
      "diagnosis_floor": 0.70
    },
    "templates": {
      "forgot_complete": " Task {task_id} appears finished; call onlyne_complete(outcome=\"done\") immediately."
    }
  },
  "triage": {
    "questions": {
      "needs_human_pager": {
        "type": "noul",
        "instructions": "Does fault on task {task_id} require waking the on-call engineer?"
      }
    }
  }
}
```
