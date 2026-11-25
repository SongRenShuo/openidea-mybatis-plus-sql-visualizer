# 注册申请（registration）

子代理**不许**直接改下列「注册类单点文件」，需要注册时写本目录下 `<lane>.md`：

- `plugin.toml`（命令、视图、能力、provider、documentIndexer、MCP 工具、javaAgent）
- `src/lib.rs`（模块声明已由主控铺好；仅 `register_plugin!` 的可选导出由主控加）
- `Cargo.toml` / `Cargo.lock`（依赖）
- `README.md`（能力表）
- `locales/*.json`（文案键）
- `.github/workflows/**`（CI）

## 格式

见 `docs/ORCHESTRATION.md` §4。每条车道只写自己的文件，彼此不覆盖：

```md
# <lane> 注册申请

## plugin.toml
[[contributes.commands]]
id = "..."
title = "..."
category = "MyBatis SQL Visualizer"

## capabilities
optional = ["process:java"]   # 为什么需要（一句）

## src/lib.rs
register_plugin!(Main, views);   # 需要导出的可选接口

## Cargo.toml
name = "version"   # 许可：MIT；理由：一句

## locales
"key": { en: "...", zh: "..." }

## README 能力表
| `process:java` | High | ... |
```

主控在全部车道合并后一次性应用并提交 `chore: 注册汇总`。