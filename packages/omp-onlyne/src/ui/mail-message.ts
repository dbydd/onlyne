import type { MessageRenderer } from "@oh-my-pi/pi-coding-agent";
import { Markdown, Text, getMarkdownTheme, node, span, text, type Component, type NativeNode, type Theme } from "@oh-my-pi/pi-tui";
import type { Entry } from "@onlyne/lib";
import { shortNodeId } from "../config.ts";

export interface OnlyneMessageDetails {
	thread_ref: Entry["thread_ref"];
	seq: string;
	message_ref: { sender: string; id: string };
	consumption: string;
	mail?: {
		from: string;
		at: string;
		subject: string;
		body: string;
		relation?: "reply" | "followup";
		in_reply_to?: { sender: string; id: string };
	};
}

class MailMessage implements Component {
	private readonly native: NativeNode;
	private readonly headers: Text;
	private readonly body: Markdown;
	private readonly footer: Text;

	constructor(details: OnlyneMessageDetails, kind: string, theme: Theme) {
		const mail = details.mail!;
		const ref = details.thread_ref;
		const href = `onlyne://threads/${encodeURIComponent(ref.id)}/messages/${encodeURIComponent(details.seq)}`;
		const label = kind === "onlyne.supplement" ? "补笺" : kind === "onlyne.routing" ? "转递" : "来信";
		const subject = mail.subject.replace(/[\r\n]+/g, " ");
		const from = shortNodeId(mail.from);
		const heading = `[onlyne] ${mail.relation === "reply" ? "Re: " : ""}${subject}`;
		const fields = [
			{ k: "From", v: `${from} <${mail.from}>` },
			{ k: "Date", v: mail.at },
			{ k: "Thread", v: `${ref.owner}/${ref.id} #${details.seq}` },
			...(mail.relation === undefined ? [] : [{ k: "Relation", v: mail.relation }]),
			...(mail.in_reply_to === undefined ? [] : [{ k: "In-Reply-To", v: `${mail.in_reply_to.sender}/${mail.in_reply_to.id}` }]),
		];
		this.native = node("card", {
			role: "onlyne.mail",
			head: [span("[onlyne] ", "muted mono"), span(`${mail.relation === "reply" ? "Re: " : ""}${subject}`, "strong")],
		}, [
			node("kv", { layout: "grid", items: fields.map(field => ({ k: [span(`${field.k}:`, "muted mono")], v: [span(field.v, "mono")] })) }),
			node("rule"),
			node("md", { text: mail.body }),
			node("rule"),
			text([span(`${label} · #${details.seq} · `, "muted mono"), span("原始记录", "link mono", { href })], { wrap: "word" }),
		]);
		this.headers = new Text([
			theme.fg("accent", theme.bold(heading)),
			...fields.map(field => `${theme.fg("muted", `${field.k}:`)} ${field.v}`),
		].join("\n"), 0, 0);
		this.body = new Markdown(mail.body, 0, 0, getMarkdownTheme());
		this.footer = new Text(theme.fg("muted", `${label} · #${details.seq}\n${href}`), 0, 0);
	}

	describe(): NativeNode { return this.native; }

	render(width: number): readonly string[] {
		const rule = "─".repeat(Math.max(0, Math.min(width, 72)));
		return [...this.headers.render(width), rule, "", ...this.body.render(width), "", rule, ...this.footer.render(width)];
	}

	invalidate(): void {
		this.headers.invalidate();
		this.body.invalidate();
		this.footer.invalidate();
	}
}

export const renderMailMessage: MessageRenderer<OnlyneMessageDetails> = (message, _options, theme) => {
	if (message.details?.mail === undefined) return undefined;
	return new MailMessage(message.details, message.customType, theme);
};
