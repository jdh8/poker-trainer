#!/usr/bin/env bash
#
# collect-shards.sh — merge remote shard workers' local tables into /srv.
#
# The fleet fans one manifest out across boxes with `--stride N --offset K`
# (see docs/shared-machine-data-gen.md). Each box writes its own disjoint set
# of flops to a LOCAL --out dir. Because offsets never overlap and the
# per-formation header-<hash>.json is a byte-identical idempotent write on
# every box, a plain `rsync -a` merge is correct: no --ignore-existing, no
# dedup, no conflict. Run this on the main box (the one with /srv) after the
# remotes finish, then run the mop-up pass.
#
# Usage (main box's own offset-0 shard already writes straight to /srv — only
# pull the remotes):
#   scripts/collect-shards.sh \
#     worker-a:poker-tables \
#     worker-b:poker-tables
#
# `host:poker-tables` is rsync-over-ssh shorthand for ~/poker-tables on host
# (point the source at whatever --out the worker used, e.g. a big shared
# volume).
set -euo pipefail

dest=/srv/var/poker/tables

if [[ $# -eq 0 || "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
	sed -n '2,/^set -euo/p' "$0" | sed 's/^# \{0,1\}//;/^set -euo/d'
	exit 0
fi

for src in "$@"; do
	echo "collect-shards: $src -> $dest/"
	rsync -a "$src/" "$dest/"
done
