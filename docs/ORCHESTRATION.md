# ORCHESTRATION：多子代理并行实施（一次性做完全部功能）

> 用法：在新仓工作区开一个会话，粘贴 §1 主控提示词；主控按 §3 逐车道派子代理，用 §2 模板填参数。
> 本文件是编排规则的**唯一权威**；`TASKS.md` 是任务内容，`ROADMAP.md` 是门禁与前提。

## 1 主控提示词（整段粘贴给新会话）

```
你是本仓的实施主控。目标：按 docs/TASKS.md 一次性实现全部功能（P1–P8），用多分支 + 多工作树并行，
你负责建车道、派子代理、合并、清理、最终门禁。不要自己写业务代码（除注册类单点文件与冲突裁决）。

【仓库与环境】
- 主仓（实施地）：F:\code\Java\Gitee\SongRenShuo\openidea-mybatis-plus-sql-visualizer
- IntelliJ 仓（只读参考 + 引擎侧改动）：F:\code\Java\Gitee\SongRenShuo\mybatis-plus-sql-visualizer
- 基线：plugin API ^1.15；SDK tag v0.1.51；Rust 1.98.1 + wasm32-wasip2；Node 22.22.3（.nvmdrc）
- Git Bash 的 PATH 里没有 cargo/openidea-plugin：
  export PATH="/c/Users/USER/.cargo/bin:$PATH"（每条涉及它们的命令都要带）
- 长命令（cargo build/clippy、openidea-plugin build、gradle）一律后台跑，靠通知继续；禁止 sleep 轮询。

【先读】
1) AGENTS.md（仓级红线）2) docs/ROADMAP.md（门禁与 7 条前提）3) docs/TASKS.md（任务卡）
4) docs/ARCHITECTURE.md（双路径/契约/置信度）5) docs/FEATURES.md（四档范围）

【开工前 15 分钟：前提收口】
先确认 docs/调研报告/…/13-prereq-validation.md 的 P-1/P-3/P-4 结论（若尚未验证，先只做不依赖它们
的车道：L1/L2/L3/L4/L6）。三条前提任一不成立时，先改 ROADMAP 对应阶段的方案，再改车道计划。

【车道与工作树】
每条车道：分支 feat/<lane>，工作树 .worktrees/lane-<lane>。
  git worktree add .worktrees/lane-<lane> -b feat/<lane>
子代理只在自己的工作树里改文件、跑门禁、提交（每完成一个可提交的节点就提交，别攒）。

【目录所有权（防冲突，硬规则）】
- 只有「注册类单点文件」由主控独占：plugin.toml、src/lib.rs、Cargo.toml、Cargo.lock、README.md、
  locales/*.json、.github/workflows/*。子代理**一律不许改**这六类。
- 子代理需要注册（命令/能力/provider/MCP 工具/视图/依赖/文案键）时，写
  docs/registration/<lane>.md，格式见 §4；主控在合并后统一应用。
- 各车道只拥有自己目录（见 §3 表的「拥有目录」列）；跨目录需要改动时在报告里提出，不许直接动。

【派子代理】
按 §3 表逐条派，用 §2 模板填参数。每条车道的报告必须含：改了哪些文件、门禁读数（fmt/clippy/test/
build/check）、提交哈希、工作树是否干净、遗留与风险。报告禁止出现没跑过的读数。

【合并顺序（依赖优先）】
1) L1/L2/L3/L4/L6（基础层，可同时开）
2) L5/L7（界面、捕获；L5 需要 L1/L4 的接口已合）
3) L8（sidecar，需要主仓 engine-jar 扩展；同一时间只允许一个 gradle 车道）
4) L9（MCP，需要 L1/L3/L4 的函数）
5) L10（发行，最后）
每次合并：git merge --no-ff feat/<lane>；立刻跑四道门禁（fmt/clippy/test/openidea-plugin build+check）；
红了先判断是「该车道缺陷」还是「与已合车道冲突」，修在该车道分支上再重合，不许在 master 上直接改业务码。

【注册文件的合入】
所有车道合并完，主控把 docs/registration/*.md 一次性应用到 plugin.toml/src/lib.rs/Cargo.toml/locales/
README，提交一次（chore: 注册汇总）。此提交后全量门禁 + 一次真机 link 走查。

【清理】
- 车道合入且门禁绿后：git worktree remove .worktrees/lane-<lane>；git branch -d feat/<lane>
  （删前先 git merge-base --is-ancestor feat/<lane> master，未合入禁删）。
- 保留 .worktrees/ 目录本身（gitignore）。
- 不删任何他人车道/分支；不 --stop gradle；不 git add -A。

【红线】
- 不 push、不建 tag、不发 Release（需用户单独授权）。
- 不复制 IntelliJ 仓源码进本仓；引擎只经 engine/ENGINE_VERSION + sha256。
- 新增依赖必须许可白名单（Apache/MIT/BSD；禁 GPL），并在 docs/registration 里写理由。
- 不猜：结果必须带 exact/partial/unresolved；unresolved 不出 SQL。
- 1.15 基线：禁止使用 1.16 的 frames / markdown_hover / customEditors.binary。
```

## 2 子代理提示词模板（每条车道填一次）

```
【车道】<lane 编号与名字>（feat/<lane>，工作树 .worktrees/lane-<lane>）
【仓库】F:\code\Java\Gitee\SongRenShuo\openidea-mybatis-plus-sql-visualizer\.worktrees\lane-<lane>
【先读】AGENTS.md、docs/ARCHITECTURE.md、docs/TASKS.md 中本车道的任务卡
【你的任务】<从 TASKS.md 抄该车道的任务卡：ID/交付/验收/估时>
【你拥有的目录】<见 §3 表>；只许改这些目录 + tests/<lane>_*
【禁止】
1) 改 plugin.toml / src/lib.rs / Cargo.toml / Cargo.lock / README.md / locales/*.json / CI；
   需要注册就写 docs/registration/<lane>.md（格式见 ORCHESTRATION §4）。
2) 改别的车道目录；跨目录需求写进报告。
3) 动 IntelliJ 仓（除本车道明确授权的路径）。
4) push / tag / release。
【环境】export PATH="/c/Users/USER/.cargo/bin:$PATH"；长命令后台跑；禁 sleep 轮询。
   （L7/L8 若需跑主仓 gradle：先 python C:/Users/USER/.zcode/cli/parallel_tasks.py check，
     再 register --name <lane> --sandbox mpsv-<lane>，结束 done --name <lane>；禁裸 done。）
【门禁（必须全绿，贴原始读数）】
  cargo fmt --all --check
  cargo clippy --all-targets -- -D warnings
  cargo test
  openidea-plugin build && openidea-plugin check .
【提交】每完成一个可提交节点立即提交（git commit -F 文件，禁 -m 反引号）；报告贴 git log --oneline 与 git status --short。
【报告】≤700 字：改动文件、门禁读数、提交哈希、工作树干净与否、遗留/风险、需要主控合入的注册片段（指向 docs/registration/<lane>.md）。
```

## 3 车道表

| 车道 | 分支 | 拥有目录 | 任务（TASKS.md） | 依赖 | 备注 |
| --- | --- | --- | --- | --- | --- |
| L1 | `feat/xml-engine` | `src/core/xml/**` | T1-1~T1-5 | 无 | OGNL/iBatis 与 XML 同车道，避免接口碎片 |
| L2 | `feat/dialect-params` | `src/core/dialect/**`、`src/core/params.rs`、`src/core/format.rs` | T1-6~T1-8 | 无 | 方言表以主仓 `Dialect.kt` 为基准 |
| L3 | `feat/rules-gen` | `src/core/perf/**`、`src/core/gen/**` | T1-9、T1-10 | 无 | 返回 messageKey，不返回文案 |
| L4 | `feat/log-parse` | `src/core/log/**` | T1-11 | 无 | 四族解析 + 去重/N+1 |
| L5 | `feat/ui` | `src/views/**`、`web/**`、`src/settings.rs`、`icons/**` | T2-1~T2-5 | L1、L4 | locales 由主控合入 |
| L6 | `feat/nav` | `src/indexer/**`、`src/providers/**` | T3-1~T3-5 | 无 | 导航默认关；injections 与宿主 #3641 共存 |
| L7 | `feat/capture` | `src/capture/**` + 主仓 `sub-projects/sql-visualizer-agent/**` | T4-1~T4-5 | 无 | 唯一允许改主仓的车道之一；gradle 需登记 |
| L8 | `feat/sidecar` | `src/sidecar/**`、`src/analyze/**`、`src/execute/**` + 主仓 `sub-projects/engine-jar/**` | T5-1~T5-7 | L1、L2 | 唯一允许改主仓的另一条；与 L7 错开 gradle 时间 |
| L9 | `feat/mcp` | `src/mcp/**` | T6-1~T6-3 | L1、L3、L4 | 工具 schema 注册走 registration |
| L10 | `feat/release` | `scripts/**`、`market/**`、`.github/**`（经主控） | T7-1~T7-4 | 全部 | 由主控自己或最后一条车道做 |

并发建议：基础层 6 条可同时开（用户已授权无并发限制），但**同时进行的重编译车道建议 ≤6**，避免内存打满；L7/L8 的 gradle 串行。

## 4 注册申请格式（`docs/registration/<lane>.md`）

```md
# <lane> 注册申请

## plugin.toml
[[contributes.commands]]
id = "<id>"
title = "<中文标题>"
category = "MyBatis SQL Visualizer"

## capabilities（新增时）
required = ["project.read"]        # 已有
optional = ["process:java"]        # 新增：为什么需要（一句）

## src/lib.rs
register_plugin!(Main, commands::<mod>, ...);

## Cargo.toml 依赖
name = "version"    # 许可：Apache-2.0；理由：一句

## locales 键
"<key>" : { en: "...", zh: "..." }

## README 能力表
| `<cap>` | <risk> | <为什么需要> |
```

## 5 合并与清理 SOP

1. `git status --short` 必须干净（含工作树）；有脏改动先让该车道提交。
2. `git merge --no-ff feat/<lane>`；冲突优先按「车道新代码 + 主控已合注册」取并集，**禁止整侧 --ours/--theirs**。
3. 合并后跑四道门禁；红了在车道分支修，重合。
4. `git merge-base --is-ancestor feat/<lane> master` 为真才 `git branch -d`；worktree 先 remove 再删分支。
5. 全部合并后：应用 registration → 一次 `chore: 注册汇总` 提交 → 全量门禁 → 真机 link 走查 → 写 `docs/开发日志.md`。
6. 只有用户明确授权后才 push / tag / release。

## 6 常见坑（来自本仓历史）

| 坑 | 对策 |
| --- | --- |
| 两条车道同时改 `plugin.toml` | 注册制；主控独占 |
| `cargo` 不在 Git Bash PATH | 每条命令前 export |
| worktree 里 `dist/` 互相覆盖 | 各自 worktree 独立；合并前只保留一份 |
| `openidea-plugin pack` 打进旧 wasm | 发布前必先 build |
| 车道「顺手」改共享文件 | 报告要求 `git status --short` 只含自己目录 |
| 合并后测试红但各车道单独绿 | 多半是注册冲突或常量重复；先在 master 复跑定位，再回车道修 |
| 忘删 worktree 导致磁盘涨 | 合并即 remove；`target/` 不共享硬链 |