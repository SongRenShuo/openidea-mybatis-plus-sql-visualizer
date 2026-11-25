# 上游提案索引（T8）

状态：Draft（尚未提交到上游）
范围：OpenIDEA 插件 API 的三项能力缺口。每份都是「问题 / 现状证据 / 提议 / 影响面 / 兼容与降级 /
测试设计 / 开放问题」的完整提案，不含本仓实现代码。

| 编号 | 提案 | 核心主张 | 建议能力 | 风险 |
| --- | --- | --- | --- | --- |
| T8-1 | [`T8-1-database-execute.md`](T8-1-database-execute.md) | 给插件一条只读、逐次确认的 SQL 执行 / EXPLAIN 通道，而不是逼它自带 JDBC sidecar | `database.execute` | high，optional |
| T8-2 | [`T8-2-symbols-detail.md`](T8-2-symbols-detail.md) | 把宿主索引里已有的语法级 detail（字段类型、形参、返回类型、bases）暴露出来 | 无新能力（`project.read`） | 低 |
| T8-3 | [`T8-3-lsp-readonly-whitelist.md`](T8-3-lsp-readonly-whitelist.md) | 只放行 LSP 只读方法白名单，不碰同步、生命周期与任何编辑 | `lsp.read:<server>` | medium，optional |

## 证据基线

上游 checkout `30e72ffa5`（tag `v0.1.51`，SDK `openidea-plugin` 0.1.51）。三份提案里的
`wit/…`、`crates/…`、`plugins/README.md` 都相对这个提交；行号在该提交上逐一 `grep -n` 复核过。
升级 SDK 后行号会漂移，提交 issue 前应按新 checkout 复核。本机 checkout 路径见本仓
`docs/SDK-REF.md:7`。

证据优先级：SDK/WIT 源码与 `plugins/README.md` 为一级；上游 `crates/` 实现为二级（用于宿主落点）；
本仓 IntelliJ 调研报告分册为三级（只用于背景，不单独支撑 API 断言）。

## 提交上游的前置

| 项 | 现状 | 出处 |
| --- | --- | --- |
| 开源许可 | Apache-2.0；提案中的接口草案可按同一许可提交 | `LICENSE:2-3` |
| 仓库 | 公开 GitHub 仓库 `l0ng-ai/openidea` | `README.md:17` |
| Issue 模板 | 使用 `.github/ISSUE_TEMPLATE/feature_request.yml`：Problem / Proposed solution / Acceptance criteria / Alternatives | `.github/ISSUE_TEMPLATE/feature_request.yml:1`、`:17`、`:24`、`:31`、`:41` |
| 受理流程 | 一 issue 一事；维护者打 `planned` 后才进入实现 | `CONTRIBUTING.md:8`、`:12` |
| PR 纪律 | 一 PR 一事；必须写「How it was verified」；UI 变更要前后截图 | `CONTRIBUTING.md:19`、`:27` |
| CLA | 未在仓库根 `CONTRIBUTING.md` / `README.md` / `LICENSE` 找到 CLA 条款（grep 未命中）；是否另有外部 CLA 需向上游确认 | 未确认 |
| API 演进 | 已发布版本冻结；新增宿主函数要升 minor、更新 `tests/api/released/` 快照，并同步 SDK 的 `wit/` 副本 | `plugins/README.md:3022-3056`、`:3059-3075` |
| 新函数落点 | WIT、`wit_host.rs`、`runtime.rs`、`ide-host/plugins/api.rs`、SDK 与 mock、能力与同意 UI、README、快照 | `plugins/README.md:3077-3090` |

提交顺序建议：先 T8-2（不加能力、争议最小），再 T8-3（只读、边界清晰），最后 T8-1（涉及执行与
高风险能力，需要产品决策）。三者都依赖同一次 minor 升级；若要一起提，可以合并成一个「插件 API
1.16 能力包」的追踪 issue，但实现与评审仍应分开。

## 三份提案的共同约定

1. 接口用新类型而不是改已发布的记录 / 变体。`types.error` 冻结（`plugins/README.md:3031-3039`），
   T8-1 因此单列 `query-error`；若上游坚持单一错误类型，文档里给了字符串码的等价方案。
2. 能力名按「对象.动作」：`database.execute`、`lsp.read:<server>`；T8-2 复用 `project.read`。
3. 高风险能力一律 optional，拒绝时必须有可用降级；降级不得把「没结果」显示成「没问题」。
4. 预算按平台既有档位设计：provider 50 ms / 500 ms、command 5 s、task 60 s、内存 256 MiB
   （`plugins/README.md:1506`、`:2649-2653`）。超预算的查询进 task，provider 只读缓存。
5. 所有结果有硬上限：行/项数、单串、总字节、超时；超限显式标记或报错，不静默截断。

## 复核记录

- 每份提案完成后，对其中的每个 `文件:行号` 用 `grep -n` / `sed -n` 回读一次；发现的漂移已在
  对应文件内修正（例如 LSP 命名空间常量与生命周期常量在源码里的真实位置，见 T8-3 的现状表）。
- 本索引不重复提案内的引用；引用总数与逐条复核结果见各提案正文与提交说明。