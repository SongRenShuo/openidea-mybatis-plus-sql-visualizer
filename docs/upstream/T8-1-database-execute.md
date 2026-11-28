# T8-1 上游提案：`database.execute` / `database.explain`（只读 + 显式确认）

状态：Draft
目标版本：1.15 之后的下一个 minor（接口草案按 `@1.16.0` 写，实际版本由上游排期决定）
提出方：OpenIDEA 端口（openidea-mybatis-sql-visualizer），非上游仓库提交

> 证据基线：上游 checkout `30e72ffa5`（tag `v0.1.51`，SDK `openidea-plugin` 0.1.51）。
> 下文 `wit/…`、`crates/…`、`plugins/README.md` 均相对该 checkout；行号在该提交上核对，
> 升级 SDK 后需复核。本仓路径（`src/…`、`docs/…`）相对 openidea-mybatis-sql-visualizer。
> 本机 checkout 位置见本仓 `docs/SDK-REF.md:7`。

## 问题

插件能把 MyBatis 语句渲染成 SQL，却无法验证它在真实库上的执行计划，也无法预览只读结果。
1.15 的 `database` 只允许插件管理自己的数据源、写凭据、请求 IDE 建连接，没有任何执行或
EXPLAIN 函数（`wit/database.wit:5-33`）。于是插件只能自带 Java/JDBC sidecar：重复实现驱动管理、
凭据读取、方言识别与取消，还拿不到 IDE 已建立的连接和它的只读会话设置。

这是插件 API 的暴露缺口，不是 IDE 能力缺口。宿主内部早就有执行与 EXPLAIN：`DbManager::execute_from`
（`crates/ide-db/src/lib.rs:1555`）、`DbManager::explain`（`crates/ide-db/src/lib.rs:1598`），
渲染进程侧的 `db_execute` / `db_execute_stream` / `db_explain`（`crates/ide-host/src/ops/db.rs:377`、
`:401`、`:320`）也在用它们；只是没有任何一条路径走到 WIT。

## 现状证据

| 事实 | 出处 |
| --- | --- |
| `database` 接口只有 5 个函数：`upsert-source` / `remove-source` / `sources` / `set-credential` / `connect` | `wit/database.wit:5`、`:24`、`:26`、`:28`、`:30`、`:33` |
| 数据源驱动只有 6 个：sqlite / postgres / mysql / sqlServer / clickhouse / oracle | `wit/database.wit:9` |
| 凭据只写不读，宿主也不提供读取函数 | `wit/database.wit:29-30`、`plugins/README.md:1154` |
| `database.sources` 是 medium 风险，只覆盖插件自有数据源、写凭据、请求连接 | `plugins/README.md:350` |
| 所有 WIT 里唯一的 `execute` 是 command-runner 的 IDE 动作执行，不是 SQL | `wit/host.wit:200-206` |
| 插件注册的 `connect` 只触发 IDE 自己的连接/内省，不返回句柄 | `wit/database.wit:31-33`、`plugins/README.md:1185-1190` |
| 宿主内部 `execute_from` 支持 `skip` / `max_rows` / 取消 / schema | `crates/ide-db/src/lib.rs:1555-1562` |
| 内部结果已是结构化行集（列名、类型名、行、affected、has_more、耗时） | `crates/ide-db/src/conn.rs:39-55` |
| 内部 EXPLAIN 已有 7 个方言的包装；自定义 JDBC 直接拒绝 | `crates/ide-db/src/plan.rs:33-45`、`crates/ide-db/src/lib.rs:1606-1608` |
| 执行取消会尽量杀服务端语句（PostgreSQL `pg_cancel_backend`、MySQL `KILL QUERY`）并丢弃连接 | `crates/ide-db/src/conn.rs:1369-1384`、`:1034-1037` |
| 数据源已有 `read_only` 语义，但只存在于宿主内部 `DataSource`，WIT 的 `data-source` 记录没有这个字段 | `crates/ide-db/src/source.rs:318`、`wit/database.wit:12-22` |
| 只读会话已按驱动设置：PostgreSQL `default_transaction_read_only=on`，MySQL `SET SESSION TRANSACTION READ ONLY` | `crates/ide-db/src/conn.rs:665`、`:674` |
| 现有 `is_write` 判定只排除 SELECT/WITH/SHOW/EXPLAIN/DESCRIBE/DESC/SET，作为只读门不够 | `crates/ide-db/src/oracle.rs:484-486` |
| 命令预算 5 s，task 打开后可 60 s；插件内存 256 MiB | `plugins/README.md:2649-2653`、`:815` |
| 当前变通是 `process:java` + JDBC sidecar（高风险、需用户授权） | `plugins/README.md:363` |

本仓自己的 SQL 分类口径是「按首 token」：`SELECT`/`WITH` 归 Select，`INSERT`/`UPDATE`/`DELETE` 各自成类，
`CREATE`/`ALTER`/`RENAME`/`DROP`/`TRUNCATE` 归 DDL，其余归 Other（`src/core/log/text.rs:49-58`、
`src/core/log/model.rs:45-63`）。提案沿用它作为第一道门，再补上下文检查。

## 提议

### 设计原则

1. 只读：`execute` 只接受单条 `SELECT` / `WITH`；`EXPLAIN` 走独立函数，永不由调用方传入现成的
   `EXPLAIN` 文本（否则 `EXPLAIN ANALYZE` 会真执行）。
2. 每次确认：确认对话框在宿主函数内部，插件拿不到「已确认」开关，无法绕过。
3. 硬上限：行数、单元格、总字节、超时都在宿主侧截断/中止；WIT 只传有界数据。
4. 凭据不外泄：结果与错误文本都不含密码/连接串；错误在宿主侧做脱敏。
5. 单条语句：多语句直接拒绝，避免 `SELECT 1; DROP TABLE t` 绕过首 token 检查。

### WIT 草案

新增独立接口而不是往 `database` 里塞函数：把只读执行这组表面集中在一处，便于整体评审与用一个
能力门把关；老的 1.15 组件不导入它就不受影响（接口增长规则见 `plugins/README.md:3040-3043`）。
若上游坚持沿用现有命名，把 `execute` / `explain` 两个函数并入 `interface database` 也可行，
能力名 `database.execute` 不变。

```wit
package openidea:plugin@1.16.0;

interface database-query {
    use workspace.{project};

    /// One result column; type-name is the driver's reported type, display text only.
    record result-column {
        name: string,
        type-name: string,
    }

    /// One bounded, materialized result page.
    /// Cell text: none is SQL NULL; binary values are `0x…` hex.
    record query-result {
        statement: string,
        columns: list<result-column>,
        rows: list<list<option<string>>>,
        /// Rows changed/affected when the dialect reports it (SELECT normally none).
        affected: option<f64>,
        /// More rows exist beyond max-rows.
        has-more: bool,
        /// Rows skipped before this page (echo of the request).
        skip: u32,
        /// UTF-8 bytes of all cell text in rows, for the caller's budget accounting.
        returned-bytes: u64,
        /// A cell was cut to its cap, or the page stopped at the byte cap.
        truncated: bool,
        elapsed-ms: f64,
        execution-ms: f64,
    }

    record plan-property {
        name: string,
        value: string,
    }

    /// One plan node; parent indexes the returned list (roots: none).
    record plan-node {
        parent: option<u32>,
        operation: string,
        params: string,
        properties: list<plan-property>,
    }

    record execute-options {
        skip: u32,
        /// 0 asks for the host default; the host clamps to its hard cap.
        max-rows: u32,
        /// 0 asks for the host default; the host clamps to its hard cap.
        timeout-ms: u32,
        /// Active schema/database for unqualified names; none uses the source default.
        schema: option<string>,
    }

    /// Why a query call failed. A new type (not `types.error`) because
    /// `types.error` is frozen and has no timeout / too-large / rejected cases.
    variant query-error {
        /// Capability not granted, or the project is not trusted.
        denied(string),
        /// Unknown, unowned or removed source id.
        not-found(string),
        /// Bad options, bad SQL, more than one statement, or a source that
        /// exists but is not connected when implicit connect is not allowed.
        invalid(string),
        /// Classified as not read-only; the payload is one of
        /// write | ddl | other | select-into | locking | cte-write.
        rejected(string),
        /// Invocation cancelled, or the user declined the confirmation dialog.
        cancelled,
        /// The host's hard timeout fired; the connection was discarded.
        timeout,
        /// The result could not be trimmed to the byte cap (for example one row
        /// is larger than the cap, or the driver materializes the whole result).
        too-large(u64),
        /// Server/driver error, already redacted by the host.
        failed(string),
    }

    /// Runs exactly one read-only statement on a plugin-owned source.
    /// Requires `database.execute` and a trusted project.
    /// The host asks the user to confirm the statement before it runs.
    execute: func(project: borrow<project>, source: string, statement: string, options: execute-options)
        -> result<query-result, query-error>;

    /// Explains exactly one read-only statement without running it.
    /// The host builds the dialect's EXPLAIN text itself; a caller-supplied
    /// EXPLAIN or EXPLAIN ANALYZE is invalid.
    explain: func(project: borrow<project>, source: string, statement: string, schema: option<string>)
        -> result<list<plan-node>, query-error>;
}
```

错误码与现有 `types.error` 的对应关系（SDK 层可以折成 `Error`）：

| `query-error` | 触发 | 对应 `types.error` |
| --- | --- | --- |
| `denied(cap)` | 未授予 `database.execute` / 项目未信任 | `denied(cap)` |
| `not-found(id)` | 不是本插件的数据源、已删除 | `not-found(id)` |
| `invalid(msg)` | 参数越界、SQL 为空、多语句、源未连接且不允许隐式连接 | `invalid(msg)` |
| `rejected(class)` | 首 token 不是 SELECT/WITH，或上下文检查失败（select-into / locking / cte-write） | `invalid("sql.rejected:<class>")` |
| `cancelled` | 调用被取消 / 用户点了取消 | `cancelled` |
| `timeout` | 宿主硬超时 | `failed("timeout")` |
| `too-large(bytes)` | 单行或不可分页的结果超过字节上限 | `invalid("result.too-large:<bytes>")` |
| `failed(msg)` | 驱动/服务端错误（已脱敏） | `failed(msg)` |

若上游坚持单一 `error` 类型，保留 `result<_, error>`、用上表的字符串码即可；两种形态的功能语义相同。

### 只读判定（规范性）

分类在宿主侧执行，顺序如下：

1. 用方言感知的 splitter 切句；语句数不等于 1 即 `invalid`。
2. 去掉前导空白与注释，取首 token（与 `src/core/log/text.rs:49-58` 同口径，服务端实现可放在
   `crates/ide-db`）。`execute` 只放行 `SELECT` / `WITH`；`EXPLAIN` 一律走 `explain` 函数。
3. `WITH`：任一 CTE 体以 `INSERT` / `UPDATE` / `DELETE` / `MERGE` / `REPLACE` / `CALL` 开头即拒绝
   （PostgreSQL 的 data-modifying CTE 首 token 仍是 `WITH`）。
4. `SELECT`：拒绝 `SELECT … INTO`（PostgreSQL / SQL Server 会建表）与 `FOR UPDATE` / `FOR SHARE`
   等加锁子句。
5. `explain`：内层语句同样过第 2-4 步；宿主用 `plan::explain_statement`（`crates/ide-db/src/plan.rs:33-45`）
   自己拼 `EXPLAIN`，从不加 `ANALYZE`；调用方传入的首 token 是 `EXPLAIN` 直接 `invalid`。
6. 永不放行：`SET` / `SHOW` / `DESCRIBE` / `DESC` / `PRAGMA` / `COPY` / `LOAD` / `CALL` / `EXEC` /
   `ATTACH` / `DETACH` / `VACUUM` / `ANALYZE` / 任何 DDL。现有 `is_write` 把 SET/SHOW/DESCRIBE 当只读
   （`crates/ide-db/src/oracle.rs:485`），新接口不复用它做最终判定。
7. 静态分类挡不住「只读语句调用有副作用的函数」（如 `SELECT nextval(...)`、`SELECT my_writer()`）。
   第二道防线是数据库会话级只读：PostgreSQL `default_transaction_read_only=on`、MySQL
   `SET SESSION TRANSACTION READ ONLY`（已有实现，`crates/ide-db/src/conn.rs:665`、`:674`）、
   Oracle `SET TRANSACTION READ ONLY`、ClickHouse 的 strict read-only（`crates/ide-db/src/clickhouse.rs:155-174`）。
   SQLite 可用 `PRAGMA query_only=1`；SQL Server 未发现与 PostgreSQL/MySQL 等价的会话级只读设置
   （需在实现前按版本验证），只能依赖分类判定并在文档中注明缺口。

### 确认、上限与超时

| 项 | 规则 | 依据 |
| --- | --- | --- |
| 确认 | 每次 `execute` / `explain` 弹宿主确认框，显示源名、语句、分类结果、行上限；默认取消 | 与 `ui.confirm` 同语义（`wit/host.wit:179`） |
| 行数 | 默认 200，硬上限 1000；`max-rows=0` 取默认 | 与 `symbols` 的 1000 上限同量级（`crates/ide-host/src/plugins/api.rs:2430`） |
| 单元格 | 单格 64 KiB UTF-8，超出置 `truncated=true` | 与语言特性的单串上限一致（`plugins/README.md:1671-1672`） |
| 总量 | 4 MiB；超出停止取行，返回已有页并置 `has-more=true`、`truncated=true` | 与 Find in Files 的 4 MiB 结果上限一致（`wit/workspace.wit:62`） |
| 超时 | 默认 15 s，硬上限 30 s；超时杀服务端语句并丢弃连接 | 取消/杀语句（`crates/ide-db/src/conn.rs:1369-1384`）、丢弃连接（`:1034-1037`） |
| 预算 | 必须放进 command（5 s）或 task（60 s）；5 s 不够时由调用方 `tasks.begin` | `plugins/README.md:2649-2653`、`:815` |

### 宿主落点

- 源与连接：`source` 是插件私有 id（1-128 字节，`wit/database.wit:8`）；宿主按现有 `database` 函数的
  同一套所有权检查解析到连接目标（未知/非自有 id 一律 `not-found`，`plugins/README.md:1170-1173`）。
  建议沿用 `connect` 的连接/密码提示路径；是否允许隐式连接见开放问题。
- 只读事务：在从连接池取出的连接上开只读事务（或对已有只读会话直接执行），结束后回滚、归池；
  `execute_from` 已支持按调用切换 schema（`crates/ide-db/src/lib.rs:1555-1562`）。
- 执行：复用 `DbManager::execute_from`（`crates/ide-db/src/lib.rs:1555`）的 `skip/max_rows/cancel`；
  宿主在 WIT 边界再做一次字节裁剪，避免 256 MiB 内存被单次结果撑满（`plugins/README.md:2653`）。
- EXPLAIN：复用 `DbManager::explain`（`crates/ide-db/src/lib.rs:1598-1637`）。方言矩阵：
  SQLite / PostgreSQL / ClickHouse / MySQL / SQL Server / Oracle（`crates/ide-db/src/plan.rs:35-42`）；
  自定义 JDBC 不支持（`crates/ide-db/src/lib.rs:1606-1608`）。插件自有源只有 6 个驱动，Snowflake 不在其列。
  Oracle 的 `EXPLAIN PLAN` 会写 `PLAN_TABLE` 再删除（`crates/ide-db/src/lib.rs:1609-1631`），
  这属于宿主自有的计划元数据写入，不是执行用户语句；SQL Server 分支会 `SET SHOWPLAN_TEXT ON`
  （`crates/ide-db/src/plan.rs:40`），属于会话状态切换。两者都要在文档中写明「不执行用户语句」的确切含义。
- 错误脱敏：驱动异常常带连接串/用户名。宿主应在返回 `failed` 前替换掉已存的密码与 URL 中的
  `password=`/`passwd=`/token 参数；结果记录里不含任何源设置（凭据本就不可读，`wit/database.wit:29-30`）。

### 不做

- 不暴露写语句、DDL、事务提交/回滚、表编辑（`apply_edits` 仍属 IDE 自身，`crates/ide-db/src/lib.rs:1779`）。
- 不支持多语句脚本、`CALL`/存储过程、`COPY`/导入导出。
- 不返回执行句柄或可续期的游标；1.x 只有「一次调用返回一页」。
- 不新增驱动；可用范围与 `database` 的 6 个驱动一致。

## 影响面

新增一个宿主函数会同时触及下列位置（上游自己的清单，`plugins/README.md:3077-3090`）：

| 位置 | 改动 |
| --- | --- |
| `wit/database.wit`（及 SDK 的 `wit/` 副本） | 新接口 + package 版本升一个 minor |
| `crates/ide-plugin/src/wit_host.rs`、`runtime.rs` | 绑定、能力映射、`HostApi` 方法 |
| `crates/ide-host/src/plugins/api.rs` | 实现：所有权、确认、只读判定、上限、脱敏 |
| `crates/ide-db` | 只读事务辅助、结果裁剪（执行/EXPLAIN 主体已存在） |
| `crates/ide-extension/src/capability.rs` + 同意 UI | 新能力 `database.execute`（高风险） |
| SDK `src/host.rs`、`src/types.rs`、mock `imp` | 公开包装与测试桩 |
| `tests/api/released/` 快照、`tests/wit_coverage.rs` | 新版本快照与覆盖检查 |
| `plugins/README.md` 能力表/API 表 | 风险等级、为什么需要、调用示例 |

能力声明：

```toml
engines.openidea = "^1.16"

[capabilities]
optional = ["database.execute"]
```

`database.execute` 建议定为 high 风险：它能读到业务数据、能触发有副作用的数据库函数；只读判定与
会话只读是防线，但不能证明所有方言上的函数都无副作用。它仍依赖已有的 `database.sources`（建源与
写凭据），两个能力独立授权。设为 optional，拒绝时插件降级为纯分析
（本仓现状：`sqlv_explain` 只格式化调用方给的计划，`docs/LIMITS.md:43-44`）。

## 兼容与降级

- 版本门：导入新函数的组件要求宿主实现对应 minor。旧 IDE 安装/`check` 会拒绝（`plugins/README.md:3045-3049`），
  不存在「函数缺失但能跑」的形态。因此本仓在 1.15 基线上不能直接使用本提案；上游落地后需要一次
  基线升级决策。optional 只控制权限，不控制 API 可用性，两者要分开写进发行说明。
- 权限被拒：`execute` / `explain` 返回 `denied("database.execute")`；插件回落到 sidecar（`process:java`）
  或纯分析模式，并在 UI 明示「未连库，仅静态分析」。
- 连接不可用：`not-found`（源不存在）/ `invalid`（未连接且不允许隐式连接）/ `failed`（连接失败）；
  不产出计划，不把「没有结果」显示成「没有性能问题」。
- 结果被裁剪：`has-more` / `truncated` 必须原样传给上层；MCP 工具不得把裁剪后的结果标成完整结果。

## 测试设计

宿主单测（不连库，用 mock）：

1. 分类矩阵：`SELECT` / `WITH ... SELECT` 通过；`INSERT` / `UPDATE` / `DELETE` / `CREATE` / `DROP` /
   `TRUNCATE` / `MERGE` / `CALL` / `SET` / `SHOW` / `PRAGMA` / `COPY` / `EXPLAIN` 全部拒绝。
2. 上下文逃逸：`WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d` 拒绝；`SELECT * INTO t2 FROM t`
   拒绝；`SELECT * FROM t FOR UPDATE` 拒绝；`EXPLAIN ANALYZE SELECT 1` 拒绝（且确认宿主没有把它
   交给执行路径）。
3. 多语句：`SELECT 1; DROP TABLE t` 拒绝；带注释/换行/大小写混写的同一矩阵各测一遍。
4. 上限：请求 5000 行被夹到 1000；4 MiB 总量时返回 `has-more`/`truncated`；单格 64 KiB 截断。
5. 超时/取消：慢语句在硬上限内返回 `timeout`，连接被丢弃；调用取消返回 `cancelled`。
6. 确认：拒绝确认时不触达连接池（用计数器断言 0 次）；接受后执行一次。
7. 只读纵深：在只读会话上执行 `SELECT nextval(...)` 应失败（证明会话级只读生效），而不是靠分类器。
8. 所有权/脱敏：别的插件 id → `not-found`；错误文本不含密码与完整 URL。

真库矩阵（本地 Docker 或测试服务器）：SQLite、PostgreSQL、MySQL、SQL Server、ClickHouse、Oracle。
每库至少两条：一条 `SELECT` 断言列名/类型/行数；一条 `EXPLAIN` 断言节点数 ≥1、根节点 `parent=none`。
Oracle 额外断言 `PLAN_TABLE` 清理后无残留；SQL Server 额外断言 `SHOWPLAN_TEXT` 状态被复位。
拒绝路径每库各跑一次写语句，确认在连库前就被拒绝（错误码 `rejected`）。

## 开放问题

1. 错误类型：独立 `query-error`，还是维持 `types.error` + 字符串码？前者可读，后者与现有约定一致。
2. 源未连接时是否允许隐式连接（含密码提示）？允许更顺手，但会让「执行」隐含一次 UI 交互。
3. 插件的执行是否进入用户的 Query History？内部 `execute_from` 会记录（`crates/ide-db/src/lib.rs:1569-1572`）；
   若记录，建议标注来源为插件，且提供关闭开关。
4. SQL Server 未发现等价的会话级只读设置，是否需要额外策略（拒绝所有非 SELECT/WITH 后仍依赖分类器）？
5. 1.x 是同步调用、整体传值；未来 2.0 的流式结果是否应该另设接口，而不是给本接口加游标？
6. 是否需要在 `database` 上新增 `set-read-only(project, id, bool)`，把插件自有源持久设为只读？
   这需要新函数（不能给已发布的 `data-source` 记录加字段），可以后置。