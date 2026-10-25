# CLI speed and footprint (measured 2026-10-25)

Status: measured on the release build of `feat/macos-background-input`. One change kept: the model-limits cache.

## How it was measured

- Machine: this Mac (Darwin 27.0.0). Every command ran from a clean directory, not a git repo.
- nca used kenari.id with `minimax-m3-1-flash`. The MiniMax key is expired.
- Wall time and CPU time come from `wait4()`, which includes finished children. RSS is the sum over the whole process tree, sampled every 50 ms with `ps`.
- Physical footprint is macOS `footprint <pid>`, sampled once, about 2 s into the run. It is the closest figure to Activity Monitor's memory. RSS also counts shared, clean framework pages, so it overstates real use.
- Figures are the median of 3 runs, except where noted. The harness lives in `/tmp/nca-bench/bench.py` and is not committed.

## Simple turn: "Reply with the word ok."

| CLI | Model | Wall | CPU | RSS (tree) | Footprint |
|---|---|---|---|---|---|
| nca, release, with cache | kenari minimax-m3-1-flash | 3.6 s | 0.033 s | 17.8 MB | 6.4 MB |
| nca, release, before cache | same | 7.5 s | 0.040 s | 19.2 MB | not sampled |
| Claude Code `claude -p` | Claude default | 3.1 s | 0.80 s | 296 MB | 162 MB (1 sample at 1.5 s) |
| Grok CLI `grok -p` | Grok default | 6.6 s | 1.77 s | 695 MB | 59 MB (1 sample at 3 s) |
| Cursor CLI `cursor-agent -p` | not run | exits in 0.36 s, not logged in | 0.39 s | 217 MB | not sampled |
| Codex CLI | not run | not installed | | | |

Cursor exited with `Authentication required`, so this row is startup only, not a turn.

## The GitHub-in-Dia test through nca

- Before the cache: 20.1 s wall, 0.125 s CPU, 28.5 MB peak RSS.
- With the cache: 16.1 s wall, 0.106 s CPU, 28.5 MB peak RSS.
- The run includes a deliberate 5 s screenshot delay and three model calls, about 11 s in total.

## Where nca's time goes

- Before the change, each run waited 2.5 to 4.3 s on the provider's `/v1/models` catalog before the first event. The kenari catalog is 84 KB with 111 models, and the server took 2.2 s to answer. The whole response was also parsed into a generic JSON tree on every run.
- Now the first event arrives 14 to 20 ms after launch. The rest of the wall time is model latency.

## What changed

- Context-window lookups are cached on disk at `<product home>/cache/model-limits.json` for 24 hours, keyed by provider, endpoint, and model. A cache hit skips the catalog request and its parse. Failed lookups are not cached.
- Tried a single-threaded tokio runtime for the CLI. It saved about 0.2 MB of footprint. Not kept, because it also changes the runtime that the interactive TUI runs on, for a negligible gain.

## Caveats

- Wall times depend on each provider's model, and nca used a different model from the others. CPU and footprint are the fair client-side comparison.
- Footprint samples are single snapshots, so they are lower bounds for each CLI's peak.
- Cursor needs `agent login`, and Codex needs installing. Both are waiting on a decision.

## Next

- Measure full turns for Cursor and Codex once they are logged in or installed.
- Replace the fixed 5 s screenshot delay with a page-load check, which would make the GitHub test faster.
- Consider a Rust-native `nca bench` command in place of the `/tmp` script.
