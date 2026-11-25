---
name: openidea-ide-acceptance
description: OpenIDEA 插件（plugin.toml + Rust/wasm 或纯声明）的真机验收作业法：构建产物与重启的生效判定、面板（包页 vs 代码绘制）走查、命令/菜单/面板/设置/MCP 分组验收、读数与缺陷登记、缺陷并行修复后复验。当用户要求「验收这个 OpenIDEA 插件」「真机走一遍功能」「点一下面板看看」或排查「改了没生效/面板是空的」时使用。
---

# OpenIDEA 插件真机验收

面向在真实 OpenIDEA（Tauri 桌面应用）里逐个功能点验，产出可复核的读数与缺陷表。单元测试绿不等于功能对：
本流程的价值在于抓「两侧各自自洽、接缝处不通」的缺陷（协议改版、包页不重渲染、设置键漂移）。

## 1 先确认三件事，否则后面全是假读数

1. **产物新鲜度**：`dist/plugin.wasm` 的 mtime 必须晚于最近一次 `src/` 改动。
   ```bash
   ls -la dist/plugin.wasm && git log -1 --format=%ci
   ```
   改了代码没 `openidea-plugin build`，IDE 跑的还是旧 wasm；**清单（plugin.toml）变更是重启生效，代码变更是 build + 重启**，两条路独立，
   别用「新菜单出现了」推断代码也是新的。
2. **清单自洽**：`openidea-plugin check .` 必须 `ok`（它只校验清单引用到的文件：图标/页面/schema；没被声明的资产不读也不进包）。
3. **能力与授权**：新增 `required` 能力会让已 link 的插件**静默不加载**。备份并检查
   `%APPDATA%\ai.l0ng.openidea\plugins\grants.json`，缺批准就补，再强杀进程重启（`Stop-Process -Force`，别只关窗口）。

CLI 常不在 Git Bash 的 PATH 上，一律用绝对路径：`C:\Users\USER\.cargo\bin\openidea-plugin.exe`。

## 2 夹具先行

在测试项目里准备**能判定**的输入，每条对应一个可写进文档的预期：

| 类型 | 建议夹具 | 判据 |
| --- | --- | --- |
| XML | 一个 namespace + `<if>`（含方法调用）+ `<foreach>` | exact / unresolved（方法调用必须 unresolved 且不出 SQL） |
| SQL | 一个无 WHERE 的 DELETE + 一条带前导通配符的 SELECT | `unbounded-write: yes`、`leading_wildcard_like` |
| 日志 | 每组 `Preparing/Parameters/Total`（带毫秒时间戳）+ 一条 `Updates` | 耗时为时间戳差、`Total` 记结果行数、`Updates` 记影响行数 |
| JSON | 表结构（含主键）与回滚输入 | CRUD 五段、回滚 snapshot+reverse |
| 运行 | Application / Spring Boot / JAR 运行配置 | 捕获链（agent 通道）才可能验证 |

## 3 走查顺序

1. **命令/菜单**：先看右键菜单条数与分组（`projectView.context` 会把选中文件**绝对路径**当参数传进命令），
   再逐个跑，把 Output 原文里的关键字段抄进文档（`source:`、`class:`、`confidence`、`entries:`）。
2. **面板**：先空态，再灌数据，再交互（筛选/搜索/暂停/清空/复制/页签）。
   - 判「复制」必须**读回剪贴板**（`Get-Clipboard`），不看按钮有没有反应。
   - 判「筛选」必须让**行数变化**（`共 2 条`），只看输入框里有没有字是假验。
3. **设置与依赖设置的功能**（导航/注入/inlay/诊断）：设置页若无法自动化操作，直接标「阻塞（原因）」，
   **不要**因为「命令都跑了」写成通过。
4. **MCP**：`tools/list` 数一遍（自带 + 插件工具），再抽 1–2 个端到端调用；确认 `readOnly` 工具不写库。
5. **安装态/降级**：无引擎 jar、未授权能力、无运行配置时，必须给出可读原因而不是崩或空转。

## 4 桌面自动化（Computer Use）要点

- 元素索引（AX）优先；web 内容里的输入框经常「接受点击但不聚焦」，**输入后必须回读**（`textfield = orders` 才算成功），
  不行就用**最新截图**上的坐标点击 + `typeText`，再回读。
- 坐标只能取自当前帧截图；虚拟化列表的 AX 坐标可能是 `y=9384` 这种，不能当屏幕坐标用。
- 独立子窗口（Settings）常是独立 Tauri 表面：坐标输入会被拒（`independent_surface_requires_association`），
  只能靠元素索引；AX 往往不暴露左侧导航——这正是设置页验收常年阻塞的原因。
- 切窗口后重新 `list_windows` 取 `window_id` 再绑定；判「按了没反应」先怀疑绑错窗口。

## 5 读数与缺陷登记

- 每条验收写成「操作 + **实际读数** + 判定」，例如
  `筛选：输入 orders → 共 2 条（只剩两条含 orders 的语句）→ 通过`。
- 缺陷表字段固定：`编号 | 现象（真机读数） | 定位（文件:行/根因） | 状态（已修+提交/待修/阻塞）`。
- **不把未验写成已验**；阻塞项写清阻塞原因与解阻条件。

## 6 缺陷并行修复 → 复验

- 一个缺陷一个车道（worktree + 分支），派活时写死「每完成一件可提交的就提交」「只跑本模块测试」。
- 复验只认**真机读数变化**（行数、耗时、剪贴板、页签内容），不接受「单测绿所以好了」。
- 修完跑一次合并门禁：`cargo fmt --all --check` + `cargo test` + `build` + `check`；构建并发 ≤2，长命令后台跑，禁 sleep 轮询。

## 7 这个平台的专属陷阱

| 陷阱 | 事实 |
| --- | --- |
| 包页（`web =`）没有宿主工具栏 | 页面自带顶栏，`View.toolbar` 不参与渲染；按钮要画进页面 |
| `host::refresh_view` 对包页不重渲染 | 页面只认 `post_view_message` 推的 `state`；命令改了状态要显式推 |
| 代码绘制的 `Body::Html` 与包页是两条通道 | 0.1.51 实测 `Html` body 的「页面 → 宿主」消息不到达；要交互就用包页 |
| 协议改版漏改生产者 | 页面只认 `state` 而命令仍发 `load`：命令输出正常、页面空态。改协议先 `grep` 全部生产者 |
| 视图位置 | 原版式侧边栏是 `location = "right"`；底部能开不代表右侧也在，图标/位置要单独复验 |
| 菜单 `when` | 没有按扩展名的现成上下文键；要过滤得自己 `host::set_context` |
| 设置键漂移 | 清单 schema 的键和代码里 `host::setting_string` 的键必须同名；写探针（改一个值看命令输出里的 `dialect:`）才能证伪 |
| MCP stdio 钉定 | `openidea --mcp-stdio --project <path>` 是**连接级**钉定；去掉 `--project` 就按客户端 cwd 解析，cwd 必须在受信任位置/后台服务的项目里 |
| 桌面输入 | 中文路径/命令行会乱码；PowerShell 5.1 读无 BOM UTF-8 脚本会炸，含中文一律 `pwsh` |