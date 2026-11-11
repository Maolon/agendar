# agendar-ffi

The UniFFI facade over `agendar-core` for mobile clients. This crate defines the FFI contract in `src/agendar.udl`, and `scripts/ios-build.sh` builds the static libraries, generated Swift bindings, `AgendarCore.xcframework` and synthetic test Vault for iOS targets.

## The contract

`src/agendar.udl` is contract v1: the calls and records exposed to generated bindings (such as Swift). The Rust side is `src/facade/`, one file per group of calls:

- `open.rs`: `Session.open`, in three stages (the Vault folder and its manifest, this device's data directory and identity, the service), so a failed open says whose fault it is;
- `read.rs`: `refresh`, `now`, `projects`, `project`; `activity.rs` fills the project page's activity, header numbers and deliveries;
- `card.rs`: `card`;
- `write.rs`: `answer_checked`, `rescue`, `receipt`;
- `delivery.rs`: `delivery` (one delivery for review), with its demos in `demo.rs`;
- `review.rs`: `review` (accept, or send back with a reason, comments on the demos, or both; a send-back that does not pass is `SendBackRefused` with what is wrong and the comment at fault as fields);
- `snooze.rs`: `snooze` (the owner's `need.snoozed` event: the need leaves `NowBoard.needs` for `NowBoard.snoozed` on every host; a need with a `default_at` returns 15 minutes before it, `snoozed_until` is that time, and a snooze that could not hide it at all is `E_VALIDATION` `snooze_default_near`);
- `spike.rs`: the spike calls;
- `types.rs` and `review_types.rs`: the records and enums.

`delivery` names each demo file by its Vault-relative path, sha256 and size, so a mobile client can ask the sync provider for it. It carries the text of a transcript or a diff only when the whole file is on the device.

A core error crosses as `Core(code, detail, field, retryable, message)`. `detail` is one of the codes in `agendar_core::error::details` or absent: the facade passes on only a listed code, so a client can treat that list as a closed set (`tests/details.rs`, and the pin in `tests/contract.rs`). `message` is a sentence for logs.

Every contract v1 call is built. `Core("E_NOT_IMPLEMENTED")` stays the answer of a call a later contract declares before it is built.

A change to a UDL declaration is a contract change for downstream bindings and mobile clients (comments and blank lines do not count): call it out in the pull request.

## What `scripts/ios-build.sh` produces

Needs macOS with Xcode (iOS 18+ SDKs) and the Rust targets for the pinned toolchain:

```sh
rustup target add --toolchain 1.94.1 aarch64-apple-ios aarch64-apple-ios-sim
scripts/ios-build.sh
```

The script writes only under `target/`:

- `target/<triple>/release/libagendar_ffi.a`: the static library per slice (under `CARGO_TARGET_DIR` when set);
- `target/ios/bindings/`: the generated `AgendarCore.swift`, `AgendarCoreFFI.h` and module map;
- `target/ios/AgendarCore.xcframework`: device (`ios-arm64`) and simulator (`ios-arm64-simulator`) slices;
- `target/ios/TestVault`: the test Vault, regenerated on every run by `crates/agendar-core/tests/ios_fixture.rs` (about 5,000 events in one project from three agent hosts, one open ask with two options);
- `target/ios/SyncBatches`: five batches of the workload's next 50 events each, as Vault-relative files to append (the last batched event is ten minutes old).

A mobile client can link `bindings`, `AgendarCore.xcframework`, `TestVault` and `SyncBatches` from `target/ios/`. Run the script again after a change to this crate, the UDL or the core so the generated bindings and archives stay current. `--no-app` is accepted for compatibility and does nothing.

The libraries in the xcframework are cargo's archives as built. Do not run `strip` on them: Xcode's `strip` removes the `.llvm.<hash>` suffix that tells apart the copies of a function rustc specialized per codegen unit. A final link then binds all callers to one copy; a copy specialized for at most 5 SQL parameters fails every index write with more (`E_INDEX`, "Got 5, needed 6"). Xcode strips the final linked binary itself.

## `unsafe`

The crate's `unsafe` exception (`AGENTS.md`) covers only the glue UniFFI generates from the UDL, included by `src/scaffolding.rs`. Every hand-written module forbids `unsafe_code`, and `tests/no_unsafe.rs` checks it.
