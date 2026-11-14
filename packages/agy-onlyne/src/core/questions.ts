import type { ThreadRef } from "@onlyne/lib";
import type { ContextRecord } from "@onlyne/node-host/types";
import { text } from "./decider-text.ts";
import { threadKey, type PeerRecord } from "./resolve.ts";

export interface ChoiceQuestion {
	type: "choice";
	instructions: string;
	criteria: Record<string, string>;
}

export type Questions = Record<string, ChoiceQuestion>;
export type Answers = Record<string, { choice?: string; text?: string; confidence?: string }>;

export function intentQuestions(): Questions {
	return {
		intent: {
			type: "choice",
			instructions: text("judge.o1.instructions"),
			criteria: {
				communicate: text("judge.o1.communicate"),
				inspect: text("judge.o1.inspect"),
				manage_thread: text("judge.o1.manage_thread"),
				manage_domain: text("judge.o1.manage_domain"),
				operate_resource: text("judge.o1.operate_resource"),
			},
		},
	};
}

export function originalQuestions(candidates: Array<{ key: string; from: string; kind: string; thread: string; seq: string; body: string }>): Questions {
	const criteria: Record<string, string> = {
		new_message: text("judge.o2a.original.new_message"),
	};
	for (const candidate of candidates) {
		criteria[candidate.key] = `${text("judge.o2a.original.candidate")} (${candidate.from} [${candidate.kind}] @ ${candidate.thread}#${candidate.seq}: ${candidate.body.slice(0, 120)})`;
	}
	return {
		original: {
			type: "choice",
			instructions: text("judge.o2a.original.instructions"),
			criteria,
		},
	};
}

export function relationQuestions(): Questions {
	return {
		relation: {
			type: "choice",
			instructions: text("judge.o2a.relation.instructions"),
			criteria: {
				reply: text("judge.o2a.relation.reply"),
				followup: text("judge.o2a.relation.followup"),
			},
		},
	};
}

export function targetQuestions(peers: readonly PeerRecord[]): Questions {
	const criteria: Record<string, string> = {};
	for (const peer of peers) {
		criteria[peer.node_id] = `${text("judge.target.candidate")} (last_seen=${peer.last_seen}${peer.base_url ? `, ${peer.base_url}` : ""})`;
	}
	criteria.none = text("judge.target.none");
	return {
		target: {
			type: "choice",
			instructions: text("judge.target.instructions"),
			criteria,
		},
	};
}

export function planQuestions(ask: { response: boolean; execution: boolean; reply: boolean }): Questions {
	const q: Questions = {};
	if (ask.response) {
		q.response = {
			type: "choice",
			instructions: text("judge.plan.response.instructions"),
			criteria: {
				await: text("judge.plan.response.await"),
				async: text("judge.plan.response.async"),
				none: text("judge.plan.response.none"),
			},
		};
	}
	if (ask.execution) {
		q.execution = {
			type: "choice",
			instructions: text("judge.plan.execution.instructions"),
			criteria: {
				existing: text("judge.plan.execution.existing"),
				...(ask.reply ? {} : { new: text("judge.plan.execution.new") }),
			},
		};
	}
	return q;
}

export function contextQuestions(published: ReadonlyArray<{ alias: string; summary?: string; state?: string }>): Questions {
	const criteria: Record<string, string> = {};
	for (const ctx of published) {
		criteria[ctx.alias] = `${text("judge.context.candidate")} (${ctx.state ?? "active"}${ctx.summary ? `: ${ctx.summary}` : ""})`;
	}
	return {
		context: {
			type: "choice",
			instructions: text("judge.context.instructions"),
			criteria,
		},
	};
}

export function threadQuestions(threads: ReadonlyArray<{ ref: ThreadRef; title: string; state: string }>): Questions {
	const criteria: Record<string, string> = {
		new: text("judge.thread.new"),
	};
	for (const item of threads) {
		const key = threadKey(item.ref);
		criteria[key] = `${text("judge.thread.candidate")} (${item.title} [${item.state}])`;
	}
	return {
		thread: {
			type: "choice",
			instructions: text("judge.thread.instructions"),
			criteria,
		},
	};
}

export function readScopeQuestions(): Questions {
	return {
		scope: {
			type: "choice",
			instructions: text("judge.read.instructions"),
			criteria: {
				directory: text("judge.read.directory"),
				neighbors: text("judge.read.neighbors"),
				threads: text("judge.read.threads"),
				inbox: text("judge.read.inbox"),
				messages: text("judge.read.messages"),
				message: text("judge.read.message"),
				evidence: text("judge.read.evidence"),
				resources: text("judge.read.resources"),
			},
		},
	};
}

export function threadOpQuestions(): Questions {
	return {
		thread_op: {
			type: "choice",
			instructions: text("judge.thread_op.instructions"),
			criteria: {
				create: "create a new thread",
				close: "close a thread",
				list: "list owned/joined threads",
				describe: "describe thread metadata and members",
				member_add: "add a member peer to a thread",
				member_remove: "remove a member peer from a thread",
			},
		},
	};
}

export function domainOpQuestions(): Questions {
	return {
		domain_op: {
			type: "choice",
			instructions: text("judge.domain_op.instructions"),
			criteria: {
				create: "create a new routing domain",
				join: "join a routing domain",
				leave: "leave a routing domain",
				list: "list known domains",
				describe: "describe domain membership",
				discover: "discover nodes inside a domain",
			},
		},
	};
}

export function inboundContextQuestions(records: readonly ContextRecord[], allowNew: boolean): Questions {
	const criteria: Record<string, string> = {};
	for (const rec of records) {
		if (rec.state === "lost") continue;
		criteria[rec.alias] = rec.alias === "main"
			? text("judge.inbound.route.main")
			: `${text("judge.inbound.route.existing")} (${rec.alias}: ${rec.summary})`;
	}
	if (allowNew) {
		criteria.new_context = text("judge.inbound.route.new_context");
	}
	return {
		route: {
			type: "choice",
			instructions: text("judge.inbound.route.instructions"),
			criteria,
		},
	};
}
