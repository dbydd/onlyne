import { OnlyneError, type Json } from "@onlyne/lib";
import type { AgyOnlyneConfig, JudgeMode } from "../config.ts";
import type { Answers, Questions } from "../core/questions.ts";
import type { JudgeFn } from "../core/decider.ts";
import { text } from "../core/decider-text.ts";

export interface McpSamplingTarget {
	supportsSampling(): boolean;
	createMessage(params: {
		messages: Array<{ role: "user"; content: { type: "text"; text: string } }>;
		systemPrompt?: string;
		maxTokens: number;
	}, signal?: AbortSignal): Promise<{ content: { type: string; text?: string } }>;
}

export interface JudgeBackendOptions {
	config: () => AgyOnlyneConfig;
	sampling?: McpSamplingTarget;
	customJudge?: JudgeFn;
	env?: Record<string, string | undefined>;
	log?(event: string, fields?: Record<string, unknown>): void;
}

function jsonSchemaFor(questions: Questions): Record<string, unknown> {
	const properties: Record<string, unknown> = {};
	const required: string[] = [];
	for (const [qName, question] of Object.entries(questions)) {
		required.push(qName);
		const keys = Object.keys(question.criteria);
		properties[qName] = {
			type: "object",
			properties: {
				choice: { type: "string", enum: keys },
				confidence: { type: "string" },
				text: { type: "string" },
			},
			required: ["choice"],
			additionalProperties: false,
		};
	}
	return {
		type: "object",
		properties,
		required,
		additionalProperties: false,
	};
}

function buildPrompt(state: Json, questions: Questions): { system: string; user: string } {
	const system = [
		"You are Onlyne SystemOne: a strict JSON choice classifier.",
		"Pick one exact key from each question's criteria.",
		"Return strictly valid JSON mapping each question name to {\"choice\": \"<exact_key>\"}. No prose or markdown.",
	].join(" ");
	const user = JSON.stringify({ state, questions }, null, 2);
	return { system, user };
}

function parseJsonObject(raw: string): Record<string, unknown> {
	const trimmed = raw.trim();
	const fenced = /^```(?:json)?\s*([\s\S]*?)```$/i.exec(trimmed);
	const body = fenced ? fenced[1]!.trim() : trimmed;
	const firstBrace = body.indexOf("{");
	const lastBrace = body.lastIndexOf("}");
	const slice = firstBrace >= 0 && lastBrace > firstBrace ? body.slice(firstBrace, lastBrace + 1) : body;
	const parsed = JSON.parse(slice);
	if (parsed === null || typeof parsed !== "object" || Array.isArray(parsed)) {
		throw new Error("Judge output is not a JSON object");
	}
	return parsed as Record<string, unknown>;
}

function validateAnswers(raw: Record<string, unknown>, questions: Questions): Answers {
	const out: Answers = {};
	for (const [qName, question] of Object.entries(questions)) {
		const validKeys = Object.keys(question.criteria);
		const entry = raw[qName];
		const chosen = typeof entry === "string"
			? entry.trim()
			: entry !== null && typeof entry === "object" && !Array.isArray(entry) && typeof (entry as { choice?: unknown }).choice === "string"
				? ((entry as { choice: string }).choice).trim()
				: undefined;
		if (chosen === undefined || !validKeys.includes(chosen)) {
			throw new Error(`Invalid judge choice for '${qName}': ${String(chosen)}`);
		}
		const rec = (entry !== null && typeof entry === "object" ? entry : {}) as { text?: unknown; confidence?: unknown };
		out[qName] = {
			choice: chosen,
			...(typeof rec.text === "string" ? { text: rec.text } : {}),
			...(typeof rec.confidence === "string" ? { confidence: rec.confidence } : {}),
		};
	}
	return out;
}

async function runSampling(sampling: McpSamplingTarget, state: Json, questions: Questions, signal?: AbortSignal): Promise<Answers> {
	const { system, user } = buildPrompt(state, questions);
	const result = await sampling.createMessage({
		systemPrompt: system,
		messages: [{ role: "user", content: { type: "text", text: user } }],
		maxTokens: 512,
	}, signal);
	if (result.content.type !== "text" || typeof result.content.text !== "string") {
		throw new Error("Sampling returned non-text content");
	}
	return validateAnswers(parseJsonObject(result.content.text), questions);
}

async function runOpenAiHttp(
	cfg: NonNullable<AgyOnlyneConfig["judge"]>,
	env: Record<string, string | undefined>,
	state: Json,
	questions: Questions,
	signal?: AbortSignal,
): Promise<Answers> {
	const apiKey = cfg.api_key
		?? (cfg.api_key_env ? env[cfg.api_key_env] : undefined)
		?? env.OPENAI_API_KEY;
	const baseUrl = (cfg.base_url ?? env.ONLYNE_JUDGE_URL ?? "https://api.openai.com/v1").replace(/\/+$/, "");
	if (!apiKey && !cfg.base_url && !env.ONLYNE_JUDGE_URL) {
		throw new Error("OPENAI_API_KEY or judge.base_url not configured");
	}
	const model = cfg.model ?? env.ONLYNE_JUDGE_MODEL ?? "gpt-4o-mini";
	const { system, user } = buildPrompt(state, questions);
	const timeoutMs = cfg.timeout_ms ?? 15_000;
	const ctrl = new AbortController();
	const timer = setTimeout(() => ctrl.abort(new Error("judge timeout")), timeoutMs);
	if (signal) signal.addEventListener("abort", () => ctrl.abort(signal.reason), { once: true });
	try {
		const res = await fetch(`${baseUrl}/chat/completions`, {
			method: "POST",
			headers: {
				"content-type": "application/json",
				...(apiKey ? { authorization: `Bearer ${apiKey}` } : {}),
			},
			body: JSON.stringify({
				model,
				temperature: 0,
				response_format: { type: "json_object" },
				messages: [
					{ role: "system", content: system },
					{ role: "user", content: user },
				],
			}),
			signal: ctrl.signal,
		});
		if (!res.ok) throw new Error(`HTTP ${res.status}: ${await res.text()}`);
		const json = await res.json() as { choices?: Array<{ message?: { content?: string } }> };
		const content = json.choices?.[0]?.message?.content;
		if (!content) throw new Error("Empty OpenAI completion response");
		return validateAnswers(parseJsonObject(content), questions);
	} finally {
		clearTimeout(timer);
	}
}

async function runGeminiHttp(
	cfg: NonNullable<AgyOnlyneConfig["judge"]>,
	env: Record<string, string | undefined>,
	state: Json,
	questions: Questions,
	signal?: AbortSignal,
): Promise<Answers> {
	const apiKey = cfg.api_key
		?? (cfg.api_key_env ? env[cfg.api_key_env] : undefined)
		?? env.GEMINI_API_KEY
		?? env.GOOGLE_API_KEY;
	if (!apiKey) throw new Error("GEMINI_API_KEY not configured");
	const model = cfg.model ?? env.ONLYNE_JUDGE_MODEL ?? "gemini-2.5-flash";
	const base = (cfg.base_url ?? "https://generativelanguage.googleapis.com/v1beta").replace(/\/+$/, "");
	const { system, user } = buildPrompt(state, questions);
	const timeoutMs = cfg.timeout_ms ?? 15_000;
	const ctrl = new AbortController();
	const timer = setTimeout(() => ctrl.abort(new Error("judge timeout")), timeoutMs);
	if (signal) signal.addEventListener("abort", () => ctrl.abort(signal.reason), { once: true });
	try {
		const res = await fetch(`${base}/models/${encodeURIComponent(model)}:generateContent?key=${encodeURIComponent(apiKey)}`, {
			method: "POST",
			headers: { "content-type": "application/json" },
			body: JSON.stringify({
				systemInstruction: { parts: [{ text: system }] },
				contents: [{ role: "user", parts: [{ text: user }] }],
				generationConfig: {
					temperature: 0,
					responseMimeType: "application/json",
				},
			}),
			signal: ctrl.signal,
		});
		if (!res.ok) throw new Error(`HTTP ${res.status}: ${await res.text()}`);
		const json = await res.json() as { candidates?: Array<{ content?: { parts?: Array<{ text?: string }> } }> };
		const content = json.candidates?.[0]?.content?.parts?.[0]?.text;
		if (!content) throw new Error("Empty Gemini completion response");
		return validateAnswers(parseJsonObject(content), questions);
	} finally {
		clearTimeout(timer);
	}
}

async function runAgyCli(
	cfg: NonNullable<AgyOnlyneConfig["judge"]>,
	state: Json,
	questions: Questions,
	signal?: AbortSignal,
): Promise<Answers> {
	const agyExe = Bun.which("agy");
	if (agyExe === null) throw new Error("agy CLI not found on PATH");
	const { system, user } = buildPrompt(state, questions);
	const schema = JSON.stringify(jsonSchemaFor(questions));
	const model = cfg.model ?? "gemini-3.8-flash-low";
	const timeoutMs = cfg.timeout_ms ?? 20_000;
	const prompt = `${system}\n\n${user}`;
	const proc = Bun.spawn([
		agyExe,
		"--output-format", "json",
		"--disable-slash-commands",
		"--model", model,
		"--json-schema", schema,
		`-p=${prompt}`,
	], { stdout: "pipe", stderr: "pipe" });

	const timer = setTimeout(() => {
		try {
			proc.kill();
		} catch {
			// Ignore.
		}
	}, timeoutMs);
	const onAbort = (): void => {
		try {
			proc.kill();
		} catch {
			// Ignore.
		}
	};
	signal?.addEventListener("abort", onAbort, { once: true });
	try {
		const [stdout, stderr, exitCode] = await Promise.all([
			new Response(proc.stdout).text(),
			new Response(proc.stderr).text(),
			proc.exited,
		]);
		const parsed = parseJsonObject(stdout);
		if (parsed.status === "ERROR" || exitCode !== 0) {
			throw new Error(String(parsed.error ?? (stderr.trim() || `agy exit ${exitCode}`)));
		}
		const structured = parsed.structured_output;
		if (structured !== null && typeof structured === "object" && !Array.isArray(structured)) {
			return validateAnswers(structured as Record<string, unknown>, questions);
		}
		if (typeof parsed.response === "string") {
			return validateAnswers(parseJsonObject(parsed.response), questions);
		}
		throw new Error("agy CLI returned no structured output");
	} finally {
		clearTimeout(timer);
		signal?.removeEventListener("abort", onAbort);
	}
}

export function createAntigravityJudge(options: JudgeBackendOptions): JudgeFn {
	const env = options.env ?? process.env;
	return async (state, questions, signal) => {
		if (options.customJudge !== undefined) {
			return options.customJudge(state, questions, signal);
		}
		const cfg = options.config().judge ?? {};
		const mode: JudgeMode = cfg.mode ?? (env.ONLYNE_JUDGE_MODE as JudgeMode | undefined) ?? "auto";
		if (mode === "none") {
			throw new OnlyneError("decision_unavailable", text("judge.unavailable", { reason: "judge.mode = none" }));
		}

		const errors: string[] = [];
		const tryBackend = async (name: string, fn: () => Promise<Answers>): Promise<Answers | undefined> => {
			try {
				return await fn();
			} catch (err) {
				const reason = err instanceof Error ? err.message : String(err);
				errors.push(`${name}: ${reason}`);
				options.log?.("onlyne.judge.backend_failed", { backend: name, error: reason });
				return undefined;
			}
		};

		if ((mode === "sampling" || mode === "auto") && options.sampling?.supportsSampling()) {
			const hit = await tryBackend("sampling", () => runSampling(options.sampling!, state, questions, signal));
			if (hit !== undefined) return hit;
		}
		if (mode === "openai" || (mode === "auto" && (cfg.base_url || env.ONLYNE_JUDGE_URL || env.OPENAI_API_KEY))) {
			const hit = await tryBackend("openai", () => runOpenAiHttp(cfg, env, state, questions, signal));
			if (hit !== undefined) return hit;
		}
		if (mode === "gemini" || (mode === "auto" && (env.GEMINI_API_KEY || env.GOOGLE_API_KEY || cfg.api_key))) {
			const hit = await tryBackend("gemini", () => runGeminiHttp(cfg, env, state, questions, signal));
			if (hit !== undefined) return hit;
		}
		if (mode === "agy-cli" || mode === "auto") {
			const hit = await tryBackend("agy-cli", () => runAgyCli(cfg, state, questions, signal));
			if (hit !== undefined) return hit;
		}

		throw new OnlyneError("decision_unavailable", text("judge.unavailable", {
			reason: errors.length > 0 ? errors.join("; ") : `mode=${mode} unavailable`,
		}));
	};
}
