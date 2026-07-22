# Running the solver on a shared machine

Regenerating the GTO solution library (`cargo run -p solve-gen`) is
**CPU-saturating**: `solve-gen` links [`postflop-solver`](https://github.com/b-inary/postflop-solver),
built with `rayon`, so each solve spins one worker per hardware thread and pegs
the whole box while it grinds the curated spots to equilibrium.

On a machine shared with colleagues, the cap has to come from the **OS**, not the
app. The policy below is what we use; it's wrapped in [`scripts/idle-run.sh`](../scripts/idle-run.sh).

## The policy: SCHED_IDLE, no quota

For a box that is **idle most of the time**, run the job in the `SCHED_IDLE`
scheduling class (plus the idle I/O class) and set **no CPU quota**:

```sh
scripts/idle-run.sh cargo run -p solve-gen --release
# expands to:  nice -n10  chrt --idle 0  ionice -c3  <command>
```

This is the *scavenger* pattern:

- **Uses 100% of spare capacity.** With no quota, a quiet box runs the job at
  full speed — a quota would just waste idle cores for no benefit.
- **Yields instantly when anyone shows up.** `SCHED_IDLE` tasks only get a core
  when no normal-priority task is runnable on it; a colleague's task preempts
  yours the moment it wakes there.

`solve-gen` already uses every core itself (one rayon worker per hardware
thread), so it owns the box on its own — don't launch several at once. This
is measured, not just etiquette (2026-07, Ryzen 8700F, dual-channel DDR5):
two concurrent 8-thread solves ran no faster than one 16-thread stream — a
single solve already saturates memory bandwidth, so extra jobs only split
it. `RAYON_NUM_THREADS` caps a job's workers if you ever need cores freed.

### Why SCHED_IDLE rather than `nice -19`

`nice -19` is the *same scheduling class* as everyone else, just at the lowest
weight — it still competes, and (see below) it does **not** reliably yield across
*users*. `SCHED_IDLE` is a distinct class strictly below all normal tasks:

- runs only on otherwise-idle CPU, with lower preemption latency;
- needs **no privilege** (de-prioritizing yourself is always allowed); and
- is **inherited by child processes**, so wrapping the parent (`cargo`) covers
  every solver thread it spawns.

We *also* prepend a cosmetic `nice -n10`: `SCHED_IDLE` ignores the nice value for
scheduling, but it's still stored on the task, so it shows up low-priority (blue) in `htop`.

Verify it took effect: `chrt -p <pid>` should print `SCHED_IDLE`. On a box
without `chrt` the script falls back to `nice -n19` and says so on stderr.

## What this does NOT cover

Scheduling priority arbitrates **CPU time on a core**. It does nothing about the
*shared* resources that often matter more, so even a perfectly-yielding job can
still slow a neighbour while both are actually running:

1. **Turbo / clock.** Many busy cores force the CPU down to all-core base clock,
   so a colleague's single-threaded job loses its turbo headroom (a silent
   ~20–40% tax). No priority or cgroup setting fixes this.
2. **Last-level cache & memory bandwidth.** Double-dummy / CFR search is
   transposition-table-heavy; flooding cores thrashes the shared L3 (on a 3D
   V-cache part, the very cache a latency-sensitive neighbour may depend on) and
   saturates memory bandwidth. Neither `nice` nor `cpu.weight` arbitrates these.
3. **Cross-user fairness.** On a modern `systemd` + cgroup-v2 box, CPU is split
   **per user slice first** (each `user-UID.slice` defaults to `cpu.weight=100`),
   and only *within* a slice does per-task priority apply. So against another
   *active user's parallel* job the kernel tends toward a ~50/50 split by slice,
   and your `nice`/`SCHED_IDLE` only re-orders your own tasks. (For the common
   case — a colleague's light or single-threaded job — they get what they need
   regardless, so this rarely bites.)

Because of (1) and (2), prefer to run off-hours when you can, rather than
assuming priority makes a flat-out box invisible.

## When to add a hard cap

If the box is **reliably busy** (not our usual case), priority isn't enough — add
a kernel-enforced ceiling via a transient `systemd` scope:

```sh
# ~6 cores' worth, low cross-user weight, RAM guard, still idle-class within it:
systemd-run --user --scope -p CPUQuota=600% -p CPUWeight=10 -p MemoryMax=12G \
  scripts/idle-run.sh cargo run -p solve-gen --release
```

- `CPUQuota=600%` — hard ceiling of 6 cores of CPU-time (kernel-enforced).
- `CPUWeight=10` — lowers *your slice's* share so it actually yields to other
  **users** (the lever `nice` lacks; see caveat 3).
- `MemoryMax` — the useful guard here: `postflop-solver` allocates a sizeable
  game tree per spot, so this stops a wide tree from OOM-ing colleagues.

## Surviving disconnect

- Run inside `tmux`/`screen`, or detach with
  `setsid nohup scripts/idle-run.sh cargo run -p solve-gen --release >run.log 2>&1 < /dev/null &`.
- The run is one-shot and the output is regenerable, so a dropped session just
  means re-running it — nothing to resume.

## Etiquette

Check who is on first (`w` / `who`), prefer nights/weekends for full-throttle
runs, and give a heads-up before a long job. The `data/solutions/*.json` files
are regenerable from `solve-gen`, so don't hoard old copies — delete and re-make
when needed.
