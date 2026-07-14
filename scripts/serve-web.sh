#!/usr/bin/env bash
# Serve the web viewer locally against the FULL table store (design doc 08):
# the all-1755 / grounded-line exports are far too big for GitHub Pages, so
# "type any flop" breadth is local-only. Pages keeps the committed
# data/tables-web tier; this script stages /srv/var/poker/tables-web instead.
#
#   scripts/serve-web.sh              # stage + serve http://localhost:8000
#   scripts/serve-web.sh --export     # first re-export tables → /srv (slow-ish)
#   scripts/serve-web.sh --build      # first rebuild the wasm pkg
#
# The export re-walks every table jsonl (no resume gate), so it's a flag, not
# a default — rerun it when new configs/flops land from the generation queue.
# ponytail: full re-export each time; add a per-file mtime gate if it drags.
set -euo pipefail
cd "$(dirname "$0")/.."

EXPORT_DIR=/srv/var/poker/tables-web
for arg in "$@"; do
  case "$arg" in
    --export)
      cargo run -q --release -- export-tables-web --tables data/tables --out "$EXPORT_DIR"
      ;;
    --build)
      (cd web && wasm-pack build --release --target web)
      ;;
    *) echo "unknown flag $arg (known: --export --build)" >&2; exit 2 ;;
  esac
done

[ -d web/pkg ] || (cd web && wasm-pack build --release --target web)
[ -d "$EXPORT_DIR" ] || {
  echo "no full export at $EXPORT_DIR yet — run: scripts/serve-web.sh --export" >&2
  exit 2
}

# Local stages (all gitignored), same pattern as web/README.
ln -sfn "$EXPORT_DIR" web/tables
[ -e web/preflop ] || ln -s ../data/preflop web/preflop
if [ ! -d web/solutions ]; then
  mkdir -p web/solutions
  cp data/solutions/*.json web/solutions/
  (cd data/solutions && ls *.json | jq -R . | jq -s -c .) > web/solutions/index.json
fi

echo "serving http://localhost:8000 (Ctrl-C to stop)"
python3 -m http.server -d web 8000
