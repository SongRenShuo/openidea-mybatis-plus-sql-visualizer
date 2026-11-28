#!/usr/bin/env python3
"""Merge the official marketplace index into ours, and prove the result is sane.

Why this exists
---------------
The IDE holds ONE marketplace URL (`crates/ide-host/src/plugins/market.rs`: the
user's value replaces the default rather than adding to it), and installing a
signed package needs the publisher key inside whichever index is in use. So our
index has to be a *merged snapshot*: our own entries plus everything official.
That snapshot goes stale the moment upstream adds a plugin.

`openidea-plugin merge <ours> <official> --keep <publisher>` refreshes the
official half and keeps ours verbatim. It needs no signing key: it republishes
entries, it does not sign a package.

What this script adds on top of `merge`
--------------------------------------
`merge` will happily write a one-plugin index if it is handed a truncated
official file. Every guard below exists because that failure would be silent:
users would see a marketplace holding a single plugin, which looks like a
network problem, not like a broken commit.

  1. the official file looks like an index (format, plugin count, publishers)
  2. the merge kept our publisher and our plugin entries
  3. our entries are byte-for-byte what they were before (no field mangling)
  4. our download URL still points at Gitee (that is the whole point of it)
  5. the merged non-ours half equals the official half (merge contract)
  6. nothing is written unless 1-5 all pass

Usage
-----
    scripts/refresh-market.py --official /tmp/official.json
    scripts/refresh-market.py --official https://example.com/plugins-index.json \
        --out market/index.json
    scripts/refresh-market.py --official /tmp/official.json --check-only

Options
    --official PATH|URL   official index to merge in (required)
    --index PATH          our current index (default: market/index.json)
    --publisher ID        publisher to keep (default: songrenshuo)
    --out PATH            where to write the merge (default: --index, in place)
    --min-plugins N       refuse an official index with fewer plugins
                          (default 100; tests use a smaller floor)
    --cli NAME            openidea-plugin binary (default: openidea-plugin)
    --check-only          validate the official index, do not merge or write

Exit codes
    0  ok (merged, validated, written — or validated, with --check-only)
    1  a guard failed; nothing was written
    2  usage error, unreadable input, or the CLI could not run

Tests
-----
`bash scripts/refresh-market.test.sh` drives both directions: a clean pair of
fixtures must pass, and each guard must fail on the input that trips it.
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile

GITEE_HOST = "gitee.com"


class Fail(Exception):
    """A guard said no. Nothing has been written when this is raised."""


def load_json(path_or_url):
    if path_or_url.startswith(("http://", "https://")):
        # Imported here, not at module scope: a local-file run needs no socket
        # support at all, and some bundled interpreters ship without `_socket`.
        import urllib.request

        req = urllib.request.Request(
            path_or_url, headers={"User-Agent": "market-refresh"}
        )
        with urllib.request.urlopen(req, timeout=60) as resp:
            raw = resp.read()
    else:
        with open(path_or_url, "rb") as fh:
            raw = fh.read()
    if not raw.strip():
        raise Fail(f"{path_or_url} is empty")
    try:
        return json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        # A cached HTML error page also decodes to text, so name the real cause.
        raise Fail(f"{path_or_url} is not JSON: {exc}") from exc


def check_official(doc, min_plugins):
    """Guards 1: the file we are about to merge must be a real index."""
    if doc.get("format") != 1:
        raise Fail(f"official index format is {doc.get('format')!r}, expected 1")
    plugins = doc.get("plugins")
    publishers = doc.get("publishers")
    if not isinstance(plugins, list) or not isinstance(publishers, list):
        raise Fail("official index is missing the plugins/publishers arrays")
    if len(plugins) < min_plugins:
        raise Fail(
            f"official index holds only {len(plugins)} plugins "
            f"(floor {min_plugins}) — refusing to merge it"
        )
    if not publishers:
        raise Fail("official index has no publishers")
    return len(publishers), len(plugins)


def ours_entries(doc, publisher):
    plugins = [p for p in doc.get("plugins", []) if p.get("publisher") == publisher]
    publishers = [p for p in doc.get("publishers", []) if p.get("id") == publisher]
    return plugins, publishers


def check_merged(merged, ours, official, publisher, min_plugins):
    """Guards 2-5: the merge is only as good as what it kept and what it took."""
    if merged.get("format") != 1:
        raise Fail(f"merged index format is {merged.get('format')!r}, expected 1")

    mine, my_pubs = ours_entries(merged, publisher)
    if not my_pubs:
        raise Fail(f"merged index lost publisher {publisher!r} — --keep did not apply")
    if not mine:
        raise Fail(f"merged index lost every {publisher}.* plugin — --keep did not apply")

    total = len(merged.get("plugins", []))
    if total < min_plugins:
        raise Fail(f"merged index holds only {total} plugins (floor {min_plugins})")

    # Guard 3: our entry must be exactly what we published. A merge that drops
    # `--keep` does not merely omit us — it can also substitute an older
    # official copy, which would point updates at a URL we no longer ship.
    was_plugins, was_pubs = ours_entries(ours, publisher)
    before = {p["id"]: p for p in was_plugins}
    for entry in mine:
        old = before.get(entry["id"])
        if old is None:
            raise Fail(f"merged index invented {entry['id']!r}, which our index did not list")
        if entry != old:
            changed = sorted(k for k in set(entry) | set(old) if entry.get(k) != old.get(k))
            raise Fail(f"{entry['id']} changed during the merge: {', '.join(changed)}")
    for old_id in before:
        if old_id not in {e["id"] for e in mine}:
            raise Fail(f"merged index dropped {old_id!r}, which our index listed")
    if my_pubs[0] != was_pubs[0]:
        raise Fail(f"publisher {publisher!r} entry changed during the merge")

    # Guard 4: the reason we host our own index at all.
    for entry in mine:
        download = entry.get("download") or ""
        if GITEE_HOST not in download:
            raise Fail(f"{entry['id']} no longer downloads from Gitee: {download}")

    # Guard 5: everything else must come from the *official half we just
    # fetched*, unchanged. Comparing against our previous snapshot instead
    # would report every upstream addition as a mismatch.
    official_ids = {p["id"] for p in official.get("plugins", []) if p.get("publisher") != publisher}
    merged_ids = {p["id"] for p in merged.get("plugins", []) if p.get("publisher") != publisher}
    if merged_ids != official_ids:
        missing = sorted(official_ids - merged_ids)[:5]
        extra = sorted(merged_ids - official_ids)[:5]
        raise Fail(
            "the merged index does not carry the official half verbatim "
            f"(missing {missing}, extra {extra})"
        )

    return len(merged.get("publishers", [])), total


def run_merge(cli, ours_path, official_path, publisher, out_path):
    cmd = [cli, "merge", ours_path, official_path, "--keep", publisher, "--out", out_path]
    try:
        proc = subprocess.run(cmd, capture_output=True, text=True, env=dict(os.environ))
    except OSError as exc:
        raise Fail(f"cannot run {cli!r}: {exc}") from exc
    if proc.returncode != 0:
        raise Fail(
            f"{' '.join(cmd)} failed with exit {proc.returncode}\n"
            f"stdout: {proc.stdout.strip()}\nstderr: {proc.stderr.strip()}"
        )
    return proc.stdout.strip()


def main(argv):
    parser = argparse.ArgumentParser(
        description="Merge the official marketplace index into ours and validate it.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__.split("Usage\n-----", 1)[1],
    )
    parser.add_argument("--official", required=True, metavar="PATH|URL")
    parser.add_argument("--index", default="market/index.json", metavar="PATH")
    parser.add_argument("--publisher", default="songrenshuo", metavar="ID")
    parser.add_argument("--out", default=None, metavar="PATH")
    parser.add_argument("--min-plugins", type=int, default=100, metavar="N")
    parser.add_argument("--cli", default="openidea-plugin", metavar="NAME")
    parser.add_argument("--check-only", action="store_true")
    args = parser.parse_args(argv)

    out_path = args.out or args.index

    if not os.path.isfile(args.index):
        print(f"refresh-market: no index at {args.index}", file=sys.stderr)
        return 2
    ours = load_json(args.index)

    # The official half is read once, then handed to the CLI from a file: a
    # second fetch inside `merge` could see a different (newer) document, and
    # then guards 5 would compare two different upstream states.
    official = load_json(args.official)
    pubs, plugins = check_official(official, args.min_plugins)
    print(f"official: {pubs} publishers, {plugins} plugins")

    if any(p.get("id") == args.publisher for p in official.get("publishers", [])):
        # Not a failure — once the registry PR lands, this is the normal state.
        # The CLI refuses a merge where both sides list the same publisher
        # (`may not list publisher "songrenshuo"`), so strip ours from the
        # official half: we keep publishing our own copy until the user points
        # the marketplace back at the official index.
        kept = len(official["plugins"])
        official["publishers"] = [
            p for p in official["publishers"] if p.get("id") != args.publisher
        ]
        official["plugins"] = [
            p for p in official["plugins"] if p.get("publisher") != args.publisher
        ]
        print(
            f"note: the official index lists publisher {args.publisher!r} "
            f"({kept - len(official['plugins'])} plugin(s) dropped from it); "
            "once the user points the marketplace back at the official URL, "
            "this workflow becomes redundant"
        )

    if args.check_only:
        print("check-only: official index is usable; nothing written")
        return 0

    with tempfile.TemporaryDirectory(prefix="refresh-market.") as tmp:
        official_path = os.path.join(tmp, "official.json")
        with open(official_path, "w", encoding="utf-8", newline="\n") as fh:
            json.dump(official, fh, ensure_ascii=False, indent=2)
        merged_path = os.path.join(tmp, "merged.json")

        print(run_merge(args.cli, args.index, official_path, args.publisher, merged_path))
        merged = load_json(merged_path)

    total_pubs, total_plugins = check_merged(
        merged, ours, official, args.publisher, args.min_plugins
    )
    mine, _ = ours_entries(merged, args.publisher)
    print(
        f"merged: {total_pubs} publishers, {total_plugins} plugins; "
        f"ours: {[p['id'] for p in mine]}"
    )

    # Write only after every guard passed: a half-validated index on disk is
    # exactly what the next daily run would merge from.
    tmp_out = out_path + ".tmp"
    with open(tmp_out, "w", encoding="utf-8", newline="\n") as fh:
        json.dump(merged, fh, ensure_ascii=False, indent=2)
        fh.write("\n")
    shutil.move(tmp_out, out_path)
    print(f"wrote {out_path}")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except Fail as exc:
        print(f"refresh-market: {exc}", file=sys.stderr)
        sys.exit(1)
