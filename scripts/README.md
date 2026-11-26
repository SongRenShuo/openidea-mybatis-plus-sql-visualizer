# Release scripts

Two release scripts, used in this order:

| Script | Output |
| --- | --- |
| `scripts/release.sh` | `dist/<id>-<version>.oiplugin` + `.sha256` |
| `scripts/build-index.sh` | `market/index.json` (the marketplace index the IDE reads) |

`scripts/verify-engine.sh` is unrelated to packaging: it checks the pinned
engine jar against `engine/ENGINE_VERSION.sha256` and runs in CI's `package`
job. `release.sh` deliberately does not call it, so a change to the pin check
cannot break an unrelated release run.

`scripts/check-manifest-api.sh` is a manifest gate, not a packaging step: it
fails when `plugin.toml` declares a contributes key newer than its own
`engines.openidea` floor allows.

## The manifest API gate

```sh
scripts/check-manifest-api.sh              # defaults to ./plugin.toml
scripts/check-manifest-api.sh path/to/plugin.toml
bash scripts/check-manifest-api.test.sh    # self-test, temp fixtures only
```

It reads the lower bound of `engines.openidea` (the minor of `^1.15` → 15) and
each contributes key present, then compares against:

| Key | Needs |
| --- | --- |
| `frames`, `markdownHover` / `markdown_hover`, `customEditors` … `binary` | API 1.16 |
| `dialogs`, `overlays` | API 1.18 |

Everything else this repo ships today (commands, keymaps, menus, views,
settings, providers, markerIcons, fileIcons, documentIndexers, mcpTools,
javaAgents, skills) is 1.15 and is not judged. A key above the floor prints
`error: <key> needs plugin API 1.18, but engines.openidea is "^1.15"` and exits
1; a clean manifest prints `ok` and exits 0. A missing `engines.openidea` is
exit 2 (not a pass). An `engines.openidea` form the script cannot parse (e.g.
`*`) yields a warning and exit 0 — the floor is unknown, so nothing is claimed
in either direction.

### Why the CLI's check cannot replace it

`openidea-plugin check` only knows the extension points of the release it was
built from. A key it does not recognise is downgraded to

```
warning: contributes.dialogs: not supported by this version of the IDE; skipped
```

which reads as harmless and exits 0. But once the host release *does* know the
point, its own validator calls `needs_api("contributes.dialogs", 18)`, and a
floor below 1.18 is a hard error: the extension point fails to prepare and the
whole package does not activate. The CLI is silent in exactly the case this gate
has to catch — and a package that silently skips a dialog shows up in manual
acceptance as "nothing happened", easily logged as our bug.

So the two checks are complementary, not redundant: the CLI catches what its own
release knows, this script catches the version-versus-manifest mismatch the CLI
structurally cannot see. Wire it next to `openidea-plugin check` in CI, and run
it locally before pushing any manifest change.

Maintenance: when upstream ships a point we intend to use, add it to the mapping
inside the script and to the self-test in the same change. The self-test copies
the real `plugin.toml` for its "all current keys" case, so it also breaks if
someone lands an API 1.16+/1.18 key without raising `engines.openidea`.

## Prerequisites

```sh
export PATH="/c/Users/USER/.cargo/bin:$PATH"   # Windows Git Bash
openidea-plugin --version                      # must report 0.1.51
```

Exactly 0.1.51: the CLI pins the SDK tag (`v0.1.51` → plugin API 1.15) that
`build` writes into `Cargo.toml`. A CLI built from `main` targets API 1.16, and
no released IDE loads such a component.

## One-time: the publisher key

```sh
openidea-plugin keygen "$HOME/mpsv-publisher.key"
# public key: <base64>   <- register this under your publisher id in the index
```

Rules that are not negotiable:

- The key file never enters the repository, a CI log, an issue or a screenshot.
  `MPSV_KEY_FILE` is read from the environment only; there is no default path.
- Keep two offline backups. Losing the key means a new key, an endorsement by
  the old one (which you no longer have, so users see a key change) and a
  re-registration in every index that lists you.
- `release.sh` refuses to run without it instead of quietly writing an unsigned
  package the IDE would then refuse to install.

`MPSV_PUBLISHER` is the publisher id the key is registered under — the prefix of
the plugin id (`songrenshuo` for `songrenshuo.mybatis-plus-sql-visualizer`). It
defaults to `authors[0]` in `plugin.toml`, and the script prints which one it
used.

## Cutting a release

1. Bump `version` in `plugin.toml`; add the entry to `CHANGELOG.md` — the
   `.oiplugin` change notes are generated from it. Both go through the normal
   review and CI gate first.
2. Run:

```sh
export MPSV_KEY_FILE="$HOME/mpsv-publisher.key"
export MPSV_PUBLISHER=songrenshuo     # optional, defaults to authors[0]
export MPSV_OUT_DIR=dist              # optional

scripts/release.sh
```

3. The script does four things, in this order:

   - `openidea-plugin build .` — `pack` copies whatever `dist/plugin.wasm` is
     lying around and does not check whether the component is stale, so the
     build is not optional and not skippable.
   - `openidea-plugin pack . --key … --publisher …` — signed package.
   - `sha256sum` → `<artifact>.sha256` next to the artifact.
   - `openidea-plugin check <artifact>` — re-reads the package as the IDE would
     install it. Its `ok` line is the gate; a non-zero exit fails the script.

   It ends by printing the three readings a release log should carry: wasm size,
   package size, sha256.

4. Verify the checksum independently before uploading:

```sh
cd dist && sha256sum -c ./*.sha256
```

5. Publish both assets (see `docs/registration/L10.md` for the `release.yml`
   this repo applies): GitHub Release and the Gitee mirror must carry the *same*
   bytes, so their sha256 matches. Compare with:

```sh
sha256sum <github-asset> <gitee-asset>
```

6. Rebuild the index: `scripts/build-index.sh` (below).

## Rebuilding the marketplace index

```sh
export MPSV_KEY_FILE="$HOME/mpsv-publisher.key"
export MPSV_PUBLISHER=songrenshuo
export MPSV_DOWNLOAD_BASE="https://github.com/<owner>/<repo>/releases/download/v0.1.0"
scripts/build-index.sh
```

The index lists **every** `.oiplugin` in `MPSV_DIST_DIR` (default `dist`), so
keep older versions there while older IDE lines still need them. Hosting,
the IDE-side URL, update semantics and failure diagnosis are in
`market/README.md`.

### Key rotation

`--previous-key` (env `MPSV_PREVIOUS_KEY`) must name a key that is **already
registered in the index being rewritten**. The CLI checks this and fails with
`--previous-key is not one of <publisher>'s valid keys in the index` if you
point it at a fresh output path. Sequence that works:

```sh
# old key already in market/index.json
MPSV_KEY_FILE=new.key MPSV_PREVIOUS_KEY=old.key scripts/build-index.sh
```

## Rollback

There is no `revoke` subcommand in 0.1.51. What actually works:

- **Stop offering a bad version**: remove that `.oiplugin` from `MPSV_DIST_DIR`
  and rebuild the index. The version disappears from the index; already
  installed copies keep working (the IDE does not uninstall backwards).
- **Fix it for installed users**: publish a higher version with the fix. Users
  do not auto-downgrade, so a version number cannot be reused — the fixed build
  must be `0.1.x+1`.
- **Do not delete release assets** that published index entries point at; a
  deleted asset turns into a 404 in the IDE. Delete the index entry first, then
  keep the asset around for anyone who installed it directly.

## Gitee mirror notes

- Gitee asset names must match the GitHub ones exactly; the checksum file
  references the artifact by name (`sha256sum -c` reads that name).
- `403 · 无权限` on upload almost always means the repository is not inside the
  Gitee token's "选择仓库" scope, not a real permission error.
- Gitee release assets are uploaded as a separate step from the release record;
  a failed asset upload leaves an empty release behind. Check the asset list
  before announcing.
- Gitee Pages requires real-name verification and serves the repository root,
  so `market/index.json` lands at `https://<user>.gitee.io/<repo>/market/index.json`
  only after the Pages service is enabled for the branch that holds the file.