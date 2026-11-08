# RFC 0019：沙箱弹性算力资源节点 (Sandbox Compute Resource Node)

状态：Draft。依赖：RFC 0001、0006、0011、0013、0017。

---

## 1. 定位与背景

在自动化软件工程、故障定位排错、仿真评测与具身代理执行中，Agent 需要执行具备高破坏性、需特定环境依赖、乃至带图形桌面（GUI / noVNC）的运行时环境。

已有架构中：
- **RFC 0013（计算作业资源节点）**：建模的是**作业驱动型批处理**（Job-driven），输入为命令与产物清单，经历 `queued -> running -> completed/failed` 的单向不可逆生命周期，执行完毕产出静态数据后即告销毁。
- **本 RFC（RFC 0019）**：建模的是**交互式弹性执行沙箱资源节点**（Interactive Sandbox Resource Node）。沙箱是带租约生命周期（Lease-controlled）、长驻有状态、对外暴露直连控制通道（SSH / Docker Exec / Web Desktop）的外部运行环境。

### 核心架构原则（SSOT）

1. **容器本身不是 Onlyne 节点**：
   容器内既无独立私钥，也不维护 Onlyne 消息状态机与线程库。容器是底层执行实例，其全部生命周期、端口暴露、凭据注水与租约维护由常驻的 **Compute Broker Node**（例如 `urn:onlyne:agent:compute`）接管。
2. **控制面与数据面彻底分离**：
   - **控制面（Control Plane）**：走 Onlyne 标准消息与资源操作总线（RFC 0006），包括申领（acquire）、续期（renew）、休眠（release）、销毁（destroy）与状态检查（status）。
   - **数据面（Data Plane）**：直连通道，调用方 Agent 凭 Broker 签发的直连凭据（SSH 密钥、noVNC 鉴权票据、原生 Exec 通道）直接与容器通信，流量**绝不跨 Onlyne 转发**。
3. **计算契约以 OCI / Docker 标准为最小语义集**：
   无论是本地 OrbStack / Docker、单机 Docker Daemon、跨云虚拟机（Aliyun/AWS）、Kubernetes Pod 还是 MicroVM（Kata/Firecracker），申领输入边界基于容器镜像与系统级挂载抽象。

---

## 2. 资源模型与三段式生命周期

沙箱实例是 Compute Broker 节点管理的一类标准 Onlyne 资源（RFC 0006）：

```json
{
  "resource_ref": { "owner": "urn:onlyne:gateway:compute", "id": "sbx-issue-89c2" },
  "kind": "compute_sandbox",
  "operations": ["acquire", "renew", "release", "destroy", "status", "handover", "describe"],
  "requires": ["compute_acquire"]
}
```

### 状态机定义

沙箱具有显式租约状态机（Lease State）：

| 状态 | 含义 | 底层执行动作 | 租约时效处理 |
|---|---|---|---|
| `provisioning` | 正在拉取镜像/分配网络/创建容器 | Docker Create / Pull | 计时未正式开始 |
| `running` | 容器正常运行中，数据面端口可连 | Docker Start / Up | 扣减 `ttl_ms`，逾期触发自动处理 |
| `stopped` | 已休眠，网络与进程停止，磁盘持久化卷保留 | Docker Stop | 扣减 `retention_ms`，逾期物理销毁 |
| `destroyed` | 物理销毁终态 | Docker Remove / Volume Prune | 记录归档/清理 |
| `failed` | 调度或启动阶段不可恢复错误 | 错误归档与清理 | 记录保留供排查 |

```mermaid
stateDiagram-v2
    [*] --> provisioning: acquire (申领)
    provisioning --> running: 容器与端口就绪
    provisioning --> failed: 镜像不存在/资源耗尽
    
    running --> running: renew (续约 / 延长TTL)
    running --> stopped: release (主动休眠) 或 TTL到期自动Stop
    running --> destroyed: destroy (主动抹除)
    
    stopped --> running: acquire (带相同 name 唤醒重新开机)
    stopped --> destroyed: destroy 或 retention_until 逾期自动清理
    
    destroyed --> [*]
    failed --> [*]
```

---

## 3. 报文规范与操作契约

调用方通过 `msg({ delivery_kind: "compute", msg, meta })` 或资源统一操作通道调用 Broker。控制报文支持在 `meta` 顶层直接声明 `operation`、`name`、`lease_id`、`container`、`resources`、`network`、`provider_meta`、`lease`、`purpose`，同时向后兼容从 `meta.data` 读取同名字段（顶层优先，`meta.data` 作为过渡别名归并入同一份 `Spec`）。

### 0. 租约所有权（Lease Ownership）与容量边界

1. **所有权绑定（Owner Binding）**：
   - 每条租约在 `acquire` 时将 `owner` 绑定为已认证发送方 `authenticated_author`（通过内部路径 `compute://` 调用时使用调用方会话对应的节点 ID，不得硬编码为静态常量）。
   - 同名 `acquire` 幂等重入或唤醒 `stopped` 机器时，MUST 校验请求方等于原租约 `owner`；若命中他人持有的活跃或保留期租约，Broker MUST 拒绝并返回 `forbidden` 错误码。
   - `renew`、`release`、`destroy` MUST 校验请求方等于租约 `owner`；非所有者调用 MUST 拒绝并返回 `forbidden`。管理员强制回收走独立管理接口，不混用租户动词。
2. **TTL 与容量边界（`max_ttl_ms` & `capacity_exceeded`）**：
   - Broker 配置维护默认租期 `default_ttl_ms`、单次/累计续期硬上限 `max_ttl_ms`、空闲超时 `idle_timeout_ms` 与停机保留期 `retention_ms`。
   - 当 `acquire` 或 `renew` 请求的 `ttl_ms`（或累计租期）超过 `max_ttl_ms` 时，Broker MUST 显式拒绝并返回 `capacity_exceeded` 错误（机器不启动、原租约不篡改），MUST NOT 用 `Math.min` 静默截断请求值。
   - 请求的 `ttl_ms` 在合法范围内时，生效的 `expires_at - now` MUST 严格等于请求值。
3. **续期心跳与到期通知（Renew Heartbeat & Expiry Notice）**：
   - `renew` 是独立一等动词（不可仅靠同名 `acquire` 代替），仅对 `running` 租约延长 `expires_at`，不触发重新 provision。
   - 持有长任务的调用方（如 `issue-runner`）SHOULD 在 `ttl / 2` 周期发送 `renew` 心跳维持租约；当租约因超时或达到 `max_ttl_ms` 在 `sweep` 中被停机时，Broker SHOULD 向租约 `owner` 发送到期停机通知（携带 `state: "stopped"` 与 `reason: "expired"`）。

### 1. 申领算力沙箱 (`acquire`)

**操作请求**：
```jsonc
{
  "operation": "acquire",
  "name": "worker-runner-issue-89c2",       // 幂等标识（相同 name 重复请求视为幂等重入/重绑）
  
  // 1. 容器核心标准规格
  "container": {
    "image": "docker.io/library/debian:12-slim",
    "image_pull_policy": "if_not_present",   // always | if_not_present | never
    "command": ["/bin/sh", "-c"],
    "args": ["sleep infinity"],
    "workdir": "/workspace",
    "env": {
      "NODE_ENV": "development",
      "CI": "true"
    },
    "expose_env": [                         // 请求 Broker 动态注水并回传的环境变量清单
      "AGENT_TOKEN",
      "GATEWAY_PORT"
    ],
    "mounts": [
      {
        "type": "bind",                     // bind | volume | tmpfs
        "host_path": "/data/workspaces/w1",
        "container_path": "/workspace",
        "read_only": false
      }
    ]
  },

  // 2. 硬件规格与调度约束
  "resources": {
    "arch": "arm64",                        // arm64 | x86_64 | any
    "cpus": 4.0,
    "memory_mb": 8192,
    "gpu": { "count": 0 },
    "shm_size_mb": 1024                     // 浏览器自动化环境必需
  },

  // 3. 网络与端口映射诉求
  "network": {
    "mode": "bridge",                       // bridge | host | overlay | none
    "nat_ports": [
      {
        "name": "vnc-desktop",
        "container_port": 6080,
        "protocol": "tcp",
        "public": true,
        "preferred_host_port": 0             // 0 表示由宿主动态分配
      },
      {
        "name": "http-dev-server",
        "container_port": 3000,
        "protocol": "tcp",
        "public": false
      }
    ],
    "egress_policy": "allow_all"            // allow_all | intranet_only | air_gapped
  },

  // 4. Provider 亲和性与底层透传
  "provider_meta": {
    "selector": "auto",                     // auto | local-orbstack | cloud-sh-01
    "fallback": ["cloud-sh-01", "local-orbstack"],
    "region": "urn:onlyne:region:cn-east",
    "driver_options": {
      "docker_privileged": false,
      "docker_caps_add": ["SYS_ADMIN"]
    }
  },

  // 5. 租约时效控制
  "lease": {
    "ttl_ms": 1800000,                      // 基础租约 30 分钟
    "max_ttl_ms": 14400000,                 // 续约硬上限 4 小时
    "idle_timeout_ms": 600000,              // 无网络/CPU动作静默关机时限
    "retention_ms": 86400000                // stopped 磁盘保留死线 24 小时
  },

  "purpose": "PR #142 automated validation run"
}
```

**响应描述符 (`descriptor`)**：
```jsonc
{
  "lease_id": "cls_01M6R38AW6HH2S",        // 全局唯一租约 ID
  "name": "worker-runner-issue-89c2",
  "owner": "urn:onlyne:agent:issue-runner",// 租约持有者节点 ID
  "resource_ref": { "owner": "urn:onlyne:gateway:compute", "id": "cls_01M6R38AW6HH2S" },
  "state": "running",
  "actual_provider": "cloud-sh-01",
  "created_at": 1793972257000,
  "expires_at": 1793974057000,              // 本次租约到期时间戳
  "retention_until": 1794058657000,         // 磁盘保留截止时间戳

  // 1. 直连数据面通道 (Data Plane Connection)
  "connection": {
    "exec_transport": "ssh",                // ssh | docker_exec | k8s_exec
    "ssh_endpoint": "10.0.12.88:22",
    "auth": {
      "type": "ephemeral_key",              // 临时公私钥认证
      "private_key": "-----BEGIN OPENSSH PRIVATE KEY-----\n...",
      "user": "root"
    },
    "endpoints": {
      "vnc-desktop": "http://10.0.12.88:32155",
      "http-dev-server": "http://10.0.12.88:3000"
    },
    "portal_urls": {                        // 自动生成的带票据交互入口
      "desktop": "http://10.0.12.88:32155/vnc.html?password=xyz&autoconnect=true"
    }
  },

  // 2. 动态解析的环境变量回传
  "resolved_env": {
    "AGENT_TOKEN": "atk_live_77189a",
    "GATEWAY_PORT": "32155"
  },

  // 3. 宿主真值
  "allocated": {
    "arch": "arm64",
    "container_id": "d0f19c83b8a12",
    "host_ip": "10.0.12.88"
  }
}
```

---

### 2. 租约维护动词

#### ① `renew`（续期）
延长沙箱的在线工作时限。累计时长受 `max_ttl_ms` 约束。
```jsonc
{
  "operation": "renew",
  "lease_id": "cls_01M6R38AW6HH2S",
  "ttl_ms": 1800000
}
```
*回执*：更新后的 `expires_at`。若达到 `max_ttl_ms`，返回 `capacity_exceeded` 错误。

#### ② `release`（停机并保留磁盘）
任务完成一个阶段，停止容器运行以省电和释放 CPU/内存，保留卷数据以便后续二次启动。
```jsonc
{
  "operation": "release",
  "lease_id": "cls_01M6R38AW6HH2S"
}
```
*回执*：状态转为 `stopped`，开始计算 `retention_until` 倒计时。

#### ③ `destroy`（物理销毁）
任务结束，抹除容器、删除匿名数据卷、回收映射端口。
```jsonc
{
  "operation": "destroy",
  "lease_id": "cls_01M6R38AW6HH2S"
}
```
*回执*：状态转为 `destroyed`，彻底不可逆。

#### ④ `status`（查探状态与健康检查）
```jsonc
{
  "operation": "status",
  "lease_id": "cls_01M6R38AW6HH2S"          // 若缺省则分页返回调用者名下的全量沙箱列表
}
```

---

## 4. 驱动架构与 SPI 实现设计

Compute Broker Node 内部采用 Driver SPI（服务提供者接口）隔离不同运行时环境：

```text
[ Onlyne Message Bus / Client msg ]
                │
                ▼ (delivery_kind: "compute")
┌────────────────────────────────────────────────────────┐
│            Compute Broker Node (RFC 0019)              │
│  - 校验申领 Schema 与操作鉴权                          │
│  - 维护沙箱账本 (Storage: kv/leases/...)               │
│  - 驱动选择与负载均衡 (Provider Selection)              │
│  - 自动清理调度器 (Safety Net Sweep: TTL / Retention) │
└─────────────────────────┬──────────────────────────────┘
                          │
            ┌─────────────┴─────────────┐
            ▼                           ▼
┌───────────────────────┐   ┌────────────────────────┐
│  Local Docker Driver  │   │  Remote Docker Driver  │
│(Dockerode / Unix Skt) │   │ (SSH Tunnel / TCP / CA)│
└───────────────────────┘   └────────────────────────┘
            │                           │
            ▼                           ▼
    [ 本地沙箱容器 ]              [ 远程云主机容器 ]
```

### 驱动接口契约 (TypeScript)

```typescript
export interface SandboxDriver {
  readonly id: string;
  readonly supportedArchs: Array<"arm64" | "x86_64">;
  
  create(spec: AcquireSpec, leaseId: string): Promise<DriverAllocated>;
  start(containerId: string): Promise<DriverEndpoints>;
  stop(containerId: string): Promise<void>;
  destroy(containerId: string): Promise<void>;
  inspect(containerId: string): Promise<ContainerStatus>;
}
```

---

## 5. 安全性与多租户隔离

1. **临时鉴权（Ephemeral Credentials）**：
   沙箱暴露的 SSH/VNC 凭据均由 Broker 动态生成临时密钥对，并在销毁时同步废止。严禁在配置文件或环境变量中持久写入静态凭据。
2. **安全网兜底（Safety Net Garbage Collection）**：
   Compute Broker Node 启动常驻维护循环（Clock Timer），即使请求方 Agent 崩溃、掉线或遗忘释放，Broker 也会严格按照 `expires_at` 执行 `stop`，按 `retention_until` 执行 `destroy`。
3. **网络与特权逃逸防护**：
   默认容器配置启用非特权模式（`docker_privileged = false`）、隔离的 Bridge 网络，禁止容器访问宿主机敏感路径（如 `/var/run/docker.sock`）。

---

## 6. 与现有 RFC 体系的映射

| 现有 RFC | 关系与差异 |
|---|---|
| **RFC 0006（资源与动态交接）** | 本 RFC 完全基于 RFC 0006 资源模型规范，沙箱是拥有唯一 `resource_ref` 的资源，支持 `handover` 将沙箱管理权转交给另一个 Agent。 |
| **RFC 0013（计算作业资源节点）** | RFC 0013 是**离线批处理作业**（Job）；RFC 0019 是**交互式长驻沙箱**（Sandbox）。二者并存，不互相取代。 |
| **RFC 0015（MCP 资源节点）** | 当沙箱内运行 MCP 服务时，Broker 可通过端口映射将其作为 MCP 资源通过 RFC 0015 进一步暴露。 |
