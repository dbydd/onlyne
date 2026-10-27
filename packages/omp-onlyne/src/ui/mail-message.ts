import type { MessageRenderer } from "@oh-my-pi/pi-coding-agent";
import { getLocale } from "@onlyne/node-host/i18n";
import { Markdown, Text, getMarkdownTheme, node, span, text, type Component, type NativeNode, type Theme, type TspSpan } from "@oh-my-pi/pi-tui";
import type { HistoryMessage, MessageRef, ThreadRef } from "@onlyne/lib";
import { shortNodeId } from "../config.ts";
import { getRuntime, type PluginRuntime } from "../runtime.ts";
import { conversationPath, messagePath } from "../urls.ts";
import { t } from "./bootstrap-text.ts";

export interface OnlyneMessageDetails {
	thread_ref: ThreadRef;
	seq: string;
	message_ref: MessageRef;
	/** What the payload can attest: this entry reached this node's inbox. */
	delivery: string;
	/** The submission-time consumption fact; the confirmed one is read from history. */
	consumption: string;
	mail?: {
		from: string;
		at: string;
		subject: string;
		body: string;
		relation?: "reply" | "followup";
		in_reply_to?: MessageRef;
	};
}

/** What the card reports about one message: the live history row, or why it could not read one. */
interface CardStatus {
	delivery: string;
	consumption?: string;
	error?: string;
}

/**
 * The cards currently in a transcript. A message renderer is handed no TUI handle, so the
 * cards hold themselves weakly and share one subscription: a rebuilt transcript drops its old
 * components without leaking listeners.
 */
const cards = new Set<WeakRef<MailMessage>>();
let watched: PluginRuntime | undefined;
/** A status key this card never writes: clearing an unset key is a no-op that asks for a frame. */
const REPAINT_KEY = "onlyne.mail";

/**
 * Ask the host to paint again. A message renderer receives no repaint callback, and the one
 * host call that renders unconditionally is a status write; clearing a key this card never
 * sets changes nothing on screen and still requests the frame.
 */
function repaint(): void {
	try {
		getRuntime()?.main.ui.setStatus(REPAINT_KEY, undefined);
	} catch {
		// A closed session has no status line; the next host paint still shows the new state.
	}
}

/** One subscription per runtime: a card refreshes when the node reports a lifecycle change. */
function watch(): void {
	const runtime = getRuntime();
	if (runtime === undefined || watched === runtime) return;
	watched = runtime;
	runtime.onChange(() => refreshMailCards());
}

/**
 * Re-read every card still in a transcript. A consumption is recorded without a notification
 * of its own, so the host calls this when it confirms one.
 */
export function refreshMailCards(): void {
	for (const reference of cards) {
		const card = reference.deref();
		if (card === undefined) cards.delete(reference);
		else void card.refresh();
	}
}

class MailMessage implements Component {
	private status: CardStatus;
	private native: NativeNode | undefined;
	private headers: Text | undefined;
	private footer: Text | undefined;
	private rendered: readonly string[] | undefined;
	private refreshing = false;
	private readonly body: Markdown;
	private locale = getLocale();

	constructor(
		private readonly details: OnlyneMessageDetails,
		private readonly kind: string,
		private readonly theme: Theme,
	) {
		this.body = new Markdown(details.mail?.body ?? "", 0, 0, getMarkdownTheme());
		this.status = { delivery: details.delivery, consumption: details.consumption };
		cards.add(new WeakRef(this));
		watch();
		void this.refresh();
	}

	/** Read the message's own row: delivery and consumption are facts, not a guess from the card. */
	async refresh(): Promise<void> {
		const runtime = getRuntime();
		if (runtime === undefined || this.refreshing) return;
		this.refreshing = true;
		let next: CardStatus;
		try {
			const row: HistoryMessage = await runtime.node.history.message(runtime.node.auth, this.details.message_ref);
			next = { delivery: row.delivery, ...(row.consumption === undefined ? {} : { consumption: row.consumption }) };
		} catch (error) {
			// A failed read is reported as unknown; claiming a state the card cannot see is worse.
			next = { delivery: "unknown", error: error instanceof Error ? error.message : String(error) };
		} finally {
			this.refreshing = false;
		}
		if (next.delivery === this.status.delivery && next.consumption === this.status.consumption && next.error === this.status.error) return;
		this.status = next;
		this.native = undefined;
		this.headers = undefined;
		this.footer = undefined;
		this.rendered = undefined;
		repaint();
	}

	describe(): NativeNode {
		this.dropLocaleCaches();
		return this.native ??= this.describeCard();
	}

	render(width: number): readonly string[] {
		this.dropLocaleCaches();
		if (this.rendered !== undefined) return this.rendered;
		const mail = this.details.mail;
		if (mail === undefined) return this.rendered = [];
		const rule = "─".repeat(Math.max(0, Math.min(width, 72)));
		this.headers ??= new Text(this.headerLines(), 0, 0);
		this.footer ??= new Text(this.theme.fg("muted", `${this.label()} · #${this.details.seq}\n${messagePath(this.details.message_ref)}`), 0, 0);
		return this.rendered = [...this.headers.render(width), rule, "", ...this.body.render(width), "", rule, ...this.footer.render(width)];
	}

	invalidate(): void {
		this.native = undefined;
		this.headers = undefined;
		this.footer = undefined;
		this.rendered = undefined;
		this.body.invalidate();
	}

	private label(): string {
		return this.kind === "onlyne.supplement"
			? t("mail.supplement")
			: this.kind === "onlyne.routing"
				? t("mail.routing")
				: t("mail.letter");
	}

	private heading(): string {
		const subject = (this.details.mail?.subject ?? "").replace(/[\r\n]+/g, " ");
		return `${this.details.mail?.relation === "reply" ? "Re: " : ""}${subject}`;
	}

	private headerLines(): string {
		const mail = this.details.mail;
		if (mail === undefined) return "";
		const fields = [
			`${this.theme.fg("muted", `${t("mail.from")}:`)} ${shortNodeId(mail.from)} <${mail.from}>`,
			`${this.theme.fg("muted", `${t("mail.date")}:`)} ${mail.at}`,
			`${this.theme.fg("muted", `${t("mail.thread")}:`)} ${this.details.thread_ref.owner}/${this.details.thread_ref.id} #${this.details.seq}`,
			...(mail.relation === undefined ? [] : [`${this.theme.fg("muted", `${t("mail.relation")}:`)} ${mail.relation}`]),
			...(mail.in_reply_to === undefined
				? []
				: [`${this.theme.fg("muted", `${t("mail.inReplyTo")}:`)} ${mail.in_reply_to.sender}/${mail.in_reply_to.id}`]),
			`${this.theme.fg("muted", `${t("mail.delivery")}:`)} ${this.status.delivery}${
				this.status.consumption === undefined ? "" : t("mail.consumed", { state: this.status.consumption })
			}`,
			...(this.status.error === undefined
				? []
				: [`${this.theme.fg("muted", `${t("mail.status")}:`)} unknown (${this.status.error})`]),
		];
		return [this.theme.fg("accent", this.theme.bold(this.heading())), ...fields].join("\n");
	}

	/**
	 * The mailing-list card: the header block a letter carries, the body, then the two ways to
	 * read this message again. Every link names a route this node serves, and one that does
	 * not resolve is not offered.
	 */
	private describeCard(): NativeNode {
		const mail = this.details.mail;
		if (mail === undefined) return text([span("onlyne", "muted mono")]);
		const messageHref = messagePath(this.details.message_ref);
		const replyHref = mail.in_reply_to === undefined ? undefined : messagePath(mail.in_reply_to);
		const fields: Array<{ k: string; v: TspSpan[] }> = [
			{
				k: t("mail.from"),
				v: [
					span(shortNodeId(mail.from), "link mono", { href: `onlyne://neighbors/${encodeURIComponent(mail.from)}` }),
					span(` <${mail.from}>`, "muted mono"),
				],
			},
			{ k: t("mail.date"), v: [span(mail.at, "mono")] },
			{
				k: t("mail.thread"),
				// Both segments are encoded, so a URN owner stays one segment of the route.
				v: [
					span(`${this.details.thread_ref.owner}/${this.details.thread_ref.id}`, "link mono", {
						href: `onlyne://threads/${encodeURIComponent(this.details.thread_ref.owner)}/${encodeURIComponent(this.details.thread_ref.id)}`,
					}),
					span(` #${this.details.seq}`, "muted mono"),
				],
			},
			...(mail.relation === undefined ? [] : [{ k: t("mail.relation"), v: [span(mail.relation, "mono")] }]),
			...(mail.in_reply_to === undefined || replyHref === undefined
				? []
				: [{
						k: t("mail.inReplyTo"),
						v: [span(`${mail.in_reply_to.sender}/${mail.in_reply_to.id}`, "link mono", { href: replyHref })],
					}]),
			{
				k: t("mail.delivery"),
				v: [
					span(this.status.delivery, this.status.delivery === "received" ? "mono" : "warning mono"),
					...(this.status.consumption === undefined
						? []
						: [span(t("mail.consumed", { state: this.status.consumption }), "mono")]),
				],
			},
			...(this.status.error === undefined
				? []
				: [{ k: t("mail.status"), v: [span(`unknown (${this.status.error})`, "warning mono")] }]),
		];
		return node("card", {
			role: "onlyne.mail",
			head: [span("[onlyne] ", "muted mono"), span(this.heading(), "strong")],
		}, [
			node("kv", { layout: "grid", items: fields.map(field => ({ k: [span(`${field.k}:`, "muted mono")], v: field.v })) }),
			node("rule"),
			node("md", { text: mail.body }),
			node("rule"),
			text([
				span(`${this.label()} · #${this.details.seq} · `, "muted mono"),
				span(t("mail.rawRecord"), "link mono", { href: messageHref }),
				span(" · ", "muted mono"),
				span(t("mail.wholeThread"), "link mono", { href: conversationPath(this.details.message_ref) }),
			], { wrap: "word" }),
		]);
	}

	/** A locale switch rewrites every label, so the frame caches of the old locale are dropped. */
	private dropLocaleCaches(): void {
		const locale = getLocale();
		if (locale === this.locale) return;
		this.locale = locale;
		this.native = undefined;
		this.headers = undefined;
		this.footer = undefined;
		this.rendered = undefined;
	}
}

export const renderMailMessage: MessageRenderer<OnlyneMessageDetails> = (message, _options, theme) => {
	if (message.details?.mail === undefined) return undefined;
	return new MailMessage(message.details, message.customType, theme);
};
