# MyBatis SQL Visualizer (OpenIDEA)

[![gate](https://github.com/SongRenShuo/openidea-mybatis-plus-sql-visualizer/actions/workflows/build.yml/badge.svg)](https://github.com/SongRenShuo/openidea-mybatis-plus-sql-visualizer/actions/workflows/build.yml)

Turns MyBatis mapper XML, annotation SQL, Wrapper chains and MyBatis/Hibernate
log output into the SQL a database would actually receive, then checks it:
dialect errors, unsafe writes, and the twenty-one performance rules.

This repository is the OpenIDEA port of the IntelliJ plugin of the same name.
The rendering engine is not duplicated here: it will be shipped as a versioned
jar from the IntelliJ repository and called from the plugin, so both editions
agree on what a statement means. `engine/ENGINE_VERSION` pins which jar; it
currently reads `0.0.0-pending` because the jar is not connected yet.

## Install

```sh
openidea-plugin build          # → dist/plugin.wasm, validated like an install
openidea-plugin pack           # → songrenshuo.mybatis-plus-sql-visualizer-<version>.oiplugin
```

Then install the `.oiplugin` file from Settings › Plugins, or load the folder
directly while developing:

```sh
openidea-plugin link .         # then Settings › Plugins › Reload
```

## Commands

| Command | What it does |
| --- | --- |
| `MyBatis SQL: Render XML Snippet` | Renders the mapper XML SQL around the caret into the Output tab. In this first build it prints a fixed statement: it is the end-to-end probe for the command → engine → Output chain. |

## Capabilities, and why each one is needed

The plugin asks for as little as it can. Every capability is shown to the user
on the install page; a plugin that asks for more than it uses is rejected in
review.

| Capability | Risk | Why this plugin needs it |
| --- | --- | --- |
| `project.read` | low | The plugin's job is to read the project's mapper XML, Java sources and SQL logs and render them. Without read access there is nothing to render. It cannot write, delete or change anything in the project. |
| `clipboard` (optional) | medium | The log panel and the workbench copy or export the rendered SQL. Refusing it only disables those two buttons; rendering, navigation and checks keep working. |
| `process:java` (optional) | high | Starts the pinned engine jar (`engine/ENGINE_VERSION`, sha256-checked) for MyBatis-Plus Wrapper rendering and EXPLAIN statement wrapping. Refusing it degrades to text mode; every other feature still works. |

The plugin never writes project files, never connects to a database, and never
opens a network connection on its own. The engine jar is started only after its
pinned sha256 matches.

## Docs

| File | What it is |
| --- | --- |
| [`docs/ROADMAP.md`](docs/ROADMAP.md) | 阶段 P0–P8、每阶段退出门禁、7 条待证伪前提（本仓排期与验收的唯一权威） |
| [`docs/TASKS.md`](docs/TASKS.md) | 阶段展开成 34 个可独立提交的任务卡（交付/验收/估时）与建议批次 |
| [`docs/ORCHESTRATION.md`](docs/ORCHESTRATION.md) | 多子代理并行实施规则：车道表、目录所有权、注册申请制、合并与清理 SOP、主控与子代理提示词 |
| [`docs/FEATURES.md`](docs/FEATURES.md) | 201 条功能的四档范围（98 现成 / 83 变通 / 1 上游 / 19 不可行）与优先顺序 |
| [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) | 平台硬约束、wasm 即时路径 + jar sidecar 双路径、引擎 JSON 契约、置信度模型 |
| [`docs/LIMITS.md`](docs/LIMITS.md) | 用户可读的「本平台不支持」清单 |

调研证据（12 份分册、逐条编号与行号）在 IntelliJ 仓：
`docs/调研报告/20261125-1336-OpenIDEA全面支持-调研报告.md`（提交 `816e8c0a0`）。
能力边界以那份报告为准，排期与验收以 `docs/ROADMAP.md` 为准。

## Licence

Apache-2.0. Third-party notices, including the JSqlParser dual-licence choice
(`Apache-2.0` branch, version 5.3), are in [`NOTICE`](NOTICE).

## Limits

The OpenIDEA plugin model cannot reproduce some features of the IntelliJ
edition at all, and some others only approximately.

本平台不支持项见 docs/LIMITS.md。