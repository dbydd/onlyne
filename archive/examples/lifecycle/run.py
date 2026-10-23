#!/usr/bin/env python3
"""examples/lifecycle：真实三根联邦集群，检验 v2.11「生命周期是声明出来的」三处修复。

运行时树在 `LIFECYCLE_ROOT`（默认 `/tmp/onlyne-lifecycle`），进程全是真进程：三个
`onlyne-server`、七个 `onlyne-client`、一个 `onlyne-im` 加它的 fake 平台后端、一个
`onlyne-agent-fake`，以及一个扮演各集群常驻 agent 的 `resident` 进程。resident 不是
模拟器：它按 adapter 协议逐帧和真实 client 的 workspace socket 对话（`crates/onlyne-adapter/PROTOCOL.md`）。

复刻的形态与 `/Users/dbydd/vibe-agent-working-dir` 上的部署一一对应：

  daily-workbench（中心，自选端口）
      _supervisor   逻辑签名位，不起 client
      dpdp          常驻外部会话 dpdp-main；IM 入站与生命周期断言都落在它身上
      caerulea      aggregate = "alexandria"，Alexandria 集群代表
      lazuli        aggregate = "research-chamber"，ResearchChamber 集群代表
      feishu-im     IM 网关角色，根在 `daily-workbench/im`，后端是 `onlyne-im-backend-fake`
  Alexandria（子集群，自选端口）      _supervisor / caerulea（与中心同名同钥匙）/ scriber / master
  ResearchChamber（子集群，自选端口） _supervisor / lazuli / drifter-advection-residual

`aggregate` 是纯标注：core 里没有任何自动跨集群转发
（`crates/onlyne-server/FEDERATION.md`）。所以联邦那一跳由代表本人跑完 —— resident 同时
握着父层与子层两条连接，把父层的投递显式发进子集群，再把子集群真实作业的结果带回中心。
唯一被脚本替代的是代表自己的判断（真部署里那是个模型），每一跳仍是真 daemon/socket/账本。

用法:
    python3 examples/lifecycle/run.py up         [--json]   起全套集群（幂等）
    python3 examples/lifecycle/run.py lifecycle             核心断言：同一会话被两条连接声明
    python3 examples/lifecycle/run.py federation            父层 → 代表 → 子集群作业 → 结果与 note 回流
    python3 examples/lifecycle/run.py im                    后端事件入站 → dpdp → 网关出站回执
    python3 examples/lifecycle/run.py idle                  空载成本（CPU 时间增量 + lo0 包速率）
    python3 examples/lifecycle/run.py status      [--json]  roles / sessions / 网关 / chat key
    python3 examples/lifecycle/run.py stop                  按记录的 PID 停掉本脚本起的进程

`resident` 是内部动词（`up` 用它起常驻进程），没有别的作用。
前置：`cargo build --workspace`。只用 python3 标准库。
"""

from __future__ import annotations

import argparse
import datetime
import hashlib
import itertools
import json
import os
import queue
import re
import signal
import socket
import string
import struct
import subprocess
import sys
import threading
import time
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BIN = ROOT / "target" / "debug"
ONLYNE = BIN / "onlyne"
SERVER = BIN / "onlyne-server"
CLIENT = BIN / "onlyne-client"
IM = BIN / "onlyne-im"
FAKE = BIN / "onlyne-agent-fake"
PS = Path("/bin/ps")

# 根短是因为 macOS 的 unix socket 路径上限 104 字节：v2 把 socket 绑在机器级 runtime
# 目录（`/tmp/onlyne-<uid>/<digest>.sock`），但根短让 `ps`、日志、报错都读得动。
TREE = Path(os.environ.get("LIFECYCLE_ROOT") or "/tmp/onlyne-lifecycle")
CENTER = TREE / "daily-workbench"
ALEXANDRIA = TREE / "Alexandria"
CHAMBER = TREE / "ResearchChamber"
# 运行时目录属于这个例子自己：v2 把每条本地 socket 绑在
# `$ONLYNE_RUNTIME_DIR/<digest>.sock`，和本机的生产集群共用 `/tmp/onlyne-<uid>`
# 会让两边的 CLI 互相读到对方的注册文件（也会让任何"按注册表找 socket"的工具拨到
# 生产 daemon）。所有子进程与 CLI 调用都带着它。
RUNTIME = Path(os.environ.get("LIFECYCLE_RUNTIME_DIR") or (TREE / "runtime"))
LOGS = TREE / "logs"
REGISTRY = TREE / "procs.json"
RESIDENT_STATE = TREE / "resident-state.json"
RUN_FILE = TREE / "run-id"
SHAPE_FILE = TREE / "paths.json"

SUP = "_supervisor"
HOUSE = "dpdp"
REP_A = "caerulea"
REP_B = "lazuli"
GW_ROLE = "feishu-im"
GW_SLUG = "feishu-main"
FAKE_CHAT = "-1000001"
# `onlyne-im-backend-fake` 自己的哨兵凭据（它的 `DEFAULT_TOKEN`）。写这一份不是因为
# 有平台要认证，而是因为 credential 加载里"已指定但不存在的路径"会压过后端的默认值：
# 没有它，`hello.authenticated=false`，后端就永远不轮询事件。
FAKE_TOKEN = "let-me-in"
ALX_NAME = "alexandria"
CHAMBER_NAME = "research-chamber"
ALX_WORKER = "scriber"
ALX_MASTER = "master"
CH_WORKER = "drifter-advection-residual"
SESSIONS = {HOUSE: "dpdp-main", REP_A: "caerulea-main", REP_B: "lazuli-main",
            ALX_WORKER: "scriber-main", ALX_MASTER: "master-main",
            CH_WORKER: "drifter-main"}
CENTER_ROLES = [SUP, HOUSE, REP_A, REP_B, GW_ROLE]
# 与 onlyne-agent-pi 声明的同一套：register/report/inject/recycle 之外没有别的，
# 所以 client 不会向我发 open（open/suspend/close 才是 hosting 运行时）。
CAPS = ["register", "report", "inject", "recycle", "probe"]
SEQ_BASE = 1000
# 常驻连接空闲时的心跳间隔。client 的看门狗按静默判死一个 session（30 秒窗口），
# 真常驻运行时在等活的时候照样报告自己还活着；取窗口的三分之一。
BEAT_INTERVAL = 10.0
UNKNOWN = ("", "gone", "exited", "offline", "terminated")
# `idle` 的两条阈值。包计数器是全机的（噪声 ~±1000 包/秒），CPU 是逐进程的。
PACKET_CEILING = 5000.0
CPU_CEILING = 0.1


def say(message: str) -> None:
    print(f"lifecycle: {message}", flush=True)


def die(message: str) -> None:
    print(f"lifecycle: {message}", file=sys.stderr)
    sys.exit(1)


def ok(what: str, detail: str = "") -> None:
    print(f"ok   {what}" + (f" — {detail}" if detail else ""), flush=True)


def fail(what: str, detail: str = "") -> None:
    print(f"FAIL {what}" + (f" — {detail}" if detail else ""), file=sys.stderr, flush=True)
    sys.exit(1)


def check(condition: bool, what: str, detail: str = "") -> None:
    if condition:
        ok(what, detail)
    else:
        fail(what, detail)


def run(args, *, capture: bool = False, check_rc: bool = True, env=None, cwd=None) -> str:
    argv = [str(a) for a in args]
    result = subprocess.run(argv, cwd=str(cwd or ROOT), capture_output=capture, text=True,
                            check=False, env=child_env(env))
    if check_rc and result.returncode != 0:
        captured = (result.stdout + result.stderr) if capture else ""
        die(f"`{' '.join(argv)}` exited {result.returncode}: {captured.strip()}")
    return result.stdout if capture else ""


def child_env(extra=None) -> dict:
    return {**os.environ, "ONLYNE_RUNTIME_DIR": str(RUNTIME), **(extra or {})}


def call_ok(args) -> bool:
    return subprocess.run([str(a) for a in args], cwd=str(ROOT), capture_output=True,
                          text=True, check=False, env=child_env()).returncode == 0


def cli_out(args, cwd=None) -> subprocess.CompletedProcess:
    return subprocess.run([str(a) for a in args], cwd=str(cwd or ROOT), capture_output=True,
                          text=True, check=False, env=child_env())


def cli_json(root: Path, *args) -> dict | None:
    """`onlyne --server-root <root> <args> --json` 的解析结果；失败给 None。"""
    out = cli_out([ONLYNE, "--server-root", str(root), *[str(a) for a in args], "--json"])
    if out.returncode != 0:
        return None
    try:
        return json.loads(out.stdout)
    except ValueError:
        return None


def rows_of(answer, *keys) -> list[dict]:
    """列表答（ledger/sessions/roles）的行；形状按 CLI 实际输出。"""
    if not isinstance(answer, dict):
        return []
    data = answer.get("data", answer)
    if isinstance(data, list):
        return [row for row in data if isinstance(row, dict)]
    if isinstance(data, dict):
        for key in ("rows", "results", *keys, "ledger", "items", "sessions", "roles"):
            value = data.get(key)
            if isinstance(value, list):
                return [row for row in value if isinstance(row, dict)]
        return [data]
    return []


def role_of(row: dict, key: str) -> str:
    raw = row.get(key)
    if isinstance(raw, str):
        try:
            raw = json.loads(raw)
        except ValueError:
            return ""
    if not isinstance(raw, dict):
        return ""
    role = raw.get("role")
    if isinstance(role, dict):
        role = role.get("role")
    return role if isinstance(role, str) else ""


def text_of(value) -> str:
    if value is None:
        return ""
    if isinstance(value, str):
        return value
    return json.dumps(value)


def ledger(root: Path, *, task: str | None = None, limit: int = 200) -> list[dict]:
    args = ["ledger", "--limit", str(limit)]
    if task:
        args += ["--task", task]
    return rows_of(cli_json(root, *args))


def session_rows(root: Path, role: str | None = None) -> list[dict]:
    args = ["sessions", "--limit", "100"] + (["--role", role] if role else [])
    return rows_of(cli_json(root, *args))


def wait_until(probe, seconds: float, interval: float = 0.25):
    """有界轮询：命中就返回它的值，超时给 None（不睡固定时间）。"""
    deadline = time.monotonic() + seconds
    value = None
    while time.monotonic() < deadline:
        value = probe()
        if value:
            return value
        time.sleep(interval)
    return value if value else None


def now_rfc3339() -> str:
    return datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")


def principal(role: str) -> dict:
    """`Principal` 是外部标记（externally tagged）的枚举，role 变体在线上套两层：
    `{"role": {"role": "caerulea"}}`（与 pi 插件的 `principalRole` 一致）。写成
    一层 `{"role": "caerulea"}` 的帧解不出来，而解不出来的帧只会让 client 静默断开
    这条连接 —— 没有答复，也没有日志。"""
    return {"role": {"role": role}}


def runtime_socket(root: Path) -> Path:
    """一个 owner root 绑定的 socket，按 onlyne-wire 的派生规则算（与 `onlyne ls` 无关）。"""
    # `workspace_digest`（crates/onlyne-wire/src/socket.rs）哈希的是规范路径的**小写**拼写。
    # 少了小写这一步，只有全小写的根（daily-workbench）凑巧对上，Alexandria 就找不到
    # 自己的注册文件。
    canonical = Path(os.path.realpath(root))
    normalized = str(canonical).replace("\\", "/").lower()
    return RUNTIME / f"{hashlib.sha256(normalized.encode()).hexdigest()[:16]}.sock"


def runtime_registration(root: Path) -> Path:
    return runtime_socket(root).with_suffix(".json")


def workspace_socket(workspace: Path) -> Path:
    return runtime_socket(workspace)


def run_id() -> str:
    """这套集群的 id：`up` 写一次，之后的每次调用都读回同一个值。"""
    if RUN_FILE.is_file():
        return RUN_FILE.read_text().strip()
    value = uuid.uuid4().hex[:8]
    RUN_FILE.write_text(value)
    return value


# 一次调用的 token。`run_id` 标的是这套集群（同一次 `up` 之后的每次调用共用它），
# 而一条投递要能被认出来自"本轮"还需要它 —— 断言发生在重挂的会话上，会话重挂会
# 先把上一轮没确认的帧重放一遍（上一轮 `im` 的入站就是这么回来的）。
INVOCATION = uuid.uuid4().hex[:6]


def marker(label: str) -> str:
    return f"{run_id()}.{label}.{INVOCATION}"


# ----------------------------------------------------------------- process bookkeeping

def registry() -> dict:
    if not REGISTRY.is_file():
        return {}
    try:
        return json.loads(REGISTRY.read_text())
    except ValueError:
        return {}


def save_registry(entries: dict) -> None:
    REGISTRY.parent.mkdir(parents=True, exist_ok=True)
    REGISTRY.write_text(json.dumps(entries, indent=2) + "\n")


def record(name: str, pid: int, expect: list[str]) -> None:
    entries = registry()
    entries[name] = {"pid": pid, "expect": expect}
    save_registry(entries)


def command_line(pid: int) -> str:
    out = subprocess.run([str(PS), "-o", "command=", "-p", str(pid)], capture_output=True,
                         text=True, check=False, env=child_env())
    return out.stdout.strip()


def owns(pid: int, expect: list[str]) -> bool:
    """PID 身份核对：命令行必须同时带上记录下来的每一段。`os.kill(pid, 0)` 认不出
    pid 复用，而杀掉一个复用来的陌生进程是本脚本最不该犯的错。"""
    if pid <= 0:
        return False
    line = command_line(pid)
    return bool(line) and all(needle in line for needle in expect)


def pid_of(name: str) -> int:
    entry = registry().get(name)
    if not entry:
        return 0
    pid = int(entry["pid"])
    return pid if owns(pid, entry["expect"]) else 0


def record_start(name: str, argv: list, expect: list[str], *, cwd=None, env=None) -> int:
    LOGS.mkdir(parents=True, exist_ok=True)
    RUNTIME.mkdir(parents=True, exist_ok=True)
    log = LOGS / f"{name}.log"
    handle = log.open("a")
    child = subprocess.Popen([str(a) for a in argv], cwd=str(cwd or ROOT),
                             stdin=subprocess.DEVNULL, stdout=handle, stderr=subprocess.STDOUT,
                             start_new_session=True, env=child_env(env))
    handle.close()
    record(name, child.pid, expect)
    say(f"started {name} pid {child.pid} -> {log}")
    return child.pid


def stop_pid(pid: int, expect: list[str]) -> bool:
    """按 PID 停一棵进程树（`start_new_session` 让每个服务自成进程组）。
    身份不符就不动手，并说清为什么。"""
    if not command_line(pid):
        return True  # 已经不在
    if not owns(pid, expect):
        say(f"pid {pid} is not the process this tree recorded; left alone")
        return False
    try:
        os.killpg(pid, signal.SIGTERM)
    except OSError:
        return True
    for _ in range(60):
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            return True
        time.sleep(0.1)
    try:
        os.killpg(pid, signal.SIGKILL)
    except OSError:
        pass
    return True


def stop_name(name: str) -> None:
    entry = registry().get(name)
    if not entry:
        return
    stop_pid(int(entry["pid"]), entry["expect"])


def stop_owned(names: list[str]) -> None:
    for name in names:
        stop_name(name)


# ----------------------------------------------------------------- cluster construction

def init_root(root: Path, port: int) -> None:
    root.mkdir(parents=True, exist_ok=True)
    if (root / ".onlyne" / "spec.toml").is_file():
        return
    run([SERVER, "init", "--root", root, "--listen", f"127.0.0.1:{port}"])
    say(f"initialized server root {root} on 127.0.0.1:{port}")


def append_spec(root: Path, text: str) -> None:
    with (root / ".onlyne" / "spec.toml").open("a") as handle:
        handle.write("\n" + text.rstrip() + "\n")


def spec_text(root: Path) -> str:
    return (root / ".onlyne" / "spec.toml").read_text()


def declared(root: Path, role: str) -> bool:
    return re.search(rf'^role = "{re.escape(role)}"$', spec_text(root), re.M) is not None


def reload_server(root: Path) -> None:
    run([ONLYNE, "--server-root", root, "reload"])


def server_port(root: Path) -> int:
    match = re.search(r'^listen = "127\.0\.0\.1:(\d+)"', spec_text(root), re.M)
    return int(match.group(1)) if match else 0


def declare_role(root: Path, workspace: Path, role: str, *, prose: str, senders: list[str],
                 targets: list[str], admin: bool = False, aggregate: str | None = None,
                 runtime: str | None = None) -> str:
    """一个角色行：钥匙从 `onlyne-client init` 铸，其余字段按本集群的部署写。

    init 打印的 `ed25519/…` 是**公钥**（身份），工作区 `.onlyne/keys/role.key` 里
    那 32 字节是私钥种子；两处不是同一个值，spec 要的是前者。`aggregate` 是标注，
    `runtime` 只写给人看的 drive/command。"""
    fragment = run([CLIENT, "init", "--workspace", workspace, "--role", role,
                    "--server-root", root], capture=True)
    key = re.search(r"ed25519/[A-Za-z0-9+/=]+", fragment)
    if not key:
        die(f"`onlyne-client init` printed no key for {role}: {fragment}")
    lines = [
        "[[client]]",
        f'role = "{role}"',
        f'key = "{key.group(0)}"',
        f'admin = {"true" if admin else "false"}',
        "max_sessions = 1",
    ]
    if aggregate:
        lines.append(f'aggregate = "{aggregate}"')
    lines.append(f"allowed_senders = {json.dumps(senders)}")
    lines.append(f"allowed_targets = {json.dumps(targets)}")
    lines.append(f"prose = {json.dumps(prose)}")
    if runtime:
        lines += ["", "[client.runtime]", 'drive = "plugin"', f'command = ["{runtime}"]']
    append_spec(root, "\n".join(lines))
    return key.group(0)


def resident_session(workspace: Path, session_id: str) -> None:
    """把工作区写成常驻形态：`placement = "external"` 表示进程不由 client 拉起，
    `[client.session] scope = "role"` + 固定 `session_id` 表示会话跨投递活着。

    这两个键是拓扑（机器属性）而 role/key 是身份，所以它们落在工作区的
    `.onlyne/config.toml` 里，而 `placement` 必须写在第一个表头之前 —— 表头之后
    写的键属于那张表。"""
    path = workspace / ".onlyne" / "config.toml"
    text = path.read_text()
    # `init` 写的模板里 `placement` 只出现在注释里（`# placement = "headless"`），
    # 所以判断要看有没有**生效的**那一行，而不是这段文字出现过没有。
    if re.search(r"^placement\s*=", text, re.M):
        return
    block = (f'placement = "external"\n\n[client.session]\nscope = "role"\n'
             f'session_id = "{session_id}"\nsession_alias = "{session_id}"\n\n')
    lines = text.splitlines(keepends=True)
    cut = next((index for index, line in enumerate(lines) if line.lstrip().startswith("[")),
               len(lines))
    path.write_text("".join(lines[:cut]) + "\n" + block + "".join(lines[cut:]))


def foreign_body(member: Path, role: str, session_id: str, workspace: Path) -> None:
    """成员集群里那份 `pi` 侧身份：role + session 别名 + 指回中心的工作区。
    本机 `Alexandria/.pi/onlyne.json` 就是这个形状。"""
    directory = member / ".pi"
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "onlyne.json").write_text(json.dumps({
        "enabled": True,
        "resident": True,
        "role": role,
        "sessionId": session_id,
        "workspace": str(workspace),
        "watch": {"autoStart": True},
    }, indent=2) + "\n")


def gateway_root(center: Path, gateway_key: str, chat: str) -> Path:
    """网关根：`onlyne-im init` 起稿，再补 `[[im.backend]]`（fake）与 `[[im.push]]`
    （入站触发器）。

    这一段是幂等的：`up` 每次都会调它，所以**所有** `[[im.backend]]` 与
    `[[im.push]]` 块（包括 init 模板里的 telegram 示例、以及上一次运行写下的那两份）
    先整块摘掉，再在文件末尾写回一份自己的。别的表和注释原样留着 —— 摘除按表头行
    判断，注释行不是表头，所以它们不会被当成块的一部分删掉，也不会随重跑堆积。
    """
    root = center / "im"
    if not (root / ".onlyne" / "config.toml").is_file():
        run([IM, "init", "--root", root, "--role", GW_ROLE, "--server-root", center], capture=True)
    keys = root / ".onlyne" / "keys"
    keys.mkdir(parents=True, exist_ok=True)
    (keys / "role.key").write_bytes(bytes.fromhex(gateway_key))
    (keys / "role.key").chmod(0o600)
    credentials = root / ".onlyne" / "credentials"
    credentials.mkdir(parents=True, exist_ok=True)
    token = credentials / "fake.token"
    token.write_text(FAKE_TOKEN + "\n")
    token.chmod(0o600)
    trigger = root / "hooks" / "im-in.py"
    trigger.parent.mkdir(parents=True, exist_ok=True)
    # 模板里有 `{chat}`/`{text}` 这类生成脚本自己的花括号，所以用 `$name` 替换而不是
    # `str.format`。
    trigger.write_text(string.Template(TRIGGER_SCRIPT).substitute(
        onlyne=ONLYNE, root=center, house=HOUSE, gateway=GW_ROLE))
    trigger.chmod(0o755)
    config = root / ".onlyne" / "config.toml"
    backend = (f'[[im.backend]]\ngateway = "fake"\n'
               f'run = ["{BIN / "onlyne-im-backend-fake"}", "--gateway", "fake"]\n')
    push = f'[[im.push]]\nmode = "trigger"\nrun = ["python3", "{trigger}"]\ntimeout = "15s"\n'
    owned = ("[[im.backend]]", "[[im.push]]")
    keep: list[str] = []
    dropping = False
    for line in config.read_text().splitlines():
        stripped = line.strip()
        header = stripped.startswith("[") and stripped.endswith("]")
        if header:
            dropping = stripped in owned
        if not dropping:
            keep.append(line)
    while keep and not keep[-1].strip():
        keep.pop()
    config.write_text("\n".join(keep) + "\n\n" + backend + "\n" + push)
    return root


TRIGGER_SCRIPT = '''#!/usr/bin/env python3
"""入站触发器：把平台事件投给中心的大管家，投递正文首行回带这条会话的 chat key
（docs/v2.3-CHAT-PROTOCOL.md §5.1）。`ONLYNE_SOCKET` 由网关注入。"""
import json
import subprocess
import sys

payload = json.load(sys.stdin)
event = payload.get("event") or {}
chat = payload["chat"]
where = f'{payload.get("platform", "?")}/{payload.get("conversation", "?")}'
text = event.get("text") or "(no text)"
body = f"<chat:{chat}>\\n\\n{text}"
out = subprocess.run(["$onlyne", "--server-root", "$root", "send", "--as", "admin",
                      "--from", "$gateway", "--to", "$house", "--note", "--text", body,
                      "--family", chat, "--force", "--yes-i-am-supervisor-not-other-role"],
                     capture_output=True, text=True)
print(f"ingress {where} -> $house: rc={out.returncode} {out.stdout.strip()[:200]}", flush=True)
if out.returncode != 0:
    print(out.stderr.strip()[:500], file=sys.stderr, flush=True)
    sys.exit(out.returncode)
'''


def need_binaries() -> None:
    for binary in (ONLYNE, SERVER, CLIENT, IM, FAKE, BIN / "onlyne-im-backend-fake"):
        if not binary.exists():
            die(f"missing binary {binary}; run cargo build --workspace first")


def free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


# ----------------------------------------------------------------- adapter client

def read_frame(sock: socket.socket):
    def recv_exact(count: int) -> bytes:
        chunks = b""
        while len(chunks) < count:
            part = sock.recv(count - len(chunks))
            if not part:
                raise ConnectionError("peer closed")
            chunks += part
        return chunks

    header = recv_exact(4)
    length = struct.unpack(">I", header)[0]
    return json.loads(recv_exact(length).decode())


class AdapterMount:
    """一条真挂载：连到真实 client 的 workspace socket，说真实 adapter 帧。

    帧格式是 onlyne-wire 的长度前缀 JSON（4 字节大端 + UTF-8 JSON），与 pi 插件
    自己走的是同一条路。挂载声明的会话就是 `hello.args.mount.session` —— §4.1 要读的
    那个声明。
    """

    def __init__(self, workspace: Path, role: str, *, session: str | None = None,
                 label: str = "", capabilities=None):
        self.workspace = Path(workspace)
        self.role = role
        self.session = session
        self.label = label or f"{role}/{session or '-'}"
        self.capabilities = list(capabilities or CAPS)
        self.path = workspace_socket(self.workspace)
        self._ids = itertools.count(1)
        self._write_lock = threading.Lock()
        self._waiters: dict[int, "queue.Queue"] = {}
        self._waiters_lock = threading.Lock()
        self._beats = SEQ_BASE
        # 空闲心跳记的是这条连接最后服务的那条投递；没有投递时退回它声明的会话名。
        self._beat_lock = threading.Lock()
        self._beat_task: str | None = None
        self._beat_session: str | None = None
        self._beat_thread: threading.Thread | None = None
        self.frames: "queue.Queue[dict]" = queue.Queue()
        # `next_assign_with` 看过的、不是本轮的投递正文：断言失败时能分清"有帧，
        # 但没有一条是本轮的"和"根本没有帧"。
        self.stale: list[str] = []
        self.closed = threading.Event()
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(10)
        self.sock.connect(str(self.path))
        self.sock.settimeout(None)
        threading.Thread(target=self._read_loop, daemon=True, name=f"read-{self.label}").start()
        mount = {"role": role, "pid": os.getpid()}
        if session:
            mount["session"] = session
        self.welcome = self.request("hello", {
            "protocol": 1,
            "plugin": "onlyne-lifecycle-resident",
            "version": "1",
            "kind": "agent",
            "capabilities": self.capabilities,
            "mount": mount,
        })
        # 客户端把 hello 的答复包成 `HostOp::Welcome`（`AdapterServer::accept`：`ResBody::ok(data)`
        # 里的 data 就是那个 host 帧），所以 ack 在 `args` 下面。
        if isinstance(self.welcome, dict) and self.welcome.get("op") == "welcome":
            self.welcome = self.welcome.get("args") or {}
        self.welcome = self.welcome or {}

    # -- framing

    def _write(self, frame: dict) -> None:
        body = json.dumps(frame).encode()
        with self._write_lock:
            self.sock.sendall(struct.pack(">I", len(body)) + body)

    def _read_loop(self) -> None:
        try:
            while True:
                frame = read_frame(self.sock)
                if "reply_to" in frame:
                    with self._waiters_lock:
                        waiter = self._waiters.get(frame["reply_to"])
                    if waiter is not None:
                        waiter.put(frame)
                    continue
                if frame.get("op") and "id" in frame:
                    # 宿主请求（probe/recycle/open）：本进程没有可答的观测，回一个空答，
                    # 让宿主不悬在那儿。真正的驱动面是 assign 通知（无 id）。
                    self._write({"reply_to": frame["id"], "ok": True, "data": None})
                    continue
                if frame.get("op") == "assign":
                    args = frame.get("args") or {}
                    with self._beat_lock:
                        self._beat_task = args.get("task_id") or self._beat_task
                        self._beat_session = args.get("session_id") or self._beat_session
                self.frames.put(frame)
        except Exception:
            pass
        finally:
            self.closed.set()

    def request(self, op: str, args: dict, timeout: float = 10.0) -> dict | None:
        frame_id = next(self._ids)
        waiter: "queue.Queue" = queue.Queue(maxsize=1)
        with self._waiters_lock:
            self._waiters[frame_id] = waiter
        self._write({"id": frame_id, "op": op, "args": args})
        try:
            frame = waiter.get(timeout=timeout)
        except queue.Empty:
            raise TimeoutError(f"{self.label}: no answer to {op}")
        finally:
            with self._waiters_lock:
                self._waiters.pop(frame_id, None)
        if not frame.get("ok"):
            raise RuntimeError(f"{self.label}: {op} refused: {json.dumps(frame.get('error'))}")
        return frame.get("data")

    def notify(self, op: str, args: dict) -> None:
        self._write({"op": op, "args": args})

    def next_frame(self, timeout: float) -> dict | None:
        try:
            return self.frames.get(timeout=timeout)
        except queue.Empty:
            return None

    def next_assign(self, timeout: float) -> dict | None:
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            frame = self.next_frame(max(0.1, deadline - time.monotonic()))
            if frame is None:
                return None
            if frame.get("op") == "assign":
                return frame["args"]
        return None

    def next_assign_with(self, needle: str, timeout: float) -> dict | None:
        """等一条正文里带 `needle` 的投递，`timeout` 是本轮所有候选帧共用的预算
        （与直接调 `next_assign` 等的时间一样长）。

        会话重挂会先把上一轮没确认的帧重放一遍，所以第一条投递不一定属于本轮：
        拿它去断言，旧帧就能决定断言的结果。
        """
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return None
            assign = self.next_assign(remaining)
            if assign is None:
                return None
            if needle in assign_text(assign):
                return assign
            self.stale.append(assign_text(assign)[:80])

    # -- plugin ops

    def say_note(self, to: str, text: str, *, admin: bool = False) -> None:
        envelope = {"protocol": 1, "id": str(uuid.uuid4()), "kind": "note",
                    "from": principal(self.role), "to": principal(to),
                    "body": {"text": text}, "ts": now_rfc3339(), "admin": admin}
        self.request("send", envelope)

    def say_task(self, to: str, text: str, *, family: str | None = None) -> str:
        """发一个任务，返回它的 task id（信封的 id 与 causality.task 都是我自己写的，
        所以子集群那一跳的 id 在这边就知道）。"""
        task_id = str(uuid.uuid4())
        causality = {"task": task_id, "hop": 0, "attempt": 0}
        if family:
            causality["family"] = family
        envelope = {"protocol": 1, "id": str(uuid.uuid4()), "op_id": f"o-{uuid.uuid4()}",
                    "kind": "task", "from": principal(self.role), "to": principal(to),
                    "causality": causality, "body": {"text": text},
                    "ts": now_rfc3339(), "admin": False}
        self.request("send", envelope)
        return task_id

    def handoff(self, task_id: str, to: str, text: str) -> dict | None:
        return self.request("handoff", {"task_id": task_id, "to": to, "text": text})

    def beat(self, task_id: str, *, session_id: str = "", agent: str = "running") -> None:
        # 交付循环与空闲心跳线程都会发心跳，序号必须在锁里取，否则同一秒的两帧撞号，
        # host 会把后面那帧当 replay 丢掉。
        with self._beat_lock:
            self._beats += 1
            seq = self._beats
        self.notify("report", {"kind": "heartbeat", "data": {
            "task_id": task_id, "session_id": session_id, "generation": 1, "seq": seq,
            # 插件只声明它能见证的维度；其余（delivery/recovery/generation_live/
            # isolate_after/terminate_after/mismatch_count）由 host 按自己的记录改写。
            "observed": {
                "version": {"generation": 1, "seq": seq},
                "generation_live": True, "isolate_after": 1, "terminate_after": 3,
                "mismatch_count": 0, "agent": agent, "delivery": "none",
                "resource": "attached", "recovery": "none",
            },
        }})

    def start_heartbeat(self, interval: float = BEAT_INTERVAL) -> None:
        """空闲时也心跳：常驻连接不投递的时候照样报告自己还活着。

        client 的看门狗按静默判死一个 session（`heartbeat_silence`，30 秒），一次空闲
        的 30 秒就够它退休一个还在等活的常驻 session —— 下一次投递落到一个已经关掉的
        session 上，行的终态就是 `session_dead`。有任务在手上时这条线程报的是那条任务
        的 session，与交付循环里那几次心跳同源。
        """
        if self._beat_thread is not None:
            return

        def loop() -> None:
            while not self.closed.wait(interval):
                with self._beat_lock:
                    task = self._beat_task or self.session or self.role
                    session = self._beat_session or self.session or ""
                try:
                    self.beat(task, session_id=session)
                except Exception:
                    return

        self._beat_thread = threading.Thread(target=loop, daemon=True,
                                             name=f"beat-{self.label}")
        self._beat_thread.start()

    def complete(self, task_id: str, head: str, details: str | None = None,
                 outcome: str = "done") -> None:
        data = {"task_id": task_id, "outcome": outcome, "head": head}
        if details:
            data["details"] = details
        self.request("report", {"kind": "complete", "data": data})

    def detach(self, reason: str) -> None:
        self.request("detach", {"reason": reason})

    def close(self) -> None:
        try:
            self.sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self.sock.close()
        self.closed.set()


def mount(workspace: Path, role: str, *, session: str | None = None, label: str = "") -> AdapterMount:
    return AdapterMount(workspace, role, session=session, label=label)


def assign_kind(assign: dict) -> str:
    return (assign.get("envelope") or {}).get("kind", "")


def miss_detail(connection: AdapterMount, needle: str) -> str:
    """`next_assign_with` 没等到时的说明：没有帧，和有帧但都不是本轮的，是两种毛病。"""
    if not connection.stale:
        return f"no assign frame arrived for {needle!r}"
    return (f"no assign carried {needle!r}; skipped {len(connection.stale)} frame(s), "
            f"latest {connection.stale[-1]!r}")


def assign_text(assign: dict) -> str:
    return assign.get("text", "") or ""


def body_text(assign: dict) -> str:
    return (((assign.get("envelope") or {}).get("body")) or {}).get("text", "") or ""


def chat_key(text: str) -> str:
    match = re.search(r"<chat:([^>]+)>", text)
    return match.group(1) if match else ""


# ----------------------------------------------------------------- up

def up(*, as_json: bool) -> None:
    need_binaries()
    TREE.mkdir(parents=True, exist_ok=True)
    LOGS.mkdir(parents=True, exist_ok=True)
    value = run_id()
    ports = {CENTER: free_port(), ALEXANDRIA: free_port(), CHAMBER: free_port()}
    for root, port in ports.items():
        init_root(root, port)

    names = []
    try:
        for tag, root in (("center", CENTER), ("alexandria", ALEXANDRIA), ("chamber", CHAMBER)):
            name = f"{tag}-server"
            names.append(name)
            if not pid_of(name):
                record_start(name, [SERVER, "run", "--root", root],
                             [str(SERVER), str(root)])

        for tag, root in (("center", CENTER), ("alexandria", ALEXANDRIA), ("chamber", CHAMBER)):
            wait_ready(root, tag)

        build_center()
        build_alexandria()
        build_chamber()
        ensure_worker_sessions()
        gateway_key = (CENTER / ".keymint" / GW_ROLE / ".onlyne" / "keys" / "role.key").read_bytes().hex()
        gateway_root(CENTER, gateway_key, FAKE_CHAT)

        for name, workspace in clients():
            if pid_of(name):
                continue
            record_start(name, [CLIENT, "run", "--workspace", workspace],
                         [str(CLIENT), str(workspace)], cwd=workspace)

        script = TREE / "chamber-worker-script.json"
        script.write_text(json.dumps({
            "hello": {"capabilities": CAPS},
            "steps": [
                {"wait_assign": True},
                {"report": "heartbeat"},
                # head_from assign_body：把投递给它的正文原样带回，于是中心那一跳的结果
                # 里带着只可能来自子集群的字节。
                {"complete": {"outcome": "done", "head_from": "assign_body"}},
            ],
            "repeat": True,
        }) + "\n")
        chamber_ws = CHAMBER / ".onlyne" / "ws" / CH_WORKER
        name = "chamber-agent-fake"
        names.append(name)
        if not pid_of(name):
            record_start(name, [FAKE, "--workspace", chamber_ws, "--script", script],
                         [str(FAKE), str(chamber_ws)])

        name = "resident"
        names.append(name)
        if not pid_of(name):
            record_start(name, [sys.executable, Path(__file__).resolve(), "resident"],
                         [str(Path(__file__).resolve()), "resident"], cwd=TREE)

        name = "center-gateway"
        names.append(name)
        if not pid_of(name):
            start_gateway({})

        for name, workspace in clients():
            wait_client(workspace, name)
        wait_resident()
        wait_gateway()
    except BaseException as error:
        # 失败只停本脚本记下的 PID；日志留在 TREE/logs 里给人看。
        say(f"up failed; logs are in {LOGS}; run `stop` to bring this tree down")
        stop_owned(names)
        sys.exit(1 if isinstance(error, SystemExit) else 2)

    shape = describe(value)
    SHAPE_FILE.write_text(json.dumps(shape, indent=2) + "\n")
    if as_json:
        print(json.dumps(shape, indent=2))
    else:
        say(f"cluster up, run id {value}")
        for line in render_shape(shape):
            say("  " + line)
        say(f"  logs: {LOGS}")
        say(f"  paths (for a real runtime): {SHAPE_FILE}")
        say("  next: python3 examples/lifecycle/run.py lifecycle")


def clients() -> list[tuple[str, Path]]:
    return [
        ("center-client-dpdp", CENTER / HOUSE),
        ("center-client-caerulea", CENTER / REP_A),
        ("center-client-lazuli", CENTER / REP_B),
        ("alexandria-client-caerulea", ALEXANDRIA / "ws" / REP_A),
        ("alexandria-client-scriber", ALEXANDRIA / "ws" / ALX_WORKER),
        ("alexandria-client-master", ALEXANDRIA / "ws" / ALX_MASTER),
        ("chamber-client-lazuli", CHAMBER / ".onlyne" / "ws" / REP_B),
        ("chamber-client-worker", CHAMBER / ".onlyne" / "ws" / CH_WORKER),
    ]


def start_gateway(env: dict) -> int:
    gateway = CENTER / "im"
    return record_start("center-gateway",
                        [IM, "run", "--root", gateway, "--socket", runtime_socket(CENTER)],
                        [str(IM), str(gateway)], cwd=CENTER, env=env)


def wait_ready(root: Path, tag: str, seconds: float = 30.0) -> None:
    """就绪 = 注册文件出现（daemon 自己声明的"I am serving here"）+ admin 面答话。"""
    registration = runtime_registration(root)
    wait_until(registration.is_file, seconds, 0.1) or die(f"{tag} server published no registration")
    answered = wait_until(lambda: call_ok([ONLYNE, "--server-root", root, "roles", "--json"]), seconds)
    if not answered:
        die(f"{tag} server never answered `roles`; log {LOGS / (tag + '-server.log')}")


def wait_client(workspace: Path, name: str, seconds: float = 30.0) -> None:
    def online() -> bool:
        out = cli_out([CLIENT, "status", "--workspace", workspace])
        return "client running" in out.stdout

    wait_until(online, seconds, 0.2) or die(f"{name} never reported running; log {LOGS / (name + '.log')}")


def wait_resident(seconds: float = 30.0) -> None:
    alive = wait_until(lambda: RESIDENT_STATE.is_file() and json.loads(RESIDENT_STATE.read_text()),
                       seconds, 0.2)
    state = alive or {}
    if not isinstance(state, dict) or not state.get("ready"):
        die(f"resident never mounted: {RESIDENT_STATE}")
    say(f"resident mounted {len(state['mounts'])} connections")


def wait_gateway(seconds: float = 30.0) -> None:
    gateway = CENTER / "im"
    ready = wait_until(lambda: call_ok([IM, "check", "--root", gateway]), seconds, 0.3)
    ready or die(f"gateway never passed `onlyne-im check`; log {LOGS / 'center-gateway.log'}")


def build_center() -> None:
    if declared(CENTER, GW_ROLE):
        return
    square = [SUP, HOUSE, REP_A, REP_B, GW_ROLE]
    mint = CENTER / ".keymint"
    declare_role(CENTER, mint / SUP, SUP, admin=True, prose=f"{SUP} of the centre cluster",
                 senders=square, targets=[HOUSE, REP_A, REP_B, GW_ROLE])
    declare_role(CENTER, mint / GW_ROLE, GW_ROLE, prose="IM gateway role",
                 senders=square, targets=[SUP, HOUSE, REP_A, REP_B])
    for role, aggregate in ((REP_A, ALX_NAME), (REP_B, CHAMBER_NAME)):
        workspace = CENTER / role
        declare_role(CENTER, workspace, role, admin=False, aggregate=aggregate,
                     prose=f"{role}, representative of {aggregate}",
                     senders=square, targets=[SUP, HOUSE, REP_A, REP_B, GW_ROLE],
                     runtime="omp")
        resident_session(workspace, SESSIONS[role])
    declare_role(CENTER, CENTER / HOUSE, HOUSE, admin=True, prose="the centre's house agent",
                 senders=square, targets=["*", *square], runtime="omp")
    resident_session(CENTER / HOUSE, SESSIONS[HOUSE])
    reload_server(CENTER)
    say(f"declared centre roles {', '.join(CENTER_ROLES)}")
    key = (CENTER / ".keymint" / GW_ROLE / ".onlyne" / "keys" / "role.key").read_bytes().hex()
    gateway = gateway_root(CENTER, key, FAKE_CHAT)
    # 会话工作区 + 它自己的角色行由产品自己的 `generate` 写（它按 admin socket 落 ACL）。
    if not (gateway / "conversations" / GW_SLUG).is_dir():
        run([IM, "generate", "--root", gateway, "--gateway", "fake",
             "--conversation", FAKE_CHAT, "--slug", GW_SLUG, "--peer", HOUSE,
             "--server-root", CENTER])
        reload_server(CENTER)


def build_alexandria() -> None:
    if declared(ALEXANDRIA, ALX_MASTER):
        return
    declare_role(ALEXANDRIA, ALEXANDRIA / ".keymint" / SUP, SUP, admin=True,
                 prose=f"{SUP} of {ALX_NAME}", senders=["*"], targets=[REP_A, ALX_WORKER, ALX_MASTER])
    declare_role(ALEXANDRIA, ALEXANDRIA / "ws" / REP_A, REP_A,
                 prose="representative of the knowledge cluster",
                 senders=["*", SUP, REP_A, ALX_WORKER, ALX_MASTER],
                 targets=["*", SUP, REP_A, ALX_WORKER, ALX_MASTER])
    share_identity(ALEXANDRIA, ALEXANDRIA / "ws" / REP_A, REP_A,
                   CENTER, CENTER / REP_A)
    declare_role(ALEXANDRIA, ALEXANDRIA / "ws" / ALX_WORKER, ALX_WORKER,
                 prose="entry from the knowledge workflow", senders=[SUP, REP_A],
                 targets=[ALX_MASTER])
    declare_role(ALEXANDRIA, ALEXANDRIA / "ws" / ALX_MASTER, ALX_MASTER,
                 prose="the knowledge workflow's reading desk", senders=[ALX_WORKER, REP_A],
                 targets=[REP_A, SUP])
    # 成员集群内部的 worker 也是常驻 body：它们的 agent 进程是外部的（由 resident 拉起、
    # 自己拨 client 的 socket），所以工作区要写 `placement = "external"` + 一个 role 作用
    # 域的固定会话。少了这一段，client 会去探测窗格宿主（macOS 上探到 tern），而一个
    # 已结束的会话之后的下一次投递会试着为没有 command 的角色开 tern 窗格，行直接
    # `rejected`（`tern spawn requires a command`）。
    resident_session(ALEXANDRIA / "ws" / ALX_WORKER, SESSIONS[ALX_WORKER])
    resident_session(ALEXANDRIA / "ws" / ALX_MASTER, SESSIONS[ALX_MASTER])
    resident_session(ALEXANDRIA / "ws" / REP_A, SESSIONS[REP_A])
    foreign_body(ALEXANDRIA, REP_A, SESSIONS[REP_A], CENTER / REP_A)
    reload_server(ALEXANDRIA)


def build_chamber() -> None:
    if declared(CHAMBER, CH_WORKER):
        return
    declare_role(CHAMBER, CHAMBER / ".keymint" / SUP, SUP, admin=True,
                 prose=f"{SUP} of {CHAMBER_NAME}", senders=["*"], targets=[REP_B, CH_WORKER])
    declare_role(CHAMBER, CHAMBER / ".onlyne" / "ws" / REP_B, REP_B,
                 prose="representative of the research cluster",
                 senders=["*", SUP, REP_B, CH_WORKER], targets=["*", SUP, REP_B, CH_WORKER])
    share_identity(CHAMBER, CHAMBER / ".onlyne" / "ws" / REP_B, REP_B,
                   CENTER, CENTER / REP_B)
    declare_role(CHAMBER, CHAMBER / ".onlyne" / "ws" / CH_WORKER, CH_WORKER,
                 prose="a research project role", senders=[SUP, REP_B],
                 targets=[REP_B, SUP])
    # 与 alexandria 的 worker 同理：科研项目角色的 body 在成员目录里，工作区是常驻形态。
    resident_session(CHAMBER / ".onlyne" / "ws" / CH_WORKER, SESSIONS[CH_WORKER])
    resident_session(CHAMBER / ".onlyne" / "ws" / REP_B, SESSIONS[REP_B])
    foreign_body(CHAMBER, REP_B, SESSIONS[REP_B], CENTER / REP_B)
    reload_server(CHAMBER)


def ensure_worker_sessions() -> None:
    """成员集群内部常驻 worker 的工作区形态（幂等：已经写过就跳过）。

    `build_alexandria`/`build_chamber` 在角色声明过之后整体早退，所以一个旧树上的
    `up` 不会经过那里面的 `resident_session` 调用。这一段让重跑也能把缺口补上 —— 它只
    改工作区的配置文件；已经在跑的 client 要重启才读它，所以验收仍该从新树起。
    """
    for workspace, role in (
        (ALEXANDRIA / "ws" / ALX_WORKER, ALX_WORKER),
        (ALEXANDRIA / "ws" / ALX_MASTER, ALX_MASTER),
        (CHAMBER / ".onlyne" / "ws" / CH_WORKER, CH_WORKER),
    ):
        if (workspace / ".onlyne" / "config.toml").is_file():
            resident_session(workspace, SESSIONS[role])


def share_identity(root: Path, workspace: Path, role: str, source_root: Path,
                   source_workspace: Path, source_role: str | None = None) -> None:
    """同一个名字在父层与子层同名同钥匙：公钥抄进本 root 的 spec 行，私钥种子抄进
    工作区。本机部署就是这样（Alexandria 的 caerulea 与中心那条 byte 相同），父层
    靠它认出这条连接 —— 除此之外联邦没有任何 core 侧机制。
    """
    source_role = source_role or role
    public = re.search(rf'^role = "{re.escape(source_role)}"\nkey = "(ed25519/[^"]+)"',
                       spec_text(source_root), re.M)
    seed = source_workspace / ".onlyne" / "keys" / "role.key"
    if not public or not seed.is_file():
        die(f"no identity to share for {source_role} in {source_root}")
    pattern = re.compile(rf'(^role = "{re.escape(role)}"\nkey = ")ed25519/[^"]+(")', re.M)
    text = pattern.sub(lambda match: match.group(1) + public.group(1) + match.group(2),
                       spec_text(root))
    (root / ".onlyne" / "spec.toml").write_text(text)
    key_file = workspace / ".onlyne" / "keys" / "role.key"
    key_file.write_bytes(seed.read_bytes())
    key_file.chmod(0o600)


def describe(value: str) -> dict:
    house = CENTER / HOUSE
    return {
        "run_id": value,
        "tree": str(TREE),
        "logs": str(LOGS),
        "runtime_dir": str(RUNTIME),
        "clusters": {
            "center": cluster_shape(CENTER, CENTER_ROLES),
            "alexandria": cluster_shape(ALEXANDRIA, [SUP, REP_A, ALX_WORKER, ALX_MASTER]),
            "chamber": cluster_shape(CHAMBER, [SUP, REP_B, CH_WORKER]),
        },
        "workspaces": {name: str(workspace) for name, workspace in clients()},
        "resident_session": {"role": HOUSE, "workspace": str(house),
                             "server_root": str(CENTER), "socket": str(workspace_socket(house)),
                             "session_id": SESSIONS[HOUSE]},
        # 一个真运行时的落点：角色工作区在中心，body 的 cwd 在成员集群那边，
        # 与 `Alexandria/.pi/onlyne.json` 指回中心工作区是同一个形状。
        "bodies": {
            REP_A: {"member_root": str(ALEXANDRIA), "workspace": str(CENTER / REP_A),
                    "role": REP_A, "session_id": SESSIONS[REP_A]},
            REP_B: {"member_root": str(CHAMBER), "workspace": str(CENTER / REP_B),
                    "role": REP_B, "session_id": SESSIONS[REP_B]},
        },
        "gateway": {"root": str(CENTER / "im"), "role": GW_ROLE,
                    "socket": str(runtime_socket(CENTER))},
        # 谁可以给常驻会话发 note（ACL 里已经放行的那几个名字）。
        "note_from": [SUP, REP_A, REP_B, GW_ROLE],
        "commands": {
            "lifecycle": f"python3 examples/lifecycle/run.py lifecycle",
            "federation": f"python3 examples/lifecycle/run.py federation",
            "im": f"python3 examples/lifecycle/run.py im",
            "idle": f"python3 examples/lifecycle/run.py idle",
            "status": f"python3 examples/lifecycle/run.py status",
            "stop": f"python3 examples/lifecycle/run.py stop",
        },
    }


def cluster_shape(root: Path, roles: list[str]) -> dict:
    return {"server_root": str(root), "port": server_port(root), "roles": roles,
            "socket": str(runtime_socket(root))}


def render_shape(shape: dict) -> list[str]:
    lines = [f"tree {shape['tree']}"]
    for name, cluster in shape["clusters"].items():
        lines.append(f"{name}: {cluster['server_root']} 127.0.0.1:{cluster['port']} "
                     f"roles={','.join(cluster['roles'])}")
    lines.append(f"resident session {shape['resident_session']['role']}@"
                 f"{shape['resident_session']['workspace']} ({shape['resident_session']['session_id']})")
    lines.append(f"gateway {shape['gateway']['root']} role {shape['gateway']['role']}")
    return lines


# ----------------------------------------------------------------- resident

def resident() -> None:
    """常驻 agent 侧：真挂载，替三个集群的常驻会话说话。

    * 中心两个代表（caerulea/lazuli）：各一条父层连接，等父层的投递。
    * 子集群两个代表：各一条子层连接，代表本人在这里把父层的活发进子集群，再把
      子集群的 note 带回父层。
    * alexandria 的 scriber/master：子集群内部两跳，真写文件、真交活。
    """
    value = run_id()
    mounts: dict[str, AdapterMount] = {}

    def connect(key: str, workspace: Path, role: str, session: str | None = None) -> None:
        # client 的 socket 由各自的进程绑定，谁先起来都可能：有界重试，不睡固定时间。
        deadline = time.monotonic() + 60.0
        last = None
        while time.monotonic() < deadline:
            try:
                mounts[key] = mount(workspace, role, session=session, label=f"{key}:{role}")
                return
            except OSError as error:
                last = error
                time.sleep(0.25)
        die(f"resident could not mount {key} on {workspace}: {last}")

    connect("parent-a", CENTER / REP_A, REP_A)
    connect("child-a", ALEXANDRIA / "ws" / REP_A, REP_A, session=SESSIONS[REP_A])
    connect("parent-b", CENTER / REP_B, REP_B)
    connect("child-b", CHAMBER / ".onlyne" / "ws" / REP_B, REP_B, session=SESSIONS[REP_B])
    connect("scribe", ALEXANDRIA / "ws" / ALX_WORKER, ALX_WORKER,
            session=SESSIONS[ALX_WORKER])
    connect("master", ALEXANDRIA / "ws" / ALX_MASTER, ALX_MASTER,
            session=SESSIONS[ALX_MASTER])
    # 每条常驻连接在等活的时候也心跳：空闲 30 秒会被 client 的看门狗判死。
    for item in mounts.values():
        item.start_heartbeat()

    RESIDENT_STATE.write_text(json.dumps({
        "pid": os.getpid(),
        "ready": True,
        "run_id": value,
        "mounts": [{"key": key, "label": item.label, "session": item.session,
                    "welcome": item.welcome} for key, item in mounts.items()],
    }, indent=2) + "\n")
    say(f"resident {os.getpid()} mounted {len(mounts)} connections")

    stop = threading.Event()

    def on_signal(*_):
        stop.set()

    for sig in (signal.SIGTERM, signal.SIGINT):
        signal.signal(sig, on_signal)

    # 子集群的 scriber：真做事（落一个文件），handoff 给 master，等结果回来，再交活。
    #
    # `handoff` 只铸一个子任务，父任务不会被它结算（`crates/onlyne-client/src/session/
    # handoff.rs` 的中继不结算父行，`settle.rs` 把交活写成单独的一步）。少了后面的
    # `complete`，scriber 的会话一直占着槽位、手上的投递永远不结 —— 下一轮 federation
    # 就派不进来。
    def scribe() -> None:
        connection = mounts["scribe"]
        while not stop.is_set():
            assign = connection.next_assign(1.0)
            if assign is None:
                continue
            task = assign["task_id"]
            text = assign_text(assign)
            session = assign.get("session_id") or ""
            artifacts = ALEXANDRIA / "artifacts"
            artifacts.mkdir(parents=True, exist_ok=True)
            note = artifacts / f"{task}.txt"
            note.write_text(f"{text}\n")
            connection.beat(task, session_id=session)
            connection.handoff(task, ALX_MASTER, f"{text}\nartifact: {note}")
            # 等 master 交活：master 的 head 里带着这个产物名（`{task}.txt`），所以按它
            # 认出来。family 不参与匹配 —— handoff 的子任务继承的是父层那一族的 id，
            # 不是我这里能事先知道的。
            deadline = time.monotonic() + 90.0
            head = ""
            while time.monotonic() < deadline:
                head = result_named(ALEXANDRIA, note.name)
                if head:
                    break
                # 等在手上这条父任务的心跳：空闲 30 秒会被看门狗判死。
                connection.beat(task, session_id=session)
                time.sleep(3.0)
            if not head:
                # 等不到结果就是失败：把失败说成 done，账本上就是一次假成功。
                connection.complete(task, f"{ALX_WORKER}@{ALX_NAME}: no result from {ALX_MASTER}",
                                    details=f"artifact: {note}", outcome="failed")
                continue
            connection.complete(task, head, details=f"artifact: {note}")

    # 子集群的 master：收下那一跳，交活，并把结果当 note 发给本集群的代表。
    def master() -> None:
        connection = mounts["master"]
        while not stop.is_set():
            assign = connection.next_assign(1.0)
            if assign is None:
                continue
            task = assign["task_id"]
            text = assign_text(assign)
            # head 里带上那一跳的产物名：它由 scriber 的 task id 命名，于是父层读到的
            # 结果能对回子集群账本上的一行，而两边的 id 都是真的。
            artifact = next((line.split(": ", 1)[1].strip() for line in text.splitlines()
                             if line.startswith("artifact: ")), "")
            name = artifact.rsplit("/", 1)[-1] if artifact else ""
            # head 里同时带两样东西：产物名（它由这一跳的 session task 命名，落在成员
            # 目录里）和投递正文的最后一行（父层发来的那句，于是父层能一眼认出这条结果
            # 回答的是哪次投递）。
            said = next((line.strip() for line in reversed(text.splitlines())
                         if line.strip() and not line.startswith("artifact: ")), "")
            head = f"{ALX_MASTER}@{ALX_NAME}: done {name} ({said[:60]})".strip()
            connection.beat(task, session_id=assign.get("session_id") or "")
            connection.complete(task, head, details=text)
            connection.say_note(REP_A, f"member note from {ALX_NAME}: {head}")

    # 父层的代表：把中心投给它的任务发进子集群，等子集群的结果，再在中心交活；
    # 顺带把子集群代表递上来的 note 转进中心。
    def bridge(parent_key: str, child_key: str, member: Path, worker: str, tag: str) -> None:
        parent = mounts[parent_key]
        child = mounts[child_key]
        while not stop.is_set():
            assign = parent.next_assign(1.0)
            if assign is None:
                continue
            if assign_kind(assign) == "task":
                task = assign["task_id"]
                text = assign_text(assign)
                session = assign.get("session_id") or ""
                parent.beat(task, session_id=session)
                family = f"{tag}:{task}"
                child_task = child.say_task(worker, text, family=family)
                # 子集群那一跳要跑一会儿，而 session 的看门狗按静默判死（30 秒没有心跳
                # 就退休这个 session，手上那条投递被写成 `session_dead`）。代表在等结果
                # 的时候照样心跳 —— 真 agent 干活时也是这样。
                deadline = time.monotonic() + 90.0
                head = ""
                while time.monotonic() < deadline:
                    head = child_receipt(member, child_task, family)
                    if head:
                        break
                    parent.beat(task, session_id=session)
                    time.sleep(3.0)
                if not head:
                    # 与 scriber 同理：没有结果就是失败，不是 done。
                    parent.complete(task, f"{tag}: no result",
                                    details="the child cluster returned nothing",
                                    outcome="failed")
                    continue
                # 结果原样带回：父层账本上的 head 就是子集群那一行的 head，一个字节都
                # 没被改写。details 里带上本次运行的标记，便于按运行读账。
                parent.complete(task, head, details=f"{tag} result ({value}): {head}")
                parent.say_note(HOUSE, f"bridge note from {tag} ({value}): {head}")

    # 子集群的代表：本集群角色递上来的 note，转进父层。
    def child_representative(child_key: str, tag: str) -> None:
        child = mounts[child_key]
        while not stop.is_set():
            assign = child.next_assign(1.0)
            if assign is None:
                continue
            if assign_kind(assign) == "note":
                # 本集群角色递上来的 note，代表自己带上标记转发进父层 —— 这一步是代表
                # 的活，不是 core 的转发。
                mounts[f"parent-{tag}"].say_note(HOUSE, f"{tag} note ({value}): {assign_text(assign)}")

    threads = [
        threading.Thread(target=scribe, daemon=True),
        threading.Thread(target=master, daemon=True),
        threading.Thread(target=bridge, args=("parent-a", "child-a", ALEXANDRIA, ALX_WORKER, "a"), daemon=True),
        threading.Thread(target=bridge, args=("parent-b", "child-b", CHAMBER, CH_WORKER, "b"), daemon=True),
        threading.Thread(target=child_representative, args=("child-a", "a"), daemon=True),
    ]
    for thread in threads:
        thread.start()
    while not stop.wait(0.5):
        pass
    for connection in mounts.values():
        connection.close()


def result_named(member: Path, name: str) -> str:
    """子集群账本里那次交活行的 `out_head`，按产物名（`{task}.txt`）认出来。

    master 的 head 里带着它，而 scriber 手里有的正是那个文件名 —— 这是两边唯一都
    握着的真标记（handoff 铸的子任务 id 不会告诉 scriber 这一边）。
    """
    for row in ledger(member, limit=200):
        if text_of(row.get("kind")) != "completion":
            continue
        head = text_of(row.get("out_head"))
        if head and name in head:
            return head
    return ""


def child_receipt(member: Path, task: str, family: str) -> str:
    """子集群账本里这次作业的结果：交活那一行的 `out_head`。

    行里的 `task` 是客户端自己铸的（scriber 的 handoff 与新会话各有一个 id），所以
    可靠的键是我发出去的那个 family（`<tag>:<父层 task>`，随 causality 一路继承），
    以及这一跳的产物名 —— 后者由子集群那一跳的 task id 命名。"""
    rows = ledger(member, limit=100)
    # 先看交活那一行：handoff 行的 out_head 也带正文，但那是中转，不是结果。
    for row in rows:
        if text_of(row.get("kind")) != "completion":
            continue
        head = text_of(row.get("out_head"))
        if not head:
            continue
        if (text_of(row.get("family")) == family
                or task in text_of(row.get("body_json"))
                or task in head):
            return head
    return ""


# ----------------------------------------------------------------- lifecycle

def lifecycle() -> None:
    """v2.11 §4.1：会话的终态只由**声明了该会话的连接**判定。

    两个挂载都声明 `dpdp-main`。第二个走掉（task 子会话那种一进一出），会话必须还活着，
    而第一个必须还能收帧；第一个走掉，会话必须真的结束 —— 修复不能把真退出也堵死。
    """
    ensure_up()
    workspace = CENTER / HOUSE
    mark = marker("lifecycle")

    first = mount(workspace, HOUSE, session=SESSIONS[HOUSE], label="parent")
    check((first.welcome or {}).get("session_id") == SESSIONS[HOUSE],
          "the first mount is answered with the session it declared",
          f"welcome.session_id={(first.welcome or {}).get('session_id')}")
    # 挂载自己发布那条常驻行（`seed_resident_parked_session`）：一个声明了会话的
    # parked 连接就是这条会话的所有者，会话在它挂上的一刻存在。
    check(bool(wait_until(lambda: session_alive(HOUSE) or None, 15.0)),
          "mounting the declaration publishes the session's row",
          f"state={session_state(HOUSE)}")

    second = mount(workspace, HOUSE, session=SESSIONS[HOUSE], label="task-child")

    # 前修复：`parked` 只是一串连接，mount 声明的会话名被丢掉，于是任何一条 parked
    # 连接离开都写 agent_gone + resource_closed，会话直接进 terminated → exited。
    check(session_alive(HOUSE), "the session reads alive while both declarations are mounted")

    second.detach(f"lifecycle: the second declaration of {SESSIONS[HOUSE]} leaves")
    second.close()

    # 前修复：这里会读到 exited/gone —— 第二条连接（同进程里被重新调用的插件工厂冒用
    # 父别名的那种）一走，dpdp-main 就被判死，父连接还在也救不回来。
    alive = wait_until(lambda: None if session_state(HOUSE) in UNKNOWN else session_state(HOUSE), 15.0)
    check(bool(alive), "the session stays alive after the second declaration leaves",
          f"state={session_state(HOUSE)}")

    # 这一条是活性对照，不是修复敏感项：心跳只证明这条连接还讲得通协议（parked 连接
    # 的心跳本就不改状态，见 reports.rs 的 serves_task 分支）。
    first.beat("lifecycle-link-probe", session_id=SESSIONS[HOUSE])
    ok("the first connection still sends a frame and is answered")

    # 前修复：会话已 terminated，服务端 `can_receive_note` 判定 dpdp 收不了 note，
    # 投递被拒或永远排队 —— 这就是同伴报的「note 进站被阻塞」。
    #
    # 发信人走 admin 面上的 `_supervisor`（中心没有第二个 worker role；跨集群那条
    # note 由 `federation` 从成员集群送进来）。这里要的是投递到 *目标* 的判断：一条
    # 声明还在的连接必须收得到 note。
    text = f"worker note to the supervisor {mark}"
    run([ONLYNE, "--server-root", CENTER, "send", "--as", "admin", "--from", SUP,
         "--to", HOUSE, "--note", "--text", text,
         "--force", "--yes-i-am-supervisor-not-other-role"], capture=True)
    # 这条挂载声明的会话在上一次 `im`/`lifecycle` 里也活着过：重挂会先重放上一轮
    # 没确认的帧（比如上一轮 `im` 的入站），所以等的是**本轮**那条，不是第一条。
    assign = first.next_assign_with(mark, 20.0)
    check(assign is not None and mark in assign_text(assign),
          "a worker note reaches the still-mounted declaration",
          f"text={assign_text(assign)[:120]!r}" if assign else miss_detail(first, mark))

    first.detach(f"lifecycle: the last declaration of {SESSIONS[HOUSE]} leaves")
    first.close()

    # 与上面成对：唯一声明者离开时必须真的结束，否则修复只是把退出也一起堵死了。
    ended = wait_until(lambda: session_state(HOUSE) in UNKNOWN, 20.0)
    check(bool(ended), "the session reaches its terminal state once the last declaration leaves",
          f"state={session_state(HOUSE)}")


def session_state(role: str) -> str:
    """这个 role 现在读到的生命周期。

    同一个 session id 在多次挂载之间会留下多行（一次绑定一行），所以一条**活着的**
    行优先：一次早先运行留下的 `exited` 行不该决定当下的断言。
    """
    rows = session_rows(CENTER, role)
    states = [text_of(row.get("public_lifecycle")) for row in rows]
    live = [state for state in states if state and state not in UNKNOWN]
    if live:
        return live[0]
    return next((state for state in states if state), "")


def session_alive(role: str) -> bool:
    return session_state(role) not in UNKNOWN


def ensure_up() -> None:
    if not pid_of("center-server") or not pid_of("resident"):
        die("the tree is not up; run `python3 examples/lifecycle/run.py up` first")


# ----------------------------------------------------------------- federation

def federation() -> None:
    """父层 → 代表 → 子集群真实作业 → 结果与 note 回到中心。

    `aggregate` 只是标注：core 不转发。代表（resident 里的 bridge）自己把父层投递发进
    子集群，读子集群账本上那一行真实结果，再在父层交活。

    这些断言是「本机那套形态真跑得通」的证据，不是修复敏感项；只有 note 那一跳与
    §4.1 同源（服务端按 `can_receive_note` 判断常驻会话收不收得到 note）。
    """
    ensure_up()
    mark = marker("federation")
    roles = cli_json(CENTER, "roles") or {}
    annotations = {row.get("name") or row.get("role"): row.get("aggregate")
                   for row in rows_of(roles, "roles")}
    check(annotations.get(REP_A) == ALX_NAME and annotations.get(REP_B) == CHAMBER_NAME,
          "the centre publishes the aggregate annotation per representative",
          f"caerulea={annotations.get(REP_A)!r} lazuli={annotations.get(REP_B)!r}")

    witness = mount(CENTER / HOUSE, HOUSE, session=SESSIONS[HOUSE], label="house-witness")
    try:
        for rep, member, worker, tag in ((REP_A, ALEXANDRIA, ALX_WORKER, "alexandria"),
                                         (REP_B, CHAMBER, CH_WORKER, "research-chamber")):
            text = f"P1 {tag} round trip {mark}"
            task = send_task(SUP, rep, text)
            child_task = wait_until(
                lambda: child_task_of(member, worker, mark), 30.0, 0.3)
            check(bool(child_task),
                  f"{tag}: the representative issued the delivery into its own cluster",
                  f"child task {child_task or '-'}")
            head = wait_until(lambda: child_receipt_any(member, [child_task, mark]), 60.0, 0.3)
            check(bool(head), f"{tag}: real work exists in the child ledger", f"out_head={head!r}")
            # 父层那条交活行的 head 就是子集群交活行的 head：返回的是子集群自己的字节。
            returned = wait_until(lambda: completion_of(CENTER, task, head), 60.0, 0.3)
            check(bool(returned),
                  f"{tag}: the child's result came back into the centre as the receipt",
                  f"out_head={returned!r}")
        note = wait_until(lambda: note_rows(CENTER, REP_A, HOUSE, mark), 30.0, 0.3)
        check(bool(note), "the member's note crossed into the centre addressed to the house role")
        # 这条会话在别的命令里也活着过：重挂先重放上一轮没确认的帧，所以等的是本轮
        # 那条跨集群 note（正文本轮标记），不是第一条投递。
        assign = witness.next_assign_with(mark, 20.0)
        check(assign is not None and mark in assign_text(assign),
              "the centre's resident session received the crossed note",
              f"text={assign_text(assign)[:120]!r}" if assign else miss_detail(witness, mark))

        # 边界：中心的账本按它自己的名字寻址，子集群的角色名只出现在子集群账本里，
        # 以及代表自己选择带回来的结果文本里（那是代表的话，不是 core 的转发）。
        visible = {role_of(row, "from") for row in ledger(CENTER, limit=200)} | \
                  {role_of(row, "to") for row in ledger(CENTER, limit=200)}
        strangers = sorted(name for name in visible if name and name not in CENTER_ROLES)
        check(not strangers, "every role name in the centre ledger is a centre-visible name",
              f"stray={strangers}")
    finally:
        witness.detach("federation: witness done")
        witness.close()


def send_task(sender: str, to: str, text: str) -> str:
    out = run([ONLYNE, "--server-root", CENTER, "send", "--as", "admin", "--from", sender,
               "--to", to, "--text", text, "--force", "--yes-i-am-supervisor-not-other-role"],
              capture=True)
    answer = json.loads(out)
    task = (answer.get("data") or {}).get("task")
    if not task:
        die(f"send to {to} answered no task: {out.strip()}")
    return task


def child_task_of(member: Path, worker: str, value: str) -> str:
    for row in ledger(member, limit=100):
        if role_of(row, "to") == worker and value in text_of(row.get("body_json")):
            return text_of(row.get("task")) or "issued"
    return ""


def child_receipt_any(member: Path, needles: list[str]) -> str:
    """子集群账本里那次作业的结果行（按交活行取），`needles` 命中任意一个即可：
    一个运行标记或它下游的那个 id 都行 —— 两边的标记方式不同（alexandria 的 head
    带产物名，chamber 的 fake agent 原样带回正文）。"""
    for row in ledger(member, limit=200):
        if text_of(row.get("kind")) != "completion":
            continue
        head = text_of(row.get("out_head"))
        if head and any(needle and needle in head for needle in needles):
            return head
    return ""


def completion_of(root: Path, task: str, value: str) -> str:
    for row in ledger(root, task=task, limit=50):
        if text_of(row.get("kind")) != "completion":
            continue
        head = text_of(row.get("out_head"))
        body = text_of(row.get("body_json"))
        if head and (value in head or value in body):
            return head
    return ""


def note_rows(root: Path, sender: str, target: str, value: str) -> bool:
    for row in ledger(root, limit=200):
        if role_of(row, "from") != sender or role_of(row, "to") != target:
            continue
        if value in text_of(row.get("body_json")) or value in text_of(row.get("out_head")):
            return True
    return False


# ----------------------------------------------------------------- im

def im() -> None:
    """真后端事件入站 → dpdp → 出站回执。

    入站不是 admin send 假扮的：它是 `onlyne-im-backend-fake` 作为子进程吐的事件
    （`ONLYNE_FAKE_BACKEND_EVENTS`），网关按 `[[im.push]]` 跑触发器，触发器投给 dpdp。
    出站是挂载上的连接自己发的 note，网关渲染回会话，后端 `send` 出的回执（带平台
    message id）落在网关自己的历史里。

    第一条断言与 §4.1 同源：投递能不能落到那条声明还在的常驻会话上，正是服务端
    `can_receive_note` 判的那件事。其余是网关链路本身的证据。
    """
    ensure_up()
    mark = marker("im")
    witness = mount(CENTER / HOUSE, HOUSE, session=SESSIONS[HOUSE], label="house-im")
    gateway = CENTER / "im"
    gateway_key = (CENTER / ".keymint" / GW_ROLE / ".onlyne" / "keys" / "role.key").read_bytes().hex()
    gateway_root(CENTER, gateway_key, FAKE_CHAT)
    log = gateway / "conversations" / GW_SLUG / "chat.jsonl"
    history = gateway / ".onlyne" / "history" / "main.jsonl"
    try:
        for path in (log, history):
            path.unlink(missing_ok=True)
        stop_name("center-gateway")
        if not wait_until(lambda: True if pid_of("center-gateway") == 0 else False, 10.0, 0.2):
            die("the gateway did not stop; refusing to start a second one")
        text = f"hello from the scripted backend {mark}"
        start_gateway({
            "ONLYNE_FAKE_BACKEND_EVENTS": f"{FAKE_CHAT}|{text}",
            "ONLYNE_FAKE_BACKEND_CHATS": f"{FAKE_CHAT}:Fake Chat:group",
            "ONLYNE_FAKE_BACKEND_IDENTITY": "fake-bot",
        })
        wait_gateway()

        # 这条会话上一轮留下过没确认的帧（上一轮自己的入站也在其中）：等的是本轮
        # 那条事件，不是重放出来的旧帧。
        assign = witness.next_assign_with(mark, 45.0)
        check(assign is not None and mark in assign_text(assign),
              "the backend subprocess's event reached the house role as a delivery",
              f"text={assign_text(assign)[:120]!r}" if assign else miss_detail(witness, mark))
        key = chat_key(assign_text(assign)) if assign else ""
        check(bool(key), "the delivery carried the conversation's chat key", f"key={key!r}")
        if not key:
            return
        reply = f"pong from the house role {mark}"
        before = out_receipts(history)
        witness.say_note(GW_ROLE, f"<chat:{key}>\n\n{reply}", admin=True)

        rendered = wait_until(lambda: read_jsonl(log, "out", reply), 45.0, 0.3)
        check(bool(rendered), "the gateway rendered the reply onto the platform",
              f"chat.jsonl={rendered!r}")
        # 回执是后端 `send` 之后才写的那一行：等的是**这一次**发送带来的新行，而不是
        # 文件里上一轮留下的旧行。
        receipt = wait_until(lambda: history_receipt(history, before), 45.0, 0.3)
        check(bool(receipt), "the outbound row carries the backend's own message id",
              f"platform_message={receipt!r}")
    finally:
        witness.detach("im: witness done")
        witness.close()


def read_jsonl(path: Path, direction: str, needle: str):
    if not path.is_file():
        return None
    for line in path.read_text().splitlines():
        if not line.strip():
            continue
        try:
            row = json.loads(line)
        except ValueError:
            continue
        if row.get("direction") != direction:
            continue
        if needle in json.dumps(row):
            return row
    return None


def out_receipts(path: Path) -> int:
    """网关自己历史里带平台 message id 的出站行数。"""
    return sum(1 for row in read_jsonl_all(path)
               if row.get("direction") == "out" and row.get("platform_message"))


def history_receipt(path: Path, before: int):
    rows = [row for row in read_jsonl_all(path)
            if row.get("direction") == "out" and row.get("platform_message")]
    return rows[-1]["platform_message"] if len(rows) > before else None


def read_jsonl_all(path: Path) -> list[dict]:
    if not path.is_file():
        return []
    rows = []
    for line in path.read_text().splitlines():
        if not line.strip():
            continue
        try:
            rows.append(json.loads(line))
        except ValueError:
            continue
    return rows


# ----------------------------------------------------------------- idle

def cpu_seconds(pid: int) -> float:
    """累计 CPU 时间（`/bin/ps -o time=`），不是 `ps -o %cpu`：后者是进程一生的平均值，
    对"这一段时间里烧了多少 CPU"这个问题答非所问。"""
    out = subprocess.run([str(PS), "-o", "time=", "-p", str(pid)], capture_output=True,
                         text=True, check=False).stdout.strip()
    if not out:
        return 0.0
    parts = out.split(":")
    seconds = 0.0
    for part in parts:
        seconds = seconds * 60 + float(part)
    return seconds


def lo0_packets(seconds: int) -> float:
    """lo0 的包速率：`netstat -I lo0 -w 1` 每行是一个 1 秒区间的增量（实测确认）。

    这个计数器是**全机**的，不是本进程的：本机上还跑着别的集群时，它的流量也会算进来。
    所以断言用的是"我的网关在跑/停着"两次读数之差。
    """
    child = subprocess.Popen(["/usr/sbin/netstat", "-I", "lo0", "-w", "1"],
                             stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
    lines: list[str] = []
    assert child.stdout is not None
    try:
        for _ in range(seconds + 2):
            line = child.stdout.readline()
            if not line:
                break
            lines.append(line)
    finally:
        child.kill()
        child.wait(timeout=5)
    samples = []
    for line in lines:
        fields = line.split()
        if len(fields) < 5 or not fields[0].isdigit():
            continue
        try:
            samples.append(int(fields[0]) + int(fields[3]))
        except ValueError:
            continue
    if not samples:
        return float("nan")
    return sum(samples) / len(samples)


def idle(window: int) -> None:
    """空载成本：网关停着/跑着两次读数之差。

    前修复症状：`onlyne-im` 每轮 pull 都带 `hold_ms: Some(20_000)`，服务端在
    `relay::pull` 里立刻回空，客户端零延迟再拉 —— lo0 稳定 ~6.6 万包/秒、约 1 个核。
    修复后空队列在服务端被握住，速率落到个位数每秒。
    """
    ensure_up()
    names = ["center-server", "center-gateway", "alexandria-server"]
    pids = {name: pid_of(name) for name in names}
    missing = [name for name, pid in pids.items() if not pid]
    if missing:
        die(f"not running: {', '.join(missing)}")

    gateway = "center-gateway"
    before_cpu = {name: cpu_seconds(pid) for name, pid in pids.items()}
    with_packets = lo0_packets(window)
    with_cpu = {name: cpu_seconds(pid) for name, pid in pids.items()}

    stop_name(gateway)
    if not wait_until(lambda: True if pid_of(gateway) == 0 else False, 10.0, 0.2):
        die("the gateway did not stop for the baseline measurement")
    base_packets = lo0_packets(window)
    start_gateway({})
    wait_gateway()

    cpu_delta = {name: round(with_cpu[name] - before_cpu[name], 3) for name in names}
    rate = with_packets - base_packets
    say(f"window {window}s; CPU seconds: " +
        ", ".join(f"{name} {cpu_delta[name]}" for name in names))
    say(f"lo0 packets/s: gateway up {with_packets:.1f}, gateway stopped {base_packets:.1f}, "
        f"delta {rate:.1f}")

    # 阈值：这个计数器是全机的，本机还跑着别人的集群（实测 lo0 上稳定 ~13 万包/秒），
    # 所以"两次读数之差"的噪声在 ±1000 包/秒量级 —— 阈值必须在这个噪声之上。
    # 5000 是噪声的 5 倍、前修复症状（~66,000/秒）的 1/13：两个方向都留了余量。
    # lo0 是全机计数器，所以这条说的是本机回环的采样增量，不是网关自己的包。
    check(abs(rate) < PACKET_CEILING, "the machine-wide loopback delta stays within the sampling bound",
          f"delta {rate:+.1f} packets/s (machine noise ~±1000/s, pre-fix symptom ~66000/s)")
    # CPU：逐进程的累计时间增量。修复后空载网关实测 0.000 s/秒；前修复是每 1 秒墙钟
    # 烧掉约 1 秒 CPU（一个核），0.1 的余量足够大。
    per_second = cpu_delta[gateway] / window
    check(per_second < CPU_CEILING, "the gateway's own CPU time stays low when idle",
          f"{per_second:.3f} s CPU per wall second (pre-fix ~1.0)")


# ----------------------------------------------------------------- status / stop

def status(*, as_json: bool) -> None:
    value = run_id()
    shape = describe(value)
    shape["procs"] = {name: {"pid": entry["pid"], "alive": owns(int(entry["pid"]), entry["expect"])}
                      for name, entry in registry().items()}
    shape["roles"] = {name: cli_json(Path(cluster["server_root"]), "roles")
                      for name, cluster in shape["clusters"].items()}
    shape["sessions"] = {tag: session_rows(root) for tag, root in
                         (("center", CENTER), ("alexandria", ALEXANDRIA), ("chamber", CHAMBER))}
    gateway = CENTER / "im"
    shape["gateway_check"] = cli_out([IM, "check", "--root", gateway]).stdout
    conversations = gateway / "conversations"
    shape["chat_keys"] = {child.name: chat_key_of(child)
                          for child in (sorted(conversations.iterdir()) if conversations.is_dir()
                                        else [])}
    resident = json.loads(RESIDENT_STATE.read_text()) if RESIDENT_STATE.is_file() else {}
    shape["resident"] = resident

    if as_json:
        print(json.dumps(shape, indent=2))
        return
    say(f"tree {TREE}, run id {value}")
    for name, cluster in shape["clusters"].items():
        alive = shape["procs"].get(f"{name}-server", {}).get("alive")
        say(f"{name}: 127.0.0.1:{cluster['port']} server {'up' if alive else 'down'}")
    for name, answer in shape["roles"].items():
        for row in rows_of(answer or {}, "roles"):
            say(f"  role {row.get('name') or row.get('role')} state={row.get('state')} "
                f"sessions={row.get('sessions')} aggregate={row.get('aggregate') or '-'}")
    for tag, rows in shape["sessions"].items():
        for row in rows:
            say(f"  {tag} session {row.get('session_id') or row.get('task_id')} "
                f"role={row.get('role')} lifecycle={row.get('public_lifecycle')}")
    for name, key in shape["chat_keys"].items():
        say(f"gateway conversation {name}: chat key {key}")
    say(f"resident mounts: " +
        ", ".join(f"{item['label']}({item['session'] or 'no session declared'})"
                  for item in shape["resident"].get("mounts", [])))


def chat_key_of(conversation: Path) -> str:
    key = conversation / ".onlyne" / "chat.key"
    return key.read_text().strip() if key.is_file() else ""


def stop() -> None:
    """只停本脚本记下的 PID，且停之前核对身份；集群根与日志留在原地给人看。"""
    entries = registry()
    order = ["center-gateway", "resident", "chamber-agent-fake",
             *[name for name, _ in clients()],
             "center-server", "alexandria-server", "chamber-server"]
    for name in order:
        entry = entries.get(name)
        if not entry:
            continue
        pid = int(entry["pid"])
        if stop_pid(pid, entry["expect"]):
            say(f"stopped {name} pid {pid}")
    for name in list(entries):
        entry = entries[name]
        if not owns(int(entry["pid"]), entry["expect"]):
            entries.pop(name)
    save_registry(entries)
    say(f"stopped; the tree at {TREE} is left in place for inspection")


# ----------------------------------------------------------------- main

def main(argv: list[str]) -> None:
    parser = argparse.ArgumentParser(prog="run.py", add_help=True)
    parser.add_argument("command", nargs="?", default="up",
                        choices=["up", "lifecycle", "federation", "im", "idle", "status",
                                 "stop", "resident"])
    parser.add_argument("--json", action="store_true")
    parser.add_argument("--window", type=int, default=10)
    args = parser.parse_args(argv)
    if args.command == "up":
        up(as_json=args.json)
    elif args.command == "lifecycle":
        lifecycle()
    elif args.command == "federation":
        federation()
    elif args.command == "im":
        im()
    elif args.command == "idle":
        idle(args.window)
    elif args.command == "status":
        status(as_json=args.json)
    elif args.command == "stop":
        stop()
    elif args.command == "resident":
        resident()


if __name__ == "__main__":
    main(sys.argv[1:])
