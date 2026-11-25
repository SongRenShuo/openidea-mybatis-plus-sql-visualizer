# MyBatis SQL Visualizer (OpenIDEA)

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

No other capability is requested in this release: no editor read/write, no
network, no process, no database, no file system writes. When the engine is
connected (see `docs/开发日志.md`), `process` will be requested for the bundled
jar and documented here in the same table.

## Licence

Apache-2.0. Third-party notices, including the JSqlParser dual-licence choice
(`Apache-2.0` branch, version 5.3), are in [`NOTICE`](NOTICE).

## Limits

The OpenIDEA plugin model cannot reproduce some features of the IntelliJ
edition at all, and some others only approximately.

本平台不支持项见 docs/LIMITS.md。