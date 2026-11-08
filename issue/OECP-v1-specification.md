# Onlyne 弹性计算代理节点协议规范 (Onlyne Elastic Compute Protocol - OECP v1)

## 一、 背景与前因后果

### 1. 痛点与现有架构演进
在目前已落地的 Agent 架构体系中（如 `oh-my-dsh`、`omd-issue-runner`），Agent 往往需要执行具有破坏性、需要特定依赖、甚至带图形桌面（GUI）的任务：
- **阶段一（本地 OrbStack 绑定）**：由 `omd-sandbox-vm` 直接通过 `orbctl` 管理本地 arm64 Linux 虚拟机，并在上层由 `omd-compute` 提供基础租约与到期回收。
- **阶段二演进需求（多云服务器与异构算力接入）**：
  在实际生产与高并发场景中，本地宿主机的 CPU/内存有限；同时用户拥有多台独立的云服务器（Aliyun/Tencent/AWS 等，涵盖 arm64 与 x86_64）。需要将计算任务透明地分发到不同的云主机、容器集群或外部 Sandbox Provider。

### 2. 核心架构决议
1. **容器本身不是 Onlyne 节点**：
   Onlyne 协议的核心语义是消息总线、会话线程、认领与注意力机制（threads / grants / attention）。容器的核心语义是状态机、资源契约与执行通道。若容器成为节点，生命周期与心跳机制将与消息协议耦合；因此：
   - **控制面（Control Plane）** 走 Onlyne 消息总线，由常驻的**计算调度翻译代理节点**（Compute Broker Node，如 `urn:onlyne:agent:compute`）接管。
   - **数据面（Data Plane）**（命令执行、文件同步、流式日志、桌面画面）走直连通道，不跨 Onlyne 转发。
2. **底层算力边界以 Docker 镜像体系为最小共识**：
   无论底层是本地 OrbStack、单机 Docker、远程云主机 Docker Daemon、K8s Pod 还是轻量级 MicroVM（Kata / Firecracker），输入边界严格基于 OCI / Docker 标准（Image, Env, Port, Volume, Workdir）。
3. **计算 = 带租约生命周期的外部外部资源**：
   采用 `acquire`（申领）、`renew`（续期）、`release`（冷休眠/停机保留磁盘）、`destroy`（物理销毁）的三段式状态机。
4. **租约所有权（Lease Ownership）、容量上限与续期心跳**：
   - **所有权强校验**：`acquire` 将租约的 `owner` 绑定为已认证发送方 `authenticated_author`（`compute://` 路径调用使用调用方会话节点 ID）。同名 `acquire` 重入及 `renew`、`release`、`destroy` 必须校验调用方等于 `owner`；非所有者调用返回 `forbidden`。
   - **容量硬上限（`max_ttl_ms`）**：当请求的 `ttl_ms` 或续期后累计时长超过 `max_ttl_ms` 时，直接拒绝并返回 `capacity_exceeded`，严禁静默截断。
   - **独立 `renew` 动词与心跳**：`renew` 作为独立动词延长 `running` 租约，调用方（如 `issue-runner`）在 `ttl / 2` 处自动发送心跳续期；租约到期或达到 `max_ttl_ms` 被 `sweep` 停机时，Broker 向 `owner` 推送 `expired` 停机通知。

---

## 二、 协议总体架构与消息流

```mermaid
sequenceDiagram
    autonumber
    participant Requester as 请求方 Agent (Bot / Worker)
    participant Bus as Onlyne 消息总线
    participant Broker as Compute Broker Node (翻译与生命周期外部管理)
    participant Provider as 算力 Provider (本地 Docker / 远程云主机 / K8s)

    Note over Requester,Broker: 1. 算力申请与实例化
    Requester->>Bus: send(delivery_kind: "compute", op: "acquire", spec)
    Bus->>Broker: 投递至 urn:onlyne:agent:compute
    Broker->>Provider: 翻译为平台原生 API (Docker Create/Start, 分配端口与网络)
    Provider-->>Broker: 返回容器真值 (实际暴露的 NAT 端口、内网 IP、动态环境凭据)
    Broker->>Bus: reply(delivery_kind: "compute", status: "running", descriptor)
    Bus-->>Requester: 获得算力描述符 (含直连 Connection 信息)

    Note over Requester,Provider: 2. 直连数据面通道 (不经过 Onlyne)
    Requester->>Provider: 直连 SSH / Docker Exec / noVNC 桌面

    Note over Requester,Broker: 3. 租约延期 (Heartbeat / Renew)
    Requester->>Bus: send(delivery_kind: "compute", op: "renew", lease_id, ttl)
    Bus->>Broker: 延长租约截止时间

    Note over Requester,Broker: 4. 任务结束与回收
    alt 主动释放 (保留环境快照)
        Requester->>Bus: send(delivery_kind: "compute", op: "release", lease_id)
        Broker->>Provider: Container Stop (保留卷与已安装包，停机断电)
    else 彻底销毁
        Requester->>Bus: send(delivery_kind: "compute", op: "destroy", lease_id)
        Broker->>Provider: Container Remove (连同临时 Volume 彻底抹除)
    else 超时孤儿回收 (Safety Net)
        Broker->>Broker: 本地 Ledger 扫描发现租约逾期
        Broker->>Provider: 自动下发 Stop / Destroy
    end
```

---

## 三、 完整报文协议定义 (OCCP v1)

请求与回执均承载于 Onlyne 的标准消息封套：
```jsonc
{
  "delivery_kind": "compute",
  "msg": "<人类可读的提示文案>",
  "meta": { ... } // 协议结构体
}
```

### 1. `acquire`（申领算力）报文规范（预防性全量字段规划）

```jsonc
{
  "delivery_kind": "compute",
  "msg": "申请全栈开发验证容器环境",
  "meta": {
    "to": "urn:onlyne:agent:compute",
    "operation": "acquire",
    "name": "worker-runner-issue-89c2",       // 租约与容器标识（相同 name 重复请求视为幂等重入/续约）
    
    // -------------------------------------------------------------
    // 1. Docker / OCI 容器标准规格 (Container Spec)
    // -------------------------------------------------------------
    "container": {
      "image": "docker.io/library/debian:12-slim", // 镜像名 (支持完整 tag 或 digest)
      "image_pull_policy": "if_not_present",      // always | if_not_present | never
      "command": ["/bin/sh", "-c"],               // 入口覆盖 (对应 Entrypoint/Cmd)
      "args": ["sleep infinity"],
      "workdir": "/workspace",                    // 容器内初始工作目录
      
      // 输入环境变量注入
      "env": {
        "NODE_ENV": "development",
        "CI": "true"
      },

      // 要求控制节点回传的环境变量清单 (从容器内动态探测、或由调度平台动态注入的安全凭据)
      "expose_env": [
        "AGENT_TOKEN",
        "GATEWAY_PORT",
        "INTERNAL_PROXY_URL"
      ],

      // 文件与目录挂载诉求
      "mounts": [
        {
          "type": "bind",                         // bind | volume | tmpfs
          "host_path": "/Users/.../workspaces/w1",// 宿主路径 (若由平台分配可留空)
          "container_path": "/workspace",         // 容器内目标路径
          "read_only": false
        }
      ]
    },

    // -------------------------------------------------------------
    // 2. 硬件资源规格与调度约束 (Resource & Placement Constraints)
    // -------------------------------------------------------------
    "resources": {
      "arch": "arm64",                            // 架构约束: arm64 | x86_64 | any
      "cpus": 4.0,                                // 算力核数限制
      "memory_mb": 8192,                          // 内存限额 (MiB)
      "gpu": {
        "model": "nvidia-t4",                     // 可选 GPU 型号
        "count": 0
      },
      "shm_size_mb": 1024                         // /dev/shm 共享内存大小 (跑无头浏览器必备)
    },

    // -------------------------------------------------------------
    // 3. 网络拓扑与端口暴露需求 (Network & Ingress Spec)
    // -------------------------------------------------------------
    "network": {
      "mode": "bridge",                           // bridge | host | overlay | none
      "nat_ports": [
        {
          "name": "vnc-desktop",                  // 端口逻辑别名
          "container_port": 6080,
          "protocol": "tcp",
          "public": true,                         // 是否需要生成公共/网关级访问端点
          "preferred_host_port": 0                // 0 表示由宿主动态分配
        },
        {
          "name": "http-dev-server",
          "container_port": 3000,
          "protocol": "tcp",
          "public": false
        }
      ],
      "egress_policy": "allow_all"                // allow_all | intranet_only | air_gapped
    },

    // -------------------------------------------------------------
    // 4. Provider 调度偏好与特定脏活参数 (Provider Meta)
    // -------------------------------------------------------------
    "provider_meta": {
      // 平台调度选择策略：auto (自动匹配) 或显式声明特定集群/节点覆盖
      "selector": "auto",                         // auto | local-orbstack | cloud-sh-01 | k8s-dev
      "fallback": ["cloud-sh-01", "local-orbstack"], // 降级提供商列表
      "region": "urn:onlyne:region:cn-east-shanghai",// 地域亲和性
      
      // 针对具体 Provider 的底层 Pass-through 参数 (供特定驱动直通)
      "driver_options": {
        "docker_privileged": false,               // Docker 特权模式
        "docker_caps_add": ["SYS_ADMIN"],         // Linux Capabilities
        "k8s_node_selector": { "disk": "ssd" },   // K8s 亲和性透传
        "cloud_security_group": "sg-xxxxxx"       // 云安全组透传
      }
    },

    // -------------------------------------------------------------
    // 5. 租约与生命周期控制 (Lease Control)
    // -------------------------------------------------------------
    "lease": {
      "ttl_ms": 1800000,                          // 租约单次时限: 30分钟
      "max_ttl_ms": 14400000,                     // 允许累计续约的硬上限: 4小时
      "idle_timeout_ms": 600000,                  // 容器内无 CPU/IO 活动时的静默自动停止时间
      "retention_ms": 86400000                    // release 停机后保留磁盘的时间 (24小时)，逾期销毁
    },

    "purpose": "PR #142 automated validation run"
  }
}
```

---

### 2. 回执报文规范（`descriptor` 资源描述符）

```jsonc
{
  "delivery_kind": "compute",
  "msg": "compute acquire: worker-runner-issue-89c2 is running on cloud-sh-01.",
  "meta": {
    "lease_id": "cls_01M6R38AW6HH2S",            // 全局唯一的租约追踪 ID
    "name": "worker-runner-issue-89c2",
    "state": "running",                           // running | stopped | provision_failed
    "actual_provider": "cloud-sh-01",             // 最终实际落地的集群/平台
    "created_at": 1793972257000,
    "expires_at": 1793974057000,                  // 本次租约截止时间
    "retention_until": 1794058657000,             // 磁盘保留死线
    
    // -------------------------------------------------------------
    // 1. 直连数据面通道 (Connection & Ingress Endpoints)
    // -------------------------------------------------------------
    "connection": {
      "exec_transport": "ssh",                    // docker_exec | ssh | k8s_exec
      "ssh_endpoint": "10.0.12.88:22",            // 直连执行入口
      "auth": {
        "type": "ephemeral_key",                  // 临时公私钥认证
        "private_key": "-----BEGIN OPENSSH PRIVATE KEY-----\n...",
        "user": "root"
      },
      
      // 端口映射真值
      "endpoints": {
        "vnc-desktop": "http://10.0.12.88:32155",
        "http-dev-server": "http://10.0.12.88:3000"
      },
      
      // 专为交互打造的带鉴权票据 URL
      "portal_urls": {
        "desktop": "http://10.0.12.88:32155/vnc.html?password=xyz&autoconnect=true"
      }
    },

    // -------------------------------------------------------------
    // 2. 动态回传的环境变量真值 (Resolved Environment)
    // -------------------------------------------------------------
    "resolved_env": {
      "AGENT_TOKEN": "atk_live_77189a",
      "GATEWAY_PORT": "32155",
      "INTERNAL_PROXY_URL": "http://10.0.0.1:7890"
    },

    // -------------------------------------------------------------
    // 3. 实际分配的资源真值
    // -------------------------------------------------------------
    "allocated": {
      "arch": "arm64",
      "container_id": "d0f19c83b8a12",
      "host_ip": "10.0.12.88"
    }
  }
}
```

---

### 3. 生命周期维护动词

#### ① `renew`（续期）
```jsonc
{
  "delivery_kind": "compute",
  "msg": "续期容器租约",
  "meta": {
    "to": "urn:onlyne:agent:compute",
    "operation": "renew",
    "lease_id": "cls_01M6R38AW6HH2S",
    "ttl_ms": 1800000                             // 申请再延长 30 分钟
  }
}
```

#### ② `release`（停机但保留磁盘状态）
```jsonc
{
  "delivery_kind": "compute",
  "msg": "任务暂时完成，释放算力并休眠",
  "meta": {
    "to": "urn:onlyne:agent:compute",
    "operation": "release",
    "lease_id": "cls_01M6R38AW6HH2S"
  }
}
```
*响应回执*：容器状态变为 `stopped`，开始倒计时 `retention_until`。

#### ③ `destroy`（物理销毁）
```jsonc
{
  "delivery_kind": "compute",
  "msg": "工单合流结束，彻底抹除环境",
  "meta": {
    "to": "urn:onlyne:agent:compute",
    "operation": "destroy",
    "lease_id": "cls_01M6R38AW6HH2S"
  }
}
```
*响应回执*：容器被 Force Remove，临时数据卷与端口映射彻底释放。

#### ④ `status`（查探状态与健康检查）
```jsonc
{
  "delivery_kind": "compute",
  "msg": "获取当前容器状态",
  "meta": {
    "to": "urn:onlyne:agent:compute",
    "operation": "status",
    "lease_id": "cls_01M6R38AW6HH2S"            // 若缺省则返回该节点名下的全量清单
  }
}
```

---

## 四、 Compute Broker 节点内部脏活封装架构 (SPI)

在实现此节点时，建议将不同平台特定的脏活封装为 Driver SPI（驱动服务接口）：

```text
[ Onlyne Message Bus ]
          │
          ▼ (delivery_kind: "compute")
┌────────────────────────────────────────────────────────┐
│           Onlyne Compute Broker Node                   │
│  - 报文解析与校验 (Schema Validation)                    │
│  - 租约账本持久化 (Ledger: leases.json)                 │
│  - 定时 Sweep 线程 (TTL 到期 Stop, Retention 到期 Delete)│
│  - 凭据签名与回传环境变量组装                          │
└─────────────────────────┬──────────────────────────────┘
                          │
            ┌─────────────┴─────────────┐
            ▼                           ▼
┌───────────────────────┐   ┌────────────────────────┐
│ Local Docker Driver   │   │ Remote Cloud Driver    │
│ (OrbStack / Dockerode)│   │ (SSH Tunnel / Docker)  │
└───────────────────────┘   └────────────────────────┘
            │                           │
            ▼                           ▼
    [ 本机 Docker 容器 ]         [ 远端云服务器 Docker ]
```

### 推荐复用的基础设施库
1. **统一容器操作**：使用 `dockerode`（Node.js 标准 Docker Engine API 客户端）或直接封装 `@opencoredev/sandbox-sdk` 的 Provider 接口。
2. **远程云服务器打通**：
   - 方式 A（轻量运维）：在云服务器上通过 `tailscale` 组建内网，Broker 直接通过 HTTP 调云服务器的 Docker Socket。
   - 方式 B（零配置）：通过 SSH 自动建立转发：`ssh -N -L /tmp/cloud.sock:/var/run/docker.sock user@cloud-ip`。

---

## 五、 状态说明与交付记录

该规范已在 `/Users/dbydd/Documents/progs/onlyne/issue/` 归档。它完全兼容我们在 `oh-my-dsh` 中已验证的 `plugins/omd-compute` 与 `omd-issue-runner` 逻辑，且为未来支持多云实例、异构 CPU 调度、NAT 自动透传留足了完备的扩展字段。
