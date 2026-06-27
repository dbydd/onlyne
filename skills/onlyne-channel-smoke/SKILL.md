---
name: onlyne-channel-smoke
description: Use this skill whenever working on Onlyne channel adapter examples, local CLI/FIFO smoke tests, broadcast/multicast/multi-channel flows, or workspace-local auth/debug validation. It keeps work inside .onlyne, avoids real credential leakage, and uses the existing Unix socket/stdio/FIFO protocol instead of adding runtime abstractions.
---

# Onlyne Channel Smoke Skill

Use this for Onlyne repo tasks that involve channel setup, adapter smoke scripts, event subscription, history reads, broadcast/multicast examples, or debugging conversation/thread IDs.

## Rules

- Keep all runtime state under the current workspace `.onlyne/`.
- Never commit `.onlyne/`, `*.log`, or `*.ndjson` files from examples.
- Prefer CLI loops over new daemon protocol when a script can call existing `send_message`.
- Use `onlyne run --debug` when the user needs conversation/thread metadata; debug replies redact token/secret/password-like values.
- Treat live platform validation as manual unless the user explicitly says credentials and external sends are allowed.

## Standard local checks

Run these before claiming an example or CLI change is done:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
./examples/wechat/smoke-wechat.sh --local-check
```

For generic examples, also run:

```bash
bash -n examples/*/smoke-*.sh
python3 -m py_compile examples/shared/*.py
```

## Example patterns

Single channel:

```bash
cd examples/wechat
../../target/debug/onlyne auth wechat
ONLYNE_WECHAT_CONVERSATION_ID='<peer_user_id>' ./smoke-wechat.sh
```

Explicit target send:

```bash
ONLYNE_TARGETS='wechat,telegram' ONLYNE_TEXT='zig' examples/shared/send-many.py
```

Shell completions:

```bash
onlyne shell-completions zsh > _onlyne
onlyne shell-completions fish > onlyne.fish
```

## Completion criteria

- Scripts have `--local-check` or a cheap syntax check.
- README documents required environment variables and manual credential steps.
- Secret scan does not show real tokens, context tokens, conversation IDs, or event payloads in committed files.
