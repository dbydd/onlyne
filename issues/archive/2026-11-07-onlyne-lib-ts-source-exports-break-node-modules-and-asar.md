# @onlyne/lib 的 exports 指向 .ts 源码，装进 node_modules 或 asar 后无法加载

日期：2026-11-07
报告人：dsh-onlyne 打包现场（DS-Bot Electron 包）
路径说明：`@onlyne/*` 在 `packages/` 下；打包脚本在 `oh-my-dsh/ds-bot/app/scripts/`。

## 现象

1. ds-bot 打包成 `DS Bot.app` 后，所有 Onlyne/DSH 插件加载失败，Electron 报 `ERR_UNSUPPORTED_NODE_MODULES_TYPE_STRIPPING`。开发态从 profile 目录直接加载同样的包，正常启动。
2. 现有绕法是打包期用 esbuild 把每个入口 bundle 成 `build/index.js` 并改写 `exports`（`oh-my-dsh/ds-bot/app/scripts/build-plugins.mjs:37-58`），脚本头部注释写明了这条错误（:1-6），清单里点名 `@onlyne/lib` 与六个 omd 插件（:15-26）。

## 根因分析与证据

1. `packages/lib/package.json` 的 `main`、`types` 指向 `./src/index.ts`（:6-7），`exports` 的 `.` 与 `./memory` 指向 `./src/index.ts`、`./src/memory.ts`（:19-21），`files` 只带 `src`（:23）。包没有任何编译产物。
2. 同族的 `@onlyne/*` 包是同一形状：`packages/*/package.json:6-7,19-20`，已确认的有 compute-core、compute-gateway、grok-bot、im-core、im-gateway、im-lark、im-qq、im-wechat、mcp-core、mcp-gateway、mcp-server、node-host。
3. Bun 与 dsh 自己的 loader 会剥类型，所以在仓库内和 profile 目录里都能跑。Node 对 `node_modules` 下的 `.ts` 一律拒绝：本机 Node v26.12.0 实测，一个只含 `export const x: number = 1` 的包被 import 时抛 `ERR_UNSUPPORTED_NODE_MODULES_TYPE_STRIPPING`，位置在 `node:internal/modules/typescript:155`。Electron 自带 Node 走同一条规则；asar 内的包路径同样落在 `node_modules` 下。
4. 绕法只覆盖 ds-bot 的 beforePack（`oh-my-dsh/ds-bot/app/scripts/build-plugins.mjs:8-9`），覆盖面由脚本里的 `PACKAGES` 清单决定：新增一个包忘了登记，整包 app 就少一个插件。开发态与其它消费方（`packages/omp-onlyne`、`external-integrations/tern-onlyne`、`qoder-onlyne`）仍然读 `.ts`。

## 影响

1. 发布到 npm 的包不可用：`files` 里没有编译产物，`main` 指向源码。
2. 任何把 `@onlyne/lib` 装进 `node_modules` 的运行环境都会启动失败；错误指向 Node 内部模块，与我们的包名无关，排障成本高。
3. 打包正确性依赖 ds-bot 的私有清单，迁移或新增插件时容易漏。
4. 同一份代码在 Bun、dsh loader、Node 三种加载器下行为不同，测试覆盖到的不等于用户拿到的。

## 建议修法

1. 每个包加构建：`src/**/*.ts` → `dist/*.js` 加 `.d.ts`；`main`/`types`/`exports` 指向 `dist`，`files` 带 `dist`。保留 `publishConfig`。
2. 开发态用条件导出（`development` 条件指向 `src/index.ts`）或 workspace 链接保留热改；发布态与打包态一律加载 `dist`。
3. `prepack` 跑构建；CI 断言 `dist` 存在且能解析（`node -e "import('@onlyne/lib')"`）。
4. `build-plugins.mjs` 里删掉 `@onlyne/lib` 与 omd-* 的 esbuild 拷贝，只留客户端资源打包；插件包由自己的构建产出。

## 验收判据

- 纯 Node 22+ 环境里 `npm pack` 后安装，`node -e "import('@onlyne/lib')"` 成功，不再抛 `ERR_UNSUPPORTED_NODE_MODULES_TYPE_STRIPPING`。
- Electron 打包不再执行 esbuild 拷贝也能加载全部 Onlyne 插件；从 `build-plugins.mjs` 的 `PACKAGES` 里删掉这些包后 app 正常启动。
- `tsc --noEmit` 使用 `dist` 的 `.d.ts` 通过，`bun tests/verify.ts` 等既有测试保持通过。
- 新增一个 `@onlyne/*` 包时不需要改 ds-bot 的打包脚本就能进 app。
