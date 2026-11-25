# Changelog

Single source of truth for release notes; the `.oiplugin` change notes are
generated from it.

## [Unreleased]

### Added

- Six commands wired to real logic: render mapper XML, check SQL, performance
  advice, generate CRUD, rollback plan, render SQL logs.
- Log panel and SQL workbench views, 41 settings in five groups, English/Chinese
  UI, gutter markers and a mapper XML file icon.
- Mapper↔XML document index with cross-file navigation, SQL injection in mapper
  XML, INSERT inlay hints and diagnostics — navigation stays off by default.
- Pure engine: mapper XML dynamic tags, restricted OGNL, includes with cycle
  detection, annotation SQL, iBatis 2.x, 15 dialects, parameter binding,
  idempotent formatting, 21 performance rules, CRUD/Java generation and
  four-family log parsing.
- Eleven MCP tools over the same engine, each returning an explicit
  `exact` / `partial` / `unresolved` confidence.
- Java agent live capture over a loopback WebSocket, plus an exported-log file
  path when no agent is attached.
- Engine sidecar over a line-oriented JSON protocol, pinned by version and
  sha256; missing or mismatched pins degrade to text mode instead of guessing.
- Release pipeline: signed package, GitHub Release, Gitee mirror and a
  self-hosted marketplace index.