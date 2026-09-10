# 描述文件与合并（TEMPLATE）

## 1. 位置与命名

- 调度输入：`<root>/.agents/.schedule/<名>/template.workspace.jsonc`，
  子目录嵌套即子 workspace（`<名>/<子名>/template.workspace.jsonc`）。
  子目录名即 workspace 名。文件名全树统一为 `template.workspace.jsonc`。
- 手写覆盖：对应真实实例 `<root>/.ws/<嵌套路径>/.onlyne/swarm.workspace.jsonc`，
  主要用途是手写跨层回边与实例级微调。调度器永不重写该文件；实例不存在时按合并结果生成。
- root 自身的描述：`<root>/.agents/.schedule` 的父级即 root workspace，
  root 的模板字段直接写在 `<root>/.onlyne/swarm.workspace.jsonc`（不存在则视为 `{}`）。
  root 也受合并规则约束（上层为空）。

## 2. 字段（终版四项 + `$schema` 编辑器提示）

```jsonc
{
  // 编辑器提示，指向 GitHub 上的 template.workspace.schema.json；调度器忽略
  "$schema": "https://raw.githubusercontent.com/dbydd/onlyne-swarm/main/template.workspace.schema.json",
  // workspace 名，缺省取目录名
  "name": "planner",
  // 系统提示词，内联写进 jsonc（.roles/ 目录已删除，不再约定 md 格式）
  "role": "你是规划节点……",
  // 模型三元组（供 orca 拉起 pi 时选用）
  "model": { "provider": "openai", "model": "gpt-5", "effort": "high" },
  // 跨层回边：目标名数组，树相对路径，支持相对写法
  // 以本 workspace 在树中的位置为基准解析：../reviewer、sibling/worker、../../a/b/c
  "back_edges": ["../reviewer"]
}
```

`max_retries / deadline_secs / backoff_secs / concurrency` 已全部删除。
告警阈值如需加，未来以 `warn_thresholds` 可选段进入，不影响执行流。

## 3. 深合并规则

优先级：上层 template < 本层 template < 实例 `swarm.workspace.jsonc`。

- 对象递归合并；标量（字符串/数字/布尔）下层覆盖上层；
- `back_edges` 取并集去重（相对路径先按声明 workspace 位置归一化为树绝对路径再合并）；
- 其余数组 whole-replace；
- `role` 为标量，覆盖语义（如下层只想追加，自行全文复制上层后改）。

## 4. 生成语义（sync）

`onlyne-swarm workspace create|sync` 与 `run` 启动时的自动 sync：

1. 遍历 `.agents/.schedule` 树，逐层合并得到每个 workspace 的 effective 描述；
2. 对缺失的 `.ws/<路径>/` 创建目录，并写入
   `.onlyne/config.toml`（loopback 专精 + `[swarm] enabled = true`）、
   `.onlyne/swarm.workspace.jsonc`（= effective 描述快照）、`channels/loopback/` 空位；
3. 已存在的实例目录：只刷新 `onlyne_in/` 软链，不覆盖 `config.toml` 与
   `swarm.workspace.jsonc`（用户手改保留）；
4. 描述树中已删除但实例仍存在的路径：不删除实例目录，TUI 告警 `orphan-instance`；
5. `back_edges` 目标不存在：`sync` 直接报错退出，非告警；
6. `onlyne_in/<目标>/` 软链：每个真实 workspace（含 root）各一份，目标为树内其他
   workspace 的 `.onlyne/channels/loopback/in` 绝对路径；目标缺失时留悬链并在
   `status` / TUI 中告警。

## 5. 示例树

```text
.agents/.schedule/planner/template.workspace.jsonc
.agents/.schedule/planner/worker/template.workspace.jsonc
.agents/.schedule/reviewer/template.workspace.jsonc
.ws/planner/
.ws/planner/worker/
.ws/reviewer/
```
