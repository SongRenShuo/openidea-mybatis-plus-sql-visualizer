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
