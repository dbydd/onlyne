import { networkInterfaces } from "node:os";

/** The optional DNS-SD module. The package is not installed by default. */
interface BonjourService {
	fqdn: string;
	name: string;
	port: number;
	host?: string;
	addresses?: string[];
	txt: unknown;
	referer?: { address?: string };
	records(): Array<{ type: string }>;
}
interface BonjourBrowser {
	on(event: "up" | "srv-update" | "txt-update" | "down", listener: (service: BonjourService) => void): void;
	stop(): void;
}
interface BonjourInstance {
	find(options: { type: string; protocol: string }): BonjourBrowser;
	publish(options: { name: string; type: string; protocol: string; port: number; host: string; probe: boolean; txt: Record<string, string> }): BonjourService;
	unpublishAll(callback: () => void): void;
	destroy(callback: () => void): void;
}
type BonjourConstructor = new (options: object, error: (error: unknown) => void) => BonjourInstance;

/** RFC 0011 §5.1: the one service type every HTTP binding publishes. */
const SERVICE_TYPE = "onlyne";
const SERVICE_PROTOCOL = "tcp";
const WIRE_PROTOCOL = "onlyne/3";
const SERVICE_PATH_SUFFIX = "/onlyne/v3";
const SOURCE = "mdns";

export interface NetworkDiscoveryOptions {
	node_id: string;
	/** Identifies this process instance in the service name and TXT record. */
	instance_id: string;
	/** Absolute URL the node is listening on; its port is the allocated one. */
	endpoint: string;
	/** Local listener address; a wildcard binding advertises a concrete interface. */
	bind: string;
	/** `expectedNode` is the identity the advertisement claims, not a verified fact. */
	onCandidate(url: string, source: string, expectedNode?: string): Promise<void>;
	onError(error: unknown): void;
	/** A candidate that was delivered earlier is no longer advertised. */
	onUnavailable?(url: string): void;
}

/**
 * Same-link discovery over DNS-SD/mDNS: publishes `_onlyne._tcp.local` and
 * offers every advertisement to the library, which owns identity resolution
 * and membership. The browser keeps other regions' candidates; only the
 * HTTP declaration decides region.
 */
export class NetworkDiscovery {
	readonly #options: NetworkDiscoveryOptions;
	#bonjour: BonjourInstance | undefined;
	#browser: BonjourBrowser | undefined;
	#service: BonjourService | undefined;
	#started = false;
	#closed: Promise<void> | undefined;
	#closing = false;
	/** Deliveries in flight, keyed by URL, so repeated advertisements run once. */
	#inflight = new Map<string, Promise<boolean>>();
	/** Delivered URL per service fqdn; a change or a goodbye withdraws the old one. */
	#delivered = new Map<string, string>();

	constructor(options: NetworkDiscoveryOptions) {
		this.#options = options;
	}

	async start(): Promise<void> {
		if (this.#started) return;
		this.#closed = undefined;
		this.#closing = false;
		const publication = plan(this.#options);
		let module: { default?: BonjourConstructor } | BonjourConstructor;
		try {
			module = await import("bonjour-service") as unknown as { default?: BonjourConstructor } | BonjourConstructor;
		} catch {
			throw new Error("Optional DNS-SD discovery is unavailable. Install bonjour-service to enable it; HTTP seed discovery still works.");
		}
		const Constructor = typeof module === "function" ? module : module.default;
		if (Constructor === undefined) throw new Error("bonjour-service does not export a constructor.");
		const bonjour = new Constructor({}, (error: unknown) => this.#fail(error));
		try {
			const browser = bonjour.find({ type: SERVICE_TYPE, protocol: SERVICE_PROTOCOL });
			browser.on("up", service => this.#consider(service));
			browser.on("srv-update", service => this.#consider(service));
			browser.on("txt-update", service => this.#consider(service));
			browser.on("down", service => this.#withdraw(service));
			this.#bonjour = bonjour;
			this.#browser = browser;

			// A probe keeps the first `records()` call asynchronous, which is what lets
			// the loopback filter below take effect before the announcement is sent.
			const service = bonjour.publish({
				name: publication.name,
				type: SERVICE_TYPE,
				protocol: SERVICE_PROTOCOL,
				port: publication.port,
				host: publication.host,
				probe: true,
				txt: publication.txt,
			});
			if (isLoopback(publication.host)) {
				// bonjour-service attaches every interface address to the SRV target. A
				// loopback binding is reachable only from this host, so publishing those
				// addresses (none of them loopback) would advertise a dead endpoint.
				const records = service.records.bind(service);
				service.records = () => records().filter(record => record.type !== "A" && record.type !== "AAAA");
			}
			this.#service = service;
			this.#started = true;
		} catch (error) {
			bonjour.destroy(() => undefined);
			throw error;
		}
	}

	async close(): Promise<void> {
		if (this.#closed !== undefined) return this.#closed;
		const bonjour = this.#bonjour;
		this.#closing = true;
		this.#started = false;
		this.#browser?.stop();
		this.#browser = undefined;
		this.#service = undefined;
		this.#bonjour = undefined;
		this.#inflight.clear();
		this.#delivered.clear();
		if (bonjour === undefined) {
			this.#closed = Promise.resolve();
			return this.#closed;
		}
		this.#closed = new Promise<void>(resolve => {
			bonjour.unpublishAll(() => bonjour.destroy(() => resolve()));
		});
		return this.#closed;
	}

	#consider(service: BonjourService): void {
		if (this.#closing) return;
		if (service.fqdn === this.#service?.fqdn) return;
		const txt = text(service);
		const node = txt["node_id"];
		const instance = txt["instance_id"];
		if (node === this.#options.node_id && instance === this.#options.instance_id) return;

		const protocol = txt["protocol"];
		if (protocol !== WIRE_PROTOCOL) {
			this.#fail(new Error(`Service ${service.name} does not speak ${WIRE_PROTOCOL}.`));
			return;
		}
		if (node === undefined || instance === undefined) {
			this.#fail(new Error(`Service ${service.name} is missing its node_id or instance_id.`));
			return;
		}
		const scheme = txt["scheme"];
		if (scheme !== "http" && scheme !== "https") {
			this.#fail(new Error(`Service ${service.name} advertises scheme ${scheme ?? "(missing)"}.`));
			return;
		}
		const path = txt["path"];
		if (path === undefined || !normalizePath(path).endsWith(SERVICE_PATH_SUFFIX)) {
			this.#fail(new Error(`Service ${service.name} does not advertise a ${SERVICE_PATH_SUFFIX} path.`));
			return;
		}
		if (!Number.isInteger(service.port) || service.port < 1 || service.port > 65_535) {
			this.#fail(new Error(`Service ${service.name} advertises port ${String(service.port)}.`));
			return;
		}

		const host = candidateHost(service);
		if (host === undefined) {
			this.#fail(new Error(`Service ${service.name} advertises no address reachable from this host.`));
			return;
		}
		if (!isLoopback(host) && scheme !== "https") {
			this.#fail(new Error(`Service ${service.name} advertises plain HTTP across hosts; Onlyne requires HTTPS there.`));
			return;
		}

		const url = `${scheme}://${host.includes(":") ? `[${host}]` : host}:${String(service.port)}${normalizePath(path)}`;
		const previous = this.#delivered.get(service.fqdn);
		if (previous === url) return;
		if (previous !== undefined) {
			this.#delivered.delete(service.fqdn);
			this.#options.onUnavailable?.(previous);
		}
		void this.#deliver(service.fqdn, url, node);
	}

	#deliver(fqdn: string, url: string, expectedNode: string): Promise<boolean> {
		const running = this.#inflight.get(url);
		if (running !== undefined) return running;
		const attempt = this.#options
			.onCandidate(url, SOURCE, expectedNode)
			.then(() => true)
			.catch((error: unknown) => {
				this.#fail(error);
				return false;
			});
		const tracked = attempt.finally(() => {
			if (this.#inflight.get(url) === tracked) this.#inflight.delete(url);
		});
		tracked.then(delivered => {
			if (delivered && !this.#closing && !this.#delivered.has(fqdn)) this.#delivered.set(fqdn, url);
		});
		this.#inflight.set(url, tracked);
		return tracked;
	}

	#withdraw(service: BonjourService): void {
		const url = this.#delivered.get(service.fqdn);
		if (url === undefined) return;
		this.#delivered.delete(service.fqdn);
		this.#options.onUnavailable?.(url);
	}

	#fail(error: unknown): void {
		if (this.#closing) return;
		try {
			this.#options.onError(error);
		} catch {
			// The error sink is the end of the chain; a throwing sink must not kill the mDNS loop.
		}
	}
}

interface Publication {
	name: string;
	host: string;
	port: number;
	txt: Record<string, string>;
}

/** Validate everything the advertisement needs before the socket exists. */
function plan(options: NetworkDiscoveryOptions): Publication {
	if (options.node_id.length === 0) throw new Error("Onlyne discovery needs a node id.");
	if (options.instance_id.length === 0) throw new Error("Onlyne discovery needs an instance id.");
	let endpoint: URL;
	try {
		endpoint = new URL(options.endpoint);
	} catch {
		throw new Error(`The Onlyne endpoint must be an absolute URL, got ${options.endpoint}.`);
	}
	if (endpoint.protocol !== "http:" && endpoint.protocol !== "https:") {
		throw new Error(`The Onlyne endpoint must use http or https, got ${endpoint.protocol}.`);
	}
	const scheme = endpoint.protocol.slice(0, -1);
	const path = servicePath(endpoint.pathname);
	const port = endpoint.port === "" ? (scheme === "https" ? 443 : 80) : Number(endpoint.port);
	if (!Number.isInteger(port) || port < 1 || port > 65_535) {
		throw new Error(`The Onlyne endpoint must name the listening port, got ${options.endpoint}.`);
	}
	const advertised = normalizeHost(endpoint.hostname);
	if (advertised === undefined) throw new Error(`The Onlyne endpoint has no host, got ${options.endpoint}.`);
	// A wildcard binding is not a connectable address; publish the interface it
	// listens on instead, and never the wildcard itself.
	const host = isWildcard(advertised) ? interfaceAddress(options.bind) : advertised;
	return {
		name: options.instance_id,
		host,
		port,
		txt: {
			protocol: WIRE_PROTOCOL,
			path,
			scheme,
			node_id: options.node_id,
			instance_id: options.instance_id,
		},
	};
}

/** The advertised address of a wildcard binding: its bind when concrete, else a live interface. */
function interfaceAddress(bind: string): string {
	const concrete = normalizeHost(bind);
	if (concrete !== undefined && !isWildcard(concrete)) return concrete;
	const ipv4: string[] = [];
	const ipv6: string[] = [];
	for (const entries of Object.values(networkInterfaces())) {
		for (const entry of entries ?? []) {
			if (entry.internal) continue;
			const address = normalizeAddress(entry.address);
			if (address === undefined || address.startsWith("fe80:") || address.startsWith("169.254.")) continue;
			(address.includes(":") ? ipv6 : ipv4).push(address);
		}
	}
	const chosen = ipv4[0] ?? ipv6[0];
	if (chosen === undefined) {
		throw new Error("No interface address can be advertised; set server.bind to a concrete address.");
	}
	return chosen;
}

/**
 * The address this host can use to reach the advertised service. Addresses that
 * this host already owns are only applicable when the packet came from this
 * host: a remote service cannot own them, and a remote loopback is unreachable.
 */
function candidateHost(service: BonjourService): string | undefined {
	const local = localAddresses();
	const referer = normalizeHost(service.referer?.address);
	const originLocal = referer !== undefined && (isLoopback(referer) || local.has(referer));
	for (const raw of service.addresses ?? []) {
		const address = normalizeAddress(raw);
		if (address === undefined || isWildcard(address)) continue;
		if (address.startsWith("fe80:") || address.startsWith("169.254.")) continue;
		if (address.startsWith("ff") || /^(22[4-9]|23\d)\./.test(address)) continue;
		if (isLoopback(address)) {
			if (originLocal) return address;
			continue;
		}
		if (local.has(address) && !originLocal) continue;
		return address;
	}
	const target = normalizeHost(service.host);
	if (target === undefined || isWildcard(target)) return undefined;
	if (isLoopback(target)) return originLocal ? target : undefined;
	return target;
}

function localAddresses(): Set<string> {
	const hosts = new Set<string>(["127.0.0.1", "::1", "localhost"]);
	for (const entries of Object.values(networkInterfaces())) {
		for (const entry of entries ?? []) {
			const address = normalizeAddress(entry.address);
			if (address !== undefined) hosts.add(address);
		}
	}
	return hosts;
}

function text(service: BonjourService): Record<string, string> {
	const fields: Record<string, string> = {};
	const raw: unknown = service.txt;
	if (raw === null || typeof raw !== "object") return fields;
	for (const [key, value] of Object.entries(raw as Record<string, unknown>)) {
		if (typeof value === "string") fields[key] = value;
		else if (Array.isArray(value) && typeof value[0] === "string") fields[key] = value[0];
	}
	return fields;
}

function servicePath(pathname: string): string {
	const path = normalizePath(pathname);
	if (!path.endsWith(SERVICE_PATH_SUFFIX)) {
		throw new Error(`The Onlyne endpoint path must end with ${SERVICE_PATH_SUFFIX}, got ${pathname}.`);
	}
	return path;
}

function normalizePath(pathname: string): string {
	const absolute = pathname.startsWith("/") ? pathname : `/${pathname}`;
	const trimmed = absolute.replace(/\/+$/, "");
	return trimmed === "" ? "/" : trimmed;
}

function normalizeAddress(value: string): string | undefined {
	let address = value.trim();
	if (address.startsWith("[") && address.endsWith("]")) address = address.slice(1, -1);
	const zone = address.indexOf("%");
	if (zone >= 0) address = address.slice(0, zone);
	return address.length === 0 ? undefined : address.toLowerCase();
}

function normalizeHost(value: string | undefined): string | undefined {
	if (value === undefined) return undefined;
	const host = normalizeAddress(value);
	if (host === undefined) return undefined;
	return host.endsWith(".") ? host.slice(0, -1) : host;
}

function isLoopback(host: string): boolean {
	return host === "localhost" || host.endsWith(".localhost") || host === "::1"
		|| host === "0:0:0:0:0:0:0:1" || host.startsWith("127.");
}

function isWildcard(host: string): boolean {
	return host === "0.0.0.0" || host === "::" || host === "0:0:0:0:0:0:0:0";
}

