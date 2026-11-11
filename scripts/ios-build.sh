#!/bin/sh
# Build the agendar-ffi static libraries for iOS targets (device and
# simulator), the generated Swift bindings, AgendarCore.xcframework and the
# synthetic 5k-event test Vault with its sync batches
# (crates/agendar-ffi/README.md).
#
# Cargo output goes to CARGO_TARGET_DIR (default target/). The assembled
# outputs go to target/ios/: bindings/, AgendarCore.xcframework, TestVault/,
# SyncBatches/. Nothing is installed and nothing outside target/ is written.
#
# Needs: macOS with Xcode (iOS SDKs) and the rustup targets aarch64-apple-ios
# and aarch64-apple-ios-sim for the pinned toolchain.
#
# Usage: scripts/ios-build.sh
#   --no-app is accepted for compatibility and does nothing.
set -eu

repo_root=$(cd "$(dirname "$0")/.." && pwd)
target_dir=${CARGO_TARGET_DIR:-$repo_root/target}
out="$repo_root/target/ios"
slices="aarch64-apple-ios aarch64-apple-ios-sim"
case "${1:-}" in
"" | --no-app) ;;
*) echo "usage: scripts/ios-build.sh" >&2; exit 2 ;;
esac

# Minimum iOS deployment target (18.0) for the static library slices.
IPHONEOS_DEPLOYMENT_TARGET=18.0
export IPHONEOS_DEPLOYMENT_TARGET

cd "$repo_root"
# Outputs that are links (an older layout made them) are removed as links:
# never write through one.
for name in bindings AgendarCore.xcframework TestVault SyncBatches; do
    if [ -L "$out/$name" ]; then rm "$out/$name"; fi
done
rm -rf "$out/bindings" "$out/headers" "$out/lib" "$out/AgendarCore.xcframework" "$out/TestVault" \
    "$out/SyncBatches"
mkdir -p "$out/bindings" "$out/headers"

for slice in $slices; do
    cargo build -p agendar-ffi --lib --release --locked --target "$slice"
    mkdir -p "$out/lib/$slice"
    # Never run `strip` on this archive. Xcode's strip drops the `.llvm.<hash>`
    # suffix that keeps apart the copies of one function rustc specialized per
    # codegen unit; a final link then binds every caller to one copy, and a
    # copy made for at most 5 SQL parameters fails every statement with more
    # (E_INDEX "Got 5, needed 6"). Xcode strips the linked binary.
    cp "$target_dir/$slice/release/libagendar_ffi.a" "$out/lib/$slice/"
done

# The pinned generator reads src/agendar.udl and crates/agendar-ffi/uniffi.toml.
cargo build -p agendar-ffi --features bindgen-cli --bin uniffi-bindgen --locked
"$target_dir/debug/uniffi-bindgen" generate crates/agendar-ffi/src/agendar.udl \
    --language swift --out-dir "$out/bindings"
cp "$out/bindings/AgendarCoreFFI.h" "$out/headers/"
cp "$out/bindings/AgendarCoreFFI.modulemap" "$out/headers/module.modulemap"

xcodebuild -create-xcframework \
    -library "$out/lib/aarch64-apple-ios/libagendar_ffi.a" -headers "$out/headers" \
    -library "$out/lib/aarch64-apple-ios-sim/libagendar_ffi.a" -headers "$out/headers" \
    -output "$out/AgendarCore.xcframework"

# Synthetic test Vault: about 5,000 events in one project from
# three agent hosts with one open ask, plus five sync batches of 50 more events
# for the relaunch timing (crates/agendar-core/tests/ios_fixture.rs). The last
# batched event is ten minutes old. Regenerated on every run.
fixture_log="$out/fixture.log"
if ! AGENDAR_IOS_FIXTURE="$out/TestVault" AGENDAR_IOS_SYNC_BATCHES="$out/SyncBatches" cargo test -p agendar-core --locked \
    --test ios_fixture -- --ignored --nocapture >"$fixture_log" 2>&1; then
    cat "$fixture_log"
    exit 1
fi
grep '^ios-fixture:' "$fixture_log"

echo "ios-build: $out/AgendarCore.xcframework"
