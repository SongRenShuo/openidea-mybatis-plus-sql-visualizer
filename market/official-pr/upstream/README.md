# Upstream PR files (templates)

These two files are the exact shapes `github.com/l0ng-ai/openidea-plugins` expects.
They are **templates**: fill `<...>` after a signed package has been published (tag +
GitHub Release + Gitee mirror). Do not open the PR before the package URL and sha256 exist —
the registry CI downloads the package, checks the hash, runs `openidea-plugin check` and
verifies the signature.

## `publishers/songrenshuo.toml`

```toml
name = "songrenshuo"
owners = ["SongRenShuo"]      # GitHub account(s) allowed to change these entries
keys = ["<public key from `openidea-plugin keygen songrenshuo.key`>"]
revoked-keys = []
```

## `plugins/songrenshuo.mybatis-sql-visualizer.toml`

```toml
repository = "https://gitee.com/songrenshuo/openidea-mybatis-sql-visualizer"
suggest = ["pom.xml", "build.gradle.kts"]

[[versions]]
version = "<next version built under the new id>"
package = "https://github.com/<owner>/openidea-mybatis-sql-visualizer/releases/download/<version>/songrenshuo.mybatis-sql-visualizer-<version>.oiplugin"
sha256 = "<sha256 of that package>"
```

The plugin id is `songrenshuo.mybatis-sql-visualizer` since 2026-11-29 (it was
`songrenshuo.mybatis-plus-sql-visualizer` before). The published 2026.11.29
package still has the old id inside, and both the registry CI and the IDE compare
the package's id with the entry's id — so this table stays empty until the first
package built under the new id ships.

Keep the `suggest` list aligned with what the plugin is for (mapper projects); the
marketplace shows the repository as the homepage when the manifest names none.