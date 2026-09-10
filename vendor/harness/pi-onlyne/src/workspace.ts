import { existsSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
export interface Workspace { root: string; onlyneDir: string; socketPath: string }
export function findWorkspace(start: string): Workspace | null {
	let dir = resolve(start);
	for (;;) {
		const onlyneDir = join(dir, ".onlyne");
		if (existsSync(onlyneDir)) return { root: dir, onlyneDir, socketPath: join(onlyneDir, "run", "s") };
		const parent = dirname(dir);
		if (parent === dir) return null;
		dir = parent;
	}
}
