import type { WorkRef } from "@onlyne/lib";

/** The work unit as one readable value: the dedupe key, its parent and the attempt number. */
export function workValue(work: WorkRef): string {
	return [
		work.unit,
		work.parent === undefined || work.parent === null ? "" : `parent ${work.parent}`,
		work.attempt === undefined ? "" : `attempt ${work.attempt}`,
	].filter(part => part.length > 0).join(" · ");
}
