--!strict
-- The `board` block: one board over every local Onlyne cluster. It lists the
-- server roots configured in `tern.kv` ("roots") plus every live server it
-- discovers through `onlyne ls --json`, and polls each root's admin surface
-- with four one-shot reads (`status`, `roles`, `sessions`, `faults --open-only`).
--
-- Tern 0.4.5's process API captures a child's whole output and cannot stream
-- `onlyne watch --follow`, so the board re-runs the reads on a fixed cadence
-- instead of following the event stream. A failed read never drops the data it
-- replaces: the last good answer per section stays on the board with the
-- problem written under the root.
--
-- Actions are the CLI's own supervisor verbs, run as one-shot processes.
-- Focus goes through `onlyne control focus` — never a pane id, which the
-- daemon does not own. Every mutating act is armed with one key press and
-- runs only when the same key (or Enter, or the Confirm badge) repeats it.

local ui = tern.ui

local POLL_MS = 10 * 1000
local RUN_TIMEOUT_MS = 8 * 1000
local PRUNE_GRACE_MS = 2000
local DEFAULT_SENDER = "_supervisor" -- the reserved operator role (onlyne_net::OPERATOR_ROLE)
local ACTION_REASON = "requested from the Onlyne Board block in Tern"
local ACK_REASON = "acknowledged from the Onlyne Board block in Tern"

-- `onlyne` by name first, then where package managers put it: a daemon started
-- by launchd or systemd often has a PATH without those directories.
local ONLYNE = {
	"onlyne",
	"/opt/homebrew/bin/onlyne",
	"/usr/local/bin/onlyne",
	"/usr/bin/onlyne",
	"/home/linuxbrew/.linuxbrew/bin/onlyne",
}

local READS = { "status", "roles", "sessions", "faults" }

-- Key -> control op for a session row, and for a fault row. recycle and cancel
-- carry --reason; focus, probe and snapshot refuse one (the CLI checks both).
local SESSION_KEYS: { [string]: string } = {
	f = "focus",
	p = "probe",
	t = "snapshot",
	r = "recycle",
	x = "cancel",
}
local FAULT_KEYS: { [string]: string } = {
	a = "ack",
	f = "focus",
	r = "retry",
}

type Json = any

-- Everything one root shows. The four answer arrays keep their last good
-- content across failed cycles; `problem` says what failed last.
type RootData = {
	root: string,
	-- "config" (the kv list) or "discovered" (live `onlyne ls`)
	source: string,
	-- whether this cycle's sources still name the root; false roots keep their
	-- data on the board until the block closes, marked gone
	live: boolean,
	cluster: string?,
	version: string?,
	roles: { Json }?,
	sessions: { Json }?,
	faults: { Json }?,
	fetched: number?, -- os.time() of the last successful read
	problem: string?,
	busy: boolean, -- a read of this root is in flight
}

-- An armed action. `key` is its identity: the second press must re-derive the
-- same key from the row under the cursor, so a poll that reshuffled the board
-- turns a confirm into a fresh arm instead of running the wrong target.
type Armed = {
	key: string,
	verb: string,
	label: string,
	root: string,
	task: string?,
	fault_id: number?,
}

-- One open block. The answer data is shared at module level, so two boards
-- cost one poll loop, not two.
type Block = {
	cx: BlockCx,
	since: number, -- tern.now() of init, for the prune grace window
	cursor: number, -- index into rows_of()
	armed: Armed?,
	note: string?, -- last action's result line
}

-- One selectable row: a session or an open fault, at its index inside its
-- root's array. Keys and the view both go through rows_of(), so the cursor
-- and the › mark column always describe the same row.
type Row = {
	root: string,
	kind: string, -- "session" | "fault"
	index: number,
}

local data: {
	order: { string },
	by_root: { [string]: RootData },
	problem: string?, -- board-level trouble (onlyne missing, ls failed)
	busy: boolean,
	updated: number?, -- os.time() when the last full cycle ended
} = {
	order = {},
	by_root = {},
	problem = nil,
	busy = false,
	updated = nil,
}

local blocks: { [number]: Block } = {}
local timer: TimerHandle? = nil
local chain_busy = false
local chain_generation = 0
local queue: { { root: string, verb: string } } = {}

local refresh: () -> ()
local schedule: () -> ()
local start_cycle: () -> ()
local next_step: (number) -> ()

-- Small helpers.

local function tilde(path: string): string
	local home = tern.getenv("HOME")
	if home and string.sub(path, 1, #home + 1) == home .. "/" then
		return "~" .. string.sub(path, #home + 1)
	end
	return path
end

local function expand_home(path: string): string
	if string.sub(path, 1, 1) == "~" then
		local home = tern.getenv("HOME")
		if home then
			return home .. string.sub(path, 2)
		end
	end
	return path
end

-- One canonical spelling per root: a trailing slash would split one root into
-- two entries.
local function normalize_root(path: string): string
	local clean = string.gsub(path, "/+$", "")
	if clean == "" then
		return "/"
	end
	return clean
end

local function kv_get(key: string): Json
	local ok, value = pcall(tern.kv.get, key)
	return if ok then value else nil
end

local function configured_roots(): { string }
	local raw = kv_get("roots")
	local out: { string } = {}
	if type(raw) == "table" then
		for _, item in raw do
			if type(item) == "string" and item ~= "" then
				table.insert(out, normalize_root(item))
			end
		end
	end
	return out
end

local function remember_roots(roots: { string })
	local ok = pcall(tern.kv.set, "roots", roots)
	if not ok then
		tern.log.warn("onlyne: could not write the roots list to kv")
	end
end

local function sender(): string
	local value = kv_get("sender")
	return if type(value) == "string" and value ~= "" then value else DEFAULT_SENDER
end

-- Seconds since the epoch of an ISO 8601 UTC time (`2026-10-05T12:00:00Z`),
-- by the days-from-civil algorithm: `os.time` with a table reads local time.
local function epoch(iso: string): number?
	local y, m, d, hh, mm, ss = string.match(iso, "^(%d+)-(%d+)-(%d+)T(%d+):(%d+):(%d+)")
	if not (y and m and d and hh and mm and ss) then
		return nil
	end
	local year, month, day = tonumber(y) :: number, tonumber(m) :: number, tonumber(d) :: number
	if month <= 2 then
		year -= 1
	end
	local era = year // 400
	local yoe = year - era * 400
	local doy = (153 * (if month > 2 then month - 3 else month + 9) + 2) // 5 + day - 1
	local doe = yoe * 365 + yoe // 4 - yoe // 100 + doy
	local days = era * 146097 + doe - 719468
	return days * 86400 + (tonumber(hh) :: number) * 3600 + (tonumber(mm) :: number) * 60 + (tonumber(ss) :: number)
end

-- How long ago `t` was, compactly: `45s`, `12m`, `5h`, `3d`.
local function ago(t: number): string
	local s = math.max(0, os.time() - t)
	if s < 60 then
		return s .. "s"
	elseif s < 3600 then
		return (s // 60) .. "m"
	elseif s < 86400 then
		return (s // 3600) .. "h"
	end
	return (s // 86400) .. "d"
end

-- The age string of an RFC 3339 timestamp, when one parses.
local function age_of(value: Json): string?
	if type(value) ~= "string" then
		return nil
	end
	local t = epoch(value)
	return if t then ago(t) else nil
end

-- A uuid cut to the width a row shows; ids are opaque, so a prefix is all a
-- person reads anyway.
local function short_id(value: Json): string
	if type(value) ~= "string" or value == "" then
		return "-"
	end
	return if #value > 8 then string.sub(value, 1, 8) .. "…" else value
end

local function field_text(value: Json): string
	if value == nil then
		return ""
	end
	return tostring(value)
end

local function field_number(value: Json): number
	return tonumber(value) or 0
end

local function first_lines(text: string, n: number): string
	local out = {}
	for line in string.gmatch(text, "[^\n]+") do
		table.insert(out, line)
		if #out == n then
			break
		end
	end
	return table.concat(out, "\n")
end

local function muted_line(text: string): Node
	return ui.text({ ui.span(text, "muted") })
end

-- Sets a node's `role` so onlyne.css can restyle it without reaching into the
-- theme's own classes.
local function with_role(node: Node, role: string): Node
	local p: { [string]: any } = node.p or {}
	p.role = role
	node.p = p
	return node
end

local function action_button(label: string, act: string, tone: string): Node
	local node = ui.badge(label, tone)
	local p: { [string]: any } = node.p or {}
	p.actions = { click = act }
	return with_role(node, "onlyne.act." .. act)
end

-- Running the CLI.

-- Starts `onlyne` with `args`, trying each place it may be installed; returns
-- the spawn error of the last attempt when none starts, else nil.
local function run_onlyne(args: { string }, done: (ProcessResult) -> ()): string?
	local last = "the onlyne CLI is not on this machine's PATH"
	for _, bin in ONLYNE do
		if bin == "onlyne" or tern.fs.exists(bin) then
			local argv: { string } = { bin }
			for _, a in args do
				table.insert(argv, a)
			end
			local opts: ProcessOptions = {
				timeout_ms = RUN_TIMEOUT_MS,
				env = { NO_COLOR = "1" },
			}
			local ok, err = pcall(function()
				tern.process.run(argv, opts, function(result: ProcessResult, _)
					done(result)
				end)
			end)
			if ok then
				return nil
			end
			last = tostring(err)
		end
	end
	return last
end

-- The four one-shot admin reads. `--quiet` keeps the answer's data payload
-- alone, which is the part the board reads; the exit code says whether the
-- body was ok.
local function read_args(root: string, verb: string): { string }
	local args: { string } = {
		"--server-root", root,
		verb,
		"--quiet",
		"--timeout", tostring(RUN_TIMEOUT_MS),
	}
	if verb == "faults" then
		table.insert(args, "--open-only")
	end
	return args
end

-- The supervisor gate's two flags name this a maintenance command from
-- outside a session; the admin surface requires `--from`, and the reserved
-- operator role always resolves.
local function control_argv(root: string, verb: string, task: string): { string }
	local args: { string } = {
		"--server-root", root,
		"--from", sender(),
		"control",
		"--task", task,
	}
	if verb == "recycle" or verb == "cancel" then
		table.insert(args, verb)
		table.insert(args, "--reason")
		table.insert(args, ACTION_REASON)
	else
		table.insert(args, verb)
	end
	table.insert(args, "--force")
	table.insert(args, "--yes-i-am-supervisor-not-other-role")
	return args
end

-- The repair family is its own verb on the admin surface and carries no
-- supervisor flags.
local function repair_argv(root: string, verb: string, task: string?, fault_id: number?): { string }
	local args: { string } = { "--server-root", root, "repair" }
	if verb == "ack" then
		table.insert(args, "ack")
		table.insert(args, "--fault-id")
		table.insert(args, string.format("%d", fault_id or 0))
		table.insert(args, "--reason")
		table.insert(args, ACK_REASON)
	else
		table.insert(args, verb) -- retry
		table.insert(args, "--task")
		table.insert(args, task or "")
		table.insert(args, "--reason")
		table.insert(args, ACTION_REASON)
	end
	return args
end

-- Root registry.

local function root_state(root: string, source: string): RootData
	local existing = data.by_root[root]
	if existing then
		return existing
	end
	local fresh: RootData = {
		root = root,
		source = source,
		live = true,
		cluster = nil,
		version = nil,
		roles = nil,
		sessions = nil,
		faults = nil,
		fetched = nil,
		problem = nil,
		busy = false,
	}
	data.by_root[root] = fresh
	return fresh
end

-- Rebuilds the root order from the kv list and what `onlyne ls` reports.
-- Configured roots lead in their stored order; discovered ones follow sorted.
-- A root that left both sources keeps its place and its data, marked gone.
local function rebuild_roots(discovered: { string })
	local wanted: { { root: string, source: string } } = {}
	local seen: { [string]: boolean } = {}
	local function add(root: string, source: string)
		if seen[root] then
			return
		end
		seen[root] = true
		table.insert(wanted, { root = root, source = source })
	end
	for _, root in configured_roots() do
		add(root, "config")
	end
	local live_roots = table.clone(discovered)
	table.sort(live_roots)
	for _, root in live_roots do
		add(root, "discovered")
	end

	local order: { string } = {}
	for _, item in wanted do
		local rd = root_state(item.root, item.source)
		rd.live = true
		rd.source = item.source
		table.insert(order, item.root)
	end
	for _, root in data.order do
		if not seen[root] then
			local rd = data.by_root[root]
			if rd then
				rd.live = false
				rd.problem = "left the runtime directory; showing the last good read"
				table.insert(order, root)
			end
		end
	end
	data.order = order
end

-- Discovery: every live server registration on this machine. `onlyne ls`
-- reports liveness as two facts — the socket file present, the pid answering —
-- and only roots with both are auto-added; a configured root is polled
-- regardless.
local function discover(done: (discovered: { string }, problem: string?) -> ())
	local spawn_error = run_onlyne({ "ls", "--json" }, function(result: ProcessResult)
		if result.status ~= 0 then
			local detail = if result.stderr ~= "" then result.stderr else result.stdout
			done({}, "onlyne ls failed: " .. first_lines(detail, 3))
			return
		end
		local ok, payload = pcall(tern.json.decode, result.stdout)
		if not ok or type(payload) ~= "table" then
			done({}, "onlyne ls printed something other than a JSON list")
			return
		end
		local found: { string } = {}
		for _, row in payload do
			if
				type(row) == "table"
				and row.kind == "server"
				and type(row.root) == "string"
				and row.socket_exists == true
				and row.pid_alive == true
			then
				table.insert(found, normalize_root(row.root))
			end
		end
		done(found, nil)
	end)
	if spawn_error then
		done({}, spawn_error)
	end
end

-- Applying one read to its root. Only a success replaces the section's last
-- good answer; a failure writes `problem` and leaves the data alone.
local function apply_read(root: string, verb: string, result: ProcessResult)
	local rd = data.by_root[root]
	if rd == nil then
		return
	end
	rd.busy = false
	if result.status ~= 0 then
		local detail = if result.stderr ~= "" then result.stderr else result.stdout
		rd.problem = verb .. " failed (exit " .. tostring(result.status) .. "): " .. first_lines(detail, 3)
		return
	end
	local ok, payload = pcall(tern.json.decode, result.stdout)
	if not ok or type(payload) ~= "table" then
		rd.problem = verb .. " printed something other than the expected JSON object"
		return
	end
	if verb == "status" then
		rd.cluster = if type(payload.cluster) == "string" then payload.cluster else nil
		rd.version = if type(payload.version) == "string" then payload.version else nil
	elseif verb == "roles" then
		if type(payload.roles) ~= "table" then
			rd.problem = "roles answer carried no roles array"
			return
		end
		rd.roles = payload.roles
	elseif verb == "sessions" then
		if type(payload.sessions) ~= "table" then
			rd.problem = "sessions answer carried no sessions array"
			return
		end
		rd.sessions = payload.sessions
	elseif verb == "faults" then
		if type(payload.faults) ~= "table" then
			rd.problem = "faults answer carried no faults array"
			return
		end
		rd.faults = payload.faults
	end
	rd.fetched = os.time()
	rd.problem = nil
end

-- Blocks.

local function prune()
	local live: { [number]: boolean } = {}
	for _, info in tern.pane.list() do
		live[info.pane] = true
	end
	local now = tern.now()
	for pane, block in blocks do
		if not live[pane] and now - block.since > PRUNE_GRACE_MS then
			blocks[pane] = nil
		end
	end
end

refresh = function()
	prune()
	for _, block in blocks do
		block.cx:render()
	end
end

-- One polling chain while any block is open: discovery, then every live
-- root's four reads, one `onlyne` process at a time. A manual refresh during
-- a cycle is a no-op, so keys cannot stack processes.
start_cycle = function()
	if chain_busy or next(blocks) == nil then
		return
	end
	chain_generation += 1
	local generation = chain_generation
	chain_busy = true
	data.busy = true
	refresh()
	discover(function(discovered, problem)
		if generation ~= chain_generation or next(blocks) == nil then
			return
		end
		data.problem = problem
		rebuild_roots(discovered)
		table.clear(queue)
		for _, root in data.order do
			local rd = data.by_root[root]
			if rd and rd.live then
				for _, verb in READS do
					table.insert(queue, { root = root, verb = verb })
				end
			end
		end
		next_step(generation)
	end)
end

next_step = function(generation: number)
	if generation ~= chain_generation or next(blocks) == nil then
		table.clear(queue)
		chain_busy = false
		data.busy = false
		return
	end
	local step = table.remove(queue, 1)
	if step == nil then
		chain_busy = false
		data.busy = false
		data.updated = os.time()
		refresh()
		schedule()
		return
	end
	local rd = data.by_root[step.root]
	if rd == nil then
		next_step()
		return
	end
	rd.busy = true
	local spawn_error = run_onlyne(read_args(step.root, step.verb), function(result: ProcessResult)
		if generation ~= chain_generation or next(blocks) == nil then
			return
		end
		apply_read(step.root, step.verb, result)
		next_step(generation)
	end)
	if spawn_error then
		-- Nothing else in this cycle can start either; say so once and stop.
		rd.busy = false
		rd.problem = spawn_error
		data.problem = spawn_error
		table.clear(queue)
		next_step(generation)
	end
end

schedule = function()
	if timer ~= nil then
		return
	end
	if next(blocks) == nil then
		return
	end
	timer = tern.timer(POLL_MS, function()
		timer = nil
		start_cycle()
	end)
end

-- Rows and actions.

local function sessions_of(rd: RootData): { Json }
	return rd.sessions or {}
end

local function faults_of(rd: RootData): { Json }
	return rd.faults or {}
end

local function row_key(row: Row): string
	return row.root .. "\0" .. row.kind .. "\0" .. tostring(row.index)
end

local function rows_of(): { Row }
	local rows: { Row } = {}
	for _, root in data.order do
		local rd = data.by_root[root]
		if rd then
			for i in ipairs(sessions_of(rd)) do
				table.insert(rows, { root = root, kind = "session", index = i })
			end
			for i in ipairs(faults_of(rd)) do
				table.insert(rows, { root = root, kind = "fault", index = i })
			end
		end
	end
	return rows
end

-- The task a control op aims at. Every control op names a task (the CLI
-- refuses one without), so a row that carries no task id has no op.
local function task_id_of(item: Json): string?
	local id = item.task_id
	return if type(id) == "string" and id ~= "" then id else nil
end

local function run_armed(block: Block)
	local armed = block.armed
	if armed == nil then
		return
	end
	block.armed = nil
	local pane = block.cx.pane
	local done = function(result: ProcessResult)
		if blocks[pane] ~= block then
			return
		end
		if result.status == 0 then
			block.note = armed.label .. ": done"
		else
			local detail = if result.stderr ~= "" then result.stderr else result.stdout
			block.note = armed.label .. " failed (exit " .. tostring(result.status) .. "): " .. first_lines(detail, 2)
		end
		-- An act that changed the cluster pays for a fresh read right away.
		start_cycle()
		refresh()
	end
	local argv: { string }
	if armed.verb == "ack" or armed.verb == "retry" then
		argv = repair_argv(armed.root, armed.verb, armed.task, armed.fault_id)
	else
		argv = control_argv(armed.root, armed.verb, armed.task or "")
	end
	local spawn_error = run_onlyne(argv, done)
	if spawn_error then
		block.note = armed.label .. ": " .. spawn_error
		refresh()
	end
end

-- The first press arms; the second press of the same key on the same target
-- runs. Any other action key re-arms, and navigation aborts.
local function arm(block: Block, armed: Armed)
	if block.armed and block.armed.key == armed.key then
		run_armed(block)
	else
		block.armed = armed
		block.note = nil
	end
end

local function forget(block: Block, row: Row)
	local rd = data.by_root[row.root]
	if rd == nil then
		return
	end
	if rd.source ~= "config" then
		block.note = tilde(row.root) .. " was discovered live; it leaves when its server stops"
		return
	end
	local kept: { string } = {}
	for _, root in configured_roots() do
		if root ~= row.root then
			table.insert(kept, root)
		end
	end
	remember_roots(kept)
	data.by_root[row.root] = nil
	local order: { string } = {}
	for _, root in data.order do
		if root ~= row.root then
			table.insert(order, root)
		end
	end
	data.order = order
	block.armed = nil
	block.note = "forgot " .. tilde(row.root)
end

-- Views.

local function status_line(rd: RootData): { Span | string }
	local parts: { Span | string } = {}
	if rd.cluster ~= nil and rd.cluster ~= "" then
		table.insert(parts, ui.span(rd.cluster :: string, "strong"))
	else
		table.insert(parts, ui.span("(status unread)", "muted"))
	end
	if rd.version ~= nil and rd.version ~= "" then
		table.insert(parts, ui.span("  onlyne " .. (rd.version :: string), "muted"))
	end
	local roles = rd.roles
	if roles then
		local online = 0
		for _, role in roles do
			if role.state == "online" then
				online += 1
			end
		end
		table.insert(parts, ui.span(string.format("  roles %d/%d online", online, #roles), "muted"))
	end
	if rd.busy then
		table.insert(parts, ui.span("  reading…", "muted"))
	elseif rd.fetched then
		table.insert(parts, ui.span("  updated " .. ago(rd.fetched :: number) .. " ago", "muted"))
	end
	return parts
end

local function roles_view(rd: RootData): Node
	local roles = rd.roles
	if roles == nil then
		return muted_line("roles not read yet")
	end
	if #roles == 0 then
		return muted_line("no roles registered")
	end
	local rows: { { Cell } } = {}
	for _, role in roles do
		local name = field_text(role.name)
		if role.aggregate ~= nil then
			name = name .. " → " .. field_text(role.aggregate)
		end
		local state = field_text(role.state)
		local queued = field_number(role.queued)
		table.insert(rows, {
			ui.span(name, "mono"),
			ui.span(state, if state == "online" then "success" elseif state == "draining" then "warning" else "muted"),
			ui.span(tostring(field_number(role.sessions)), "num"),
			ui.span(tostring(queued), if queued > 0 then "warning num" else "muted num"),
		})
	end
	local cols: { Col } = {
		{ id = "role", head = "role", grow = 1 },
		{ id = "state", head = "state" },
		{ id = "sess", head = "sess", align = "end" },
		{ id = "queue", head = "queue", align = "end" },
	}
	return ui.table(cols, rows)
end

local function sessions_view(rd: RootData, marked: { [string]: boolean }): Node
	local sessions = rd.sessions
	if sessions == nil then
		return muted_line("sessions not read yet")
	end
	if #sessions == 0 then
		return muted_line("no sessions")
	end
	local rows: { { Cell } } = {}
	for i, session in ipairs(sessions) do
		local hit = marked[rd.root .. "\0session\0" .. i]
		local lifecycle = field_text(session.public_lifecycle)
		local projection = session.projection
		local agent = if type(projection) == "table" then field_text(projection.agent) else ""
		local stale = session.heartbeat_stale == true
		local phase = lifecycle .. "/" .. agent .. (if stale then "+stale" else "")
		local age = age_of(session.last_seen) or age_of(session.updated_at)
		table.insert(rows, {
			{ ui.span(if hit then "›" else " ", if hit then "accent strong" else "") },
			{ ui.span(field_text(session.role), "strong") },
			{ ui.span(short_id(session.task_id), "mono") },
			{ ui.span(phase, if stale then "warning" else "") },
			{ ui.span(age or "-", "muted num") },
		})
	end
	local cols: { Col } = {
		{ id = "mark", head = "" },
		{ id = "role", head = "role" },
		{ id = "task", head = "task" },
		{ id = "phase", head = "phase", grow = 1 },
		{ id = "age", head = "age", align = "end" },
	}
	return ui.table(cols, rows)
end

local function faults_view(rd: RootData, marked: { [string]: boolean }): Node
	local faults = rd.faults
	if faults == nil then
		return muted_line("faults not read yet")
	end
	if #faults == 0 then
		return muted_line("no open faults")
	end
	local rows: { { Cell } } = {}
	for i, fault in ipairs(faults) do
		local hit = marked[rd.root .. "\0fault\0" .. i]
		local created = field_number(fault.created_at)
		table.insert(rows, {
			{ ui.span(if hit then "›" else " ", if hit then "accent strong" else "") },
			{ ui.span("#" .. tostring(field_number(fault.id)), "muted num") },
			{ ui.span(field_text(fault.kind), "error") },
			{ ui.span(field_text(fault.role), "strong") },
			{ ui.span(field_text(fault.reason)) },
			{ ui.span(if created > 0 then ago(created) else "-", "muted num") },
		})
	end
	local cols: { Col } = {
		{ id = "mark", head = "" },
		{ id = "id", head = "id" },
		{ id = "kind", head = "kind" },
		{ id = "role", head = "role" },
		{ id = "reason", head = "reason", grow = 1 },
		{ id = "age", head = "age", align = "end" },
	}
	return ui.table(cols, rows)
end

local function root_nodes(rd: RootData, marked: { [string]: boolean }): { Node }
	local parts: { Node } = {}
	local head: { Span | string } = { ui.span(tilde(rd.root), "strong") }
	if not rd.live then
		table.insert(head, ui.span("  gone", "error"))
	end
	table.insert(parts, with_role(ui.rule(head), "onlyne.root"))
	table.insert(parts, ui.text(status_line(rd)))
	if rd.problem then
		local tone = if rd.live then "warning" else "error"
		table.insert(parts, with_role(ui.text({ ui.span(rd.problem, tone) }), if rd.live then "onlyne.problem" else "onlyne.down"))
	end
	table.insert(parts, roles_view(rd))
	table.insert(parts, sessions_view(rd, marked))
	table.insert(parts, faults_view(rd, marked))
	return parts
end

local function dock_view(block: Block): Node
	local sessions, faults = 0, 0
	for _, root in data.order do
		local rd = data.by_root[root]
		if rd then
			sessions += #sessions_of(rd)
			faults += #faults_of(rd)
		end
	end
	local lines: { Node } = {}
	if data.busy then
		table.insert(lines, ui.text({ ui.span("polling…", "muted") }))
	else
		local head: { Span | string } = {
			ui.span(
				string.format(
					"%d cluster%s · %d session%s · %d open fault%s",
					#data.order,
					if #data.order == 1 then "" else "s",
					sessions,
					if sessions == 1 then "" else "s",
					faults,
					if faults == 1 then "" else "s"
				),
				"strong"
			),
		}
		if data.updated then
			table.insert(head, ui.span("  refreshed " .. ago(data.updated) .. " ago", "muted"))
		end
		table.insert(lines, ui.text(head))
	end
	if data.problem then
		table.insert(lines, ui.text({ ui.span(data.problem, "warning") }))
	end
	if block.note then
		table.insert(lines, ui.text({ ui.span(block.note, "muted") }))
	end
	local armed = block.armed
	if armed then
		table.insert(lines, ui.text({
			ui.span(armed.label, "strong warning"),
			ui.span("  — press the same key again (or ⏎) to confirm · esc aborts", "warning"),
		}))
	end
	table.insert(lines, ui.text({
		ui.span("↑↓ move  f focus  p probe  t snap  r recycle  x cancel  a ack  ⏎ confirm  . refresh", "dim"),
	}))
	local buttons: { Node } = {}
	if armed then
		table.insert(buttons, action_button("Confirm " .. armed.label, "confirm", "accent"))
		table.insert(buttons, action_button("Abort", "disarm", "neutral"))
	end
	table.insert(buttons, action_button("Refresh", "refresh", "neutral"))
	return ui.col({ ui.lines(lines), ui.row(buttons) })
end

tern.block.define("board", {
	init = function(cx: BlockCx, args: { string }, saved: any): Block
		local block: Block = {
			cx = cx,
			since = tern.now(),
			cursor = if type(saved) == "table" and type(saved.cursor) == "number" then saved.cursor else 1,
			armed = nil,
			note = nil,
		}
		-- Launch arguments are server roots to watch; they join the kv list,
		-- so they survive restarts and reach every other board too.
		if #args > 0 then
			local roots = configured_roots()
			local added = 0
			for _, arg in args do
				local root = normalize_root(expand_home(arg))
				if root ~= "" and root ~= "/" then
					local known = false
					for _, known_root in roots do
						if known_root == root then
							known = true
							break
						end
					end
					if not known then
						table.insert(roots, root)
						added += 1
					end
				end
			end
			if added > 0 then
				remember_roots(roots)
			end
		end
		blocks[cx.pane] = block
		start_cycle()
		schedule()
		return block
	end,

	title = function(_block: Block): string?
		local sessions, faults = 0, 0
		for _, root in data.order do
			local rd = data.by_root[root]
			if rd then
				sessions += #sessions_of(rd)
				faults += #faults_of(rd)
			end
		end
		return string.format("Onlyne Board (%d sess · %d faults)", sessions, faults)
	end,

	view = function(block: Block, _cx: BlockCx): BlockView
		local rows = rows_of()
		block.cursor = math.clamp(block.cursor, 1, math.max(1, #rows))
		local marked: { [string]: boolean } = {}
		local cursor_row = rows[block.cursor]
		if cursor_row then
			marked[row_key(cursor_row)] = true
		end
		local parts: { Node } = {}
		if #data.order == 0 then
			local empty: { Span | string } = { ui.span("No Onlyne cluster is visible yet.", "muted") }
			if data.problem then
				table.insert(empty, ui.span("  " .. data.problem, "warning"))
			end
			table.insert(parts, ui.text(empty))
			table.insert(parts, ui.text({
				ui.span(
					"Run `onlyne-server run --root <dir>`, or watch a root with “Watch this directory as an Onlyne server root”.",
					"dim"
				),
			}))
		else
			for _, root in data.order do
				local rd = data.by_root[root]
				if rd then
					for _, node in root_nodes(rd, marked) do
						table.insert(parts, node)
					end
				end
			end
		end
		-- A region draws its root node's children, not the root itself, so the
		-- view sits inside a column.
		return { main = ui.col(parts), dock = dock_view(block) }
	end,

	key = function(block: Block, key: Key, _cx: BlockCx): boolean?
		if key.ctrl or key.alt or key.meta then
			return false
		end
		local k = key.text or key.name
		local rows = rows_of()
		local count = #rows
		local clamp = math.clamp(block.cursor, 1, math.max(1, count))

		if key.name == "up" or k == "k" then
			block.armed = nil
			block.cursor = math.clamp(clamp - 1, 1, math.max(1, count))
		elseif key.name == "down" or k == "j" then
			block.armed = nil
			block.cursor = math.clamp(clamp + 1, 1, math.max(1, count))
		elseif key.name == "page_up" then
			block.armed = nil
			block.cursor = math.clamp(clamp - 10, 1, math.max(1, count))
		elseif key.name == "page_down" then
			block.armed = nil
			block.cursor = math.clamp(clamp + 10, 1, math.max(1, count))
		elseif key.name == "home" or k == "g" then
			block.armed = nil
			block.cursor = 1
		elseif key.name == "end" or k == "G" then
			block.armed = nil
			block.cursor = math.max(1, count)
		elseif key.name == "escape" then
			block.armed = nil
		elseif key.name == "enter" then
			if block.armed then
				run_armed(block)
			else
				return false
			end
		elseif k == "." or key.name == "f5" then
			start_cycle()
		elseif k == "D" then
			if count == 0 then
				return false
			end
			forget(block, rows[clamp])
		else
			if count == 0 then
				return false
			end
			local row = rows[clamp]
			local verb = if row.kind == "session" then SESSION_KEYS[k] else FAULT_KEYS[k]
			if verb == nil then
				return false
			end
			local rd = data.by_root[row.root]
			local pool = if rd == nil then nil else if row.kind == "session" then sessions_of(rd) else faults_of(rd)
			local item = if pool then pool[row.index] else nil
			if rd == nil or item == nil then
				return false
			end
			if verb == "ack" then
				local id = field_number(item.id)
				if id <= 0 then
					return false
				end
				arm(block, {
					key = "ack\0" .. row.root .. "\0" .. tostring(id),
					verb = "ack",
					label = "repair ack fault #" .. tostring(id),
					root = row.root,
					fault_id = id,
				})
			else
				local task = task_id_of(item)
				if task == nil then
					block.note = verb .. " needs a task; this row names none"
				else
					arm(block, {
						key = verb .. "\0" .. row.root .. "\0" .. task,
						verb = verb,
						label = verb .. " task " .. short_id(task),
						root = row.root,
						task = task,
					})
				end
			end
		end
		return true
	end,

	event = function(block: Block, ev: UiEvent, _cx: BlockCx)
		if ev.ev ~= "action" then
			return
		end
		if ev.act == "refresh" then
			start_cycle()
		elseif ev.act == "disarm" then
			block.armed = nil
		elseif ev.act == "confirm" then
			run_armed(block)
		end
	end,

	save = function(block: Block): any
		return { cursor = block.cursor }
	end,
})
