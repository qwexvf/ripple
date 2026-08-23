---
title: "Daemon"
description: "A resident, file-watching index server — one daemon serves many projects, keeps their graphs hot, and re-indexes on save"
sidebar:
  label: "Daemon"
  order: 3
---

`ripple daemon` is a long-running process that holds project graphs in memory and
re-indexes them as their files change. It exists to remove the one real cost of a CLI
query — **startup**. Every cold `ripple` invocation compiles all thirteen adapters'
tree-sitter queries (~0.8s) before it can answer; the daemon pays that once and keeps the
resident graph hot, so a query becomes a socket round-trip.

## Why one daemon for many projects

A single daemon serves every project you register, which is what makes the shared
tree-sitter query compile pay off — it happens once for the whole machine, not once per
project. It also lets a cross-repo query (a frontend calling a backend indexed separately)
work without a second process.

It stays bounded the way a well-behaved service should:

- **Demand-load + LRU eviction.** A project's graph is built on its first query and
  dropped once it is the least-recently-used past `--max-resident` (default 8). RAM is
  bounded by the cap, not by how many repos you register.
- **A single, de-duplicating reindex queue.** Every rebuild goes through one worker
  thread, and a project already queued is not queued again — a burst of saves collapses
  to one rebuild, so CPU stays near a single core no matter how many editors are firing.
- **Scoped, filtered watches.** `.ripple/`, `.git/`, `node_modules/`, `target/` and the
  like are ignored, so the daemon's own graph writes never trigger a rebuild loop.

## Commands

```bash
ripple daemon                     # run the daemon (foreground; a service manager keeps it up)
ripple daemon --max-resident 16   # raise the resident-graph cap

ripple daemon register <path>     # build + start watching a project
ripple daemon status              # which projects are resident, and their node/edge counts
ripple daemon stop                # ask the daemon to exit
```

The socket lives at `$RIPPLE_SOCKET`, else `$XDG_RUNTIME_DIR/ripple/daemon.sock`, else a
temp path. Clients (the `register`/`status`/`stop` subcommands) find it the same way.

## Wire protocol

Newline-delimited JSON over a Unix domain socket — one request object, one response line:

```json
{"op":"impact","root":"/path/to/repo","symbol":"handleLogin","budget":10}
{"op":"neighbors","root":"/path/to/repo","symbol":"handleLogin","dir":"in"}
{"op":"status"}
```

A response is `{"ok":true,"data":{…}}` or `{"ok":false,"error":"…"}`.

## Running under systemd (Linux)

A user unit ships in [`contrib/systemd/ripple-daemon.service`](https://github.com/qwexvf/ripple/blob/main/contrib/systemd/ripple-daemon.service):

```bash
mkdir -p ~/.config/systemd/user
cp contrib/systemd/ripple-daemon.service ~/.config/systemd/user/
systemctl --user daemon-reload
systemctl --user enable --now ripple-daemon
ripple daemon register .          # from any project
```

`RuntimeDirectory=ripple` gives the daemon `/run/user/<uid>/ripple` for its socket, and
`MemoryMax` / `CPUQuota` put a hard ceiling on top of the daemon's own LRU/queue bounds.

Other init systems and platforms work the same way — the daemon is a plain foreground
process with a Unix socket; a launchd/OpenRC/Windows-service wrapper is future work.
