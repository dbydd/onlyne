# Onlyne v2 work split

`docs/v2-PLAN.md` is the design and this file is the work split, the way `docs/v1-CONTRACT.md`
was for v1. The plan wins on any disagreement; this file exists so two slices running in
parallel cannot invent divergent shapes for the same interface.

Every slice names: the files it owns, the interface it must land, and what proves it done.
A slice that needs a shape this file does not fix should stop and ask rather than settle it
locally, because the other side of the interface is being written at the same time.

## Rules that apply to every slice

- One class of fact has one owner (§2 of `AGENTS.md`). A slice that finds itself holding a
  second copy of a fact is building the bug the plan exists to remove.
- A refusal is a contract: its code and its text are asserted by tests. Do not reword one
  without changing the test in the same change.
- Schema changes bump the marker in the same change that changes the DDL, and the
  `unsupported schema` refusal stays byte-exact.
- Every slice leaves the workspace green: `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.

---

## Slice 1: the session table rekeys on `session_id`, and scopes appear

The plan's §"server、client、会话的重新划分" lines 200-251.

v1's session table is keyed by `task_id` (`projection.rs` backfilled the task id when a
session id was missing), which makes "one session serves several deliveries" inexpressible.
v2 keys it by `session_id` and records bindings in a second table.

### Interface

**Server schema (`crates/onlyne-store/src/server.rs`), marker `onlyne-server` 4 → 5:**

```sql
CREATE TABLE IF NOT EXISTS sessions(
  session_id TEXT PRIMARY KEY,
  role TEXT NOT NULL,
  generation INTEGER NOT NULL,
  seq INTEGER NOT NULL,
  agent_state TEXT NOT NULL,
  delivery_state TEXT NOT NULL,
  resource_state TEXT NOT NULL,
  recovery_substate TEXT NOT NULL,
  desired_json TEXT NOT NULL,
  observed_json TEXT NOT NULL,
  mismatch_count INTEGER NOT NULL,
  -- The mirror's own freshness: what a reader judges a stale row by. v1's
  -- mirror could be hours old and read as current.
  last_seen TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS sessions_role_idx ON sessions(role);
CREATE INDEX IF NOT EXISTS sessions_updated_idx ON sessions(updated_at);
CREATE TABLE IF NOT EXISTS session_tasks(
  session_id TEXT NOT NULL,
  task_id TEXT NOT NULL,
  bound_at TEXT NOT NULL,
  released_at TEXT,
  PRIMARY KEY (session_id, task_id)
);
CREATE INDEX IF NOT EXISTS session_tasks_task_idx ON session_tasks(task_id);
CREATE INDEX IF NOT EXISTS session_tasks_open_idx ON session_tasks(session_id, released_at);
```

**Client schema (`crates/onlyne-store/src/client.rs`), marker `onlyne-client` 2 → 3:** the
same two tables, same columns, same keys. The client's `task` table keeps its own key
(`task_id`) and is unchanged: it answers for a task, the session tables answer for a
session, and neither borrows the other's key.

**Proto (`crates/onlyne-proto`):**

- `Outcome` gains `Blocked`. It is a first-class result, not a synonym for `Failed`: the
  plan's ending rule settles a `oneshot` delivery whose second turn ended without a
  `complete` as `blocked`, and the board shows it as "waiting" rather than "failed".
- `SessionRow` (in `ops.rs`) is addressed by `session_id`. It keeps
  `task_id: Option<String>` because a reader wants to know which delivery a session is
  currently serving, but that field is now *derived from the open `session_tasks` row*,
  not the row's key.
- `SessionRow` gains `last_seen: Option<String>` beside `updated_at`: the caller prints it
  and judges freshness, and the server never decides staleness from it.
- `HandshakeArgs.live_tasks: Vec<String>` becomes
  `live_sessions: Vec<LiveSession>`, where

  ```rust
  pub struct LiveSession {
      pub session_id: String,
      /// The delivery this session is bound to, if any.
      pub task_id: Option<String>,
      /// A session the client holds but has released its process for.
      pub suspended: bool,
  }
  ```

  An absent or empty list requeues every unacknowledged row, which is what a client from
  an earlier build sends: the field is additive and the old spelling is the old behavior.

**Client session scopes** (`<workspace>/.onlyne/config.toml`, parsed in
`crates/onlyne-config`):

```toml
[client.session]
scope = "oneshot"   # oneshot (default) | task | role
idle_close = "2h"   # absent means the scope's own default
```

Semantics are the plan's table (lines 222-228) and are enforced entirely on the client:
the server delivers by role and knows nothing about scope. `task` keys on the task family
(`causality.family_root`), so a second delivery to one role in a family lands in the
session that served the first.

Whatever an unsupported `scope` value does, it MUST NOT silently become `oneshot`: it is a
configuration refusal with the line number, like every other spec error.

### Acceptance

Scenarios (one test binary, `crates/onlyne-testkit/tests/scenarios.rs`), each asserting an
externally visible contract:

- **Rekey.** One session serves two deliveries in turn; `query_sessions` answers one row
  addressed by the session id, and `session_tasks` carries two bindings, the first with a
  `released_at`.
- **Scope `oneshot`.** Two deliveries to one role land in two sessions.
- **Scope `task`.** Two deliveries of one family to one role land in one session; a
  delivery of a different family lands in a different one.
- **Scope `role`.** Deliveries up to `max_sessions` reuse the pooled sessions, and a third
  concurrent delivery waits rather than opening a session past the bound.
- **Suspension.** An idle `task`-scope session suspends (process released, slot counted as
  free), and the next delivery of its family resumes it rather than opening a new session.
  A runtime that cannot resume degrades to "process alive, session alive" and the scenario
  asserts that degradation rather than skipping it.
- **Adoption.** A client that restarts while a pane-hosted runtime lives re-reports its
  sessions through `hello.live_sessions` and the server's adoption requeue skips those
  deliveries. A client that reports nothing requeues everything.
- **Freshness.** A session whose heartbeat only refreshes `last_seen` writes no new
  `updated_at`, publishes no event, and still answers a reader with the `last_seen` it
  holds.

A v1 database (marker 4 server / 2 client) is refused with the byte-exact
`onlyne: unsupported schema; v1.0.0 does not migrate`.
