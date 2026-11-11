# AGENTS.md — agendar

Read this before changing anything in this repository.

## Sources of truth
1. The code and its tests are the contract. The seed scenarios in `crates/agendar-core/tests/oracle/seeds/` are reviewed oracles for the reducer: never edit an expected result to make a test pass; raise the contradiction instead.
2. `README.md` says what Agendar is for; `CONTRIBUTING.md` how changes land.

## Boundaries
- Work only inside this repository.
- Never install anything globally, never write to `~/.claude`, `~/.codex` or other user config. Tests that exercise hook installation must use a temporary HOME.

## Hygiene
- No credentials, tokens, personal data, absolute local paths (`/Users/...`) or session transcripts in the repository.
- Pin dependency versions you add, and only use versions you verified exist (`cargo search`, `npm view`).
- Keep files small and focused; follow the existing module layout.

## What must hold
- Contracts that protect Vault data integrity (single writer, append/create only, tail-safe reads, convergence) and security (no secrets, consent before installing anything, report sandbox, IPC permissions, no `unsafe` outside `agendar-notify`, `agendar-chrome` and the generated glue in `agendar-ffi`) are mandatory.
- Exhaustive hardening beyond that (rare platform durability fallbacks, exotic filesystem aliases, extra defensive branches) may be simplified. Say what you simplified.
- Keep `unsafe` out of the crates: use std's safe platform extensions or small, pinned, maintained crates instead. The only exceptions are `crates/agendar-notify` (native notification FFI), `crates/agendar-chrome` (macOS window glass) and `crates/agendar-ffi` (the UniFFI facade for mobile clients), where the exception covers only the code UniFFI generates from `src/agendar.udl`. That glue is included by `src/scaffolding.rs`, the one module allowed `unsafe_code`; every hand-written module forbids it, and `cargo test -p agendar-ffi` (`tests/no_unsafe.rs`) fails if the `unsafe` keyword appears in the crate's sources or another module allows `unsafe_code`.

## Checks before a change is done
- `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`, and the frontend checks: `pnpm -C app install --frozen-lockfile`, `pnpm -C app typecheck`, `pnpm -C app test`, `pnpm -C app build`. The frontend package manager is `pnpm@10.33.2`, pinned in `app/package.json`.
- The workspace also builds for the other desktop platforms (zig is the cross C toolchain, so bundled C code such as SQLite compiles too):
  `cargo-zigbuild check --workspace --all-targets --target x86_64-unknown-linux-gnu --target-dir target/cross` and the same with `--target x86_64-pc-windows-gnu`. Both must finish without errors. Use `check`, not a full build. MSVC-only behavior is verified on real Windows or CI.
- Do not create other build directories; everything goes under `target/`.
- List anything you could not finish or verify; never report a check as passing that you did not run.

## Pull requests
- Every change reaches `main` through a pull request; see `CONTRIBUTING.md`.
- The PR title follows Conventional Commits (`feat: ...`, `fix: ...`, `docs: ...`); it becomes the squash commit that release-please reads.
- Never bump versions by hand: release PRs do it, and `node scripts/check-version.mjs` checks that they agree.
