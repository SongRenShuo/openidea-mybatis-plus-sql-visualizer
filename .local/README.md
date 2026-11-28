# `.local/` — 机器本地文件目录

本目录存放**只属于当前机器**的文件：签名密钥、令牌、本机路径覆盖。参考主仓（IntelliJ 版）的 `.local/` 约定。

## 约定

- **整目录被 git 忽略**（`.gitignore` 的 `.local/*`），**唯一例外是本文件**（`!.local/README.md` 白名单入库）——仓库里能看到这份说明，但看不到同目录下的任何真实内容。
- **新增机器本地文件直接放这里即可，不需要再改 `.gitignore`**。
- 注意 git 规则：父目录被整排除时无法再包含子文件，所以忽略规则必须是 `.local/*` + `!.local/README.md` 两条；不要改成 `.local/`（那样本文件也会被忽略）。

## 当前清单

| 文件 | 用途 | 说明 |
| --- | --- | --- |
| `songrenshuo.key` | 发布者 Ed25519 私钥（`openidea-plugin keygen` 生成），发版时 `openidea-plugin pack --key .local/songrenshuo.key --publisher songrenshuo` 签名 | **拿到它的人就能以 songrenshuo 的身份发布插件**——绝不入库、绝不经聊天工具明文传输；换密钥时用 `endorse` 为新密钥背书 |

发布者公钥（公开信息，登记仓 `publishers/songrenshuo.toml` 的 `keys` 与本表同步维护）：

```
8drSIdqiKMSfYJ/LXpCgRAMXtvdB+cDg95qJ8xGFIzg=
```

## 相关流程

- 发版：`scripts/release-smoke.sh` 本地自验（不产出包）→ tag → CI 的 `release.yml` 构建签名并双端上传 → 回填 CI 那一轮的 SHA-256 → 登记 PR（`docs/registry/` 草稿，提 PR 需用户确认）。
- 平台文档：`docs/zh/plugins/publishing/packaging.mdx`（签名）、`policy.mdx`（审核规则）。
