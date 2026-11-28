# 上架官方注册表

这份文件是**材料准备与自查**，不是已验证的对接流程。原因见第 1 节：官方没有公开
的第三方提交入口，本轮也没有提交任何 PR。

可直接提交的材料在 [`official-pr/`](official-pr/README.md)：PR 描述（`pr-body.md`）、
注册表条目骨架（`registry-entry.json`）与提交步骤。**提交与审核是上游动作**，需要
已发布的签名包、可用的下载 URL，以及用户对目标仓库的 fork/push 授权。

## 1 现状：入口不明（本轮实测）

- `openidea-plugin --help`（0.1.51）共 11 个子命令：
  `new / build / check / link / unlink / links / keygen / endorse / pack / index / merge`。
  **没有** `submit` 或 `publish`。
- 官方仓 `github.com/l0ng-ai/openidea` 的 README 只写了两件事：「插件从
  Settings › Plugins 安装」「新插件用 `openidea-plugin new`」。
- 同仓 `plugins/official/` 目录里只有官方插件本身，没有 CONTRIBUTING 或提交说明。

所以第 3 节是**去问入口时需要带什么**，第 4 节是**按本仓纪律预期的拒绝项**。等对方
给出入口后，这一节要按实际流程改写，别把预期当承诺。

## 2 前置条件（可自查，缺一条就别提）

| 项 | 要求 | 本仓现状 |
| --- | --- | --- |
| 开源 | 源码公开、构建可复现 | Apache-2.0（`LICENSE`），CI 固定 CLI 0.1.51 + Rust 1.98.1 |
| 许可白名单 | Apache-2.0 / MIT / BSD 系；禁 GPL、AGPL | 直接依赖只有 `serde`、`serde_json` 与 SDK；`NOTICE` 已记 JSqlParser（取 Apache-2.0 分支）与 ASM（BSD-3-Clause） |
| 插件 id | `<publisher>.<name>`，前缀与 publisher 完全一致 | `songrenshuo.mybatis-sql-visualizer`（2026-11-29 起；旧名 `…mybatis-plus-sql-visualizer`，已发布的 2026.11.29 包包内仍是旧名） |
| 版本一致 | tag = `plugin.toml` 版本 | `release.yml` 里有一条硬校验 |
| 签名 | 公钥已登记在该索引的 publisher 下；包 signed | `scripts/release.sh` 缺密钥直接失败 |
| digest 一致 | 索引条目的 `digest` 由 `build-index.sh` 从同一份包生成（它是内容摘要，不是文件 sha256） | 见 `docs/registration/L10.md` 的一致性步骤 |
| 能力诚实 | `plugin.toml` 声明的能力 = README 能力表 = 真实行为 | `project.read` + `events`（required），`clipboard` / `process:java` / `net:127.0.0.1`（optional，均有降级），`java.agent` 由 manifest 派生 |
| 描述 | 不加未实现的功能承诺 | `plugin.toml` 的 description 只写已能做与将要做的事 |
| 主页 | 可达的 HTTPS | 指向 Gitee 仓，提交前确认能打开 |
| API 线 | `^1.15`，不含 1.16 特性 | `engines.openidea = "^1.15"`；`frames` / `markdown_hover` / `customEditors.binary` 未使用 |

提交前跑一遍能跑的部分：

```sh
openidea-plugin check dist/*.oiplugin        # 按安装口径校验，必须 ok
awk -F'"' '/^version/{print "version:", $2; exit}' plugin.toml
sha256sum dist/*.oiplugin                    # 与两处镜像的 .sha256 比对（索引 digest 不是文件 sha256）
```

## 3 提交流程（入口待确认）

1. **先问入口，别猜**。带上 publisher id、插件 id、公钥、包下载 URL 与 sha256，
   问清三件事：提交到哪个仓库（索引仓还是插件仓）、条目由谁写入、密钥如何登记。
2. **材料包**（对方要什么给什么，一次给全省一轮往返）：

   - 索引条目：`openidea-plugin index` 产出的 `plugins[]` 中属于本插件的那一条
     （字段：`id / name / version / description / authors / publisher / download /
     digest / capabilities / engine / categories / homepage`）
   - 发布者公钥（base64 单行）与 publisher id
   - 包的 sha256 与下载 URL（GitHub、Gitee 各一份，字节相同）
   - 复现构建：`openidea-plugin build . && MPSV_KEY_FILE=… scripts/release.sh`
   - `LICENSE`、`NOTICE`、`CHANGELOG.md` 的链接
3. **真提交**：只在用户明确授权后做，且 PR 只动自己的条目与附件，不改别人
   的 publisher、不动别人的包。

## 4 审核常见拒绝项（预期清单）

按本仓「不猜」与「能力最小化」的纪律反推，实际审核口径以对方说明为准：

- **能力超量**：请求了 `process:java`、`java.agent` 或 `fs.write` 却没有降级路径，
  或 README 能力表与 `plugin.toml` 不一致。
- **许可问题**：出现 GPL/AGPL 依赖、缺 `LICENSE`、`NOTICE` 漏第三方归属
  （引擎 jar 的 JSqlParser、agent jar 里 relocate 的 ASM 属于必须记的）。
- **描述失实**：写了尚未实现的能力（例如现在就宣称「支持全部 15 种方言」），
  或把 `partial` 结果说成精确结果。
- **签名链断裂**：包未签名、公钥未登记、密钥与 publisher 不匹配；轮换时没带
  `--previous-key` 背书。
- **内容与索引不符**：索引 `digest` 与包内容对不上（同版本改了代码却没升版本）；
  或发版后原地替换了包却没升版本。
- **API 线不符**：用了 1.16 特性，1.15 线 IDE 一装就报「需要更高版本」。
- **id/主页**：id 前缀与 publisher 不一致（等同冒充），主页 404，下载 URL 非 HTTPS。

## 5 上架之后

- 每次发版都要同步索引条目（`scripts/build-index.sh` 同一套产出，两边指向同一份字节）。
- 撤回只能发更高版本（0.1.51 没有 `revoke` 子命令）；已装用户不会自动降级。
- 自建索引与官方注册表**可以并存**：自建给国内网络，官方给公开生态；两边条目必须
  指向相同 sha256 的包，否则同一版本在不同来源装出来的东西不一样。

## 6 本轮未做的事

- 未提交 PR、未联系官方、未验证提交入口与审核时长。
- 未做第三方依赖许可的自动化扫描（未引入新工具）；`NOTICE` 目前靠人工核对
  `Cargo.lock` 与引擎/agent jar 的来源。