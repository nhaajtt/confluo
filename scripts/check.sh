#!/usr/bin/env bash
# Every check the project has, in one command. CI runs the same script.
#
#   scripts/check.sh          everything (the desktop crate included)
#   scripts/check.sh rust     the Rust workspace without the desktop crate, which
#                             needs the system WebView libraries (Linux CI has none)
#   scripts/check.sh app      the window's TypeScript: types, lint, a production build
#
# Works in Git Bash on Windows, and on Linux and macOS.
set -euo pipefail
cd "$(dirname "$0")/.."
what=${1:-all}

rust() {
  local exclude=()
  [ "$1" = without-app ] && exclude=(--exclude confluo-app)
  cargo fmt --all --check
  cargo clippy --workspace ${exclude[@]+"${exclude[@]}"} --all-targets -- -D warnings
  cargo test --workspace ${exclude[@]+"${exclude[@]}"}
  node --check extension/background.js
  node --check extension/options.js
  for f in site/*.js; do node --check "$f"; done
}

app() {
  (
    cd app
    pnpm install --frozen-lockfile
    pnpm typecheck
    pnpm lint
    # Built into a throwaway folder: this checks the build, it does not make one.
    out=$(mktemp -d)
    pnpm exec vite build --outDir "$out" --emptyOutDir
    rm -rf "$out"
  )
}

case "$what" in
  all) rust with-app; app ;;
  rust) rust without-app ;;
  app) app ;;
  *) echo "usage: scripts/check.sh [all|rust|app]" >&2; exit 2 ;;
esac
echo "all checks passed"
