// The adapter socket a pi session dials.
//
// macOS gives `sun_path` 104 bytes, so the kernel refuses a unix socket path
// past 103 (`UNIX_SOCKET_PATH_MAX` in `crates/onlyne-layout/src/lib.rs`). A
// generated role workspace nests three levels below its server root
// (`<root>/.onlyne/ws/<topology>/<role>/.onlyne/run/s`), so a deep root carries
// the canonical spelling past that bound. The client answers by serving such a
// workspace from a short path under the temporary directory
// (`<temp>/onlyne-<16hex>/s`) and publishing the choice it bound in the marker
// file `<workspace>/.onlyne/run/socket`, one line holding the absolute served
// path. `onlyne-layout::SocketEndpoint::publish` writes that file; every reader
// in the product reaches one live socket through it.
//
// So the plugin has three answers in order, cheapest first: the path the client
// injected when it spawned this process (`ONLYNE_SOCKET`), the path the running
// daemon published in the marker, and the canonical spelling. The third one is
// a complete answer for every workspace short enough to serve from `run/s`,
// because `bind_socket` writes the marker at every start and such a tree
// publishes `run/s` in it; the two readings give one path. A marker that is
// missing, unreadable, or blank holds no published override, so resolution
// falls through silently. That keeps a hand-started pi in a workspace with a
// running daemon on the right socket with no environment at all.

import { readFileSync } from "node:fs";
import { isAbsolute, join } from "node:path";

/** The canonical socket leaf every role workspace names; `SOCKET_FILE_NAME` in `crates/onlyne-layout/src/lib.rs`. */
export const SOCKET_RELATIVE_PATH = join(".onlyne", "run", "s");

/** Marker beside it naming the path the daemon actually serves. */
export const SOCKET_MARKER_RELATIVE_PATH = join(".onlyne", "run", "socket");

/**
 * The socket path to dial for the workspace at `cwd`.
 *
 * @param {Record<string, string | undefined>} env the process environment
 * @param {string} cwd the pi working directory, the role workspace itself
 * @param {{ readFile?: (path: string) => string }} [options]
 * @returns {string} an absolute path: the injected one, the published one, or `run/s`
 */
export function resolveSocketPath(env, cwd, options = {}) {
  const readFile = options.readFile ?? ((path) => readFileSync(path, "utf8"));
  const injected = typeof env.ONLYNE_SOCKET === "string" ? env.ONLYNE_SOCKET.trim() : "";
  if (injected) return injected;
  const marker = join(cwd, SOCKET_MARKER_RELATIVE_PATH);
  let published = "";
  try {
    published = readFile(marker).trim();
  } catch {
    published = "";
  }
  if (published && isAbsolute(published)) return published;
  return join(cwd, SOCKET_RELATIVE_PATH);
}
