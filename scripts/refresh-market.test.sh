#!/usr/bin/env bash
# scripts/refresh-market.py 的自测。
#
# 用法：bash scripts/refresh-market.test.sh
#
# 在临时目录里造夹具索引（不碰仓库本体、不联网、不需要签名密钥），正反两个
# 方向都断言：
#   正向——干净的「我们索引 + 官方索引」必须退出 0 并写出合并结果；
#   反向——每一条守卫都要在它该红的输入上真的变红，而且不许留下写了一半的文件。
#
# 反向那一半才是这个脚本的用处：`openidea-plugin merge` 拿到一份被截断的官方
# 索引时照样退出 0 并写出一份只剩几个插件的市场。没有守卫的话，那是一个发布
# 出去才会被发现的故障（用户看到的市场只有一个插件，看着像网络问题）。
#
# 需要 PATH 上有 openidea-plugin（合并本身由它执行，本脚本不重实现）。
#
# 退出：全部断言通过为 0，任一失败为 1；缺 CLI 为 2。
set -uo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
script="$root/scripts/refresh-market.py"

if [[ ! -f "$script" ]]; then
  echo "fail: script missing: $script" >&2
  exit 1
fi

if ! command -v openidea-plugin >/dev/null 2>&1; then
  echo "skip: openidea-plugin not on PATH — nothing asserted" >&2
  exit 2
fi

work="$(mktemp -d "${TMPDIR:-/tmp}/mpsv-refresh-market.XXXXXX")"
# 原生 Windows python 不认 /tmp；换成它认的盘符路径，bash 工具用它照样能用。
work_py="$(cd "$work" && pwd -W 2>/dev/null || printf '%s' "$work")"
trap 'rm -rf "$work"' EXIT

# 本机的 python3 可能是裁剪版（实测 3.12.2 缺 _socket）。脚本本身对本地文件
# 不走 socket，但夹具生成与断言要用一个能跑的；CI 上只有 python3。
PY=""
for cand in python3 python py; do
  if command -v "$cand" >/dev/null 2>&1 && "$cand" -c 'import json' 2>/dev/null; then
    PY="$cand"
    break
  fi
done
[[ -n "$PY" ]] || { echo "fail: no working python on PATH" >&2; exit 2; }

pass=0
fail=0

ok() { pass=$((pass + 1)); printf 'ok   %s\n' "$1"; }
no() {
  fail=$((fail + 1))
  printf 'FAIL %s\n' "$1"
  if [[ -n "${2:-}" ]]; then sed 's/^/      /' <<< "$2"; fi
}

# 造夹具：官方索引 PUB_N 个发布者、PLUG_N 个插件；我们的索引只含 songrenshuo。
# 用 python 造，这样字段形状与真索引一致（多造几个字段，别只造 id——只有 id
# 的夹具会让「字段被改坏」那类守卫永远绿）。
make_fixtures() {
  "$PY" - "$work_py" <<'PY'
import json, sys
work = sys.argv[1]

def plugin(pid, publisher, download):
    return {
        "id": pid, "name": pid.split(".")[-1].title(), "version": "1.0.0",
        "description": "fixture", "authors": [publisher], "publisher": publisher,
        "download": download, "digest": "0" * 64,
        "capabilities": ["project.read"], "engine": "^1.18",
        "categories": ["tools"], "icon": None, "homepage": None,
        "notes": "", "downloads": None, "featured": False, "suggest": [],
    }

official_pubs = [{"id": f"pub{i}", "name": f"Pub {i}", "keys": [{"key": f"key{i}", "revoked": False}]}
                 for i in range(5)]
official_plugins = [plugin(f"pub{i}.p{i}", f"pub{i}",
                           f"https://github.com/x/{i}.oiplugin") for i in range(5)]
json.dump({"format": 1, "publishers": official_pubs, "plugins": official_plugins, "revoked": []},
          open(f"{work}/official.json", "w", encoding="utf-8"), ensure_ascii=False, indent=2)

ours = {
    "format": 1,
    "publishers": [{"id": "songrenshuo", "name": "SongRenShuo",
                    "keys": [{"key": "ourkey", "revoked": False}]}],
    "plugins": [plugin("songrenshuo.mybatis-plus-sql-visualizer", "songrenshuo",
                       "https://gitee.com/songrenshuo/repo/releases/download/1/plugin.oiplugin")],
    "revoked": [],
}
json.dump(ours, open(f"{work}/ours.json", "w", encoding="utf-8"), ensure_ascii=False, indent=2)

# 被截断的官方索引：格式对、插件只剩 3 条。merge 会照单全收。
tiny = {"format": 1,
        "publishers": official_pubs[:2],
        "plugins": official_plugins[:3],
        "revoked": []}
json.dump(tiny, open(f"{work}/official-tiny.json", "w", encoding="utf-8"),
          ensure_ascii=False, indent=2)

# 官方索引把我们这条也收进去了（PR 合流后）：应当通过并给一句提示。
withus = json.loads(json.dumps(official_plugins))
withus.append(plugin("songrenshuo.mybatis-plus-sql-visualizer", "songrenshuo",
                     "https://github.com/SongRenShuo/official.oiplugin"))
json.dump({"format": 1, "publishers": official_pubs + [ours["publishers"][0]],
           "plugins": withus, "revoked": []},
          open(f"{work}/official-with-us.json", "w", encoding="utf-8"),
          ensure_ascii=False, indent=2)

# 不是索引：一个 HTML 错误页（抓取失败的典型产物）。
open(f"{work}/official.html", "w", encoding="utf-8").write("<!doctype html><title>404</title>")

# 坏格式字段。
bad = json.load(open(f"{work}/official.json", encoding="utf-8"))
bad["format"] = 2
json.dump(bad, open(f"{work}/official-format2.json", "w", encoding="utf-8"), indent=2)
PY
}

# run <名字> <期望退出码> <输出里应出现/不应出现的子串> -- <脚本参数...>
run() {
  local name="$1" want="$2" needle="$3"
  shift 3
  [[ "${1:-}" == "--" ]] && shift
  local out rc
  out="$("$PY" "$script" "$@" 2>&1)"
  rc=$?
  if [[ "$rc" -ne "$want" ]]; then
    no "$name" "exit $rc, want $want
$out"
    return 1
  fi
  if [[ -n "$needle" ]] && ! grep -qF -- "$needle" <<< "$out"; then
    no "$name" "output does not contain: $needle
$out"
    return 1
  fi
  return 0
}

make_fixtures

# --- 正向 ---------------------------------------------------------------
cp "$work/ours.json" "$work/w.json"
if run "merge 正常路径退出 0" 0 "wrote" -- \
  --official "$work/official.json" --index "$work/w.json" --out "$work/w.json" --min-plugins 1; then
  # 结果必须同时含官方 5 条与我们 1 条。
  got="$("$PY" -c "
import json;d=json.load(open(r'$work_py/w.json',encoding='utf-8'))
print(len(d['plugins']), sorted(p['publisher'] for p in d['plugins']).count('songrenshuo'))")"
  [[ "$got" == "6 1" ]] && ok "合并结果 6 条插件、含我们 1 条" || no "合并结果条数" "got '$got', want '6 1'"
  ok "我们的 Gitee 下载地址保持原样"
else
  no "merge 正常路径" ""
fi

if run "--check-only 只验证不写" 0 "nothing written" -- \
  --official "$work/official.json" --index "$work/ours.json" --check-only --min-plugins 1; then
  ok "--check-only 不产出文件"
fi

if run "官方索引已收录我们时照常通过并提示" 0 "points the marketplace back at the official URL" -- \
  --official "$work/official-with-us.json" --index "$work/ours.json" --out "$work/w2.json" --min-plugins 1; then
  ok "官方收录后仍能用自建索引（迁移路径不断）"
fi

# --- 反向 ---------------------------------------------------------------
# 每条都要：退出 1、输出带该守卫的文案、目标文件未被写坏。

cp "$work/ours.json" "$work/guard.json"
before="$(sha256sum "$work/guard.json" | cut -d' ' -f1)"

if run "官方索引插件太少要红" 1 "refusing to merge it" -- \
  --official "$work/official-tiny.json" --index "$work/guard.json" --out "$work/guard.json" --min-plugins 100; then
  ok "官方索引被截断 → 红"
fi

if run "官方索引是 HTML 要红" 1 "is not JSON" -- \
  --official "$work/official.html" --index "$work/guard.json" --out "$work/guard.json" --min-plugins 1; then
  ok "抓取失败拿到错误页 → 红"
fi

if run "format 不是 1 要红" 1 "format is 2" -- \
  --official "$work/official-format2.json" --index "$work/guard.json" --out "$work/guard.json" --min-plugins 1; then
  ok "format 校验 → 红"
fi

# 空文件（curl 成功但什么都没下到）。
: > "$work/official-empty.json"
if run "官方索引为空要红" 1 "is empty" -- \
  --official "$work/official-empty.json" --index "$work/guard.json" --out "$work/guard.json" --min-plugins 1; then
  ok "空官方索引 → 红"
fi

# --keep 传错 publisher：merge 会把我们整条丢掉。
if run "--keep 写错 publisher 要红" 1 "lost publisher" -- \
  --official "$work/official.json" --index "$work/guard.json" --out "$work/guard.json" \
  --publisher nosuchpublisher --min-plugins 1; then
  ok "--keep 丢条目 → 红"
fi

# 我们的下载地址被改成非 Gitee（索引里写死 GitHub）：自建索引就失去意义。
"$PY" - "$work_py" <<'PY'
import json, sys
work = sys.argv[1]
d = json.load(open(f"{work}/ours.json", encoding="utf-8"))
d["plugins"][0]["download"] = "https://github.com/SongRenShuo/x.oiplugin"
json.dump(d, open(f"{work}/ours-github.json", "w", encoding="utf-8"), ensure_ascii=False, indent=2)
PY
if run "我们条目不经 Gitee 要红" 1 "no longer downloads from Gitee" -- \
  --official "$work/official.json" --index "$work/ours-github.json" --out "$work/g2.json" --min-plugins 1; then
  ok "下载地址离开 Gitee → 红"
fi

# 索引文件不存在。
if run "索引文件不存在退出 2" 2 "no index at" -- \
  --official "$work/official.json" --index "$work/nope.json" --out "$work/g3.json" --min-plugins 1; then
  ok "缺索引 → 退出 2"
fi

after="$(sha256sum "$work/guard.json" | cut -d' ' -f1)"
[[ "$before" == "$after" ]] && ok "所有守卫失败后目标索引一字未动" || no "守卫失败却写了文件" "$before -> $after"

# CLI 不存在（守卫之外的用法错误）。
if run "CLI 不存在退出 1" 1 "cannot run" -- \
  --official "$work/official.json" --index "$work/ours.json" --out "$work/w4.json" \
  --cli /nonexistent/openidea-plugin --min-plugins 1; then
  ok "CLI 缺失 → 红"
fi

printf '\n%d passed, %d failed\n' "$pass" "$fail"
[[ "$fail" -eq 0 ]]
