// Session state comes from the role workspace's own client socket only:
//
//    onlyne --socket <workspace>/.onlyne/run/s --as client sessions --json
//
// `<workspace>/.onlyne/run/s` is served by the workspace-local onlyne-client,
// which forwards `query_sessions` to the server. The surface flag is required:
// `--as auto` infers the admin surface from the canonical `.onlyne/run/s`
// suffix, and an admin frame sent to a client socket is closed without an
// answer ("socket closed before an answer arrived"). Measured against the
// workspace binary on 2026-09-11, with a live client link:
//   * `--socket <ws>/.onlyne/run/s sessions --json`            -> exit 1, closed
//   * `--socket <ws>/.onlyne/run/s --as client sessions --json` -> exit 0, rows
//   * `--workspace <ws> sessions --json`                        -> exit 0, rows
// Answers are always JSON; a dead link answers
// `{"ok":false,"error":{"code":"internal","message":"the connection is not
// ready"}}`, and a missing socket, or a CLI that does not know the verb, come
// back as a normalized failure so the caller can degrade to the mapping file's
// own `state`.

import { join } from "node:path";

export const CLIENT_SOCKET_RELATIVE_PATH = ".onlyne/run/s";

export function clientSocketPath(workspacePath) {
  return join(workspacePath, CLIENT_SOCKET_RELATIVE_PATH);
}

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

function sessionsOf(value) {
  const direct = value?.sessions;
  if (Array.isArray(direct)) return direct;
  const nested = value?.data?.sessions;
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
  /**
   * @returns {{ok: true, sessions: Array} |
   *           {ok: false, code: string, message: string}}
   */
  async function querySessions(socketPath, { role = null, timeout = 6000 } = {}) {
    const args = ["--socket", socketPath, "--as", "client", "sessions", "--json"];
    if (role) args.push("--role", role);
    const result = await runner.runJson(binary, args, { timeout });
    if (!result.ok) {
      const code =
        result.code === "cli_error" && surfaceMismatch(result.message)
          ? "cli_surface_mismatch"
          : result.code;
      return { ok: false, code, message: result.message };
    }
    const rows = sessionsOf(result.value);
    if (!rows) {
      return {
        ok: false,
        code: "unexpected_shape",
        message: "sessions answer carried no session rows",
      };
    }
    return { ok: true, sessions: rows.map(normalizeSessionRow).filter(Boolean) };
  }

  return { querySessions };
}
