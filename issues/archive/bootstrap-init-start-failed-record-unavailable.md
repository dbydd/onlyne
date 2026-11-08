# /onlyne init 半初始化死锁与启动阶段 not_found 穿透（The requested record is unavailable）

日期：2026-11-06
报告人：user 现场反馈（dbydd）

## 现象

用户在 OMP 项目中进行节点初始化（`/onlyne init`）时遭遇连续死锁和报错：

1. **首次 init 被拒**：在 git 仓库中执行 `/onlyne init`，表单填写保存后，报错阻拦：
   `Refusing to create a node key inside a git work tree: .onlyne/keys is not ignored. Add '.onlyne/keys' to .gitignore...`
   但此时 `.onlyne/config.toml` **已经被持久化到磁盘**。
2. **二次 init 无法执行**：用户按提示在 `.gitignore` 中追加 `.onlyne` 后，回到 OMP 再次执行 `/onlyne init`，提示：
   `This project is configured. Use /onlyne config to edit it.`（这个项目已有配置。请用 /onlyne config 修改），初始化命令直接拒绝执行。
3. **改用 config 启动报错**：用户转而执行 `/onlyne config`，点击保存并启动，界面弹错：
   `Configuration written, start failed: The requested record is unavailable.`
4. **彻底清空重试依然失败**：用户删除整个 `.onlyne` 目录后重新执行 `/onlyne init`，依然报错：
   `Configuration written, start failed: The requested record is unavailable.`。

## 根因分析与证据（文件与行号）

本缺陷由两个设计缺陷叠加导致：

### 1. not_found 异常在跨模块加载环境下未能正确捕获（核心崩溃点）

* **错误抛出点**：`packages/lib/src/domains.ts:169-173`
  ```ts
  async describe(auth: Auth, id: string): Promise<DomainMembership> {
    this.authenticate(auth);
    validateDomainId(id);
    return this.#ctx.storage.transaction(async tx => requireValue(await tx.get<DomainMembership>(key('domain', id))));
  }
  ```
  在全新数据库中，节点尚未加入任何 region，`describe` 查找 `key('domain', id)` 必定返回 `undefined`，`requireValue`（`packages/lib/src/core.ts:23`）抛出带有 `'The requested record is unavailable.'` 的 `OnlyneError('not_found', ...)`。
* **错误逃逸点**：`packages/omp-onlyne/src/runtime.ts:298-303`
  ```ts
  if (!current || current.domain_id !== this.config.region) {
    let expected_revision: string | undefined;
    try { expected_revision = (await this.node.domains.describe(this.node.auth, this.config.region)).revision; }
    catch (error) { if (!(error instanceof OnlyneError && error.error.code === "not_found")) throw error; }
    await this.node.domains.join(this.node.auth, this.config.region, { ...mutation(), expected_revision });
  }
  ```
  同类隐患亦存在于 `packages/omp-onlyne/src/runtime.ts:447-449`（`ensureTrustThread` 中的 `threads.describe`）。
* **原因**：OMP 加载插件机制（如带 `?mtime=` 的动态加载或符号链接隔离）导致 `omp-onlyne` 运行时上下文中持有的 `OnlyneError` 与 `@onlyne/lib` 抛出的 `OnlyneError` 构造函数并非同一引用。`error instanceof OnlyneError` 判定为 `false`，导致本应用作常规探测分支（初次启动时不存在记录）的 `not_found` 异常未能被识别，直接被 rethrow 冒泡至 `startRuntime` 外部，将启动流程彻底打断。

### 2. 配置落盘与启动检查的非原子性与排他门禁死锁（交互设计缺陷）

* **落盘早于密钥校验**：`packages/omp-onlyne/src/ui/bootstrap.ts:223-225`
  ```ts
  await saveConfig(config);
  persisted = true;
  const runtime = await startRuntime(this.pi, this.ctx);
  ```
  `saveConfig` 先写磁盘，随后才调用 `startRuntime`。而 `ensureKeyDir`（`packages/node-host/src/keys.ts:90-95`）对 gitignore 的检查发生在 `startRuntime` 的最开头。一旦检查不通过抛错，系统进入**配置已保存、节点未启动、密钥未生成**的半死状态。
* **门禁阻断修复**：`packages/omp-onlyne/src/ui/index.ts:73-79`
  ```ts
  if (command === "init") {
    const existing = runtime?.config ?? (await loadConfig(ctx.cwd).catch(() => undefined));
    if (existing !== undefined) {
      ctx.ui.notify(t("configured"), "info");
      return;
    }
  }
  ```
  用户修正 `.gitignore` 后，重新运行 `/onlyne init` 时因检测到 `config.toml` 已存在而被直接弹回，被迫转向 `/onlyne config`，心智负担极高。

## 影响

1. **首次配置可用性严重受损**：任何处于未 gitignore 仓库环境下的项目，首次执行 `/onlyne init` 都会因半初始化状态进入死锁。
2. **全新安装启动必崩**：一旦存在模块实例差，无论全新 init 还是 config，只要底层调用 `describe` 触发合法的 `not_found`，就会因 `instanceof` 失效抛出 `The requested record is unavailable.`，导致节点完全无法启动。

## 建议修法

1. **健壮化 not_found 错误匹配**：
   在 `packages/omp-onlyne/src/runtime.ts` 中废弃脆弱的 `instanceof OnlyneError`，改用结构化推导或统一工具函数，参照 `packages/lib/src/resolve.ts:22-25` 的做法：
   ```ts
   function isNotFoundError(error: unknown): boolean {
     if (error instanceof OnlyneError) return error.error.code === "not_found";
     if (typeof error === "object" && error !== null) {
       const failure = (error as { error?: { code?: string } }).error;
       if (failure?.code === "not_found") return true;
       if ((error as { message?: string }).message === "The requested record is unavailable.") return true;
     }
     return false;
   }
   ```
   同步检查 `runtime.ts:301` 与 `runtime.ts:448`。
2. **前置 gitignore 与权限预检**：
   在 `bootstrap.ts` 执行 `saveConfig` 之前，或在表单校验阶段（`save()` 方法前半段），提前预检 `ensureKeyDir` 是否会被 git 阻拦。若阻拦，直接在表单内提示报错，不写入任何配置文件。
3. **放宽 `/onlyne init` 的排他限制**：
   当检测到 `existing !== undefined` 但当前 `runtime === undefined`（即配置存在但未成功跑起来过）时，不应直接 return，而应允许重新打开 bootstrap 面板或重试初始化。

## 验收判据

1. 在干净的未将 `.onlyne` 写入 `.gitignore` 的 git 仓库中，执行 `/onlyne init` 被阻拦后，磁盘上不残留半初始化的 `config.toml`；或若已残留，再次运行 `/onlyne init` 不会直接拒收，能顺利继续启动。
2. 跨模块/多实例加载场景下，全新的节点执行 `/onlyne init` 时，`domains.describe` 返回的 `not_found` 被正确捕获并推进到 `domains.join`，启动成功显示 `Onlyne 已初始化`，不再报 `The requested record is unavailable.`。
