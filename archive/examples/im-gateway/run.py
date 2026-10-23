#!/usr/bin/env python3
"""examples/im-gateway: 一个最小集群 + 一个 IM 网关根 + 三个平台后端。

网关在集群里是**一条** `[[client]]`（本例叫 `im`）；每个 IM 会话是
`conversations/<slug>/` 下一个工作区，它的身份是工作区里那把 chat key，投递文本
带它、网关靠它决定这条消息渲染进哪个会话（`docs/v2.3-CHAT-PROTOCOL.md`）。一个
`[[im.backend]]` 是一个平台后端进程：本例写 Telegram、飞书、QQ 三家，外加一个
`fake`（`onlyne-im-backend-fake`），后者不需要任何凭据，`smoke` 靠它把整条链路跑通。

用法:
    python3 examples/im-gateway/run.py up           起集群、写网关根、按平台绑定会话
    python3 examples/im-gateway/run.py serve        up + 前台跑网关（给终端 host 的 pane 用）
    python3 examples/im-gateway/run.py status       roles / 网关 check / 会话目录
    python3 examples/im-gateway/run.py smoke        用 fake 后端跑一条真实的往返
    python3 examples/im-gateway/run.py stop         收摊

`IM_DEMO_NOTIFY_TO=<pi 会话 id>` 时，入站脚本还会把每条平台消息转给那个会话
（pi-intercom），用于盯着这个 demo 跑。

真实平台需要凭据，从 `examples/im-gateway/.env` 读（模板是 `.env.example`，这个
文件被 .gitignore 忽略），或者从 shell 环境读——环境里已有的变量优先。变量名：
    ONLYNE_TELEGRAM_TOKEN      Telegram bot token
    TELEGRAM_CHAT_ID           要绑的会话 id（群是负数，例如 -1001234567890）
    FEISHU_CREDENTIAL          飞书 `app_id:app_secret`
    FEISHU_APP_ID              飞书 `app_id`（与 FEISHU_APP_SECRET 成对，与上面等效）
    FEISHU_APP_SECRET          飞书 `app_secret`
    FEISHU_CHAT_ID             飞书的 chat_id / open_id
    QQBOT_CREDENTIAL           QQ `app_id:app_secret`
    QQBOT_APP_ID               QQ `app_id`（与 QQBOT_APP_SECRET 成对，与上面等效）
    QQBOT_APP_SECRET           QQ `app_secret`
    QQBOT_CHAT_ID              QQ 的会话 id（形如 `group:<id>` / `channel:<id>`）

三家都不能扫码：飞书的扫码给的是 `user_access_token`（用户登录网页应用），而机器
人后端要 `app_id`+`app_secret` 换 `tenant_access_token`；Telegram 只有 @BotFather；
QQ 只有控制台的 AppID/AppSecret。

没有对应变量时，那家平台**不绑会话**，网关照样起、日志里一行说清它缺什么——这是
设计而非缺陷：后端缺凭据是 `onlyne-im login` 要修的状态，不是网关的致命错误。

前置：`cargo build --workspace`。运行时树在 `/tmp/onlyne-im-demo`（`IM_DEMO_ROOT`
可改），路径短是因为 macOS 的 unix socket 路径上限 104 字节。只用 python3 标准库。
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BIN = ROOT / "target" / "debug"
SERVER = BIN / "onlyne-server"
CLIENT = BIN / "onlyne-client"
ONLYNE = BIN / "onlyne"
IM = BIN / "onlyne-im"

CLUSTER = Path(os.environ.get("IM_DEMO_ROOT") or "/tmp/onlyne-im-demo")
SPEC = CLUSTER / ".onlyne" / "spec.toml"
WS = CLUSTER / "ws"
GW = CLUSTER / "gw"
# 消息的处理者：不跑 agent，由操作员自己接管（入站推给一个 pi 会话，回复手工
# `send --from _supervisor --to im`）。集群里不需要第二个 client 进程。
PEER = "_supervisor"
GATEWAY_ROLE = "im"
PID_FILE = CLUSTER / "server.pid"

# 一个 `[[im.backend]]` 一行。 `binary` 是本仓库 build 出来的名字，`slug` 是会话目录
# （本例一家一个会话），`conversation_var` / `credential_var` 是要从环境拿的东西。
# `fake` 不需要任何凭据：它是 `smoke` 的凭据无关路径。
PLATFORMS = [
    {
        "gateway": "telegram",
        "binary": "onlyne-im-backend-telegram",
        "slug": "tg-main",
        "conversation_var": "TELEGRAM_CHAT_ID",
        "credential_var": "ONLYNE_TELEGRAM_TOKEN",
    },
    {
        "gateway": "feishu",
        "binary": "onlyne-im-backend-feishu",
        "slug": "fs-main",
        "conversation_var": "FEISHU_CHAT_ID",
        "credential_var": "FEISHU_CREDENTIAL",
    },
    {
        "gateway": "qqbot",
        "binary": "onlyne-im-backend-qqbot",
        "slug": "qq-main",
        "conversation_var": "QQBOT_CHAT_ID",
        "credential_var": "QQBOT_CREDENTIAL",
    },
    {
        "gateway": "fake",
        "binary": "onlyne-im-backend-fake",
        "slug": "fake-main",
        "conversation_var": "FAKE_CHAT_ID",
        "credential_var": None,
    },
]
FAKE_CHAT = "-1000001"


def say(message: str) -> None:
    print(f"im-demo: {message}", flush=True)


def dotenv(path: Path) -> dict[str, str]:
    """Read a `KEY=value` file, ignoring comments and blanks.

    Two rules make it a reader and not a parser: `#` starts a comment anywhere
    on a line (a secret never carries one), and quotes are stripped so a value
    can carry a `#` or a space. No export directive, no substitution — the
    credential loader already resolves `$VAR` against the environment, which is
    the one indirection worth having.
    """
    if not path.is_file():
        return {}
    values: dict[str, str] = {}
    for raw in path.read_text().splitlines():
        line = raw.split("#", 1)[0].strip()
        if not line or "=" not in line:
            continue
        name, value = line.split("=", 1)
        name = name.strip()
        value = value.strip().strip('"').strip("'")
        if name:
            values[name] = value
    return values


def die(message: str) -> None:
    print(f"im-demo: {message}", file=sys.stderr)
    sys.exit(1)


def run(args: list[str | Path], *, capture: bool = False, check: bool = True,
        env: dict[str, str] | None = None) -> str:
    argv = [str(a) for a in args]
    result = subprocess.run(argv, cwd=str(ROOT), capture_output=capture, text=True,
                            check=False, env={**os.environ, **(env or {})})
    if check and result.returncode != 0:
        captured = (result.stdout + result.stderr) if capture else ""
        die(f"`{' '.join(argv)}` exited {result.returncode}: {captured.strip()}")
    return result.stdout if capture else ""


def free_port() -> int:
    import socket
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def server_socket() -> Path:
    """The server root's admin socket, derived the way `onlyne-wire` derives it.

    触发器脚本拿到的 `ONLYNE_SOCKET` 就是这个路径，是它们回到集群的唯一入口。
    规则（`crates/onlyne-wire/src/socket.rs`）：runtime 目录是
    `$ONLYNE_RUNTIME_DIR` 或 `/tmp/onlyne-<uid>`，文件名是该 root 规范路径
    sha256 的前 16 个十六进制字符加 `.sock`。本例自己算一遍，是为了不依赖
    `onlyne ls` 在用户机器上历史注册表的大小（那可能是几百 KB）。
    """
    import hashlib
    runtime = os.environ.get("ONLYNE_RUNTIME_DIR")
    base = Path(runtime) if runtime else Path(f"/tmp/onlyne-{os.getuid()}")
    canonical = Path(os.path.realpath(CLUSTER))
    digest = hashlib.sha256(str(canonical).encode()).hexdigest()[:16]
    return base / f"{digest}.sock"


def need_binaries() -> None:
    for binary in (SERVER, CLIENT, ONLYNE, IM):
        if not binary.exists():
            die(f"missing binary {binary}; run cargo build --workspace first")


def client_roles(text: str) -> list[str]:
    """spec 里真实 `[[client]]` 表的 role 名，按文件顺序。

    逐行解析：`onlyne-server init` 写的注释块里也有 `[[client]]` 字样，子串匹配会
    把它当成已经种好的表。
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


def init_cluster() -> None:
    """server root；幂等，重跑不会重建 spec（重建会换掉 cert_pin）。"""
    CLUSTER.mkdir(parents=True, exist_ok=True)
    if not SPEC.is_file():
        run([SERVER, "init", "--root", CLUSTER, "--listen", f"127.0.0.1:{free_port()}"])


def declared(role: str) -> bool:
    return role in client_roles(SPEC.read_text()) if SPEC.is_file() else False


def append_fragment(fragment: str) -> None:
    with SPEC.open("a") as out:
        out.write("\n" + fragment)


def seed_peer() -> None:
    """一个 agent 角色，是会话在集群里说话的对象。

    `onlyne-client init` 打印的片段就是这个角色的 `[[client]]` 行，含它自己铸的
    key；直接追加进 spec 并 reload，和 e2e 的 `client_init` 是同一个动作。
    """
    workspace = WS / PEER
    if declared(PEER) and (workspace / ".onlyne" / "config.toml").is_file():
        say(f"role {PEER} already seeded")
        return
    fragment = run([CLIENT, "init", "--workspace", workspace, "--role", PEER,
                    "--server-root", CLUSTER], capture=True)
    append_fragment(fragment)
    run([ONLYNE, "--server-root", CLUSTER, "reload"])
    say(f"seeded role {PEER} -> {workspace}")


def seed_gateway_role() -> None:
    """网关在集群里的那条 role：铸一把 key，放进网关根，写进 spec。

    `onlyne-im` 自己不铸 key（key 是操作者的），所以这里借 `onlyne-client init`
    在一个临时工作区铸一把，把 32 字节的私钥文件搬到网关根的
    `.onlyne/keys/role.key`（`Gateway::key_path` 读的正是这个路径），再把同一行的
    key 写进 spec。网关进程和服务器必须对上同一把。
    """
    if declared(GATEWAY_ROLE) and (GW / ".onlyne" / "keys" / "role.key").is_file():
        say(f"gateway role {GATEWAY_ROLE} already declared")
        return
    scratch = CLUSTER / "keymint"
    fragment = run([CLIENT, "init", "--workspace", scratch, "--role", GATEWAY_ROLE,
                    "--server-root", CLUSTER], capture=True)
    keys = GW / ".onlyne" / "keys"
    keys.mkdir(parents=True, exist_ok=True)
    (keys / "role.key").write_bytes((scratch / ".onlyne" / "keys" / "role.key").read_bytes())
    (keys / "role.key").chmod(0o600)
    append_fragment(fragment)
    run([ONLYNE, "--server-root", CLUSTER, "reload"])
    say(f"minted the gateway role's key -> {keys / 'role.key'}")


def backend_rows() -> str:
    """`[[im.backend]]` 表：一个平台一行，argv 指向本仓库的 build。

    绝对路径是刻意的：`onlyne-im run` 是用 `Command::new` 起的进程，不做 PATH
    查找，写相对名字会捡到装在系统里的另一个版本。
    """
    rows = []
    for platform in PLATFORMS:
        argv = [str(BIN / platform["binary"]), "--gateway", platform["gateway"]]
        rendered = ", ".join(f'"{part}"' for part in argv)
        rows.append(f'[[im.backend]]\ngateway = "{platform["gateway"]}"\nrun = [{rendered}]\n')
    return "\n".join(rows)


def write_gateway_root() -> None:
    """网关根自己的文档：`[gateway]`、`[server]`、四行 `[[im.backend]]`。

    用 `onlyne-im init` 起稿再补后端行，而不是手写整个文件：init 写的注释和
    结构就是产品认可的形态，本例只改它必须改的那一段。
    """
    if not (GW / ".onlyne" / "config.toml").is_file():
        run([IM, "init", "--root", GW, "--server-root", CLUSTER])
    path = GW / ".onlyne" / "config.toml"
    text = path.read_text()
    # init 的模板自带一行示例后端（`telegram`），本例要四家，所以从第一个
    # `[[im.backend]]` 起到下一个表为止的整段换成自己的表。这一段里可能已经有
    # 上一轮留下的后端行，`[[im.push]]` 也可能已经挂在后面，所以按表头切而不
    # 是按"文件里有没有这个词"判断。
    marker = "[[im.backend]]"
    if marker not in text:
        path.write_text(text.rstrip() + "\n\n" + backend_rows())
        say(f"wrote the gateway root -> {path}")
        return
    # 按行切：要替换的是从第一个 `[[im.backend]]` 到第一个**未注释**的
    # `[[im.push]]`（或文件末尾）之间的整段。init 写的模板在这段里还带一块注释
    # 掉的 `[[im.push]]` 示例，按字符串切会把它当成真的表一起搬走，于是解析出
    # 一个缺 `mode` 的表头。
    lines = text.splitlines()
    start = next(i for i, line in enumerate(lines) if line.strip() == marker)
    end = len(lines)
    for index in range(start + 1, len(lines)):
        stripped = lines[index].strip()
        if stripped.startswith("[[im.push]]"):
            end = index
            break
    header = "\n".join(lines[:start]).rstrip() + "\n"
    tail = "\n".join(lines[end:]).strip("\n")
    path.write_text(header + "\n" + backend_rows() + ("\n\n" + tail + "\n" if tail else ""))
    say(f"wrote the gateway root -> {path}")


def write_trigger_script() -> None:
    """入站脚本：把平台消息投给 peer 角色，第一行带上这条会话的 chat key。

    这是 v2.3 的形状（§5.1）：脚本收到事件 JSON，从 `chat` 字段取令牌，写在投递
    文本第一行。`ONLYNE_SOCKET` 由网关注入，脚本不用自己找 socket。

    `IM_DEMO_NOTIFY_TO` 给了 pi 会话 id 时，脚本在投完之后顺带把这条入站转一句
    话给那个会话（走 pi-intercom 的 CLI）。这是**这台机器**上的观察窗口，不是
    产品机制：脚本只多一条命令，网关和 core 都不知道谁在看。

    脚本每次重写（形状可能变，例如 `IM_DEMO_NOTIFY_TO` 这次给了、下次没给），
    但 `[[im.push]]` 只在还没有时追加——它指的一直是同一个路径，重跑一次就多
    一个表的事不该发生。
    """
    path = CLUSTER / "im-in.sh"
    notify = ""
    if notify_to := os.environ.get("IM_DEMO_NOTIFY_TO", "").strip():
        cli = os.environ.get("PI_INTERCOM_CLI") or str(
            Path.home() / ".pi/agent/npm/node_modules/pi-intercom/cli.mjs")
        notify = f"""
# 旁路：把这条入站连同投递结果转给一个 pi 会话。通知自己失败不算失败（|| true），
# 但投递失败要把码原样交回，网关才会按它的重试策略再来一次。
notify_text=$(printf '[IM 入站] %s\\n%s\\n投递: %s' "$where" "$text" "$result")
node "{cli}" send --to "{notify_to}" --text "$notify_text" --json >/dev/null 2>&1 || true
"""
    path.write_text(f"""#!/usr/bin/env bash
set -euo pipefail
# stdin 是事件 JSON，$1 是模式字。`chat` 是这条会话的令牌，投递文本第一行回带它，
# 网关靠它把回复渲染回同一个会话；它同时也是这条投递的 **family**（`--family`），
# 于是这条会话的每条消息都属于同一族，`[client.session] scope = "task"` 的那个
# 会话会挨条服务它们，而不是一条消息开一个新会话。
input=$(cat)
eval "$(printf '%s' "$input" | python3 -c 'import json,shlex,sys
d = json.load(sys.stdin)
e = d.get("event") or {{}}
print("chat=" + shlex.quote(str(d["chat"])))
print("where=" + shlex.quote(str(d.get("platform", "?")) + "/" + str(d.get("conversation", "?"))))
print("text=" + shlex.quote(str(e.get("text") or "(no text)")))')"
printf '%s\\n\\n%s\\n' "<chat:$chat>" "$text" > "{CLUSTER}/body.txt"
if out=$("{ONLYNE}" send --socket "$ONLYNE_SOCKET" --as admin --from {GATEWAY_ROLE} \\
         --to {PEER} --text "$(cat "{CLUSTER}/body.txt")" --family "$chat" --force \\
         --yes-i-am-supervisor-not-other-role 2>&1); then
  result="已投给 {PEER}"
else
  rc=$?
  result="投递失败 rc=$rc: $out"
  printf '%s\\n' "$result" >&2
fi
{notify}
# 投递失败把码原样交回，网关才会按它的重试策略再来一次；通知本身不参与这个码。
if [ "${{rc:-0}}" -ne 0 ]; then exit "$rc"; fi""")
    path.chmod(0o755)
    config = GW / ".onlyne" / "config.toml"
    has_push = any(line.strip().startswith("[[im.push]]")
                   for line in config.read_text().splitlines())
    if not has_push:
        with config.open("a") as out:
            out.write(f'\n[[im.push]]\nmode = "trigger"\nrun = ["{path}"]\ntimeout = "15s"\n')
    say(f"wrote the inbound trigger -> {path}")


def bind_conversations() -> None:
    """按平台写凭据文件；**会话本身不在这里绑**。

    一个 chat id 只有在机器人已经在那个会话里之后才读得到，而机器人是为了服务
    会话才起的——所以这个例子不要求预先知道任何会话 id。`[im.listen]` 缺省全监听：
    gateway 收到一条没人注册过的会话的事件，就当场 adopt（铸 chat key、写工作区），
    日志里一行说明，重启后照旧服务同一个会话。

    `generate`（以及集群那一半的 ACL 编辑）只在操作者**显式**给出了会话 id 时才
    跑：`TELEGRAM_CHAT_ID` / `FEISHU_CHAT_ID` / `QQBOT_CHAT_ID` 填了就用它预绑，
    留空就交给 adopt。两种路径写出来的是同一种工作区。

    凭据（telegram token / feishu app_id:app_secret / qq app_id:app_secret）写进
    网关根的 0600 文件,后端进程从 `ONLYNE_IM_CREDENTIAL` 读它。
    """
    bound = 0
    for platform in PLATFORMS:
        gateway = platform["gateway"]
        slug = platform["slug"]
        conversation = ""
        if platform["conversation_var"]:
            conversation = os.environ.get(platform["conversation_var"], "").strip()
        if gateway == "fake" and not conversation:
            conversation = FAKE_CHAT
        credential = os.environ.get(platform["credential_var"], "").strip() if platform["credential_var"] else ""
        if platform["credential_var"] and not credential:
            say(f"{gateway}: no {platform['credential_var']}; the backend will report it needs one")
        if credential:
            run([IM, "login", "--root", GW, "--gateway", gateway, "--token", credential])
        if not conversation:
            # 没有会话 id：这正是 adopt 的形状，网关起来就服务它听到的会话。
            say(f"{gateway}: no conversation id; the gateway adopts what it hears")
            continue
        if (GW / "conversations" / slug).is_dir():
            say(f"{gateway}: {slug} already generated")
            bound += 1
            continue
        out = run([IM, "generate", "--root", GW, "--gateway", gateway,
                   "--conversation", conversation, "--role", slug, "--peer", PEER,
                   "--server-root", CLUSTER], capture=True)
        key = next((line.split(" ", 2)[2] for line in out.splitlines()
                    if line.startswith("chat key ")), "")
        say(f"{gateway}: bound {slug} -> chat key {key}")
        bound += 1
    say("no conversation was pre-bound; start the gateway and @ the bot — the first "
        "message adopts its conversation and the agent answers it")


def server_running() -> bool:
    return subprocess.run([str(ONLYNE), "--server-root", str(CLUSTER), "roles", "--json"],
                          capture_output=True, text=True, check=False).returncode == 0


def start_server() -> None:
    """后台起一个 `onlyne-server run`；没有 start 动词，run 只在前台跑。

    "已经有一个服务器在跑"以 pid 文件为准，而不是以 socket 能答为准：socket 在
    机器级 runtime 目录里，按 root 路径的摘要命名，所以上一次跑留下的**孤儿进程**
    ——`rm -rf` 掉运行时树、或别的原因没走到 `stop`——会以同样的路径答话。信它
    的话，本例会跟着一个已经换过 cert_pin 的旧 spec 干活，而 client 永远连不上
    （连接被拒，因为旧进程听的是旧端口）。

    而这个判断要核对**身份**，不能只看 pid 还活着：`os.kill(pid, 0)` 对任何 pid
    都成功，包括那个 pid 已经被系统回收、现在挂着一个毫不相干进程的场合。用
    `ps -o command=` 读那一行命令行，里面必须带着这个集群的 root 路径——这才是
    "这个 pid 仍然是我起的那个 server"。实测踩过：上一次会话被杀之后 pid 复用，
    本例信了 pid 文件跳过启动，随后每一步都找不到 socket。

    两种陈旧都按同一处理：文件丢掉，往下起一个新的。pid 文件里不是一个数字同样
    算陈旧——写坏了，或写到一半崩了，无论哪种它都没有指明任何进程。
    """
    pid = None
    if PID_FILE.is_file():
        try:
            pid = int(PID_FILE.read_text().strip())
        except ValueError:
            pid = None
    if pid is not None:
        if pid_owns_server(pid):
            say("server already running")
            return
        PID_FILE.unlink(missing_ok=True)
    log = CLUSTER / "logs" / "server.log"
    log.parent.mkdir(parents=True, exist_ok=True)
    with log.open("a") as out:
        child = subprocess.Popen([str(SERVER), "run", "--root", str(CLUSTER)], cwd=str(ROOT),
                                 stdin=subprocess.DEVNULL, stdout=out,
                                 stderr=subprocess.STDOUT, start_new_session=True)
    PID_FILE.write_text(f"{child.pid}\n")
    run([ONLYNE, "--server-root", CLUSTER, "wait-ready"])
    say(f"server up, log {log}")


def pid_owns_server(pid: int) -> bool:
    """`pid` 是否仍是这个集群的 `onlyne-server run --root <CLUSTER>`。

    读那一行的命令本身。pid 活着但命令是别的（包括别的集群的 server）都算不
    是——本 root 下还有一个 server 在跑，对本例毫无意义，而且会让下面的
    `reload` 打到一个没有这个 spec 的 socket 上。
    """
    try:
        line = subprocess.run(["ps", "-o", "command=", "-p", str(pid)],
                              capture_output=True, text=True, check=False).stdout.strip()
    except OSError:
        return False
    return SERVER.name in line and str(CLUSTER) in line


def stop_server() -> None:
    import signal
    if not PID_FILE.is_file():
        return
    pid = int(PID_FILE.read_text().strip())
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        pass
    for _ in range(100):
        try:
            os.kill(pid, 0)
        except ProcessLookupError:
            break
        time.sleep(0.1)
    PID_FILE.unlink(missing_ok=True)


def configure_acl() -> None:
    """让网关和 `_supervisor` 互发：`init` 只给角色和它自己，双向要显式写。

    `_supervisor` 没有 client 进程——消息的处理者是操作员本人（入站顺带推给
    一个 pi 会话，回复手工 `send --from _supervisor --to im`）。所以这里没有
    client 要起、也没有会话要结算：投递行停在 `_supervisor` 名下等操作员回来
    结它，这正是"挂起"的意思。
    """
    text = SPEC.read_text()
    changed = False
    for role, targets, senders in [
        (PEER, f'["{PEER}", "{GATEWAY_ROLE}"]', f'["*", "{PEER}", "{GATEWAY_ROLE}"]'),
        (GATEWAY_ROLE, f'["{PEER}"]', f'["{PEER}"]'),
    ]:
        lines = text.splitlines(keepends=True)
        start = next((i for i, line in enumerate(lines)
                      if line.strip().startswith(f'role = "{role}"')), None)
        if start is None:
            continue
        end = next((i for i in range(start + 1, len(lines))
                    if lines[i].strip().startswith("[[")), len(lines))
        patched = []
        seen_targets = seen_senders = False
        for line in lines[start:end]:
            code = line.split("#", 1)[0].strip()
            if code.startswith("allowed_targets"):
                line, seen_targets = f"allowed_targets = {targets}\n", True
            elif code.startswith("allowed_senders"):
                line, seen_senders = f"allowed_senders = {senders}\n", True
            patched.append(line)
        if not seen_targets:
            patched.insert(0, f"allowed_targets = {targets}\n")
        if not seen_senders:
            patched.insert(0, f"allowed_senders = {senders}\n")
        text = "".join(lines[:start] + patched + lines[end:])
        changed = True
    if changed:
        SPEC.write_text(text)
        run([ONLYNE, "--server-root", CLUSTER, "reload"])
        say(f"ACL: {PEER} <-> {GATEWAY_ROLE} both ways")


def up() -> None:
    need_binaries()
    init_cluster()
    # The server has to be up before the first socket call: seeding a role ends
    # in a `reload` and `generate` writes through the admin socket, and neither
    # can reach a daemon that is not running yet.
    start_server()
    seed_peer()
    seed_gateway_role()
    configure_acl()
    write_gateway_root()
    write_trigger_script()
    bind_conversations()
    say(f"cluster {CLUSTER}; gateway root {GW}")
    say(f"  {IM} check --root {GW}")
    say(f"  {IM} run --root {GW} --socket {server_socket()}"
        "    # 前台跑网关；凭据从 .env 或 `onlyne-im login` 来")
    say(f"  入站投给 {PEER} 并挂起；回复手工 "
        f"`{ONLYNE} send --as admin --from {PEER} --to {GATEWAY_ROLE} --text '<chat:KEY>…'`")
    say("  python3 examples/im-gateway/run.py smoke   # 用 fake 后端跑一条真实往返")


def status() -> None:
    run([ONLYNE, "--server-root", CLUSTER, "roles"])
    run([IM, "check", "--root", GW])
    conversations = GW / "conversations"
    if conversations.is_dir():
        for child in sorted(conversations.iterdir()):
            key = child / ".onlyne" / "chat.key"
            say(f"  {child.name}: chat key {(key.read_text().strip() if key.is_file() else '(none)')}")


def smoke() -> None:
    """一条真实的往返，用 fake 后端，不需要任何凭据。

    `ONLYNE_FAKE_BACKEND_EVENTS` 让 fake 后端在启动后吐一条入站事件：网关收到 →
    触发脚本 → `send --to _supervisor`（没有 client，行停在那里等操作员）→ 本
    函数替操作员回一条 `send --from _supervisor --to im` 带 chat key → 网关渲染
    出 `send` 帧 → fake 后端把它记下。证据是会话目录里 `chat.jsonl` 的两个方向
    各一行。

    手工回复这一步就是人工跑时的形状，所以 smoke 同时验了这条路径。
    """
    need_binaries()
    if not server_running():
        die("the server is not running; run `run.py up` first")
    workspace = GW / "conversations" / "fake-main"
    if not workspace.is_dir():
        die("no fake conversation; run `run.py up` (it binds one with no credential)")
    log = workspace / "chat.jsonl"
    if log.is_file():
        log.unlink()

    env = {
        **os.environ,
        "ONLYNE_FAKE_BACKEND_EVENTS": f"{FAKE_CHAT}|smoke hello",
        "ONLYNE_FAKE_BACKEND_CHATS": f"{FAKE_CHAT}:Fake Chat:group",
        "ONLYNE_FAKE_BACKEND_IDENTITY": "fake-bot",
    }
    say("starting the gateway with the fake backend; one inbound event is scripted")
    child = subprocess.Popen([str(IM), "run", "--root", str(GW),
                              "--socket", str(server_socket())], cwd=str(ROOT),
                             stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                             stderr=subprocess.STDOUT, text=True, env=env)
    try:
        read_log = lambda: [json.loads(line) for line in
                            log.read_text().splitlines() if line.strip()] \
            if log.is_file() else []
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline and \
              not any(l.get("direction") == "in" for l in read_log()):
            time.sleep(0.5)
        lines = read_log()
        if not any(l.get("direction") == "in" for l in lines):
            die("the inbound event never reached the conversation; gateway output:\n"
                + (child.stdout.read() if child.stdout else ""))
        inbound = next(l for l in lines if l.get("direction") == "in")
        key = inbound["chat"]
        say(f"inbound landed, chat key {key}; replying as {PEER}")
        # 角色记录里的出站行由后端任务在平台回话之后写（`Sent` 帧），比
        # chat.jsonl 那行晚一步；计下已有的出站行数，等的是**这一次发送**带来的
        # 新行，而不是文件里上一轮留下的旧行。
        history = GW / ".onlyne" / "history" / "main.jsonl"

        def read_history():
            if not history.is_file():
                return []
            return [json.loads(line) for line in history.read_text().splitlines()
                    if line.strip()]

        outs_before = sum(1 for row in read_history() if row.get("direction") == "out")
        run([ONLYNE, "--server-root", CLUSTER, "send", "--as", "admin",
             "--from", PEER, "--to", GATEWAY_ROLE,
             "--text", f"<chat:{key}>\n\nsmoke hello back", "--force",
             "--yes-i-am-supervisor-not-other-role"])
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline and \
              not any(l.get("direction") == "out" for l in read_log()):
            time.sleep(0.5)
        lines = read_log()
        directions = {line.get("direction") for line in lines}
        if "out" not in directions:
            die("the hand-written reply never rendered back into the conversation; "
                "chat log lines: " + str(directions))
        outbound = next(line for line in lines if line.get("direction") == "out")
        # The inbound line is the event itself (`text` at the top level); the
        # outbound one wraps the standard payload, whose text is the delivery's
        # body after the gateway took the chat key off its first line.
        said = inbound.get("text") or ""
        rendered = (outbound.get("payload") or {}).get("text") or ""
        assert "smoke hello" in said, inbound
        assert "smoke hello back" in rendered, outbound
        say(f"PASS: inbound {said!r} -> outbound {rendered.strip()!r}")
        say(f"chat log: {log}")

        recorded = []
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            recorded = [row for row in read_history() if row.get("direction") == "out"]
            if len(recorded) > outs_before:
                break
            time.sleep(0.3)
        # The gateway is stopped once both records have been read: a backend
        # killed mid-send would take the row with it.
        if child.poll() is None:
            child.terminate()
        try:
            child.wait(timeout=10)
        except subprocess.TimeoutExpired:
            child.kill()
        if len(recorded) <= outs_before:
            die(f"the send wrote no outbound row into the role's record: {history}")
        # 那条行必须带着平台给的 id。没有它，网关自己的消息不在回复链上，
        # `chain::forks` 数到第一跳就断——真机上抓到过的缺陷，症状是"来回
        # 引用多少轮都不开分支"。
        if not recorded[-1].get("platform_message"):
            die("the outbound row carries no platform id, so a later reply cannot "
                f"point at it; rows: {read_history()}")
        say(f"PASS: the outbound row carries the platform's id "
            f"{recorded[-1]['platform_message']!r}")
    finally:
        if child.poll() is None:
            child.kill()


def stop() -> None:
    stop_server()
    say("stopped")


def serve() -> None:
    """把集群跑在**这个前台 pane** 里：先 `up`，再前台跑网关，Ctrl-C 收摊。

    开给终端 host（`tern new tab -- python3 .../run.py serve`）用。`up` 起的
    server / client / agent 按设计是后台进程，但它们的日志都在这个集群自己的
    root 下（`server.log` / `client.log` / `agent.log`），网关的输出则就在这个
    pane 里、看得见。

    收摊只停这个 root 记下的 pid —— 多集群同时跑时，pid 文件各在各的 root，
    没有任何一步按进程名下手，所以停不到别的集群身上。
    """
    up()
    say(f"gateway in this pane; Ctrl-C stops the cluster at {CLUSTER}")
    try:
        run([IM, "run", "--root", GW, "--socket", server_socket()])
    except KeyboardInterrupt:
        pass
    finally:
        stop()


def main(argv: list[str]) -> None:
    command = argv[0] if argv else "up"
    if command == "up":
        up()
    elif command == "serve":
        serve()
    elif command == "status":
        status()
    elif command == "smoke":
        smoke()
    elif command == "stop":
        stop()
    else:
        die("usage: run.py up | serve | status | smoke | stop")


if __name__ == "__main__":
    # The credentials live in `examples/im-gateway/.env` beside this script, so
    # `run.py up` reads a filled-in file rather than needing an exported shell.
    # A variable already in the environment wins, so an operator who exports
    # one on purpose is never overruled by the file.
    for name, value in dotenv(Path(__file__).resolve().parent / ".env").items():
        os.environ.setdefault(name, value)
    main(sys.argv[1:])
