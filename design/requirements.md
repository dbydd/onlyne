# Onlyne v3 系统设计需求清单

- REQ-1 Agent 模型入口保持 `msg({ delivery_kind, msg, meta })`，由 SystemOne 解释意图并委派到具体动作；严禁绕开统一消息与资源入口引入专有网络旁路。
- REQ-2 节点去中心化：每个节点代表一个已认证主体，基于 Ed25519 签名与公钥交换，只维护经手自己的账本与事件，不存在中心化服务。
- REQ-3 会话线程与归属：thread 由其宿主节点独占拥有（owner），分配单调递增 seq，参与者通过 grant notice 获得访问与回复许可。
- REQ-4 资源模型与三段式交接：资源是 endpoint 托管的有状态或计算实体，拥有唯一 `resource_ref`；支持动态切换 handler，交接遵循三段式冻结与提交（RFC 0006）。
- REQ-5 沙箱算力节点边界：执行沙箱容器本身不是 Onlyne 节点，由 Compute Broker 节点统一管理控制面（RFC 0019）。
- REQ-6 沙箱控制面与数据面分离：申领（acquire）、续期（renew）、休眠（release）、销毁（destroy）走 Onlyne 消息与资源总线；命令执行（SSH/Exec）与桌面（noVNC）直连容器端口，流量不跨 Onlyne 转发。
- REQ-7 沙箱租约状态机与超时安全网：沙箱具有 `provisioning`、`running`、`stopped`、`destroyed`、`failed` 状态机；Compute Broker 设独立时钟，逾期租约（TTL）自动停机，逾期保留期（retention）自动物理销毁。
- REQ-8 OCI 容器契约规范：沙箱定义严格基于 Docker/OCI 标准镜像、环境变量、端口暴露映射与数据卷挂载。
