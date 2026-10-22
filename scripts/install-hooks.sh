#!/usr/bin/env bash
# Install the repository's git hooks.
#
# The hooks live in scripts/ and are tracked in git, so every clone of this
# repository can install the same ones rather than each machine accumulating
# its own private version. That is the whole point: a check that only exists
# on the laptop where it was written is not a check.
#
# Run it after cloning:
#
#     ./scripts/install-hooks.sh
#
# Or wire it into `git clone`:
#
#     git clone <url> && cd <dir> && ./scripts/install-hooks.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GIT_DIR="$(git rev-parse --git-dir)"

echo "Installing hooks into $GIT_DIR/hooks..."

# Only the hooks this repository ships. Anything already in .git/hooks that
# is not on this list — a hand-written commit-msg, a post-merge, a
# gitleaks install — is left exactly as it is.
for hook in pre-commit pre-push; do
  source_hook="$SCRIPT_DIR/$hook"
  [ -f "$source_hook" ] || continue

  cp "$source_hook" "$GIT_DIR/hooks/$hook"
  chmod +x "$GIT_DIR/hooks/$hook"
  echo "  ✓ $hook"
done

cat <<'EOF'

pre-push (runs before each push):
  • cargo fmt --all --check  — the same command CI's fmt job runs
  • cargo audit             — every committed lockfile, waiving the
                               advisories already waived in deny.toml

To bypass (not recommended — CI runs the same checks and does not honour it):
  git push --no-verify
EOF