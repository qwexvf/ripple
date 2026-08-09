#!/usr/bin/env bash
# ripple installer: prebuilt binary + the ripple-orient skill + a CLAUDE.md note
# telling the agent when to use ripple. Re-runnable — the CLAUDE.md edit is a
# marked block that gets replaced, never duplicated.
#
#   curl -fsSL https://raw.githubusercontent.com/qwexvf/ripple/main/install.sh | bash
#   # or, to also register the MCP server and patch a repo's CLAUDE.md:
#   ./install.sh --target /path/to/your/repo --mcp
set -euo pipefail

REPO="qwexvf/ripple"
BINDIR="${BINDIR:-$HOME/.local/bin}"
SKILLS_DIR="${SKILLS_DIR:-$HOME/.claude/skills}"
TARGET="$PWD"   # repo whose CLAUDE.md gets the note
WITH_MCP=0

while [ $# -gt 0 ]; do
  case "$1" in
    --target) TARGET="$2"; shift 2;;
    --mcp) WITH_MCP=1; shift;;
    -h|--help) sed -n '2,9p' "$0"; exit 0;;
    *) echo "unknown arg: $1" >&2; exit 1;;
  esac
done

say() { printf '\033[1;36m▸\033[0m %s\n' "$*"; }

tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT

# Run from a ripple checkout → use the local binary + skill (offline, no download).
# Run standalone (curl | bash) → download the release binary + skill from GitHub.
SCRIPT_DIR="$(cd "$(dirname "$0")" 2>/dev/null && pwd || echo "")"
LOCAL_REPO=""
[ -n "$SCRIPT_DIR" ] && [ -f "$SCRIPT_DIR/crates/cli/Cargo.toml" ] && LOCAL_REPO="$SCRIPT_DIR"

# ── 1. binary ────────────────────────────────────────────────────────────────
if [ -n "$LOCAL_REPO" ]; then
  bin="$LOCAL_REPO/target/release/ripple"
  if [ ! -x "$bin" ]; then
    say "building ripple (release) from $LOCAL_REPO"
    ( cd "$LOCAL_REPO" && cargo build --release --locked -p ripple-cli )
  else
    say "using existing build $bin"
  fi
  cp "$bin" "$tmp/ripple"
else
  case "$(uname -s)-$(uname -m)" in
    Linux-x86_64)   triple=x86_64-unknown-linux-gnu;;
    Darwin-arm64)   triple=aarch64-apple-darwin;;
    Darwin-x86_64)  triple=x86_64-apple-darwin;;
    *) echo "no prebuilt binary for $(uname -s)-$(uname -m); run this from a clone to build from source" >&2; exit 1;;
  esac
  say "downloading ripple ($triple)"
  curl -fsSL "https://github.com/$REPO/releases/latest/download/ripple-$triple.tar.gz" | tar -xz -C "$tmp"
fi
mkdir -p "$BINDIR"
install -m755 "$tmp/ripple" "$BINDIR/ripple"
say "installed → $BINDIR/ripple"
case ":$PATH:" in *":$BINDIR:"*) :;; *) echo "  ⚠ add $BINDIR to your PATH";; esac

# ── 2. skill ─────────────────────────────────────────────────────────────────
say "installing ripple-orient skill → $SKILLS_DIR/ripple-orient"
mkdir -p "$SKILLS_DIR/ripple-orient"
if [ -n "$LOCAL_REPO" ]; then
  cp "$LOCAL_REPO/.claude/skills/ripple-orient/SKILL.md" "$SKILLS_DIR/ripple-orient/SKILL.md"
else
  curl -fsSL "https://raw.githubusercontent.com/$REPO/main/.claude/skills/ripple-orient/SKILL.md" \
    -o "$SKILLS_DIR/ripple-orient/SKILL.md"
fi

# ── 3. CLAUDE.md note (idempotent marked block) ──────────────────────────────
claude_md="$TARGET/CLAUDE.md"
say "patching $claude_md"
block="$tmp/block.md"
cat > "$block" <<'BLOCK'
<!-- ripple:start (managed by install.sh — edits between these markers are overwritten) -->
## Orienting with ripple

This repo is indexed by [ripple](https://github.com/qwexvf/ripple). Before implementing
or changing code, orient with it instead of grepping blind:

- **Starting a task** ("implement/add/fix X"): run `ripple locate "<task in plain words>"`
  first — it returns risk-ranked entrypoints with a blast-radius preview. Start there.
- **Before changing a symbol**: `ripple impact <symbol>` (what breaks) and
  `ripple neighbors <symbol> --in` (callers).
- **After editing**: `ripple reindex` (or `ripple index .`) — the graph is stale until you do.

See the `ripple-orient` skill for the full workflow.
<!-- ripple:end -->
BLOCK

if [ -f "$claude_md" ] && grep -q '<!-- ripple:start' "$claude_md"; then
  awk -v bf="$block" '
    BEGIN { while ((getline l < bf) > 0) blk = blk l ORS }
    /<!-- ripple:start/ { printf "%s", blk; skip = 1; next }
    /<!-- ripple:end -->/ { skip = 0; next }
    !skip
  ' "$claude_md" > "$claude_md.tmp" && mv "$claude_md.tmp" "$claude_md"
  say "  updated existing ripple block"
else
  { [ -f "$claude_md" ] && printf '\n'; cat "$block"; } >> "$claude_md"
  say "  appended ripple block"
fi

# ── 4. MCP (optional) ────────────────────────────────────────────────────────
if [ "$WITH_MCP" = 1 ]; then
  if command -v claude >/dev/null 2>&1; then
    say "registering MCP server (root: $TARGET)"
    claude mcp add ripple -- "$BINDIR/ripple" mcp --root "$TARGET" || \
      echo "  ⚠ 'claude mcp add' failed — add it by hand (see docs/reference/mcp.md)"
  else
    echo "  ⚠ --mcp given but 'claude' not on PATH; skipping MCP registration"
  fi
fi

say "done. try:  ripple locate \"<a task>\" --root \"$TARGET\""
