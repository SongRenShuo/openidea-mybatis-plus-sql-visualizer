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

## `plugins/songrenshuo.mybatis-plus-sql-visualizer.toml`

```toml
repository = "https://gitee.com/songrenshuo/openidea-mybatis-plus-sql-visualizer"
suggest = ["pom.xml", "build.gradle.kts"]

[[versions]]
version = "0.1.0"
package = "https://github.com/<owner>/openidea-mybatis-plus-sql-visualizer/releases/download/v0.1.0/songrenshuo.mybatis-plus-sql-visualizer-0.1.0.oiplugin"
sha256 = "<sha256 of that package>"
```

Keep the `suggest` list aligned with what the plugin is for (mapper projects); the
marketplace shows the repository as the homepage when the manifest names none.