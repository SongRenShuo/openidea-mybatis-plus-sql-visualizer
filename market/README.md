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
export MPSV_DOWNLOAD_BASE="https://github.com/<owner>/<repo>/releases/download/2026.11.28"
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

1. `Ctrl+,` 打开 Settings（设置）› Plugins（插件）页。
2. 找到索引/仓库（marketplace index / repository）一行，填入
   `https://…/market/index.json`，保存后刷新列表。
3. 搜索插件名 `MyBatis SQL Visualizer`，或直接看 id
   `songrenshuo.mybatis-plus-sql-visualizer`。
4. 判断是否生效，别只看界面文案：Windows 上
   `%APPDATA%\ai.l0ng.openidea\plugins\market-index.json` 会被重写，`publishers`
   里应出现 `songrenshuo`。文件没变就是 URL 没被接受或没刷新。
5. 1.15 里这一项的具体标签名以 IDE 实际界面为准（本轮没有在真机点过这一步，
   ROADMAP 的 P-6 仍然是「待证伪」）。没有这个入口时，退回手动安装：把
   `.oiplugin` 放进插件目录，或先用 `sha256sum -c` 校验再安装。

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