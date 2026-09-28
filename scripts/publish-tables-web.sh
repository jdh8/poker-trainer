#!/usr/bin/env bash
# Publish the grounded-line web tables (design doc 10, phase 2) as the
# `tables-web.tar` asset of the `tables-web` GitHub release; pages.yml untars
# it over the committed data/tables-web tier. Too big for git (~50 MB/line),
# so it ships out of band — one release tag, reused, asset clobbered.
#
#   scripts/publish-tables-web.sh               # export + pack + upload
#   scripts/publish-tables-web.sh --no-upload   # stop after packing
#
# HDD-bound: each line re-reads its full table dir (~100 GB of JSONL): ~12 min to
# export a line plus gzip, a few hours in all. Rerun when new lines/flops land.
# ponytail: full re-export each run; add an mtime gate if reruns get frequent.
set -euo pipefail
cd "$(dirname "$0")/.."

# Pages caps the whole site at 1 GB; wasm + git-tracked data is ~50 MB.
BUDGET=$((900 * 1000 * 1000))
STAGE=/srv/var/poker/tables-release
TAR=/srv/var/poker/tables-web.tar

# Priority order: cash-hu55 (the site's HU preflop tier), then cash89/mtt89
# interleaved by arrival mass (manifests/lines-*.toml comments). A prefix
# ships: the first line that would cross $BUDGET ends it.
LINES=(
  cash-hu55_r2.5-c cash-hu55_r3-c cash-hu55_c-x cash-hu55_c-r3-c
  cash-hu55_r2.5-r7.5-c
  mtt89_f-f-f-f-c-x mtt89_f-f-f-r3-f-c mtt89_f-f-r3-f-f-c
  cash89_f-f-f-f-r3-c mtt89_f-r3-f-f-f-c mtt89_r3-f-f-f-f-c
  mtt89_f-f-f-f-c-r3-c cash89_f-f-f-r3-f-c cash89_f-f-r3-f-f-c
  cash89_f-r3-f-f-f-c mtt89_f-f-f-f-r3-c cash89_r3-f-f-f-f-c
  cash89_f-f-f-f-c-x mtt89_f-f-f-r3-c-f cash89_r2.5-f-f-f-f-c
  mtt89_f-f-r3-f-c-f cash89_f-f-f-r3-c-f mtt89_f-r3-f-f-c-f
  cash89_f-f-r3-f-c-f cash89_f-f-f-f-c-r3-c
  cash-hu55_r3-r9-c cash-hu55_r2.5-r10-c
)

upload=1
for arg in "$@"; do
  case "$arg" in
    --no-upload) upload=0 ;;
    *) echo "unknown flag $arg (known: --no-upload)" >&2; exit 2 ;;
  esac
done

cargo build -q --release --bin poker-trainer
rm -rf "$STAGE" && mkdir -p "$STAGE/.index"
total=0
shipped=()
for line in "${LINES[@]}"; do
  nice -n 19 ionice -c3 target/release/poker-trainer export-tables-web \
    --tables data/tables --out "$STAGE" --formation "$line"
  mv "$STAGE/index.json" "$STAGE/.index/$line.json"
  gzip -9 "$STAGE/$line"/*.bin
  size=$(du -sb "$STAGE/$line" | cut -f1)
  if (( total + size > BUDGET )); then
    echo "budget: $line ($((size / 1000000)) MB) would cross $((BUDGET / 1000000)) MB — stopping" >&2
    rm -rf "${STAGE:?}/$line"
    break
  fi
  total=$((total + size))
  shipped+=("$line")
  echo "$line: $((size / 1000000)) MB, total $((total / 1000000)) MB" >&2
done

((${#shipped[@]})) || { echo "no line fits the budget" >&2; exit 1; }
(cd "$STAGE/.index" && jq -s add "${shipped[@]/%/.json}") >"$STAGE/index.json"
tar -C "$STAGE" -cf "$TAR" index.json "${shipped[@]}"
echo "packed ${#shipped[@]} lines → $TAR ($(du -h "$TAR" | cut -f1))" >&2

if ((upload)); then
  gh release view tables-web >/dev/null 2>&1 ||
    gh release create tables-web --prerelease --title "Web tables (data)" \
      --notes "Grounded-line flop tables for the Pages site (design doc 10). Rebuilt by scripts/publish-tables-web.sh; pages.yml downloads tables-web.tar."
  gh release upload tables-web "$TAR" --clobber
fi
