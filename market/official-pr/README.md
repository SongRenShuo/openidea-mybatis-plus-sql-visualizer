# T7-4 官方注册表上架（PR 暂缓：先由用户自行验收）

上游已确认：**`github.com/l0ng-ai/openidea-plugins`**。PR 需要
`publishers/<publisher>.toml` 与 `plugins/<publisher>.<name>.toml`，条目里的 `package`
必须指向**已签名、已发布**的包 URL，并附该包的 sha256；注册表 CI 会下载包、校验 hash、
跑 `openidea-plugin check` 并验签。

**2026-11-26 用户决定：先自行验收，验收通过后再提 PR。** 在此之前不建 tag、不发 Release、
不提 PR。本地已就绪：发布者密钥（仓库外）、签名包（sha256 `acdec26f…`，check 通过）、
`upstream/` 两份 TOML（公钥与 sha256 已填，仅缺 package URL）。

可直接使用的模板在 [`upstream/`](upstream/README.md)（两份 TOML 的精确形状）。

## 前置条件（缺一不可）

1. **已发布的签名包**：`songrenshuo.mybatis-plus-sql-visualizer-2026.11.29.oiplugin`（仓库根目录，非 `dist/`）
   及其 `.sha256`，由 `scripts/release.sh` 产出，并已上传到 GitHub Release / Gitee 镜像
   （见 `market/README.md` 与 `.github/workflows/release.yml`）。
2. **发布者密钥已进索引**：`scripts/build-index.sh` 用 `MPSV_KEY_FILE` 生成 `market/index.json`，
   并把该索引托管到 HTTPS（Gitee Pages 或任意静态托管）。
3. **仓库公开**：官方注册表只收开源插件；本仓为 Apache-2.0，`NOTICE` 已列 JSqlParser/ASM。
4. **许可证白名单**：依赖只有 SDK（Apache-2.0）与 serde/serde_json（MIT/Apache-2.0）；无 GPL。
5. **上游流程确认**：官方仓库地址、目录结构、PR 模板与是否要求 CLA，需以当时上游 README 为准
   （本仓 2026-11-25 的 checkout `30e72ffa5` 未在 `plugins/README.md` 找到独立 PR 章节，
   只说明了「插件放在自己的仓库，开源后列入社区市场」）。

## 提交材料

- `pr-body.md`：可直接粘贴的 PR 描述（能力表、测试证据、兼容基线）。
- `registry-entry.json`：注册表条目骨架（发布 URL 与 sha256 为占位，发版后由 `build-index.sh` 填充）。
- 本文件：步骤与判定标准。

## 操作步骤（需用户授权后执行）

```sh
# 1) 确认包与哈希：包由 CI 的 release.yml 产出，从 Release 附件下载
gh release download <version> --pattern '*.oiplugin*' --dir dist

# 2) 生成/更新自建索引（已含官方索引合并用法）
MPSV_KEY_FILE=$HOME/mpsv-publisher.key \
MPSV_PUBLISHER=<publisher-id> \
MPSV_DOWNLOAD_BASE=https://github.com/<owner>/<repo>/releases/download \
bash scripts/build-index.sh

# 3) 到上游注册表仓库 fork + 新建分支 + 放入 registry-entry.json + 贴 pr-body.md
#    （仓库地址与目录以上游 README 为准；本仓不擅自 fork/push）

# 4) 合并后回读：IDE 设置 › 插件 › 市场，搜索 "MyBatis SQL Visualizer"，
#    安装 → 版本号与 dist/ 的 sha256 一致 → 升级路径再用下一个日期版验一次
```

## 验收标准（与 TASKS 一致）

- 审核通过并上架（上游动作）。
- 从索引安装成功后，IDE 能显示版本与发布者；升级一次仍可安装。
- 三处回读一致：`.oiplugin` sha256 = GitHub 资产 = Gitee 资产。