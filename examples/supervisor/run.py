#!/usr/bin/env python3
"""examples/supervisor: 一个野生操作 agent 加一条流水灯环。

`_supervisor` 是用户的集群操作 agent：它跑在 Orca 的一个标签页里，用产品自己的
`onlyne` CLI 派活、看账、开关集群，成品由环上的角色产出。环上的 `a..e` 是服务器
管理的角色，每个任务由客户端拉起一个 pi 会话（在 Orca 里各占一个标签页），会话跑
完就退出、标签页随之关闭。一轮流水灯 = `len(RING) * 2` 跳，每跳往记录文件追加一行
`<k>:<字母>`，最后一跳把文件内容当结果交回，账本里留下每个任务一行。

用法:
    python3 examples/supervisor/run.py up     [text]  起集群并让 supervisor 开工
    python3 examples/supervisor/run.py lights        脚本直接派一轮环（不经模型）
    python3 examples/supervisor/run.py send   <text> 原始 admin send 给环首 a
    python3 examples/supervisor/run.py status        roles / sessions / ledger
    python3 examples/supervisor/run.py stop          收摊

前置：`cargo build --workspace` 跑过，PATH 上有可用的 `pi`。在 Orca 标签页里执行
时，环上的会话标签页会出现在同一个 worktree；不在 Orca 里时 export
`ONLYNE_BACKEND=exec` 走无头后端，supervisor 则作为子进程把输出写进
`<root>/logs/supervisor.log`。运行时树在 `/tmp/onlyne-sup`（`SUP_DEMO_ROOT` 可改），
短路径是因为 macOS 的 unix socket 路径上限 104 字节。脚本只用 python3 标准库。
"""

from __future__ import annotations

import json
import os
import shlex
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BIN = ROOT / "target" / "debug"
TEMPLATES = ROOT / "examples" / "supervisor" / "templates"
ONLYNE = BIN / "onlyne"
SERVER = BIN / "onlyne-server"
CLUSTER = Path(os.environ.get("SUP_DEMO_ROOT")
               or ("/tmp/onlyne-sup" if sys.platform == "darwin" else str(Path.home() / "onlyne-sup")))
WS = CLUSTER / "ws" / "demo"
SPEC = CLUSTER / ".onlyne" / "spec.toml"
LIGHTS = CLUSTER / "lights.txt"
TAB_FILE = CLUSTER / "supervisor-tab"
PID_FILE = CLUSTER / "supervisor.pid"
SUPERVISOR = "_supervisor"
# 环的顺序就是这张表：加一个成员只改这里。successor 绕回表头。
RING = ["a", "b", "c", "d", "e"]
ROLES = [SUPERVISOR] + RING
TOTAL = len(RING) * 2
# Shape-correct placeholder (`ed25519/` + 32 zero bytes): `generate` needs the
# roles in the spec to render, and the fragment it prints carries the keys the
# workspaces really hold (D13 keeps spec writes with the operator).
ZERO_KEY = "ed25519/AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA="
# Tokens this driver renders into the staged templates. `generate` reads the
# staged copy and rejects any `{{...}}` it does not know, so these three are
# resolved here, and the documented placeholders (`{{role}}`, `{{cluster}}`,
# ...) stay for `generate` itself. Server-root paths never enter a template:
# `generate` scans its output for the server root and refuses to write a
# workspace containing it, so the root travels in the supervisor prompt.
STAGED_TOKENS = {
    "{{onlyne_cli}}": str(ONLYNE),
    "{{ring}}": ",".join(RING),
    "{{ring_total}}": str(TOTAL),
}
RING_TEXT = ",".join(RING)
DEFAULT_TASK = (
    f"开一轮流水灯：{RING[0]} 起步，{len(RING)} 元环两圈共 {TOTAL} 跳，"
    f"每跳把 <k>:<字母> 追加进 {LIGHTS}，最后一跳把文件内容带回来，做完向我汇报。\n"
    f"集群参数：集群根 {CLUSTER}（命令里写 --server-root {CLUSTER}），"
    f"记录文件 {LIGHTS}，环 {RING_TEXT}。\n"
    "就绪的派活命令：\n"
    f"{ONLYNE} --server-root {CLUSTER} send --from {SUPERVISOR} --to {RING[0]} "
    f'--text "RING={RING_TEXT} FILE={LIGHTS} K=1 TOTAL={TOTAL}"'
)


def say(message: str) -> None:
    print(f"supervisor-demo: {message}", flush=True)


def die(message: str) -> None:
    print(f"supervisor-demo: {message}", file=sys.stderr)
    sys.exit(1)


def run(args: list[str | Path], *, capture: bool = False, check: bool = True,
        env: dict[str, str] | None = None) -> str:
    argv = [str(a) for a in args]
    result = subprocess.run(argv, cwd=ROOT, capture_output=capture,
                            text=True, check=False,
                            env={**os.environ, **(env or {})})
    if check and result.returncode != 0:
        captured = (result.stdout + result.stderr) if capture else ""
        die(f"`{' '.join(argv)}` exited {result.returncode}: {captured.strip()}")
    return result.stdout if capture else ""


def call_ok(args: list[str | Path]) -> bool:
    return subprocess.run([str(a) for a in args], cwd=str(ROOT),
                          capture_output=True, text=True, check=False).returncode == 0


def client_env() -> dict[str, str]:
    """Environment for `onlyne client start`, which the daemon inherits.

    `onlyne-client` resolves `ONLYNE_BACKEND` in `backend_for`
    (`crates/onlyne-session/src/backend/mod.rs`); an empty value falls back to
    the zellij backend, and this demo wants the pi sessions as Orca tabs, which
    is the orca backend. An operator's own `ONLYNE_BACKEND` value wins, which is
    how `exec` gives a headless run.
    """
    chosen = os.environ.get("ONLYNE_BACKEND", "").strip()
    if chosen:
        say(f"client backend: ONLYNE_BACKEND={chosen}")
        return {}
    if os.environ.get("ORCA_WORKTREE_ID"):
        say("client backend: orca (session tabs land in this worktree)")
        return {"ONLYNE_BACKEND": "orca"}
    say("client backend: no ORCA_WORKTREE_ID and no ONLYNE_BACKEND; "
        "export ONLYNE_BACKEND=exec for a headless run")
    return {}


def need_binaries() -> None:
    for binary in (ONLYNE, SERVER, BIN / "onlyne-client", BIN / "onlyne-tui"):
        if not binary.exists():
            die(f"missing binary {binary}; run cargo build --workspace first")
    if not shutil.which("pi"):
        die("missing `pi` on PATH; the demo spawns pi sessions")


def free_port(base: int) -> int:
    for candidate in range(base, base + 50):
        with socket.socket() as probe:
            try:
                probe.bind(("127.0.0.1", candidate))
            except OSError:
                continue
        return candidate
    die(f"no free port near {base}")
    return 0


def stage_templates() -> None:
    """Copy the repo templates under the server root, resolving driver tokens.

    `generate` reads `<server root>/.onlyne/templates` and errors on an unknown
    `{{...}}`, so the tokens this driver owns are replaced while staging.
    """
    staged = CLUSTER / ".onlyne" / "templates"
    shutil.rmtree(staged, ignore_errors=True)
    for base, dirs, files in os.walk(TEMPLATES):
        relative_base = Path(base).relative_to(TEMPLATES)
        for name in dirs:
            (staged / relative_base / name).mkdir(parents=True, exist_ok=True)
        for name in files:
            source = Path(base) / name
            target = staged / relative_base / name
            target.parent.mkdir(parents=True, exist_ok=True)
            text = source.read_text()
            for token, value in STAGED_TOKENS.items():
                text = text.replace(token, value)
            target.write_text(text)


def init_cluster() -> None:
    CLUSTER.mkdir(parents=True, exist_ok=True)
    if not SPEC.is_file():
        run([SERVER, "init", "--root", CLUSTER, "--listen",
             f"127.0.0.1:{free_port(int(os.environ.get('PORT', '7901')))}"])
    stage_templates()


def seed_entries() -> str:
    """The `[[client]]` rows the spec holds until `generate` replaces the keys.

    `_supervisor` is one ordinary entry here: its key registers the operator
    identity and its `admin = true` is what lets the admin surface send as it.
    No client is ever started for it.
    """
    # A visible pi TUI in the session tab. The task text does not ride in the
    # argv: `{task}` renders the task *id* (crates/onlyne-client/src/dispatch.rs
    # `render_tokens`), and the plugin delivers the payload itself, through
    # `wakeUser` on `assign`, once its socket is up.
    command = json.dumps(
        ["pi", "--session-id", "{session}",
         "--session-dir", ".pi/sessions", "-ns"], ensure_ascii=False
    )
    supervisor_prose = (
        f"You are _supervisor, the operator agent of cluster sup-demo. The onlyne "
        f"CLI is {ONLYNE} and the cluster root is {CLUSTER}. The ring roles are "
        f"{','.join(RING)} and their record file is {LIGHTS}. With --server-root "
        f"{CLUSTER}: send --from _supervisor --to <role> --text <text> dispatches "
        "work and answers with data.task; roles, sessions, ledger --task <id>, "
        "faults and watch inspect state; client start or stop --workspace <dir> "
        "and server status, reload and stop operate the cluster. On a user "
        "request: dispatch, follow the work through the record file and the "
        "ledger, report the evidence in the language the user writes, and propose "
        "the next move. Delegation is your craft; the deliverable belongs to the "
        "ring roles."
    )
    ring_prose = (
        "You are ring member {role} of cluster sup-demo. A task text arrives in "
        "the form RING=<letters> FILE=<abs path> K=<n> TOTAL=<m>. Work exactly: "
        "append one line to FILE with echo using your letter, then find your "
        "successor in RING, wrapping around at the end; if K < TOTAL run the "
        "handoff command from your AGENTS.md to pass the same task text to that "
        "successor with K incremented by one; if K == TOTAL do not hand off, "
        "read FILE and answer onlyne_complete carrying the lines of FILE. Answer "
        "nothing else."
    )

    def entry(role: str, prose: str, *, admin: bool, senders: list[str],
              targets: list[str], reuse: bool) -> str:
        return (
            "[[client]]\n"
            f'role = "{role}"\n'
            f'key = "{ZERO_KEY}"\n'
            f"prose = '{prose}'\n"
            f"admin = {'true' if admin else 'false'}\n"
            "max_sessions = 2\n"
            f"reuse = {'true' if reuse else 'false'}\n"
            f"allowed_senders = {json.dumps(senders)}\n"
            f"allowed_targets = {json.dumps(targets)}\n"
            f"session_command = {command}\n\n"
        )

    rows = [entry(SUPERVISOR, supervisor_prose, admin=True,
                  senders=list(RING), targets=list(RING), reuse=True)]
    for index, role in enumerate(RING):
        predecessor = RING[index - 1]
        successor = RING[(index + 1) % len(RING)]
        # The pair is what makes an ACL edge: `acl_edges` emits
        # sender -> target only when the receiver also names the sender.
        neighbours = [successor, predecessor, SUPERVISOR]
        rows.append(entry(role, ring_prose.format(role=role), admin=False,
                          senders=neighbours, targets=neighbours, reuse=False))
    return "".join(rows)


def client_roles(text: str) -> list[str]:
    """Role names of the spec's real `[[client]]` tables, in file order.

    Matching is per parsed line, because `onlyne-server init` writes a comment
    block whose prose names `[[client]]` (`crates/onlyne-server/src/cli.rs`,
    `spec_template`), and a raw substring test would read that comment as a
    seeded spec.
    """
    roles: list[str] = []
    inside = False
    for line in text.splitlines():
        code = line.split("#", 1)[0].strip()
        if code.startswith("["):
            inside = code == "[[client]]"
            continue
        if inside and code.startswith("role") and "=" in code:
            roles.append(code.partition("=")[2].strip().strip('"\''))
            inside = False
    return roles


def write_seed_entries() -> None:
    """Seed the placeholder rows, keeping the header and the package path."""
    text = SPEC.read_text()
    if client_roles(text) == ROLES:
        return
    lines = text.splitlines(keepends=True)
    cut = next((i for i, line in enumerate(lines)
                if line.startswith("[[client]]")), len(lines))
    header = "".join(lines[:cut]).rstrip() + "\n"
    header = header.replace('agent_package = ""',
                            f'agent_package = "{ROOT / "integrations" / "pi-onlyne"}"')
    SPEC.write_text(header + "\n" + seed_entries())


def generate_workspaces() -> None:
    """Render one workspace per role, then fold the printed keys into the spec.

    The placeholder key decides whether the fold already happened: workspaces
    can exist while the spec still carries `ZERO_KEY` (a spec rebuilt from
    scratch beside a live `ws/`), and clients started in that state are refused
    with `key is not registered for role`.
    """
    rendered = all((WS / role / ".onlyne").is_dir() for role in ROLES)
    if rendered and ZERO_KEY not in SPEC.read_text():
        return
    fragment = run([ONLYNE, "--server-root", CLUSTER, "generate",
                    *[a for role in ROLES for a in ("--role", role)],
                    "--out", CLUSTER / "ws", "--force"], capture=True)
    # The printed fragment is the durable truth of which keys the workspaces
    # hold; the seed rows go back out of the file before it lands (D13, the
    # same move case 9 makes).
    lines = SPEC.read_text().splitlines(keepends=True)
    cut = next((i for i, line in enumerate(lines)
                if line.startswith("[[client]]")), len(lines))
    SPEC.write_text("".join(lines[:cut]) + fragment)
    for role in ROLES:
        (WS / role / ".pi" / "sessions").mkdir(parents=True, exist_ok=True)


def server_running() -> bool:
    """An admin read over `<root>/.onlyne/run/s` is the liveness probe.

    The admin surface answers `ping` with a `res` frame, which the CLI reports
    as `bad_frame`, so the probe uses a read verb.
    """
    return call_ok([ONLYNE, "--server-root", CLUSTER, "roles", "--json"])


def client_running(workspace: Path) -> bool:
    """`onlyne-client status` exits 0 either way; the answer is in its text."""
    out = run([BIN / "onlyne-client", "status", "--workspace", workspace],
              capture=True, check=False)
    return "client running" in out


def start_server() -> None:
    if server_running():
        say("server already running")
        return
    run([ONLYNE, "server", "start", "--root", CLUSTER])
    run([ONLYNE, "--server-root", CLUSTER, "wait-ready"])


def start_clients() -> None:
    for role in RING:
        workspace = WS / role
        if client_running(workspace):
            say(f"client already running for {role}")
            continue
        sock = workspace / ".onlyne" / "run" / "s"
        if sock.exists():
            sock.unlink()
        run([ONLYNE, "client", "start", "--workspace", workspace],
            env=client_env())


def wait_online(seconds: float = 60.0) -> None:
    deadline = time.monotonic() + seconds
    last = ""
    while time.monotonic() < deadline:
        last = run([ONLYNE, "--server-root", CLUSTER, "roles", "--json"],
                   capture=True, check=False)
        try:
            data = json.loads(last.strip().splitlines()[-1])
            rows = data.get("data", data)
            if isinstance(rows, dict):
                rows = rows.get("roles", [])
            online = {r.get("name") for r in rows if r.get("state") == "online"}
            if all(role in online for role in RING):
                return
        except (ValueError, IndexError, AttributeError, TypeError):
            pass
        time.sleep(0.25)
    die(f"the ring roles did not come online: {last}")


def lights_instruction() -> str:
    return f"RING={RING_TEXT} FILE={LIGHTS} K=1 TOTAL={TOTAL}"


def dispatch_to_ring() -> None:
    """Truncate the record file and hand the first hop to a through the admin CLI."""
    cleared = close_settled_tabs(live_tasks())
    if cleared:
        say(f"closed {len(cleared)} tab(s) left by an earlier round")
    LIGHTS.write_text("")
    run([ONLYNE, "--server-root", CLUSTER, "send", "--from", SUPERVISOR,
         "--to", RING[0], "--text", lights_instruction()])


def task_tabs(role: str) -> list[dict]:
    """The tab⇄session lines a role's Orca backend appended.

    `<workspace>/.onlyne/cache/orca-tabs.jsonl` is the documented supervisor
    side-channel (AGENTS.md §5), and it is the record that outlives the
    dispatch slot: a settled task releases its slot, so the client's own
    shutdown sweep cannot see the tab any more.
    """
    path = WS / role / ".onlyne" / "cache" / "orca-tabs.jsonl"
    if not path.is_file():
        return []
    rows = []
    for line in path.read_text().splitlines():
        if line.strip():
            rows.append(json.loads(line))
    return rows


def live_tasks() -> set[str]:
    """Task ids whose session still reads `working`, from the sessions query."""
    out = run([ONLYNE, "--server-root", CLUSTER, "sessions", "--json"],
              capture=True, check=False)
    try:
        payload = json.loads(out.strip().splitlines()[-1])
        rows = payload.get("data", {}).get("sessions") or []
    except (ValueError, IndexError, AttributeError, TypeError):
        return set()
    return {row.get("task_id") for row in rows
            if row.get("public_lifecycle") == "working"}


def close_settled_tabs(keep: set[str]) -> list[str]:
    """Close the tabs of sessions that are no longer working.

    `onlyne_complete` is the documented exit for a session, and while the pi
    process it runs in keeps living the tab stays behind. This sweep is the
    operator's backstop: it closes every recorded tab whose task is not in
    `keep`, and it verifies each close.
    """
    closed = []
    for role in ROLES:
        for row in task_tabs(role):
            handle = row.get("handle")
            if not handle or row.get("task_id") in keep:
                continue
            if handle not in orca_tabs():
                continue
            run(["orca", "terminal", "close", "--terminal", handle, "--tab", "--json"],
                capture=True, check=False)
            if handle in orca_tabs():
                die(f"orca still lists {handle} after close; finish it with "
                    f"`orca terminal close --terminal {handle} --tab`")
            closed.append(handle)
    return closed


def supervisor_tab_handle() -> str:
    return TAB_FILE.read_text().strip() if TAB_FILE.is_file() else ""


def orca_tabs() -> str:
    return run(["orca", "terminal", "list"], capture=True, check=False)


def spawn_supervisor_tab(prompt: str) -> None:
    """Put the operator agent in front of the user: an Orca tab, or a child process."""
    workspace = WS / SUPERVISOR
    handle = supervisor_tab_handle()
    if handle and handle in orca_tabs():
        say(f"supervisor tab already open ({handle})")
        return
    if os.environ.get("ORCA_WORKTREE_ID"):
        command = f"cd {shlex.quote(str(workspace))} && exec pi {shlex.quote(prompt)}"
        payload = json.loads(run(
            ["orca", "terminal", "create", "--title", "onlyne-supervisor",
             "--command", command, "--json"], capture=True))
        terminal = payload["result"]["terminal"]
        TAB_FILE.write_text(terminal["handle"])
        say(f"supervisor tab {terminal['handle']} (pane {terminal['paneKey']})")
        return
    log = CLUSTER / "logs" / "supervisor.log"
    log.parent.mkdir(parents=True, exist_ok=True)
    with log.open("ab") as sink:
        child = subprocess.Popen(["pi", "-p", prompt], cwd=str(workspace),
                                 stdin=subprocess.DEVNULL, stdout=sink,
                                 stderr=subprocess.STDOUT, start_new_session=True)
    PID_FILE.write_text(str(child.pid))
    say(f"no Orca worktree in the environment; supervisor runs as pid {child.pid}, "
        f"log {log}")


def stop_supervisor() -> None:
    handle = supervisor_tab_handle()
    if handle:
        run(["orca", "terminal", "close", "--terminal", handle, "--tab", "--json"],
            capture=True, check=False)
        if handle in orca_tabs():
            die(f"orca still lists {handle} after close; run `orca terminal close "
                f"--terminal {handle} --tab` by hand")
        TAB_FILE.unlink()
        say(f"closed supervisor tab {handle}")
    if PID_FILE.is_file():
        pid = int(PID_FILE.read_text().strip())
        try:
            os.kill(pid, 15)
        except ProcessLookupError:
            pass
        PID_FILE.unlink()
        say(f"signalled supervisor pid {pid}")


def status() -> None:
    for verb in ("roles", "sessions", "ledger"):
        run([ONLYNE, "--server-root", CLUSTER, verb])


def stop_all() -> None:
    stop_supervisor()
    # The ring's tabs exist only while their sessions live; the plugin closes
    # that loop itself once it exits pi on completion. This sweep covers a
    # session the plugin could not finish and the rounds this driver ran
    # before its slot was released.
    closed = close_settled_tabs(set())
    if closed:
        say(f"closed {len(closed)} session tab(s)")
    for role in RING:
        call_ok([ONLYNE, "client", "stop", "--workspace", WS / role])
    call_ok([ONLYNE, "server", "stop", "--root", CLUSTER])
    say("stopped")


def up(text: str) -> None:
    need_binaries()
    init_cluster()
    write_seed_entries()
    generate_workspaces()
    start_server()
    start_clients()
    wait_online()
    say(f"cluster live under {CLUSTER}; the ring is {','.join(RING)}, "
        f"{TOTAL} hops per round")
    spawn_supervisor_tab(text)
    say("the supervisor is starting the round; ride along with:")
    say(f"  {ONLYNE} --server-root {CLUSTER} watch --follow")
    say(f"  {ONLYNE} --server-root {CLUSTER} sessions   # ring rows carry the pane binding")
    say(f"  wc -l {LIGHTS}   # one line per hop; {TOTAL} lines is the finished round")
    say(f"  {BIN / 'onlyne-tui'} --server-root {CLUSTER}   # page 1 role network, page 2 swarm rows")
    say("keep talking to the supervisor tab; `run.py lights` replays the round "
        "with the script in the dispatch seat")


def main(argv: list[str]) -> None:
    command = argv[0] if argv else "up"
    text = " ".join(argv[1:])
    if command == "up":
        up(text or DEFAULT_TASK)
    elif command == "lights":
        dispatch_to_ring()
    elif command == "send":
        if not text:
            die("usage: run.py send <text>")
        run([ONLYNE, "--server-root", CLUSTER, "send", "--from", SUPERVISOR,
             "--to", RING[0], "--text", text])
    elif command == "status":
        status()
    elif command == "stop":
        stop_all()
    else:
        die("usage: run.py up [text] | lights | send <text> | status | stop")


if __name__ == "__main__":
    main(sys.argv[1:])
