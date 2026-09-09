#!/bin/sh
# Builds the Rust core and puts the dylib where the linker expects it.
#
# Run as a build phase ahead of "Link Binary With Libraries". Before this
# existed, the dylib was a 5 MB binary committed to git and copied by hand from
# target/release, while the project's LIBRARY_SEARCH_PATHS pointed at an
# absolute path to target/debug — so the committed copy was never actually
# linked, Release silently linked the debug build, and forgetting to run
# `cargo build` produced "symbol(s) not found" from a stale library.
set -eu

MANIFEST="$SRCROOT/../rust_core/Cargo.toml"
DEST="$SRCROOT/WarpApp/librust_core.dylib"

# Match the Cargo profile to the Xcode configuration. Getting this wrong is how
# a Release build ends up shipping an unoptimised library.
case "${CONFIGURATION:-Debug}" in
  Release) PROFILE_FLAG="--release"; PROFILE_DIR="release" ;;
  *)       PROFILE_FLAG="";          PROFILE_DIR="debug"   ;;
esac

# Xcode build phases do not inherit an interactive shell's PATH, so a bare
# `cargo` is usually not found even when it works fine in Terminal.
CARGO=""
for candidate in \
  "${CARGO_HOME:-$HOME/.cargo}/bin/cargo" \
  /opt/homebrew/bin/cargo \
  /usr/local/bin/cargo \
  "$(command -v cargo 2>/dev/null || true)"
do
  if [ -n "$candidate" ] && [ -x "$candidate" ]; then CARGO="$candidate"; break; fi
done

if [ -z "$CARGO" ]; then
  echo "error: cargo not found. Install Rust from https://rustup.rs, or set CARGO_HOME." >&2
  exit 1
fi

echo "note: building rust_core ($PROFILE_DIR) with $CARGO"
# shellcheck disable=SC2086
"$CARGO" build $PROFILE_FLAG --manifest-path "$MANIFEST"

BUILT="$SRCROOT/../rust_core/target/$PROFILE_DIR/librust_core.dylib"
if [ ! -f "$BUILT" ]; then
  echo "error: expected $BUILT after a successful cargo build" >&2
  exit 1
fi

# Only copy when it actually changed, so we don't dirty the file (and retrigger
# downstream phases) on every no-op build.
if ! cmp -s "$BUILT" "$DEST"; then
  mkdir -p "$(dirname "$DEST")"
  cp "$BUILT" "$DEST"
  echo "note: updated $DEST"
fi
