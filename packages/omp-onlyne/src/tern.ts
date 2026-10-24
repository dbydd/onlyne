/**
 * Tern identity for the bootstrap defaults.
 *
 * `TERN_PANE` is the block id the process runs in. `tern ls --json` is the
 * supported query for the session that owns a block; a missing binary, a
 * non-zero exit, malformed JSON or an unknown block all return `undefined`.
 * The caller may fall back to its own default, but this module never guesses a
 * session name from the cwd or from a prefix.
 */

interface TernBlock {
	id?: unknown;
}

interface TernTab {
	blocks?: TernBlock[];
}

interface TernSession {
	name?: unknown;
	tabs?: TernTab[];
}

interface TernList {
	sessions?: TernSession[];
}

/** The Tern block this process runs in, when Tern set it. */
export function detectTernBlock(): string | undefined {
	const block = process.env.TERN_PANE?.trim();
	return block === undefined || block.length === 0 ? undefined : block;
}

/** The session name owning `block` (default: this process's block), `onlyne:` stripped. */
export function detectTernSession(block: string | undefined = detectTernBlock()): string | undefined {
	if (block === undefined) return undefined;
	let output: string;
	try {
		const result = Bun.spawnSync([process.env.TERN_ACP ?? "tern", "ls", "--json"], {
			stdout: "pipe",
			stderr: "ignore",
		});
		if (result.exitCode !== 0) return undefined;
		output = new TextDecoder().decode(result.stdout);
	} catch {
		return undefined;
	}
	let parsed: TernList;
	try {
		parsed = JSON.parse(output) as TernList;
	} catch {
		return undefined;
	}
	const sessions = Array.isArray(parsed.sessions) ? parsed.sessions : [];
	for (const session of sessions) {
		const tabs = Array.isArray(session.tabs) ? session.tabs : [];
		for (const tab of tabs) {
			const blocks = Array.isArray(tab.blocks) ? tab.blocks : [];
			if (!blocks.some(candidate => String(candidate.id) === block)) continue;
			if (typeof session.name !== "string") return undefined;
			const name = session.name.replace(/^onlyne:/, "").trim();
			return name.length === 0 ? undefined : name;
		}
	}
	return undefined;
}
