# 插件市场登记（l0ng-ai/openidea-plugins）草稿

上架 OpenIDEA 插件市场的登记文件草稿。**本目录文件不自动提交**——首次上架需真机测试通过后，由仓库所有者确认再提 PR。

## 文件

| 文件 | 提交到登记仓的路径 |
| --- | --- |
| `publishers.songrenshuo.toml` | `publishers/songrenshuo.toml`（首次，注册发布者与公钥） |
| `plugins.songrenshuo.mybatis-sql-visualizer.toml` | `plugins/songrenshuo.mybatis-sql-visualizer.toml` |

> 2026-11-29 插件 id 改名（`mybatis-plus-` → `mybatis-`）：文件名与 id 同步改。登记仓
> 以文件名作插件键，且 CI 会比对「包内 id = 登记 id」，所以条目里的 `[[versions]]`
> 先注释着，等第一份按新 id 构建的包发出来再回填。

## 上架流程（官方 docs/zh/plugins/publishing/marketplace.mdx）

1. **发版**：`bash scripts/release-smoke.sh` 本地自验（fmt/测试/build/check，**不产出包**）→ 提交并推 master → 打 tag `<version>`（即版本号原文，不带 `v` 前缀）→ `release.yml` 是唯一构建者，自动 build + 签名 + 双端上传 → 把**那一轮 CI 打印的** SHA-256 回填进 `plugins.*.toml` 的 `[[versions]]`。
2. **提 PR**：fork `l0ng-ai/openidea-plugins` → 新分支 → 按上表拷两个文件 → PR（首次含发布者注册，维护者做源码审核：看源码仓、能力及理由、重复性）。
3. **生效**：登记仓 CI 校验 SHA-256 + `openidea-plugin check` + 签名 → 发布它自己的 `index.json` → 平台的 `plugins-index.yml` 一小时内并入官方 release 索引 → IDE 每天读取一次（齿轮菜单「重新扫描插件」立即读）。
4. **后续版本**：发新版 = 再开一个 PR 加一个 `[[versions]]` 表；索引对每种 API 要求只保留最新一个版本。
5. **持续义务**：新 OpenIDEA release 下 `check` 失败会在登记仓开 issue，30 天不修复即下架——我们仓的 CI（CLI_TAG 随平台 release 升级）就是这个义务的盯守。

## 换密钥

私钥泄露或轮换时：`openidea-plugin endorse "<新公钥>" --key 旧密钥 --publisher songrenshuo` 打印背书 JSON，随 PR 更新 `publishers/songrenshuo.toml` 的 `keys`（旧 key 保留在被吊销前仍有效；被盗则加 `revoked-keys`）。

## 提 PR 的具体步骤

**第一步已经完成**：`2026.11.30-beta.1` 是按新 id 构建的包，双端字节一致
（sha256 `69bde5fb5f4b5abf5df054d2461636753ab472dd50f38f6f9b62f5f4b935721d`），
包内 `id = "songrenshuo.mybatis-sql-visualizer"`。

**仍不能提**，卡在第二件事：这一版带 `-beta` 后缀，而按
`docs/规范/版本号规范-说明文档.md`「上架市场的版本必须去掉后缀」。登记要一个
正式版本对一条 `[[versions]]`，所以等正式版 `2026.11.30` 发出来再走回填。

为什么不能拿旧包凑：宿主与登记仓都要求「包内 id = 登记 id」：

- 宿主：`crates/ide-host/src/plugins/market.rs` 的 `verify_market_package`，把
  `manifest.id` / `manifest.version` / `digest` 三者与索引条目一次比较，不等即报
  `the download does not match the marketplace's entry`；
- 签名也绑 id：`crates/ide-extension/src/package.rs` 的 `signed_message` 首段就是 id，
  所以旧包不能靠改索引糊弄过去；
- 登记仓 CI 下载包、跑 `check`、验签，同样过不去。

因此本目录两个 `.toml` 的 `[[versions]]` 仍是**注释状态**，`market/official-pr/registry-entry.json`
的 `download` / `digest` 仍是 `null`。这是刻意的：填 beta 会被上游判「市场版本不应带后缀」、
填旧包会被判 id 不符，两种都让 CI 一提交就红。

### 解锁顺序（缺一不可）

1. ~~**发一版按新 id 构建的包**~~ ✅ 2026-11-30 完成：`2026.11.30-beta.1`。
   正式版 `2026.11.30` 仍按同一套流程发（改 `plugin.toml` + `Cargo.toml` 的 version →
   提交推 master → `git tag <版本>` → 推 tag → `release.yml` 是唯一构建者）。确认包内
   `plugin.toml` 的 `id` 已是 `songrenshuo.mybatis-sql-visualizer`：

   ```sh
   unzip -p songrenshuo.mybatis-sql-visualizer-<版本>.oiplugin plugin.toml | head -1
   # 应打印: id = "songrenshuo.mybatis-sql-visualizer"
   ```

2. **三方 sha256 校验**（CI 自报 = GitHub 资产 = Gitee 资产），读数见 SKILL §6。
3. **回填三处**（哈希必须一致，否则登记仓 CI 直接拒收）：
   - `docs/registry/plugins.songrenshuo.mybatis-sql-visualizer.toml` 的 `[[versions]]`
     取消注释并填新包；
   - `market/official-pr/registry-entry.json` 的 `download` 与 `digest`；
   - `market/official-pr/upstream/plugins/songrenshuo.mybatis-sql-visualizer.toml`。
4. **本地跑一遍门禁**：`bash scripts/check-version-format.sh` 退出 0。
5. **提 PR**（体例取自上游 `l0ng-ai/openidea-plugins` 的 README：一个 publisher
   文件 + 一个插件文件，纯文本 TOML，无 CLA）：

   ```sh
   gh repo fork l0ng-ai/openidea-plugins --clone=false
   gh repo clone <your-fork> /tmp/openidea-plugins
   cd /tmp/openidea-plugins
   git checkout -b add-songrenshuo-mybatis-sql-visualizer
   cp <本仓>/docs/registry/publishers.songrenshuo.toml publishers/songrenshuo.toml
   cp <本仓>/docs/registry/plugins.songrenshuo.mybatis-sql-visualizer.toml \
      plugins/songrenshuo.mybatis-sql-visualizer.toml
   git add publishers/songrenshuo.toml plugins/songrenshuo.mybatis-sql-visualizer.toml
   git commit -F <提交信息文件>
   git push -u origin HEAD
   gh pr create --repo l0ng-ai/openidea-plugins \
     --title "Add songrenshuo.mybatis-sql-visualizer" \
     --body-file <本仓>/market/official-pr/pr-body.md
   ```

   标题走上游惯例 `Add <publisher>.<plugin>`；正文中英混排在上游在册 PR 里都常见。
6. **等审核**：新发布者与新插件由维护者人工读源码（源码仓公开、开源许可、包的
   构建来源、每项高风险能力的理由）。可能需要几轮修改。
7. **合流后回读**：登记仓 CI 发布它自己的 `index.json`，一小时内并入官方
   `plugins-index.json`。取官方索引确认出现我们的条目：

   ```sh
   curl -fsSL https://github.com/l0ng-ai/openidea/releases/latest/download/plugins-index.json \
     | python -c "import json,sys;d=json.load(sys.stdin);print([p['id'] for p in d['plugins'] if p['publisher']=='songrenshuo'])"
   ```

   然后把 IDE 的市场地址改回官方默认（或清空），本仓的 `market-refresh.yml`
   就成了无害空转，见 `market/README.md` 的「这份索引怎么保持新鲜」。

### 需要用户确认的点

以下每条都不该由代理代决：

1. **什么时候发那一版新 id 的包**——日期制下版本号即发布当天，跨午夜要用户指定日期。
2. **是否授权 push tag / 触发 `release.yml` / 双端上传**——发布是对外动作，本仓规矩是
   必须先拿到明确授权。
3. **是否授权 fork 与提 PR**——PR 会署名到用户的 GitHub 账号，且触发维护者人工审核。
4. **`publishers.songrenshuo.toml` 的 `owners`**——现在写的是 `SongRenShuo`。这是
   「有权改这条发布者条目的 GitHub 账号」，选错等于把改条目权交给别人。
5. **市场地址是否改回官方**——改回后国内用户拉 GitHub 附件可能变慢；留着自建索引
   则失去「官方收录」的部分意义。两边各有代价，属于产品取舍。
6. **是否保留自建索引 workflow**——官方收录后它可以删，也可以留着当 Gitee 镜像入口。

