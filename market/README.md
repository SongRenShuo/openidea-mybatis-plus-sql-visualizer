# 自建插件索引

`market/` 放的是自建 marketplace 索引的**说明**，不放占位数据。真正的
`market/index.json` 由 `scripts/build-index.sh` 在正式发行时从已签名的包生成，
生成后再提交/推送（Gitee Pages 直接从仓库根目录读文件）。

为什么要有自建索引：官方注册表要开源、要审核、下载走 GitHub；国内用户拉
GitHub release 附件经常失败。自建索引 + 国内静态托管能让安装和升级都不碰
GitHub。

## 索引的内容

`index.json` 是 `format: 1` 的 JSON，字段与官方索引同源（本机 IDE 缓存的
`%APPDATA%\ai.l0ng.openidea\plugins\market-index.json` 就是同一 schema，264 条
插件，键为 `format / publishers / plugins / revoked`）：

| 字段 | 含义 |
| --- | --- |
| `publishers[].id` / `keys[]` | 发布者与其签名公钥；换了密钥要带 `endorsement` |
| `plugins[].id` / `version` | 插件标识与版本；同一 id 可以有多条表示多版本 |
| `plugins[].download` | 绝对 URL（由 `--download-base` + 包文件名拼成） |
| `plugins[].digest` | 包的内容摘要：manifest 与代码参与，README 与签名不参与（改 README、换密钥重签都不变，改 `plugin.toml` 会变——实测）；它不是文件 sha256 |
| `plugins[].engine` | 支持的插件 API 线（这里是 `^1.15`） |

两条硬约束：`download` 必须指向这次要发的那份包，`digest` 必须由 `build-index.sh`
从同一份包算出。别把 `digest` 当成文件校验和——实测本机官方索引里
`openidea.checkstyle` 的 digest 是 `7a2f798b…`，而它发布文件的 sha256 是
`3e85bede…`；同一份内容换密钥重签，digest 也不变（本地两把一次性密钥分别 pack，
digest 相同），改 `plugin.toml` 才会变。**镜像之间比一致性要用 `.sha256`（文件级
校验和），不能拿 digest 代。**

## 生成

```sh
export MPSV_KEY_FILE="$HOME/mpsv-publisher.key"
export MPSV_PUBLISHER=songrenshuo
export MPSV_DOWNLOAD_BASE="https://github.com/<owner>/<repo>/releases/download/2026.11.29"
scripts/build-index.sh
```

- 每次发版都重跑。索引列举 `dist/` 里**所有** `.oiplugin`：只要旧版本包还在，
  旧 IDE 线就能装到它。
- `MPSV_INDEX_OUT` 默认 `market/index.json`。
- 需要同时给用户官方插件时，加官方索引一起合并（`MPSV_OFFICIAL_INDEX` 可以是 URL
  或本地文件）：

```sh
MPSV_OFFICIAL_INDEX="<官方索引的 index.json 地址>" scripts/build-index.sh
```

  官方索引的确切地址没有公开文档：本轮实测
  `raw.githubusercontent.com/l0ng-ai/openidea/{master,main}/index.json` 都是 404。
  先从 IDE 的市场设置或官方渠道拿到真实地址再合并；拿不到就不合并——自建索引只列
  本仓插件也能正常安装与升级。

  脚本内部执行 `openidea-plugin merge <我的索引> <官方索引> --keep <我的 publisher>`：
  除自己以外的 publisher、插件与吊销记录全部取自官方索引，用户只填一个 URL 就能
  看到整个生态。手工等价命令：

```sh
openidea-plugin merge market/index.json https://…/index.json --keep songrenshuo --out market/index.json
```

- 换密钥用 `MPSV_PREVIOUS_KEY`（旧密钥文件）。CLI 会核对「旧密钥已在目标索引里」，
  所以必须覆盖已有的 `market/index.json`，不能写到新路径，否则报
  `--previous-key is not one of <publisher>'s valid keys in the index`。

## 托管

按可行性排序：

1. **Gitee Pages**：国内直连、免费。需要实名认证并开启 Pages 服务，服务的是仓库
   根目录，所以文件放在 `market/index.json`，URL 形如
   `https://<user>.gitee.io/<repo>/market/index.json`。Pages 构建有延迟，推完要
   点一次「更新」再验证。
2. **对象存储 / 静态站**（OSS、COS、R2、自建 nginx）：确保
   `Content-Type: application/json`；下载 URL 是绝对的，包可以放在不同域名下。
3. **仓库 raw 链接**：只适合临时验证。GitHub raw 国内不稳，Gitee raw 有重定向。

统一要求 HTTPS，且发布后不要原地替换 `index.json` 与包的字节——要变就发新版本。

## 在 IDE 里填 URL

**本仓已提交 `market/index.json`**，直接用它的 raw 地址：

```
https://gitee.com/songrenshuo/openidea-mybatis-sql-visualizer/raw/master/market/index.json
```

实测该地址直接返回 JSON（HTTP 200），不需要开 Gitee Pages。

1. Settings（设置）› Plugins（插件）页 → 齿轮菜单 → **Plugin Repository**
   （前端 `src/features/plugins/PluginsPage.tsx` 的 `editMarketUrl`，接口
   `plugin_set_market_url`）。
2. 填入上面的地址，保存后列表刷新。
3. 搜索 `MyBatis SQL Visualizer`。插件 id 已于 2026-11-29 改为
   `songrenshuo.mybatis-sql-visualizer`，但**本文件的 `index.json` 里仍是旧 id**
   （`songrenshuo.mybatis-plus-sql-visualizer`）：索引由 `scripts/build-index.sh` 从已签名的包
   生成，而线上那份 2026.11.29 包的包内 id 是旧名。宿主安装时比对「包内 id = 索引 id」
   （`crates/ide-host/src/plugins/market.rs` 的 `verify_market_package`），比不过就报
   `the download does not match the marketplace's entry`。所以索引不能先于包改：
   第一份按新 id 构建的包发出去之后，重跑 `build-index.sh` 覆盖本文件，id 自然变新。
4. 判断是否生效，别只看界面文案：Windows 上
   `%APPDATA%\ai.l0ng.openidea\plugins\market-index.json` 会被重写，`publishers`
   里应出现 `songrenshuo`。文件没变就是 URL 没被接受或没刷新。

**为什么必须有这份自建索引**：签名包安装时要求索引里有发布者公钥
（`crates/ide-extension/src/package.rs` 的 `verify`）。官方索引里没有
`songrenshuo`，直接装包会报 `unknown publisher "songrenshuo"`。本仓这份以官方
索引为基底 merge 进来，填了它既能看到官方 275 个插件，也能装本插件。

`download` 指向 Gitee 发行版而非 GitHub：国内拉 GitHub 附件经常失败，这正是
自建索引的意义所在。

## 这份索引怎么保持新鲜

`market/index.json` 是**官方索引的合并快照**，不是只列本插件的索引。IDE 只有一个
市场地址（`crates/ide-host/src/plugins/market.rs` 的 `market_url()`：用户填的值
**替换**默认值，不是追加），所以填了自建地址就等于换掉了官方那份——官方后来新增的
插件，除非我们跟着同步，否则永远看不到。

`.github/workflows/market-refresh.yml` 每天 03:17 UTC 跑一次：

1. 拉官方索引
   `https://github.com/l0ng-ai/openidea/releases/latest/download/plugins-index.json`；
2. `scripts/refresh-market.py` 调
   `openidea-plugin merge <我们的> <官方> --keep songrenshuo --out <我们的>`，
   我们的条目（Gitee 下载地址、homepage）原样保留，其余发布者与插件整体取官方最新；
3. 校验通过才写盘，内容真变了才提交，不变就不产生提交。

所以最坏情况是落后一天。**这一条不依赖签名私钥**：merge 只是重新发布索引条目，
不签名任何包。

守卫都在 `scripts/refresh-market.py` 里，不是内联在 YAML 中——内联的 Python 块
放在 `run: |` 里跑不出本地，而这些守卫正是这个作业的全部价值。脚本的正面与反面
用例在 `scripts/refresh-market.test.sh`（干净输入必须过，每一种坏输入必须红）：

| 守卫 | 挡的是 |
| --- | --- |
| 官方索引形态（`format`、插件数下限、发布者非空） | 抓取失败拿到错误页/被截断的文件，merge 后市场只剩几个插件 |
| merge 后仍有 `songrenshuo` 发布者与插件 | `--keep` 失效，市场里本插件直接消失 |
| 我们那条与合并前逐字段相同 | merge 用官方里的旧副本替换掉我们，下载地址被改回 GitHub |
| 我们那条的 `download` 含 `gitee.com` | 自建索引的意义（国内可拉）没了 |
| 官方那一半与刚拉的一致 | merge 没按约定整体替换 |
| 全部通过才写盘 | 半校验的索引落盘，下一次运行会以它为基底继续合并 |

本地跑一遍（不需要联网、不需要密钥）：

```sh
export PATH="$HOME/.cargo/bin:$PATH"
python scripts/refresh-market.py --official /tmp/plugins-index.json --check-only
bash scripts/refresh-market.test.sh
```

**提 PR 被官方收录之后**：`--keep` 仍会保留我们这条，但两边同时列同一个发布者时
CLI 会拒绝（`may not list publisher "songrenshuo"`）。脚本检测到官方索引里已出现
该发布者时，会把它从官方那一半剪掉再合并，并打印一句提示——流程继续能用，不会红。
真到了那天，把市场地址改回官方默认地址即可，这个 workflow 变成无害的空转
（`--keep` 保住的仍是我们自己的条目），可以随时删。

**每次发版都要重生成自己的部分**：`scripts/build-index.sh` 用签名私钥生成自己的
条目（这一步必须本地做，私钥不入库），把结果并入 `market/index.json` 后提交；
官方那一半由 `market-refresh.yml` 每天自动跟上，不用手工管。完整命令见
`.agents/skills/openidea-release-workflow/SKILL.md`。

## 更新语义

- 版本比较只看版本号，**不降级**：线上发了坏版本，修好后必须升版本号，不能复用。
- 同版本不同字节不会自动替换：改了包就必须升版本，否则已装用户永远拿不到。
- `engine` 是硬门槛：`^1.15` 的条目不会被 1.16 线的 IDE 取用，反之亦然——这正是
  多版本条目存在的理由。
- 撤回（0.1.51 没有 `revoke` 子命令）：把该版本包从 `dist/` 移除后重建索引，
  索引里就没有它了；已安装的副本不受影响。别删已经发布过的附件——那会变成 404。

## 失败排查

| 症状 | 大概率原因 |
| --- | --- |
| 列表里搜不到 | `index.json` 404/403；`Content-Type` 不是 json；URL 是 http；IDE 缓存没刷新（删 `plugins/market-index.json` 后重启） |
| 看到插件但下载 404 | `MPSV_DOWNLOAD_BASE` 少了版本 tag 段；附件没上传成功；Gitee 镜像漏传 |
| 安装时 digest 不符 | 同版本换了内容（manifest/代码变了）而索引没重建；或索引偷懒用了别的包 |
| 两处镜像 sha256 不同 | 一侧被重新打包；Gitee 附件同名未删除导致旧附件还在被引用 |
| 装完不更新 | 索引里没有更高 `version`；或自建索引缺签名 `keys`，更新链断掉 |
| 显示「需要更高版本的 IDE」 | 索引条目 `engine` 与当前 IDE 线不匹配 |