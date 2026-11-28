# 登记仓 PR 草稿（l0ng-ai/openidea-plugins）

首次上架。**提 PR 前需要用户真机测试通过**——本文件是可提交审阅的最终稿，微调直接改本文件。

## 提交内容

两个文件（内容见同目录两个 .toml）：

1. `publishers/songrenshuo.toml` —— 注册发布者与公钥
2. `plugins/songrenshuo.mybatis-plus-sql-visualizer.toml` —— 插件仓库地址与版本表

## PR 标题

```
Add songrenshuo.mybatis-plus-sql-visualizer
```

标题走登记仓的英文惯例（`Add <publisher>.<plugin>`），正文用中文——两者在在册 PR 里都很常见（#367 全中文、#375 全英文）。

## PR 正文

```markdown
## 新发布者：`songrenshuo`

- `publishers/songrenshuo.toml` —— 名称 "SongRenShuo"，owner `@SongRenShuo`，一把 Ed25519 公钥，`revoked-keys = []`。

## 新插件：`songrenshuo.mybatis-plus-sql-visualizer` 0.1.0

OpenIDEA 的 MyBatis SQL 调试插件。它解决的是「这条 SQL 最后到底发出去什么」这个每天都要猜的问题。

### 做什么

**写代码时不启动应用**：把 mapper XML、注解 SQL、iBatis 2.x `<sqlMap>`、MyBatis-Plus / MPJ 的 Wrapper 链（联表、联表更新与删除、子查询）还原成数据库真正会收到的那条 SQL，参数自动回填。

**运行后看真实执行**：Java agent 增强捕获 Application / Spring Boot / JAR 三类运行配置的真实参数与耗时；或者把 MyBatis / iBatis / Hibernate / JdbcTemplate 四族控制台日志粘进面板还原。同结构聚类、去重、慢查询高亮、N+1 疑似检测，双击跳回源码。

**每条结果先过检查**：方言写法、不安全的写操作、性能规则、回滚计划。判不出来就标 `unresolved` 并给原因，绝不猜——这是插件的产品纪律，`unresolved` 不出 SQL。

工具窗五页签：SQL 日志、日志转 SQL、SQL 转 Java、历史、SQL 预览。另有 12 个只读 MCP 工具把同一引擎交给 AI 智能体。

### 源码仓库

https://github.com/SongRenShuo/openidea-mybatis-plus-sql-visualizer

Apache-2.0。发布包由该仓 CI 从源码构建（`.github/workflows/release.yml`，tag 驱动），每次推送还会跑 fmt、clippy、全量测试与 `openidea-plugin check`。

### 为什么需要这些能力

**必需**（安装时整体同意）：

- `project.read` —— 还原与检查语句要读项目里的 mapper XML、Java 源码与日志文件。只读，插件不写项目文件。
- `events` —— 用 `websocket-ready` 唤醒捕获消费而不是轮询；也用于 gutter 与导航标记。不落盘。
- `storage` —— 「SQL 历史」页把日志快照存进插件自己的键值存储（按项目隔离，每项目最多 10 个、单文件 512 KB）。

**可选**（每一项都有可用的降级路径，被拒时相关功能会在界面上说明）：

- `java.agent`（高）—— 把插件的捕获 agent 挂到 Application / Spring Boot / JAR 运行配置上，经回环套接字送回真实参数与耗时。被拒：增强捕获不可用，界面明说并指向下面的粘贴路径。agent jar 随包分发，宿主按哈希钉住。
- `net:127.0.0.1`（中）—— 同一条捕获通道，作为 WebSocket 客户端连 `127.0.0.1:17890`。数据不出本机；清单只声明回环主机，没有其它网络能力。
- `process:java`（高）—— 启动随包分发、钉了版本与 sha256 的本地引擎 jar，用于 Wrapper 渲染与 EXPLAIN 语句包装。被拒：降级为纯文本模式，界面明示。
- `clipboard` / `clipboard.read`（中）—— 把渲染出的 SQL 复制出去；从剪贴板读取粘贴的日志。被拒：两个按钮会解释原因，面板自己的输入框照常可用。
- `editor.read`（低）—— `preview-sql` 不带参数时读当前编辑器的选区或全文。被拒：仍可用路径 + 语句 id 与右键入口。

**粘贴还原**（把四族控制台日志粘进面板）完全不需要任何能力，所以即使所有可选能力都被拒绝，插件依然可用。

### 如何验证过

- `scripts/release-smoke.sh`：fmt 检查、全量 `cargo test`（2,273 个测试）、`openidea-plugin build`、签名 `pack`、对签名包跑 `openidea-plugin check`、SHA-256。
- 发布包重新下载复验：SHA-256 与附带的 `.sha256` 一致、`check` 通过、`SIGNATURE.json` 的发布者与公钥与本 PR 注册的一致。
- 安装态真机走查：五页签各开一遍；静态渲染（mapper XML / 注解 SQL / Wrapper 链 / MPJ 联表）各验一条；捕获被拒与捕获获准两条路径；粘贴还原；设置各子页（含 Global / This project 切换）；MCP 工具调用一次。

### 版本

`engines.openidea = "^1.18"`。一个版本 0.1.0，首次发布。
```

## 提交步骤（真机测试通过后）

```bash
gh repo fork l0ng-ai/openidea-plugins --clone=false
gh repo clone <your-fork> /tmp/openidea-plugins
cd /tmp/openidea-plugins
git checkout -b add-songrenshuo-mybatis-plus-sql-visualizer
cp <本仓>/docs/registry/publishers.songrenshuo.toml publishers/songrenshuo.toml
cp <本仓>/docs/registry/plugins.songrenshuo.mybatis-plus-sql-visualizer.toml \
   plugins/songrenshuo.mybatis-plus-sql-visualizer.toml
git add publishers/songrenshuo.toml plugins/songrenshuo.mybatis-plus-sql-visualizer.toml
git commit -F <提交信息文件>
git push -u origin HEAD
gh pr create --repo l0ng-ai/openidea-plugins --title "Add songrenshuo.mybatis-plus-sql-visualizer" --body-file <PR 正文文件>
```

## 审核方会查的点（policy.mdx）

- 源码仓公开、包由该源码构建 ✓（CI tag 驱动发版）
- README 说明插件做什么 + 为什么需要每项高风险能力 ✓（本仓 README 能力表）
- 无混淆/压缩代码 ✓（Rust → wasm 由 CI 构建，源码在仓）
- 登记仓 CI 会下载包、校验 SHA-256、跑 `check`、验签名
- 新发布者与新插件由维护者人工读源码
