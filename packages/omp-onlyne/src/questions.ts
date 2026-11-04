import type { NodeId, ThreadRef } from "@onlyne/lib";
import { text } from "./decider-text.ts";
import type { PeerRecord } from "./resolve.ts";

/**
 * The choice catalogs of the decision nodes. Each builder returns the question the judge
 * answers, with only the options that can actually execute: an option whose choice is
 * guaranteed to fail is a question the model cannot answer correctly.
 */
export interface ChoiceQuestion {
	type: "choice";
	instructions: string;
	criteria: Record<string, string>;
}

export type Questions = Record<string, ChoiceQuestion>;

export interface ChoiceAnswer {
	type?: string;
	choice?: string;
}

export type Answers = Record<string, ChoiceAnswer | undefined>;

/** The one answer a choice question carries, or `undefined` when the judge gave none. */
export function choice(answers: Answers, key: string): string | undefined {
	const answer = answers[key];
	if (answer === undefined || answer === null || typeof answer !== "object") return undefined;
	return typeof answer.choice === "string" ? answer.choice : undefined;
}

/** Node O-1. `resource` is offered only when meta names a complete resource action. */
export function intentQuestions(options: { resource: boolean }): Questions {
	return {
		intent: {
			type: "choice",
			instructions: text("judge.o1.instructions"),
			criteria: {
				communicate: text("judge.o1.communicate"),
				inspect: text("judge.o1.inspect"),
				manage_thread: text("judge.o1.manage_thread"),
				manage_domain: text("judge.o1.manage_domain"),
				...(options.resource ? { operate_resource: text("judge.o1.operate_resource") } : {}),
			},
		},
	};
}

/** Node O-2A, question 1: the relation a reference carries. */
export function relationQuestions(): Questions {
	return {
		relation: {
			type: "choice",
			instructions: text("judge.relation.instructions"),
			criteria: {
				reply: text("judge.relation.reply"),
				followup: text("judge.relation.followup"),
			},
		},
	};
}

/** Node O-3A: whichever of response and execution the caller left open. */
export function planQuestions(open: { response: boolean; execution: boolean; reply: boolean }): Questions {
	return {
		...(open.response
			? {
					response: {
						type: "choice" as const,
						instructions: text("judge.response.instructions"),
						criteria: {
							none: text("judge.response.none"),
							async: text("judge.response.async"),
							await: text("judge.response.await"),
						},
					},
				}
			: {}),
		...(open.execution
			? {
					execution: {
						type: "choice" as const,
						instructions: text("judge.execution.instructions"),
						criteria: {
							existing: open.reply
								? text("judge.execution.existing.reply")
								: text("judge.execution.existing.new"),
							new: text("judge.execution.new"),
						},
					},
				}
			: {}),
	};
}

/** Node O-3A-Ctx: which published context of the target continues the work. */
export function contextQuestions(published: Array<{ alias: string; summary: string; state: string }>): Questions {
	return {
		context: {
			type: "choice",
			instructions: text("judge.context.instructions"),
			criteria: {
				...Object.fromEntries(published.map(context => [
					context.alias,
					text("judge.context.published", { summary: context.summary, state: context.state }),
				])),
				none: text("judge.context.none"),
			},
		},
	};
}

/** Node O-2A, question 2: the recipient among the peers this node can name. */
export function targetQuestions(peers: PeerRecord[]): Questions {
	return {
		target: {
			type: "choice",
			instructions: text("judge.target.instructions"),
			criteria: {
				// The abstain exit: an unnamed recipient must surface as ambiguous_target,
				// never as a low-confidence random pick among peers.
				none: text("judge.target.none"),
				...Object.fromEntries(peers.map(peer => [
					peer.node_id,
					text("judge.target.peer", {
						node: peer.node_id,
						base_url: peer.base_url,
						instance: peer.instance_id,
						last_seen: peer.last_seen,
					}),
				])),
			},
		},
	};
}

/** Node O-3B-Th: which local thread a read or thread operation names. */
export function threadQuestions(threads: Array<{ ref: ThreadRef; title: string; state: string }>): Questions {
	return {
		thread: {
			type: "choice",
			instructions: text("judge.thread.instructions"),
			criteria: Object.fromEntries(threads.map(thread => [
				thread.ref.id,
				text("judge.thread.item", {
					title: thread.title,
					owner: thread.ref.owner,
					id: thread.ref.id,
					state: thread.state,
				}),
			])),
		},
	};
}

/** Node O-2A: which unanswered inbound message this call answers, or none. */
export function originalQuestions(
	facts: Array<{ key: string; from: NodeId; kind: string; thread: string; seq: string; body: string }>,
): Questions {
	const criteria: Record<string, string> = { new_message: text("judge.original.new") };
	for (const fact of facts) {
		criteria[fact.key] = text("judge.original.candidate", {
			from: fact.from, kind: fact.kind, thread: fact.thread, seq: fact.seq, body: fact.body,
		});
	}
	return { original: { type: "choice", instructions: text("judge.original.instructions"), criteria } };
}

/** Node O-2B: which readable channel the message asks about. */
export function readScopeQuestions(): Questions {
	return {
		read_scope: {
			type: "choice",
			instructions: text("judge.read_scope.instructions"),
			criteria: {
				peers: text("judge.read_scope.peers"),
				inbox: text("judge.read_scope.inbox"),
				messages: text("judge.read_scope.messages"),
				evidence: text("judge.read_scope.evidence"),
				resources: text("judge.read_scope.resources"),
				none: text("judge.read_scope.none"),
			},
		},
	};
}

/** Node O-2C: the local thread operation. */
export function threadOpQuestions(): Questions {
	return {
		thread_op: {
			type: "choice",
			instructions: text("judge.thread_op.instructions"),
			criteria: {
				create: text("judge.thread_op.create"),
				close: text("judge.thread_op.close"),
				list: text("judge.thread_op.list"),
				describe: text("judge.thread_op.describe"),
				member_add: text("judge.thread_op.member_add"),
				member_remove: text("judge.thread_op.member_remove"),
			},
		},
	};
}

/** Node O-2D: the domain topology operation. */
export function domainOpQuestions(): Questions {
	return {
		domain_op: {
			type: "choice",
			instructions: text("judge.domain_op.instructions"),
			criteria: {
				create: text("judge.domain_op.create"),
				join: text("judge.domain_op.join"),
				leave: text("judge.domain_op.leave"),
				list: text("judge.domain_op.list"),
				describe: text("judge.domain_op.describe"),
				discover: text("judge.domain_op.discover"),
			},
		},
	};
}

/** Node I-0: whether this node owes the sender an answer. */
export function needsAnswerQuestions(): Questions {
	return {
		needs_answer: {
			type: "choice",
			instructions: text("judge.needs_answer.instructions"),
			criteria: {
				yes: text("judge.needs_answer.yes"),
				no: text("judge.needs_answer.no"),
			},
		},
	};
}

/** Node I-2: how a main-session delivery enters the agent's work. */
export function insertionQuestions(options: { fragment: boolean }): Questions {
	return {
		insertion_mode: {
			type: "choice",
			instructions: text("judge.insertion.instructions"),
			criteria: {
				steer: text("judge.insertion.steer"),
				follow_up: text("judge.insertion.follow_up"),
				...(options.fragment ? { fragment: text("judge.insertion.fragment") } : {}),
			},
		},
	};
}
