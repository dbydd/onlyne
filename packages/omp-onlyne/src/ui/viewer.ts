import type { KeyExchange, MessageRef, MsgInput, SendOutcome, TrustPhase } from "@onlyne/lib";
import {
	type Component,
	type DescribeContext,
	Editor,
	type NativeChild,
	type NativeNode,
	type NativeScroll,
	type NativeUiEvent,
	type Theme,
	type TspSpan,
	getEditorTheme,
	matchesKey,
	node,
	span,
	text as textNode,
} from "@oh-my-pi/pi-tui";
import { normalizeServiceUrl, shortNodeId } from "../config.ts";
import { DEFAULT_ACQUISITION, parseAllowBlock, type AcquirePolicy } from "@onlyne/node-host/acquire";
import { getRuntime, type PluginRuntime } from "../runtime.ts";
import { runDecision } from "../decider.ts";
import { workValue } from "../work.ts";
import type {
	AcquisitionAuditEntry,
	AcquisitionSnapshot,
	OnlyneStatus,
	ViewerDiscovery,
	ViewerMessage,
	ViewerSnapshot,
	ViewerThread,
} from "../types.ts";
import { ageText, clockText, fit, oneLine, shortAlias, sinceText } from "./format.ts";
import { type ViewerTextKey, t } from "./viewer-text.ts";

const REGION_PREFIX = "urn:onlyne:region:";
const NODE_PREFIX = "urn:onlyne:";
/** Groups for peers that declare no region, keyed by the kind their node id carries. */
const GATEWAY_KEY = "kind:gateway";
const OTHER_KEY = "kind:other";

/** The label column of a fact row: wide enough for the longest label (四格汉字), one gap before the value. */
const FACT_LABEL = "10ch";

/** A region URI is stored for stability; every label shows the short name the user typed. */
function regionLabel(region: string | undefined): string {
	if (region === undefined) return t("region.undeclared");
	if (!region.startsWith(REGION_PREFIX)) return region;
	const name = region.slice(REGION_PREFIX.length);
	try {
		return decodeURIComponent(name);
	} catch {
		return name;
	}
}

function nodeKind(id: string): string | undefined {
	if (!id.startsWith(NODE_PREFIX)) return undefined;
	const rest = id.slice(NODE_PREFIX.length);
	const colon = rest.indexOf(":");
	return colon > 0 ? rest.slice(0, colon) : undefined;
}

/** The group a peer belongs to: its declared region, else the kind its identity names. */
function groupKeyOf(record: ViewerDiscovery): string {
	if (record.region !== undefined) return record.region;
	return nodeKind(record.node_id) === "gateway" ? GATEWAY_KEY : OTHER_KEY;
}

function groupLabelOf(record: ViewerDiscovery): string {
	if (record.region !== undefined) return regionLabel(record.region);
	return nodeKind(record.node_id) === "gateway" ? t("group.gateway") : regionLabel(undefined);
}

function errorText(error: unknown): string {
	return error instanceof Error ? error.message : String(error);
}

function plainAuthor(author: string): string {
	const tail = author.split(":").pop();
	return tail === undefined || tail === "" ? shortNodeId(author) : tail;
}

/** The stage names, in the words the approval flow uses: we wait on `invited`, the peer on `requested`. */
const PHASE_LABEL: Record<TrustPhase, ViewerTextKey> = {
	exchanged: "trust.phase.exchanged",
	requested: "trust.phase.requested",
	invited: "trust.phase.invited",
	revoked: "trust.phase.revoked",
};

const PHASE_TONE: Record<TrustPhase, string> = {
	exchanged: "success",
	requested: "info",
	invited: "warning",
	revoked: "error",
};

const PHASE_SHORT: Record<TrustPhase, ViewerTextKey> = {
	exchanged: "trust.short.exchanged",
	requested: "trust.short.requested",
	invited: "trust.short.invited",
	revoked: "trust.short.revoked",
};

interface Peer {
	record: ViewerDiscovery;
	trust?: { phase: TrustPhase; fingerprint: string; observed?: string };
}

/**
 * One group in the rail. `key` is the selection handle and is always defined; `id` carries the
 * region URI only when the peers declared one.
 */
interface DomainGroup {
	key: string;
	id?: string;
	label: string;
	peers: Peer[];
}

interface Invite {
	node_id: string;
	fingerprint: string;
	base_url: string;
	observed?: string;
}

/** What the main column shows: one thread, the queue waiting on this node, or the trust rules. */
type View = { kind: "thread"; alias: string } | { kind: "inbox" } | { kind: "trust" };

/** The chrome tones this surface paints; every one is a prop value `node()` accepts. */
type SurfaceTone = "accent" | "info" | "success" | "warning" | "error" | "muted";

/** RFC 0018 §5: the ring says where the exchange stands; selection is a separate background. */
const CANDIDATE_TONE: Record<TrustPhase, SurfaceTone> = {
	exchanged: "success",
	requested: "info",
	invited: "info",
	revoked: "error",
};

/** The newest decisions the panel lists; the logger keeps the durable record. */
const AUDIT_LIMIT = 20;

const POLICY_TEXT: Record<AcquisitionSnapshot["policy"], ViewerTextKey> = {
	all: "trustpage.policy.all",
	segment: "trustpage.policy.segment",
	none: "trustpage.policy.none",
};

const POLICY_VALUES: readonly AcquirePolicy[] = ["all", "segment", "none"];

const AUDIT_ACTOR: Record<AcquisitionAuditEntry["actor"], ViewerTextKey> = {
	human: "trustpage.actor.human",
	agent: "trustpage.actor.agent",
	seed: "trustpage.actor.seed",
	policy: "trustpage.actor.policy",
};

const AUDIT_TONE: Record<AcquisitionAuditEntry["actor"], SurfaceTone> = {
	human: "accent",
	agent: "info",
	seed: "warning",
	policy: "muted",
};

/** A described node with a scroll request attached; `node()` only builds the wire shape. */
function scrolling(described: NativeNode, scroll: NativeScroll): NativeNode {
	return { ...described, scroll } as NativeNode;
}

/** A TSP line extent, for heights the terminal must never recompute from content. */
type LineExtent = `${number}lines`;

function linesOf(count: number): LineExtent {
	return `${Math.max(1, Math.floor(count))}lines` as LineExtent;
}

/**
 * The Onlyne surface, laid out as the design's four columns: the domain rail,
 * the thread sidebar, the conversation, and the member column with the
 * selected member's detail. Every operation is a click on a described node or
 * a select on a list row; `draftText` is the one way message text enters,
 * because a native editor reports text through the edit events itself.
 */
export class OnlyneViewer implements Component {
	private snapshot: ViewerSnapshot | undefined;
	private status: OnlyneStatus | undefined;
	private loading = true;
	private error: string | undefined;
	/** The main column's subject; undefined shows the first thread. */
	private view: View | undefined;
	/** The domain the rail highlights; undefined follows this node's own region. */
	private domainKey: string | undefined;
	private memberKey: string | undefined;
	/** An explicit recipient starts a new direct message. */
	private target: string | undefined;
	private replyTo: MessageRef | undefined;
	/** The composer's text, mirrored from the editor so a rebuild keeps it. */
	private draft = "";
	private composer: Editor | undefined;
	private sending = false;
	/** The address card: a node typed by hand, and the one read in flight. */
	private addressOpen = false;
	private address: Editor | undefined;
	private addressText = "";
	private connecting = false;
	/** The exchange whose fingerprint card is open; confirming is the session's own call. */
	private exchange?: KeyExchange;
	/** Only the newest request may open a card: a slower earlier lookup must not repaint it. */
	private exchangeToken = 0;
	/** Bumped to ask the terminal to scroll the message list to its newest row. */
	private messageScroll = 0;
	private messageScrollBy: NativeScroll["by"] = "end";
	/** The message opened for full reading; target selection is its own action. */
	private opened: ViewerMessage | undefined;
	/** A policy change restarts the node, so one runs at a time. */
	private policyBusy = false;
	/** The allow set being edited in the trust page. */
	private allowEditing = false;
	private allowEditor: Editor | undefined;
	private allowDraft = "";
	private notice: { text: string; error: boolean } | undefined;
	private noticeTimer: Timer | undefined;
	private unsubscribe: (() => void) | undefined;
	/** The runtime instance the change subscription belongs to; a restart replaces it. */
	private watched: PluginRuntime | undefined;
	private described: NativeNode | undefined;
	private describedKey: string | undefined;
	private rendered: { key: string; lines: readonly string[] } | undefined;
	private version = 0;
	private disposed = false;
	private generation = 0;
	focused = true;
	// The four columns need the pane: a fixed `lg` sheet is about 68 cells and drops one of them.
	readonly nativeOverlay = { role: "onlyne.viewer", size: "full" as const, head: "Onlyne" };

	/** The text the composer holds; a native `edit`/`undo` event updates it through `handleNativeEvent`. */
	get draftText(): string {
		return this.draft;
	}

	constructor(
		private readonly theme: Theme,
		private readonly requestRender: () => void,
		private readonly done: () => void,
		private readonly signal: AbortSignal,
		private readonly rows: () => number = () => 40,
	) {
		this.bindRuntime();
		void this.reload();
	}

	/** A config save restarts the node: the old runtime stops notifying, so bind the new one. */
	private bindRuntime(): void {
		const runtime = getRuntime();
		if (runtime === this.watched) return;
		this.unsubscribe?.();
		this.watched = runtime;
		this.unsubscribe = runtime?.onChange(() => {
			if (this.disposed || this.signal.aborted) return;
			void this.reload();
		});
	}

	private changed(): void {
		if (this.disposed || this.signal.aborted) return;
		this.version++;
		this.described = undefined;
		this.rendered = undefined;
		this.requestRender();
	}

	/**
	 * The rows the described sheet's body may draw: the fullscreen frame spends one row on
	 * the title and one on the inset around the columns.
	 */
	private bodyRows(): number {
		return Math.max(8, this.rows() - 2);
	}

	/**
	 * A list inside a column needs its own height cap: `grow` alone expands to the content,
	 * so a long thread pushes the composer past the pane and no scroller exists to page.
	 * The caps below subtract every fixed block that shares the column.
	 */
	private messageListRows(): number {
		// Thread head 2, composer 5, the shared cards, one card per approval, and gaps.
		const chrome = 2 + 5 + this.flowExtrasRows() + this.invites().length * 7;
		return Math.max(3, this.bodyRows() - chrome - 3);
	}

	/** The opened message takes the message list's place, so it gets that whole budget. */
	private detailListRows(): number {
		return Math.max(3, this.bodyRows() - 2 - 5 - this.flowExtrasRows() - 3);
	}

	private inboxListRows(): number {
		return Math.max(3, this.bodyRows() - 2 - this.flowExtrasRows() - 3);
	}

	private sidebarListRows(): number {
		// Three identity lines and the column's one small gap.
		return Math.max(3, this.bodyRows() - 5);
	}

	private memberListRows(): number {
		// Head row, optional address card, the selected candidate's detail, and gaps.
		const detail = this.memberKey === undefined ? 0 : 11;
		const address = this.addressOpen ? 9 : 0;
		return Math.max(3, this.bodyRows() - 4 - detail - address);
	}

	/** Every card above the list in the flow column: errors, notices and an open exchange. */
	private flowExtrasRows(): number {
		return (this.error === undefined ? 0 : 2) + (this.notice === undefined ? 0 : 1) + (this.exchange === undefined ? 0 : 8);
	}

	async reload(): Promise<void> {
		if (this.disposed || this.signal.aborted) return;
		this.bindRuntime();
		const generation = ++this.generation;
		const runtime = getRuntime();
		if (runtime === undefined) {
			this.snapshot = undefined;
			this.status = undefined;
			this.loading = false;
			this.error = t("error.notConfigured");
			this.changed();
			return;
		}
		this.loading = this.snapshot === undefined;
		try {
			const [snapshot, status] = await Promise.all([runtime.snapshot(), runtime.status()]);
			if (this.disposed || this.signal.aborted || generation !== this.generation) return;
			this.snapshot = snapshot;
			this.status = status;
			if (this.domainKey === undefined) this.domainKey = snapshot.region;
			this.error = undefined;
		} catch (error) {
			if (this.signal.aborted || generation !== this.generation) return;
			this.error = errorText(error);
		} finally {
			if (!this.disposed && !this.signal.aborted && generation === this.generation) {
				this.loading = false;
				this.changed();
			}
		}
	}

	private setNotice(text: string, error = false): void {
		this.notice = { text, error };
		clearTimeout(this.noticeTimer);
		this.noticeTimer = setTimeout(() => {
			this.noticeTimer = undefined;
			this.notice = undefined;
			this.changed();
		}, 8000);
		this.changed();
	}

	// ── data ────────────────────────────────────────────────────────────────

	private threads(): ViewerThread[] {
		const threads = this.snapshot?.threads ?? [];
		return [...threads].sort((left, right) => {
			const bySeq = Number(right.last_seq) - Number(left.last_seq);
			if (Number.isFinite(bySeq) && bySeq !== 0) return bySeq;
			return left.alias.localeCompare(right.alias);
		});
	}

	private selectedThread(): ViewerThread | undefined {
		const threads = this.threads();
		const view = this.view;
		if (view?.kind === "thread") {
			const named = threads.find(thread => thread.alias === view.alias);
			if (named !== undefined) return named;
		}
		if (view?.kind === "inbox") return undefined;
		// A message card's peer target names the conversation it belongs to.
		if (this.target !== undefined) {
			const live = threads.find(thread => thread.messages.some(message => message.author === this.target));
			if (live !== undefined) return live;
		}
		return threads[0];
	}

	/** Key requests the peer already approved: the ones waiting for this user. */
	private invites(): Invite[] {
		return (this.snapshot?.trust ?? [])
			.filter(entry => entry.phase === "invited")
			.map(entry => ({
				node_id: entry.node_id,
				fingerprint: entry.fingerprint,
				base_url: entry.base_url,
				...(entry.observed === undefined ? {} : { observed: entry.observed }),
			}));
	}

	/**
	 * Peers grouped by the domain they declare. Expired records stay in the
	 * list, flagged: the member column is the one place a user can revoke or
	 * retry a node whose observation aged out.
	 */
	private domainGroups(): DomainGroup[] {
		const groups = new Map<string, DomainGroup>();
		for (const record of this.snapshot?.discovery ?? []) {
			if (record.node_id === this.status?.node_id) continue;
			const key = groupKeyOf(record);
			const group = groups.get(key) ?? {
				key,
				...(record.region === undefined ? {} : { id: record.region }),
				label: groupLabelOf(record),
				peers: [],
			};
			groups.set(key, group);
			const trust = this.snapshot?.trust?.find(entry => entry.node_id === record.node_id);
			group.peers.push({
				record,
				...(trust === undefined
					? {}
					: {
						trust: {
							phase: trust.phase,
							fingerprint: trust.fingerprint,
							...(trust.observed === undefined ? {} : { observed: trust.observed }),
						},
					}),
			});
		}
		const own = this.snapshot?.region;
		for (const group of groups.values()) {
			group.peers.sort((left, right) => left.record.node_id.localeCompare(right.record.node_id));
		}
		return [...groups.values()].sort((left, right) => {
			if (left.key === own) return -1;
			if (right.key === own) return 1;
			return left.label.localeCompare(right.label);
		});
	}

	private currentDomain(): DomainGroup | undefined {
		const groups = this.domainGroups();
		if (this.domainKey !== undefined) {
			const picked = groups.find(group => group.key === this.domainKey);
			if (picked !== undefined) return picked;
		}
		return groups.find(group => group.key === this.snapshot?.region) ?? groups[0];
	}

	private peers(): Peer[] {
		return this.currentDomain()?.peers ?? [];
	}

	private sendBlocked(): string | undefined {
		if (this.error !== undefined) return this.error;
		if (this.draft.trim() === "") return t("send.blocked.empty");
		const thread = this.selectedThread();
		// A joined thread is another node's: the viewer shows it and offers no write it cannot
		// perform. A manual recipient starts a new message and is unaffected.
		if (this.target === undefined && thread?.read_only === true) return t("send.blocked.readOnly");
		if (this.target === undefined && this.replyTo === undefined && thread === undefined) {
			return t("send.blocked.noTarget");
		}
		return undefined;
	}

	/** The opened message as the current thread holds it: a reload replaces the row object. */
	private openedIn(thread: ViewerThread | undefined): ViewerMessage | undefined {
		const opened = this.opened;
		if (thread === undefined || opened === undefined) return undefined;
		return thread.messages.find(message => message.ref.sender === opened.ref.sender && message.ref.id === opened.ref.id);
	}

	// ── recurring pieces ────────────────────────────────────────────────────

	private button(label: string, act: string, options: { tone?: "accent" | "error"; title?: string } = {}): NativeNode {
		return node(
			"row",
			// Tone stays on the glyph, never the row: `accent` is the terminal's button paint and
			// reads as a selection highlight when spread over a whole row.
			{ role: "omp.btn", align: "center", actions: { click: act }, title: options.title ?? label },
			[textNode([span(label, options.tone === undefined ? undefined : options.tone === "error" ? "error" : "accent strong")])],
			act,
		);
	}

	/** The dim one-line label that separates the sidebar's sections inside its scrolling list. */
	private heading(label: string, count: number): NativeNode {
		return node(
			"item",
			{
				label: [span(label, "dim")],
				...(count === 0 ? {} : { value: [span(String(count), "dim")] }),
				disabled: true,
				role: "onlyne.heading",
			},
			undefined,
			`head:${label}`,
		);
	}

	/**
	 * Label/value pairs as rows with a fixed label column. A `kv` grid sizes its
	 * label column in characters, so a Chinese label (two cells per character)
	 * leaves the value touching it and each row starting the value at a
	 * different column; a `ch` bound counts the cells the terminal draws.
	 */
	private facts(items: ReadonlyArray<{ k: string; v: string }>): NativeNode {
		return node(
			"col",
			{ role: "onlyne.facts", gap: "xs", align: "stretch" },
			items.map((item, index) =>
				node(
					"row",
					{ gap: "none", align: "baseline" },
					[
						textNode([span(item.k, "dim")], { truncate: "end", min: { w: FACT_LABEL }, max: { w: FACT_LABEL } }),
						textNode([span(item.v)], { wrap: "word", grow: 1, shrink: 1 }),
					],
					`fact-${index}`,
				),
			),
			"facts",
		);
	}

	// ── columns ─────────────────────────────────────────────────────────────

	/** The domain rail: one letter per declared domain, with the count waiting on this user. */
	private railNode(): NativeNode {
		const groups = this.domainGroups();
		const current = this.currentDomain();
		const rows: NativeChild[] = groups.map(group => {
			const waiting = group.peers.filter(peer => peer.trust?.phase === "invited").length;
			const open = current?.key === group.key;
			return node(
				"item",
				{
					// The rail names the domain: one letter cannot be told apart from another, and
					// the column width already truncates a long name.
					label: [span(group.label, open ? "strong" : "muted")],
					...(waiting === 0 ? {} : { value: [span(String(waiting), "error")] }),
					...(open ? { tone: "accent" } : {}),
					title: t("rail.domain.title", { label: group.label, count: group.peers.length }),
				},
				undefined,
				`domain:${group.key}`,
			);
		});
		return node(
			"col",
			// Wide enough for a one-word domain name (`daily-workbench` is 15 cells) so the rail
			// names a domain instead of truncating it to a prefix two domains can share.
			{ role: "onlyne.rail", gap: "none", min: { w: "16ch" }, max: { w: "22ch" } },
			[
				node(
					"list",
					{
						role: "onlyne.rail.list",
						grow: 1,
						basis: 0,
						max: { lines: Math.max(3, this.bodyRows() - 1) },
						selected: current === undefined ? null : `domain:${current.key}`,
						empty: [span(t("rail.empty"), "muted")],
					},
					rows,
					"domains",
				),
			],
			"rail",
		);
	}

	/**
	 * The thread sidebar: this node's identity, then every row that opens
	 * something, in one list that scrolls inside its own cap.
	 */
	private sidebar(): NativeNode {
		const invites = this.invites();
		const threads = this.threads();
		const domain = this.currentDomain();
		const fingerprint = this.snapshot?.identity?.fingerprint;
		const queue = this.snapshot?.queue.length ?? 0;
		const selected = this.selectedThread();
		const rows: NativeChild[] = [];

		rows.push(this.heading(t("sidebar.pending"), invites.length));
		for (const invite of invites) {
			rows.push(
				node(
					"item",
					{
						label: [span("▢ ", "warning"), span(shortNodeId(invite.node_id), "strong")],
						value: [span(invite.fingerprint.slice(0, 8), "dim")],
						tone: "warning",
						title: t("sidebar.invite.title", { node: invite.node_id }),
					},
					undefined,
					`invite:${invite.node_id}`,
				),
			);
		}

		rows.push(this.heading(t("sidebar.threads"), threads.length));
		for (const thread of threads) {
			const open = selected?.alias === thread.alias && this.view?.kind !== "inbox";
			rows.push(
				node(
					"item",
					{
						label: [
							span(`# ${thread.title}`, open ? "strong" : undefined),
							...(thread.state === "closed" ? [span(` ${t("thread.closed")}`, "dim")] : []),
							...(thread.read_only === true ? [span(` ${t("thread.readOnly")}`, "dim")] : []),
						],
						// One number per row, always the same number: the unread count. A row that
						// showed the message total once everything was read made two rows of equal
						// width mean different things.
						value: thread.unread > 0 ? [span(String(thread.unread), "info")] : [],
						title: t("sidebar.thread.title", {
							title: thread.title,
							count: thread.messages.length,
							alias: shortAlias(thread.alias),
						}),
					},
					undefined,
					`thread:${thread.alias}`,
				),
			);
		}
		if (threads.length === 0) {
			rows.push(node("item", { label: [span(t("sidebar.empty"), "muted")], disabled: true }, undefined, "head:empty-threads"));
		}

		// The queue is its own section, so it needs no heading: the row names it and counts
		// what opening it shows.
		rows.push(
			node(
				"item",
				{
					label: [span(t("sidebar.inbox"), "muted")],
					value: [span(String(queue), queue === 0 ? "dim" : "info")],
				},
				undefined,
				"inbox",
			),
		);
		rows.push(
			node(
				"item",
				{
					label: [span(t("trustpage.open"), "muted")],
					value: [span(String(invites.length), invites.length === 0 ? "dim" : "warning")],
					title: t("trustpage.open.title"),
				},
				undefined,
				"trust",
			),
		);

		return node(
			"col",
			{ role: "onlyne.threads", gap: "sm", align: "stretch", min: { w: "24ch" }, max: { w: "36ch" } },
			[
				node(
					"col",
					{ gap: "none" },
					[
						textNode([span(regionLabel(domain?.id ?? this.snapshot?.region), "dim")], { truncate: "end" }),
						textNode([span(shortNodeId(this.status?.node_id ?? ""), "strong")], { truncate: "end" }),
						textNode([span(fingerprint ?? t("sidebar.noKey"), "dim")], { truncate: "end" }),
					],
					"head",
				),
				node(
					"list",
					{
						role: "onlyne.threads.list",
						grow: 1,
						basis: 0,
						max: { lines: this.sidebarListRows() },
						selected: this.view?.kind === "trust"
							? "trust"
							: this.view?.kind === "inbox"
								? "inbox"
								: selected === undefined
									? null
									: `thread:${selected.alias}`,
						empty: [span(t("sidebar.empty"), "muted")],
					},
					rows,
					"rows",
				),
			],
			"threads",
		);
	}

	/** The approval bar: one row per pending request, with both answers on it. */
	private approvalsCard(): NativeChild[] {
		const invites = this.invites();
		if (invites.length === 0) return [];
		const mine = this.snapshot?.identity?.fingerprint ?? t("key.missing");
		return invites.map(invite =>
			node(
				"card",
				{
					role: "onlyne.approval",
					tone: "warning",
					head: [span(t("approvals.head", { node: shortNodeId(invite.node_id) }), "strong")],
				},
				[
					this.facts([
						{ k: t("field.peerFingerprint"), v: invite.fingerprint },
						{ k: t("field.ownFingerprint"), v: mine },
						{ k: t("field.declared"), v: invite.base_url },
						{ k: t("field.observed"), v: invite.observed ?? t("member.observed.none") },
					]),
					node(
						"row",
						{ gap: "sm", align: "center" },
						[
							this.button(t("action.accept"), `onlyne.accept:${invite.node_id}`, { tone: "accent", title: t("approvals.accept.title") }),
							this.button(t("action.reject"), `onlyne.reject:${invite.node_id}`, { title: t("approvals.reject.title") }),
							this.button(t("action.details"), `onlyne.member:${invite.node_id}`, { title: t("approvals.details.title") }),
						],
						"acts",
					),
				],
				`approval-${invite.node_id}`,
			),
		);
	}

	private messageCard(message: ViewerMessage): NativeNode {
		const out = message.direction === "out";
		const messageKey = `${encodeURIComponent(message.ref.sender)}/${encodeURIComponent(message.ref.id)}`;
		const answered = this.replyTo?.sender === message.ref.sender && this.replyTo.id === message.ref.id;
		// A message is a flat entry, not a box: `section` gives the head and body placement of a
		// card without its ring. A `list` row is one fixed line, so a multi-line message must not
		// be the row itself.
		return node(
			"section",
			{
				role: "omp.btn",
				...(answered ? { tone: "accent" } : {}),
				head: [
					span(out ? "→ " : "← ", out ? "accent" : "dim"),
					span(plainAuthor(message.author), "strong"),
					span(`  ${clockText(message.at) || "—"}`, "dim"),
					...(message.relation === undefined ? [] : [span(`  ${message.relation}`, "dim")]),
				],
				title: t("message.open.title", { seq: message.seq }),
				actions: { click: `onlyne.message:${messageKey}` },
			},
			[textNode(message.text, { wrap: "word" })],
			`message-${messageKey}`,
		);
	}

	/** The full body in the message list's place; reply and recipient are two explicit actions. */
	private messageDetailCard(message: ViewerMessage): NativeNode {
		const own = message.author === this.status?.node_id;
		const identity = `${encodeURIComponent(message.ref.sender)}/${encodeURIComponent(message.ref.id)}`;
		return node(
			"card",
			{
				role: "onlyne.message-detail",
				inset: true,
				head: [span(t("messageDetail.head", {
					seq: message.seq,
					author: plainAuthor(message.author),
					time: clockText(message.at) || t("time.unknown"),
				}), "strong")],
			},
			[
				textNode(message.text, { wrap: "word" }),
				...(message.work === undefined
					? []
					: [textNode([span(t("messageDetail.work", { value: workValue(message.work) }), "dim")], { wrap: "word" })]),
				node(
					"row",
					{ gap: "sm", align: "center", wrap: true },
					[
						...(own ? [] : [
							this.button(t("messageDetail.reply"), `onlyne.message.reply:${identity}`, {
								tone: "accent",
								title: t("messageDetail.reply.title"),
							}),
							this.button(t("messageDetail.target"), `onlyne.message.target:${identity}`, {
								title: t("messageDetail.target.title"),
							}),
						]),
						this.button(t("messageDetail.close"), "onlyne.message.close", { title: t("messageDetail.close.title") }),
					],
					"acts",
				),
			],
			identity,
		);
	}

	private composerNode(): NativeNode {
		const thread = this.selectedThread();
		const blocked = this.sendBlocked();
		if (this.composer === undefined) {
			const editor = new Editor(getEditorTheme());
			editor.disableSubmit = true;
			editor.setText(this.draft);
			editor.setMaxHeight(3);
			this.composer = editor;
		}
		// One caret at a time: the address card owns it while it is open.
		this.composer.focused = this.focused && !this.addressOpen && !this.allowEditing;
		const where = this.replyTo !== undefined
			? t("composer.reply", { node: shortNodeId(this.replyTo.sender), id: this.replyTo.id })
			: this.target !== undefined
				? t("composer.new", { node: shortNodeId(this.target) })
				: thread === undefined
					? t("send.blocked.noTarget")
					: t("composer.thread", { title: thread.title, owner: shortNodeId(thread.ref.owner) });
		return node(
			"card",
			// `bare` keeps the head-over-body grouping without a ring: the editor draws its own
			// field, and a card ring around it reads as a box inside a box.
			{ role: "onlyne.composer", inset: true, variant: "bare" },
			[
				this.composer,
				node(
					"row",
					{ gap: "md", align: "center", justify: "between" },
					[
						textNode([span(where, blocked === undefined ? "dim" : "warning")], { wrap: "word", grow: 1 }),
						...(this.replyTo === undefined ? [] : [this.button(t("composer.cancelReply"), "onlyne.reply.clear", { title: t("composer.cancelReply.title") })]),
						...(this.target === undefined ? [] : [this.button(t("composer.back"), "onlyne.target:", { title: t("composer.back.title") })]),
						node(
							"row",
							{
								role: "omp.btn",
								align: "center",
								actions: { click: "onlyne.send" },
								title: blocked ?? t("composer.send.title"),
							},
							[textNode([span(this.sending ? t("composer.sending") : t("composer.send"), blocked === undefined ? "accent strong" : "muted")])],
							"send",
						),
					],
					"row",
				),
			],
			"composer",
		);
	}

	/** The queue the inbox row opens: one scrolling list, grouped by what each row is. */
	private inboxRows(): NativeNode {
		const queue = this.snapshot?.queue ?? [];
		const rows: NativeChild[] = [];
		for (const [head, kind] of [
			[t("inbox.heading.inbound"), "inbound"],
			[t("inbox.heading.outbound"), "outbound"],
			[t("inbox.heading.context"), "context"],
			[t("inbox.heading.decision"), "decision"],
		] as const) {
			const items = queue.filter(entry => entry.kind === kind);
			if (items.length === 0) continue;
			rows.push(this.heading(head, items.length));
			for (const entry of items) {
				rows.push(
					node(
						"item",
						{
							label: [span(entry.label, "strong")],
							detail: [span(oneLine(entry.detail), "dim")],
							...(entry.at === undefined ? {} : { value: [span(ageText(entry.at), "dim")] }),
							title: entry.detail,
						},
						undefined,
						`queue:${entry.id}`,
					),
				);
			}
		}
		return node(
			"list",
			{
				role: "onlyne.inbox.list",
				grow: 1,
				basis: 0,
				max: { lines: this.inboxListRows() },
				empty: [span(t("inbox.empty"), "muted")],
			},
			rows,
			"queue",
		);
	}

	/** RFC 0018 §5: one page carries the policy, the waiting peers and the decision record. */
	private trustPage(): NativeNode {
		const acquisition = this.snapshot?.acquisition;
		const current = getRuntime()?.config.acquisition ?? acquisition ?? DEFAULT_ACQUISITION;
		const effective = acquisition ?? current;
		const allowLoopback = "allow_loopback" in effective ? effective.allow_loopback : false;
		const invited = this.invites();
		const audit = (this.snapshot?.audit ?? []).slice(0, AUDIT_LIMIT);
		const children: NativeChild[] = [
			node(
				"card",
				{ role: "onlyne.trust-policy", head: [span(t("trustpage.title"), "strong")] },
				[
					this.facts([
						{ k: t("trustpage.policy"), v: t(POLICY_TEXT[effective.policy]) },
						{
							k: t("trustpage.allow"),
							v: effective.allow.length === 0
								? t("trustpage.allow.none")
								: effective.allow.join(", "),
						},
						{
							k: t("trustpage.allowLoopback"),
							v: allowLoopback
								? t("trustpage.allowLoopback.on")
								: t("trustpage.allowLoopback.off"),
						},
					]),
					node(
						"row",
						{ role: "onlyne.policy", gap: "sm", align: "center", wrap: true },
						POLICY_VALUES.map(value =>
							node(
								"row",
								{
									role: "omp.btn",
									actions: { click: `onlyne.policy:${value}` },
									tone: current.policy === value ? "accent" : undefined,
									title: t("trustpage.policy.set", { value }),
								},
								[textNode([span(value, current.policy === value ? "accent strong" : "muted")])],
								`policy-${value}`,
							),
						),
						"policy-switch",
					),
					...(this.allowEditing
						? [this.allowEditorNode()]
						: [
							node("row", { gap: "sm", align: "center" }, [
								this.button(t("trustpage.allow.edit"), "onlyne.allow.open", { title: t("trustpage.allow.edit.title") }),
							], "allow-edit"),
						]),
					textNode([span(t("trustpage.edit"), "dim")], { wrap: "word" }),
					textNode([span(t("trustpage.warn.lan"), "warning")], { wrap: "word" }),
					textNode([span(t("trustpage.warn.axes"), "dim")], { wrap: "word" }),
					textNode([span(t("trustpage.warn.limits"), "dim")], { wrap: "word" }),
				],
				"trust-policy",
			),
			this.heading(t("trustpage.invited.head"), invited.length),
		];
		if (invited.length === 0) children.push(textNode([span(t("trustpage.invited.empty"), "muted")], { wrap: "word" }));
		else children.push(...this.approvalsCard());
		children.push(this.heading(t("trustpage.audit.head"), audit.length));
		if (audit.length === 0) children.push(textNode([span(t("trustpage.audit.empty"), "muted")], { wrap: "word" }));
		else children.push(...audit.map((entry, index) => this.auditCard(entry, index)));
		return node(
			"list",
			{
				role: "onlyne.trust.list",
				grow: 1,
				basis: 0,
				max: { lines: Math.max(3, this.bodyRows() - 3) },
			},
			children,
			"trust",
		);
	}

	/**
	 * One recorded decision. The actor is the whole point: a human's approval that bypassed
	 * the policy reads differently from a policy auto-approval, an agent's call, or a seed.
	 */
	private auditCard(entry: AcquisitionAuditEntry, index: number): NativeNode {
		const tone = AUDIT_TONE[entry.actor];
		const head: TspSpan[] = [
			span(t(AUDIT_ACTOR[entry.actor]), `${tone} strong`),
			span(` · ${entry.action}`, "strong"),
		];
		if (entry.override === true) head.push(span(` · ${t("trustpage.override")}`, "warning"));
		if (entry.actor === "seed") head.push(span(` · ${t("trustpage.seed")}`, "warning"));
		const when = clockText(entry.at);
		if (when !== "") head.push(span(`  ${when}`, "dim"));
		const peer = entry.node_id === undefined
			? ""
			: `${shortNodeId(entry.node_id)}${entry.fingerprint === undefined ? "" : ` ${entry.fingerprint.slice(0, 8)}`}`;
		const verdict = entry.allowed === undefined
			? ""
			: ` · ${t(entry.allowed ? "trustpage.audit.allowed" : "trustpage.audit.denied")}`;
		const judgement = [
			entry.policy === undefined ? "" : `${t("trustpage.policy")}: ${entry.policy}`,
			entry.reason === undefined ? "" : entry.reason,
		].filter(part => part !== "").join(" · ");
		const addresses = [
			entry.observed === undefined ? "" : `${t("field.observed")}: ${entry.observed}`,
			entry.declared === undefined ? "" : `${t("field.declared")}: ${entry.declared}`,
		].filter(part => part !== "").join(" · ");
		return node(
			"section",
			{ role: "onlyne.audit", tone },
			[
				textNode(head, { wrap: "word" }),
				...(peer === "" ? [] : [textNode([span(peer, "muted")], { wrap: "word" })]),
				...(judgement === "" && verdict === ""
					? []
					: [textNode([span(`${judgement}${verdict}`, "dim")], { wrap: "word" })]),
				...(addresses === "" ? [] : [textNode([span(addresses, "dim")], { wrap: "word" })]),
			],
			`audit-${index}`,
		);
	}

	/** The allow set as one editable line; entries are CIDR blocks or host names. */
	private allowEditorNode(): NativeNode {
		if (this.allowEditor === undefined) {
			const editor = new Editor(getEditorTheme());
			editor.disableSubmit = true;
			editor.setText(this.allowDraft);
			editor.setMaxHeight(2);
			editor.onChange = () => {
				this.allowDraft = editor.getExpandedText();
				this.described = undefined;
				this.requestRender();
			};
			this.allowEditor = editor;
		}
		this.allowEditor.focused = this.focused;
		return node(
			"col",
			{ role: "onlyne.allow-editor", gap: "xs" },
			[
				this.allowEditor,
				node("row", { gap: "sm", align: "center" }, [
					this.button(t("trustpage.allow.save"), "onlyne.allow.save", { tone: "accent" }),
					this.button(t("trustpage.allow.cancel"), "onlyne.allow.cancel"),
				], "acts"),
			],
			"allow-editor",
		);
	}

	/** The main column: the open thread with its composer, or the queue. */
	private flowNode(): NativeNode {
		const children: NativeChild[] = [];
		if (this.error !== undefined) {
			children.push(textNode([span(this.error, "error")], { wrap: "word" }));
			children.push(this.button(t("action.reload"), "onlyne.reload"));
		}
		if (this.notice !== undefined) {
			children.push(textNode([span(this.notice.text, this.notice.error ? "error" : "info")], { wrap: "word" }));
		}
		if (this.exchange !== undefined) {
			children.push(
				node(
					"card",
					{ role: "onlyne.exchange", tone: "warning", head: [span(t("exchange.head", { node: shortNodeId(this.exchange.node_id) }), "strong")] },
					[
						this.facts([
							{ k: t("field.peerNode"), v: this.exchange.node_id },
							{ k: t("field.peerFingerprint"), v: this.exchange.fingerprint },
							{ k: t("field.ownFingerprint"), v: this.snapshot?.identity?.fingerprint ?? t("key.missing") },
						]),
						node("row", { gap: "sm", align: "center" }, [
							this.button(t("action.confirmExchange"), "onlyne.exchange.confirm", { tone: "accent" }),
							this.button(t("action.cancel"), "onlyne.exchange.cancel"),
						], "acts"),
					],
					"exchange",
				),
			);
		}

		if (this.view?.kind === "trust") {
			children.push(
				node("row", { role: "onlyne.thread-head", gap: "md", align: "center", justify: "between" }, [
					node("col", { gap: "none", grow: 1 }, [
						textNode([span(t("trustpage.title"), "strong")], { truncate: "end" }),
						textNode([span(t("trustpage.meta", {
							pending: this.invites().length,
							audit: Math.min(this.snapshot?.audit?.length ?? 0, AUDIT_LIMIT),
						}), "dim")], { truncate: "end" }),
					]),
				], "thread-head"),
				this.trustPage(),
			);
		} else if (this.view?.kind === "inbox") {
			children.push(
				node("row", { role: "onlyne.thread-head", gap: "md", align: "center", justify: "between" }, [
					node("col", { gap: "none", grow: 1 }, [
						textNode([span(t("inbox.title"), "strong")], { truncate: "end" }),
						textNode([span(t("inbox.pending", { count: this.snapshot?.queue.length ?? 0 }), "dim")], { truncate: "end" }),
					]),
				], "thread-head"),
				this.inboxRows(),
			);
		} else {
			const thread = this.selectedThread();
			if (thread === undefined) {
				children.push(textNode([span(t("flow.empty"), "muted")], { wrap: "word", lines: 3 }));
			} else {
				const messages = thread.messages;
				const opened = this.openedIn(thread);
				const last = messages.at(-1);
				const lastLine = last === undefined
					? ""
					: t("thread.meta.last", { age: sinceText(last.at) || t("time.unknown") });
				children.push(
					node("row", { role: "onlyne.thread-head", gap: "md", align: "center", justify: "between" }, [
						node("col", { gap: "none", grow: 1 }, [
							textNode([
								span(`# ${thread.title}`, "strong"),
								...(thread.state === "closed" ? [span(` · ${t("thread.closed")}`, "dim")] : []),
								...(thread.read_only === true ? [span(` · ${t("thread.readOnly")}`, "dim")] : []),
							], { truncate: "end" }),
							textNode([span(t("thread.meta", {
								messages: messages.length,
								speakers: new Set(messages.map(message => message.author)).size,
								owner: shortNodeId(thread.ref.owner),
								last: lastLine,
							}), "dim")], { truncate: "end" }),
						]),
					], "thread-head"),
					...this.approvalsCard(),
					opened === undefined
						? scrolling(
							node(
								"list",
								{
									role: "onlyne.messages",
									grow: 1,
									basis: 0,
									max: { lines: this.messageListRows() },
									empty: [span(t("thread.empty"), "muted")],
								},
								messages.map(message => this.messageCard(message)),
								"messages",
							),
							{ by: this.messageScrollBy, n: this.messageScroll },
						)
						: scrolling(
							node(
								"list",
								{
									role: "onlyne.message-detail",
									grow: 1,
									basis: 0,
									max: { lines: this.detailListRows() },
								},
								[this.messageDetailCard(opened)],
								"message-detail",
							),
							{ by: this.messageScrollBy, n: this.messageScroll },
						),
					this.composerNode(),
				);
			}
		}
		if (this.loading) children.push(node("spinner", { label: [span(t("loading"), "muted")] }, undefined, "loading"));
		return node(
			"col",
			{ role: "onlyne.flow", gap: "sm", grow: 1, shrink: 1, align: "stretch", min: { w: 0 } },
			children,
			"flow",
		);
	}

	/**
	 * A node reached by address instead of by discovery: mDNS never leaves the link, and a
	 * remote node on a public name has no advertisement at all. The address is read once and
	 * saved as a seed, so the next start finds the node again without this card.
	 */
	private addressCard(): NativeNode {
		if (this.address === undefined) {
			const editor = new Editor(getEditorTheme());
			editor.disableSubmit = true;
			editor.setText(this.addressText);
			editor.setMaxHeight(1);
			editor.onChange = () => {
				this.addressText = editor.getExpandedText();
				this.described = undefined;
				this.requestRender();
			};
			this.address = editor;
		}
		this.address.focused = this.focused;
		return node(
			"card",
			{ role: "onlyne.connect", inset: true, head: [span(t("connect.head"), "strong")] },
			[
				this.address,
				textNode(
					[span(t("connect.hint"), "dim")],
					{ wrap: "word" },
				),
				node(
					"row",
					{ gap: "sm", align: "center", wrap: true },
					[
						this.button(this.connecting ? t("connect.connecting") : t("connect.submit"), "onlyne.connect.submit", {
							tone: "accent",
							title: t("connect.submit.title"),
						}),
						this.button(t("action.cancel"), "onlyne.connect.cancel", { title: t("connect.cancel.title") }),
					],
					"acts",
				),
			],
			"connect",
		);
	}

	/** The member column: the current domain's peers, then the selected one's detail. */
	private memberColumn(): NativeNode {
		const peers = this.peers();
		const rows: NativeChild[] = peers.map(peer => this.candidateCard(peer));
		const children: NativeChild[] = [
			node("row", { gap: "sm", align: "center", justify: "between" }, [
				textNode([span(t("members.head", { count: peers.length }), "dim")]),
				node(
					"row",
					{ gap: "xs", align: "center" },
					[
						...(this.addressOpen ? [] : [this.button(t("members.add"), "onlyne.connect.open", { title: t("members.add.title") })]),
						this.button(t("members.refresh"), "onlyne.discovery.refresh", { title: t("members.refresh.title") }),
					],
					"acts",
				),
			], "head"),
			...(this.addressOpen ? [this.addressCard()] : []),
			node(
				"list",
				{
					role: "onlyne.members.list",
					grow: 1,
					basis: 0,
					max: { lines: this.memberListRows() },
					empty: [span(t("members.empty"), "muted")],
				},
				rows,
				"roster",
			),
		];
		const peer = peers.find(entry => entry.record.node_id === this.memberKey);
		if (peer !== undefined) {
			const phase = peer.trust?.phase;
			const stale = Date.parse(peer.record.expires_at) <= Date.now();
			children.push(
				node(
					"card",
					{
						role: "onlyne.member-detail",
						tone: phase === undefined ? undefined : CANDIDATE_TONE[phase],
						head: [span(shortNodeId(peer.record.node_id), "strong"), ...(phase === undefined ? [] : [span(` · ${t(PHASE_LABEL[phase])}`, PHASE_TONE[phase])])],
					},
					[
						this.facts([
							{ k: t("field.fingerprint"), v: peer.trust?.fingerprint ?? t("member.fingerprint.none") },
							{ k: t("field.domain"), v: regionLabel(peer.record.region) },
							{ k: t("field.status"), v: phase === undefined ? t("trust.none") : t(PHASE_LABEL[phase]) },
							{ k: t("field.declared"), v: peer.record.base_url },
							{ k: t("field.observed"), v: peer.trust?.observed ?? t("member.observed.none") },
							{ k: t("field.source"), v: peer.record.source === "bootstrap" ? t("member.source.seed") : peer.record.source },
							{
								k: t("field.lastSeen"),
								v: `${sinceText(peer.record.last_seen) || t("time.unknown")}${stale ? t("member.lastSeen.stale") : ""}`,
							},
						]),
						node("row", { gap: "sm", align: "center", wrap: true }, [
							this.button(t("action.useTarget"), `onlyne.target:${peer.record.node_id}`, { title: t("member.target.title") }),
							...(phase === "invited" ? [this.button(t("action.acceptExchange"), `onlyne.accept:${peer.record.node_id}`, { tone: "accent" })] : []),
							...(phase === undefined || phase === "revoked"
								? [this.button(t("action.requestExchange"), `onlyne.request:${peer.record.node_id}`, { title: t("member.request.title") })]
								: []),
							...(phase === "exchanged" || phase === "requested" ? [this.button(t("action.revoke"), `onlyne.revoke:${peer.record.node_id}`, { tone: "error" })] : []),
						], "acts"),
					],
					"member-detail",
				),
			);
		}
		return node(
			"col",
			{ role: "onlyne.members", gap: "sm", align: "stretch", min: { w: "24ch" }, max: { w: "34ch" } },
			children,
			"members",
		);
	}

	/**
	 * RFC 0018 §5 / design §5.3: the ring says where the exchange stands (blue while it is
	 * in progress, green once both sides confirmed), and selection paints its own background
	 * over that ring instead of replacing it.
	 */
	private candidateCard(peer: Peer): NativeNode {
		const phase = peer.trust?.phase;
		const open = this.memberKey === peer.record.node_id;
		return node(
			"card",
			{
				role: "onlyne.candidate",
				tone: phase === undefined ? undefined : CANDIDATE_TONE[phase],
				selected: open,
				actions: { click: `onlyne.member:${peer.record.node_id}` },
				head: [
					span(shortNodeId(peer.record.node_id), "strong"),
					span(` · ${phase === undefined ? t("trust.none") : t(PHASE_SHORT[phase])}`, phase === undefined ? "dim" : PHASE_TONE[phase]),
				],
				title: `${peer.record.node_id}\n${peer.record.base_url}`,
			},
			[textNode([span(peer.record.base_url, "dim")], { truncate: "end" })],
			`peer:${peer.record.node_id}`,
		);
	}

	describe(cx: DescribeContext): NativeNode {
		const key = `${this.version}:${cx.cols}:${this.rows()}`;
		if (this.described !== undefined && this.describedKey === key) return this.described;
		// The full sheet draws a title row and one inset row around the body: bound the columns
		// to the rest of the pane, or the composer lands past the bottom edge.
		const body = linesOf(Math.max(8, this.rows() - 2));
		const columns = node(
			"row",
			{ gap: "md", align: "stretch", grow: 1, shrink: 1, min: { h: body }, max: { h: body } },
			[this.railNode(), this.sidebar(), this.flowNode(), this.memberColumn()],
			"columns",
		);
		this.described = node("card", { role: "onlyne.viewer", grow: 1 }, [columns], "root");
		this.describedKey = key;
		return this.described;
	}

	// ── events ──────────────────────────────────────────────────────────────

	/** A row of the sidebar or the member list: its id names what the click opens. */
	private selectRow(id: string): void {
		if (id.startsWith("thread:")) {
			const alias = id.slice("thread:".length);
			if (!this.threads().some(thread => thread.alias === alias)) return;
			this.view = { kind: "thread", alias };
			this.target = undefined;
			this.replyTo = undefined;
			this.opened = undefined;
			this.allowEditing = false;
			this.allowEditor = undefined;
			this.allowDraft = "";
			this.messageScrollBy = "end";
			this.messageScroll++;
		} else if (id.startsWith("invite:")) {
			this.memberKey = id.slice("invite:".length);
			this.view = { kind: "trust" };
		} else if (id.startsWith("domain:")) {
			this.domainKey = id.slice("domain:".length);
			this.memberKey = undefined;
		} else if (id.startsWith("peer:")) {
			const peer = id.slice("peer:".length);
			this.memberKey = this.memberKey === peer ? undefined : peer;
		} else if (id === "inbox") {
			this.view = { kind: "inbox" };
			this.opened = undefined;
			this.allowEditing = false;
			this.allowEditor = undefined;
			this.allowDraft = "";
		} else if (id === "trust") {
			this.view = { kind: "trust" };
			this.opened = undefined;
		} else {
			return;
		}
		this.changed();
	}

	/** A message in the open thread, by the identity its card carries. */
	private messageByIdentity(identity: string): ViewerMessage | undefined {
		return this.selectedThread()?.messages.find(
			message => `${encodeURIComponent(message.ref.sender)}/${encodeURIComponent(message.ref.id)}` === identity,
		);
	}

	handleNativeEvent(event: NativeUiEvent): void {
		if (this.disposed || this.signal.aborted) return;
		if (event.type === "edit" || event.type === "undo") {
			if (this.addressOpen) {
				this.address?.handleNativeEvent(event);
				this.addressText = this.address?.getExpandedText() ?? this.addressText;
			} else if (this.allowEditing) {
				this.allowEditor?.handleNativeEvent(event);
				this.allowDraft = this.allowEditor?.getExpandedText() ?? this.allowDraft;
			} else {
				this.composer?.handleNativeEvent(event);
				this.draft = this.composer?.getExpandedText() ?? this.draft;
			}
			this.described = undefined;
			this.requestRender();
			return;
		}
		// The keypath carries the component's ancestry (`root/columns/sidebar/rows`), so match its leaf.
		if (event.type === "select" || event.type === "activate") {
			const leaf = event.key.slice(event.key.lastIndexOf("/") + 1);
			if (leaf === "rows" || leaf === "roster" || leaf === "domains") this.selectRow(event.item);
			return;
		}
		if (event.type !== "action") return;
		const act = event.act;
		if (act === "onlyne.close") {
			this.done();
			return;
		}
		if (act === "onlyne.reload") {
			void this.reload();
			return;
		}
		if (act === "onlyne.send") {
			void this.send();
			return;
		}
		if (act === "onlyne.reply.clear") {
			this.replyTo = undefined;
			this.changed();
			return;
		}
		if (act === "onlyne.message.close") {
			this.opened = undefined;
			this.changed();
			return;
		}
		if (act === "onlyne.discovery.refresh") {
			void this.refreshDiscovery();
			return;
		}
		if (act === "onlyne.connect.open") {
			this.addressOpen = true;
			this.changed();
			return;
		}
		if (act === "onlyne.connect.cancel") {
			this.closeAddress();
			return;
		}
		if (act === "onlyne.connect.submit") {
			void this.connect();
			return;
		}
		if (act === "onlyne.exchange.confirm") {
			void this.confirmExchange();
			return;
		}
		if (act === "onlyne.exchange.cancel") {
			this.exchangeToken++;
			this.exchange = undefined;
			this.changed();
			return;
		}
		if (act.startsWith("onlyne.domain:")) {
			this.domainKey = act.slice("onlyne.domain:".length);
			this.memberKey = undefined;
			this.changed();
			return;
		}
		if (act.startsWith("onlyne.member:")) {
			const id = act.slice("onlyne.member:".length);
			this.memberKey = this.memberKey === id ? undefined : id;
			this.changed();
			return;
		}
		if (act.startsWith("onlyne.target:")) {
			const id = act.slice("onlyne.target:".length);
			this.target = id === "" || this.target === id ? undefined : id;
			this.replyTo = undefined;
			this.changed();
			return;
		}
		if (act.startsWith("onlyne.policy:")) {
			const policy = act.slice("onlyne.policy:".length);
			if (policy === "all" || policy === "segment" || policy === "none") void this.setPolicy(policy);
			return;
		}
		if (act === "onlyne.allow.open") {
			this.openAllow();
			return;
		}
		if (act === "onlyne.allow.save") {
			void this.saveAllow();
			return;
		}
		if (act === "onlyne.allow.cancel") {
			this.closeAllow();
			return;
		}
		if (act.startsWith("onlyne.message:")) {
			const identity = act.slice("onlyne.message:".length);
			const message = this.messageByIdentity(identity);
			if (message === undefined) return;
			const already = this.opened !== undefined
				&& `${encodeURIComponent(this.opened.ref.sender)}/${encodeURIComponent(this.opened.ref.id)}` === identity;
			this.opened = already ? undefined : message;
			if (!already) {
				// A long body starts at its head, not wherever the message list was left.
				this.messageScrollBy = "start";
				this.messageScroll++;
			}
			this.changed();
			return;
		}
		if (act.startsWith("onlyne.message.reply:")) {
			const message = this.messageByIdentity(act.slice("onlyne.message.reply:".length));
			if (message === undefined || message.ref.sender === this.status?.node_id) return;
			this.replyTo = message.ref;
			this.target = undefined;
			this.opened = undefined;
			this.changed();
			return;
		}
		if (act.startsWith("onlyne.message.target:")) {
			const message = this.messageByIdentity(act.slice("onlyne.message.target:".length));
			if (message === undefined || message.ref.sender === this.status?.node_id) return;
			this.target = message.author;
			this.replyTo = undefined;
			this.opened = undefined;
			this.changed();
			return;
		}
		if (act.startsWith("onlyne.accept:")) {
			void this.accept(act.slice("onlyne.accept:".length));
			return;
		}
		if (act.startsWith("onlyne.reject:")) {
			// The runtime's only refusal is a revocation, which leaves the peer able to ask again.
			void this.revoke(act.slice("onlyne.reject:".length), true);
			return;
		}
		if (act.startsWith("onlyne.revoke:")) {
			void this.revoke(act.slice("onlyne.revoke:".length));
			return;
		}
		if (act.startsWith("onlyne.request:")) {
			void this.request(act.slice("onlyne.request:".length));
		}
	}

	/** Escape closes the overlay; the page keys scroll the open list; everything else is the composer's. */
	handleInput(data: string): void {
		if (this.disposed || this.signal.aborted) return;
		if (matchesKey(data, "escape")) {
			if (this.addressOpen) {
				this.closeAddress();
				return;
			}
			if (this.allowEditing) {
				this.closeAllow();
				return;
			}
			if (this.opened !== undefined) {
				this.opened = undefined;
				this.changed();
				return;
			}
			this.done();
			return;
		}
		if (this.addressOpen) {
			// A single-line field: Enter is the one way to submit it from the keyboard.
			if (matchesKey(data, "enter")) {
				void this.connect();
				return;
			}
			this.address?.handleInput(data);
			this.addressText = this.address?.getExpandedText() ?? this.addressText;
			this.described = undefined;
			this.requestRender();
			return;
		}
		if (this.allowEditing) {
			if (matchesKey(data, "enter")) {
				void this.saveAllow();
				return;
			}
			this.allowEditor?.handleInput(data);
			this.allowDraft = this.allowEditor?.getExpandedText() ?? this.allowDraft;
			this.described = undefined;
			this.requestRender();
			return;
		}
		if (matchesKey(data, "ctrl+enter")) {
			void this.send();
			return;
		}
		if (matchesKey(data, "pageUp") || matchesKey(data, "pageDown") || matchesKey(data, "end")) {
			this.messageScrollBy = matchesKey(data, "pageUp") ? "page-up" : matchesKey(data, "pageDown") ? "page-down" : "end";
			this.messageScroll++;
			this.changed();
			return;
		}
		if (this.composer === undefined) return;
		this.composer.handleInput(data);
		this.draft = this.composer.getExpandedText();
		this.described = undefined;
	}

	private async refreshDiscovery(): Promise<void> {
		const runtime = getRuntime();
		if (runtime === undefined) return;
		this.setNotice(t("notice.refreshing"));
		try {
			await runtime.refreshDiscovery();
			this.setNotice(t("notice.refreshed"));
		} catch (error) {
			this.setNotice(t("notice.refreshFailed", { error: errorText(error) }), true);
		}
		await this.reload();
	}

	/** RFC 0018 §2: a policy change is configuration, and never rewrites existing trust. */
	private async setPolicy(policy: AcquirePolicy): Promise<void> {
		const runtime = getRuntime();
		if (runtime === undefined || this.policyBusy) return;
		const current = runtime.config.acquisition ?? DEFAULT_ACQUISITION;
		if (current.policy === policy) return;
		this.policyBusy = true;
		this.changed();
		try {
			await runtime.updateConfig({ ...runtime.config, acquisition: { ...current, policy } });
			this.setNotice(t("notice.policySaved", { policy }));
		} catch (error) {
			this.setNotice(t("notice.policyFailed", { error: errorText(error) }), true);
		} finally {
			this.policyBusy = false;
			await this.reload();
		}
	}

	private openAllow(): void {
		const runtime = getRuntime();
		const current = runtime?.config.acquisition ?? this.snapshot?.acquisition ?? DEFAULT_ACQUISITION;
		this.allowDraft = current.allow.join(", ");
		this.allowEditor = undefined;
		this.allowEditing = true;
		this.changed();
	}

	private closeAllow(): void {
		this.allowEditing = false;
		this.allowEditor = undefined;
		this.allowDraft = "";
		this.changed();
	}

	private async saveAllow(): Promise<void> {
		const runtime = getRuntime();
		if (runtime === undefined) {
			this.setNotice(t("notice.notRunning"), true);
			return;
		}
		const allow = this.allowDraft
			.split(/[,\s]+/)
			.map(entry => entry.trim())
			.filter(entry => entry.length > 0);
		for (const entry of allow) {
			if (parseAllowBlock(entry) === undefined) {
				this.setNotice(t("trustpage.allowInvalid", { value: entry }), true);
				return;
			}
		}
		const current = runtime.config.acquisition ?? DEFAULT_ACQUISITION;
		try {
			await runtime.updateConfig({ ...runtime.config, acquisition: { policy: current.policy, allow } });
			this.closeAllow();
			this.setNotice(t("notice.allowSaved"));
		} catch (error) {
			this.setNotice(t("notice.allowFailed", { error: errorText(error) }), true);
		}
		await this.reload();
	}

	private closeAddress(): void {
		this.addressOpen = false;
		this.address = undefined;
		this.addressText = "";
		this.changed();
	}

	/**
	 * Reads the typed address through the runtime, which owns normalization, the discovery
	 * read and the seed write. The node keeps running; the new peer arrives in the member
	 * list, where the ordinary exchange request applies to it.
	 */
	private async connect(): Promise<void> {
		if (this.connecting) return;
		const runtime = getRuntime();
		if (runtime === undefined) {
			this.setNotice(t("notice.notRunning"), true);
			return;
		}
		const typed = this.addressText.trim();
		if (typed.length === 0) {
			this.setNotice(t("notice.addressEmpty"), true);
			return;
		}
		this.connecting = true;
		this.changed();
		let seed: string | undefined;
		try {
			seed = normalizeServiceUrl(typed);
			await runtime.contact(typed);
			this.setNotice(t("notice.connected", { seed }));
			this.closeAddress();
		} catch (error) {
			seed = undefined;
			this.setNotice(t("notice.connectFailed", { error: errorText(error) }), true);
		} finally {
			this.connecting = false;
			await this.reload();
		}
		if (seed === undefined) return;
		// Land on the new peer, in its own domain: its detail card is where the exchange starts.
		const target = seed;
		const record = this.snapshot?.discovery.find(entry => entry.base_url === target);
		if (record === undefined) return;
		this.domainKey = groupKeyOf(record);
		this.memberKey = record.node_id;
		this.changed();
	}

	private async accept(nodeId: string): Promise<void> {
		const runtime = getRuntime();
		const invite = this.invites().find(entry => entry.node_id === nodeId);
		if (runtime === undefined || invite === undefined) return;
		try {
			await runtime.trustAccept(nodeId, invite.fingerprint);
			this.setNotice(t("notice.accepted", { node: shortNodeId(nodeId) }));
		} catch (error) {
			this.setNotice(t("notice.acceptFailed", { error: errorText(error) }), true);
		}
		await this.reload();
	}

	/** `refused` distinguishes a rejection of the peer's request from a revocation of trust. */
	private async revoke(nodeId: string, refused = false): Promise<void> {
		const runtime = getRuntime();
		if (runtime === undefined) return;
		try {
			await runtime.trustRevoke(nodeId);
			this.setNotice(refused
				? t("notice.rejected", { node: shortNodeId(nodeId) })
				: t("notice.revoked", { node: shortNodeId(nodeId) }));
		} catch (error) {
			this.setNotice(t("notice.failed", { verb: t(refused ? "verb.reject" : "verb.revoke"), error: errorText(error) }), true);
		}
		await this.reload();
	}

	private async request(nodeId: string): Promise<void> {
		const runtime = getRuntime();
		if (runtime === undefined) return;
		const token = ++this.exchangeToken;
		this.exchange = undefined;
		this.changed();
		try {
			const session = await runtime.trustBegin(nodeId);
			if (this.disposed || this.signal.aborted || token !== this.exchangeToken) return;
			this.exchange = session;
			this.changed();
		} catch (error) {
			if (token !== this.exchangeToken) return;
			this.setNotice(t("notice.requestFailed", { error: errorText(error) }), true);
		}
	}

	private async confirmExchange(): Promise<void> {
		const session = this.exchange;
		if (session === undefined) return;
		this.exchange = undefined;
		this.changed();
		try {
			await session.confirm(session.fingerprint);
			this.setNotice(t("notice.exchangeSent", { node: shortNodeId(session.node_id) }));
		} catch (error) {
			this.setNotice(t("notice.requestFailed", { error: errorText(error) }), true);
		}
		await this.reload();
	}

	/**
	 * The composer sends down the one message path the tool uses: the node's own
	 * client with this session as the consumption context, so a click and a `msg`
	 * call leave the same records. A reply names the message it answers, which is
	 * what lands it in the thread holding that message.
	 */
	private async send(): Promise<void> {
		if (this.sending) return;
		const runtime = getRuntime();
		const blocked = this.sendBlocked();
		if (blocked !== undefined) {
			this.setNotice(blocked, true);
			return;
		}
		if (runtime === undefined) return;
		const thread = this.selectedThread();
		const meta: NonNullable<MsgInput["meta"]> = {};
		if (this.replyTo !== undefined) {
			meta.in_reply_to = { sender: this.replyTo.sender, id: this.replyTo.id };
			meta.relation = "reply";
		} else if (this.target !== undefined) {
			meta.to = this.target;
		} else if (thread !== undefined) {
			meta.to = thread.ref.owner;
			meta.thread = { owner: thread.ref.owner, id: thread.ref.id };
		}
		const input: MsgInput = {
			delivery_kind: this.replyTo !== undefined
				? t("delivery.reply", { node: shortNodeId(this.replyTo.sender) })
				: thread === undefined
					? t("delivery.to", { node: shortNodeId(this.target ?? "") })
					: t("delivery.thread", { title: thread.title }),
			msg: this.draft.trim(),
			...(Object.keys(meta).length === 0 ? {} : { meta }),
		};
		this.sending = true;
		this.changed();
		try {
			// The library decides every send through the plugin, and the plugin only decides inside
			// a bound scope: without one the composer reported a send failure (audit §2.3). The
			// panel is the main session's input, so it carries the main session's scope.
			const outcome = (await runDecision({ ctx: runtime.main, input }, () =>
				runtime.node.client.msg(input, { context: runtime.mainAgentId }))) as SendOutcome;
			this.setNotice(this.describeOutcome(outcome), outcome.admission === "refused" || outcome.error !== undefined);
			if (outcome.admission === "accepted" || outcome.admission === "unknown") {
				this.draft = "";
				this.composer?.setText("");
				this.replyTo = undefined;
				this.opened = undefined;
				this.messageScrollBy = "end";
				this.messageScroll++;
			}
		} catch (error) {
			this.setNotice(t("notice.sendFailed", { error: errorText(error) }), true);
		} finally {
			this.sending = false;
			await this.reload();
		}
	}

	private describeOutcome(outcome: SendOutcome): string {
		if (outcome.admission === "refused") {
			return t("outcome.refused", { error: outcome.error?.message ?? t("outcome.refused.peer") });
		}
		const reply = outcome.reply?.message.msg;
		if (reply !== undefined) return t("outcome.replied", { reply: oneLine(reply) || t("outcome.reply.empty") });
		if (outcome.admission === "accepted") return t("outcome.accepted");
		return t("outcome.unknown");
	}

	/** The ANSI fallback for a terminal without the native surface: a listing, no interaction. */
	render(width: number): readonly string[] {
		const key = `${this.version}:${width}`;
		if (this.rendered !== undefined && this.rendered.key === key) return this.rendered.lines;
		const status = this.status;
		const lines: string[] = [
			fit(
				t("fallback.status", {
					region: regionLabel(status?.region),
					nodes: status?.nodes ?? "…",
					neighbors: status?.neighbors ?? "…",
					alerts: status?.alerts ?? 0,
					unread: status?.unread ?? 0,
				}),
				width,
			),
		];
		if (this.error !== undefined) lines.push(this.theme.fg("error", fit(this.error, width)));
		for (const line of this.ansiRows(width)) lines.push(line);
		this.rendered = { key, lines };
		return this.rendered.lines;
	}

	private ansiRows(width: number): string[] {
		const lines: string[] = [];
		const short = (text: string): string => fit(text, width);
		if (this.view?.kind === "trust") {
			const acquisition = this.snapshot?.acquisition;
			lines.push(short(t("trustpage.title")));
			if (acquisition !== undefined) {
				lines.push(short(`${t("trustpage.policy")}: ${t(POLICY_TEXT[acquisition.policy])}`));
				lines.push(short(`${t("trustpage.allow")}: ${acquisition.allow.length === 0 ? t("trustpage.allow.none") : oneLine(acquisition.allow.join(", "))}`));
				lines.push(short(`${t("trustpage.allowLoopback")}: ${acquisition.allow_loopback ? t("trustpage.allowLoopback.on") : t("trustpage.allowLoopback.off")}`));
			} else lines.push(short(t("trustpage.policy.unknown")));
			lines.push(short(t("trustpage.warn.lan")));
			lines.push(short(t("trustpage.warn.limits")));
			lines.push(short(`${t("trustpage.invited.head")} · ${this.invites().length}`));
			for (const invite of this.invites()) {
				lines.push(short(
					`${shortNodeId(invite.node_id)} · ${invite.fingerprint} · ${t("field.declared")}: ${invite.base_url} · ${t("field.observed")}: ${invite.observed ?? t("member.observed.none")}`,
				));
			}
			const audit = (this.snapshot?.audit ?? []).slice(0, AUDIT_LIMIT);
			lines.push(short(`${t("trustpage.audit.head")} · ${audit.length}`));
			for (const entry of audit) {
				lines.push(short(
					`${t(AUDIT_ACTOR[entry.actor])} · ${entry.action}${entry.override === true ? ` · ${t("trustpage.override")}` : ""}${entry.node_id === undefined ? "" : ` · ${shortNodeId(entry.node_id)}`}${entry.reason === undefined ? "" : ` · ${entry.reason}`}`,
				));
			}
			return lines;
		}
		if (this.view?.kind === "inbox") {
			lines.push(short(t("fallback.inbox", { count: this.snapshot?.queue.length ?? 0 })));
			for (const entry of this.snapshot?.queue ?? []) {
				lines.push(short(`  ${entry.kind} ${entry.label} · ${oneLine(entry.detail)}`));
			}
			return lines;
		}
		const thread = this.selectedThread();
		lines.push(short(t("fallback.threads", {
			count: this.threads().length,
			title: thread === undefined ? t("fallback.none") : thread.title,
		})));
		for (const invite of this.invites()) {
			lines.push(short(t("fallback.invite", {
				node: shortNodeId(invite.node_id),
				fingerprint: `${invite.fingerprint} · ${invite.base_url} · ${invite.observed ?? t("member.observed.none")}`,
			})));
		}
		const opened = this.openedIn(thread);
		if (opened !== undefined) {
			lines.push(short(t("messageDetail.head", {
				seq: opened.seq,
				author: plainAuthor(opened.author),
				time: clockText(opened.at) || t("time.unknown"),
			})));
			for (const line of opened.text.split("\n")) lines.push(short(`  ${line}`));
		}
		for (const message of opened === undefined ? thread?.messages ?? [] : []) {
			lines.push(short(`${message.direction === "out" ? "→" : "←"} [${message.seq}] ${plainAuthor(message.author)} ${clockText(message.at)}`));
			for (const line of message.text.split("\n").slice(0, 6)) lines.push(short(`  ${line}`));
		}
		if (this.addressOpen) {
			lines.push(short(t("fallback.address", { address: oneLine(this.addressText) || t("fallback.empty") })));
		}
		const blocked = this.sendBlocked();
		lines.push(short(t("fallback.draft", {
			text: oneLine(this.draft) || t("fallback.empty"),
			blocked: blocked === undefined ? "" : ` · ${blocked}`,
		})));
		return lines;
	}

	invalidate(): void {
		this.version++;
		this.described = undefined;
		this.rendered = undefined;
	}

	dispose(): void {
		if (this.disposed) return;
		this.disposed = true;
		clearTimeout(this.noticeTimer);
		this.noticeTimer = undefined;
		this.unsubscribe?.();
		this.unsubscribe = undefined;
		this.watched = undefined;
		this.composer = undefined;
		this.address = undefined;
		this.allowEditor = undefined;
		this.described = undefined;
		this.rendered = undefined;
	}
}
