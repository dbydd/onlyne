# issues

本目录只放**未修的缺陷单**，一份一个文件：现象、证据（文件与行号）、影响、建议修法、验收判据。修完就删，历史在 git 里。

`archive/` 放 2026-10-25 到 10-27 的轮次报告（14 份现场证据）。它们是当时能复现、后来未必还在的观察，不是工单；需要考古时再读，平时不要拿它当现状。

## 未修（0）

2026-10-28 的五张单（主动发起解析到自己、短名与 urn 双写被拒、手填 UUID、网关上下文目录、信任对端不可寻址）已随「解析与投递收敛」一次修掉，验收判据落在 `packages/lib/tests/verify.ts`（phaseAddressee 跨节点断言、phaseInitiateTrust）与 `packages/omp-onlyne/tests/decider.test.ts`（O-2A 原消息选择、引用归一与冲突预检）的回归里；实施记录见 `docs/2026-10-28-audit-and-restructure.md`。遗留的结构性还债（decider 拆模块、client/http 拆文件、tern vendor→共享包）不是缺陷，走该文档第 6 步。

## 已修删除（留档于 git 之外的备份）

`injection-not-consumed.md`、`cyanea-trust-carly.md`、`migration-owner-remap-grant-not-reissued.md` —— 分别由 `3235f2f`/`f23ffe6`/`b4aee3e`、`6f6bc40`、`b4de14b`/`503785d` 修掉。
