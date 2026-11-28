# Add MyBatis SQL Visualizer to the plugin marketplace

**Plugin id:** `songrenshuo.mybatis-sql-visualizer`
**Version:** 2026.11.29
**Repository:** https://gitee.com/songrenshuo/openidea-mybatis-sql-visualizer
**License:** Apache-2.0 (see `LICENSE` and `NOTICE`)

## What it does

Renders MyBatis mapper XML, annotation SQL, iBatis 2.x, MyBatis-Plus wrappers and
SQL logs into executable SQL, checks a statement before it reaches a database,
and exposes the same engine to agents through 12 read-only MCP tools. Every result
carries an explicit `exact` / `partial` / `unresolved` confidence; `unresolved`
never produces SQL.

## Package

- `songrenshuo.mybatis-sql-visualizer-<version>.oiplugin`（2026.11.29 那份包的包内 id 仍是旧名，下一版起按新 id 构建）
- sha256: `<filled by dist/*.sha256 after release>`
- Built with `openidea-plugin` 0.1.55, plugin API 1.18, Rust 1.98.1 (wasm32-wasip2).

## Capabilities, and why each is needed

| Capability | Risk | Why |
| --- | --- | --- |
| `project.read` | low | Reads mapper XML, Java sources and exported logs — the whole point. Never writes. |
| `events` | low | Wakes the capture consumer on `websocket-ready` instead of polling. |
| `clipboard` (optional) | medium | Log panel / workbench copy SQL. Refusal only disables those buttons. |
| `process:java` (optional) | high | Starts the pinned engine jar (version + sha256 checked) for wrapper rendering and EXPLAIN statement wrapping. Refusal degrades to text mode. |
| `net:127.0.0.1` (optional) | medium | Connects to the Java agent's loopback capture socket. Refusal leaves the log-file path. |
| `java.agent` (derived) | high | Declared through `[[contributes.javaAgents]]`; attaches to Application / Spring Boot / JAR run configurations. Not listed in `[capabilities]`. |

The plugin never writes project files, never connects to a database, and never
opens a network connection on its own.

## Verification

- Local: `cargo test` — lib 704 passed, `cases_parity` 150 cases (144 green +
  6 documented whitelist entries), `l11_commands` 444 passed;
  `cargo clippy --all-targets -- -D warnings` clean.
- CI: GitHub Actions gate `ci/gate-10` — fmt+clippy, test, build+check+artifact,
  engine pin check all green.
- Real IDE: OpenIDEA 0.1.51, Find Action lists 13 commands; `capture-status`
  prints to its Output tab; the SQL Logs and SQL Workbench tool windows open
  and render.
- `openidea-plugin check` on the signed package: `ok`.

## Third-party notices

JSqlParser 5.3 is used through its Apache-2.0 branch (not LGPL). ASM is
relocated inside the bundled agent jar (BSD-3-Clause). Details in `NOTICE`.