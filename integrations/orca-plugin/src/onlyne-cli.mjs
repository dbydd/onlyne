// Session state comes from the onlyne admin surface of a configured server root:
//
//    onlyne --server-root <S> sessions --json -> {ok:true,data:{sessions:[…]}}
//    onlyne --server-root <S> roles    --json -> {ok:true,data:{roles:[…]}}
//
// `<S>/.onlyne/run/s` is that root's local admin socket. The plugin names the
// root and never a role workspace socket: session identity lives in the
// adapter/pi plugin protocol, and the backend no longer registers one workspace
// per role, so there is nothing to probe per workspace.
//
// Measured on 2026-09-11 against target/debug/onlyne (v1.0.0):
//   * a root whose socket is absent -> exit 3 with the canonical
//     "onlyne: no onlyne socket found; pass --socket, --server-root, or
//     --workspace" on stderr and nothing on stdout, which the runner reports as
//     `cli_error` — the board degrades that root, it does not fail.
//   * a live admin socket -> exit 0 and `{ok:true,data:{sessions:[…]}}`.
//   * a legacy CLI that does not know the verb -> usage text on stderr,
//     reported as `cli_surface_mismatch` so the caller can name the binary.
// `--json` is accepted for legibility; every verb already prints JSON.

export function normalizeSessionRow(row) {
  if (!row || typeof row !== "object") return null;
  const taskId = typeof row.task_id === "string" && row.task_id ? row.task_id : null;
  if (!taskId) return null;
  const projection = row.projection && typeof row.projection === "object" ? row.projection : {};
  return {
    taskId,
    role: typeof row.role === "string" ? row.role : null,
    sessionId: typeof row.session_id === "string" ? row.session_id : null,
    lifecycle:
      typeof row.public_lifecycle === "string"
        ? row.public_lifecycle
        : typeof projection.lifecycle === "string"
          ? projection.lifecycle
          : null,
    agent: typeof projection.agent === "string" ? projection.agent : null,
    delivery: typeof projection.delivery === "string" ? projection.delivery : null,
    resource: typeof projection.resource === "string" ? projection.resource : null,
    outcome:
      typeof row.outcome === "string" ? row.outcome : typeof projection.outcome === "string" ? projection.outcome : null,
    updatedAt: typeof row.updated_at === "string" ? row.updated_at : null,
    seq: typeof row.seq === "number" ? row.seq : null,
  };
}

/** One `roles` row: the registry record plus live presence. */
export function normalizeRoleRow(row) {
  if (!row || typeof row !== "object") return null;
  const role = typeof row.name === "string" && row.name ? row.name : null;
  if (!role) return null;
  return {
    role,
    admin: row.admin === true,
    maxSessions: typeof row.max_sessions === "number" ? row.max_sessions : null,
    presence: typeof row.state === "string" ? row.state : null,
    sessions: typeof row.sessions === "number" ? row.sessions : null,
  };
}

/** Both verbs answer `{ok:true,data:{<verb>:[…]}}`; `--quiet` drops the body. */
function rowsOf(value, key) {
  const direct = value?.[key];
  if (Array.isArray(direct)) return direct;
  const nested = value?.data?.[key];
  return Array.isArray(nested) ? nested : null;
}

function surfaceMismatch(message) {
  const text = String(message ?? "");
  return (
    /unrecognized subcommand|unexpected argument|invalid subcommand|usage:/i.test(text) ||
    /unknown verb/i.test(text)
  );
}

export function createOnlyneCli({ runner, binary = "onlyne" }) {
  async function query(serverRoot, verb) {
    const result = await runner.runJson(binary, ["--server-root", serverRoot, verb, "--json"]);
    if (!result.ok) {
      const code =
        result.code === "cli_error" && surfaceMismatch(result.message)
          ? "cli_surface_mismatch"
          : result.code;
      return { ok: false, code, message: result.message };
    }
    const rows = rowsOf(result.value, verb);
    if (!rows) {
      return {
        ok: false,
        code: "unexpected_shape",
        message: `${verb} answer carried no ${verb} rows`,
      };
    }
    return { ok: true, rows };
  }

  /** @returns {{ok:true, sessions:Array} | {ok:false, code, message}} */
  async function querySessions(serverRoot) {
    const result = await query(serverRoot, "sessions");
    if (!result.ok) return result;
    return { ok: true, sessions: result.rows.map(normalizeSessionRow).filter(Boolean) };
  }

  /** @returns {{ok:true, roles:Array} | {ok:false, code, message}} */
  async function queryRoles(serverRoot) {
    const result = await query(serverRoot, "roles");
    if (!result.ok) return result;
    return { ok: true, roles: result.rows.map(normalizeRoleRow).filter(Boolean) };
  }

  return { querySessions, queryRoles };
}
