# Contributing

Thanks for helping. Agendar is a Rust workspace (core, CLI, native helpers) plus a
Tauri 2 + React desktop app. Read [`AGENTS.md`](AGENTS.md) first: it holds the
rules for people and coding agents alike.

## Set up

- Rust 1.94.1 with `rustfmt` and `clippy` (pinned in `rust-toolchain.toml`).
- Node 26 and `pnpm@10.33.2` for the app.
- macOS: Xcode command-line tools. Linux: the WebKitGTK packages listed in
  [`.github/workflows/check.yml`](.github/workflows/check.yml).

```sh
pnpm -C app install --frozen-lockfile
cargo test --workspace
pnpm -C app test
cd app && VITE_AGENDAR_FIXTURES=1 pnpm dev   # the app with sample data
```

## How changes land

`main` is protected. Every change is a pull request into `main`:

1. Branch from `main` (`feat/short-name`, `fix/short-name`).
2. Open a pull request. Its **title** follows
   [Conventional Commits](https://www.conventionalcommits.org): `feat: ...`,
   `fix: ...`, `docs: ...`, `refactor: ...`, `test: ...`, `build: ...`, `ci: ...`,
   `chore: ...` or `perf: ...`. Pull requests are squash-merged, so the title
   becomes the commit on `main`.
3. The checks must pass: format, clippy and tests on macOS, Windows and Linux,
   the frontend type-check and tests, the seed oracles and the version check.
   Each pull request also builds installable apps for every platform; find them
   under the run's artifacts.
4. Squash-merge once the checks are green.

## Versions and releases

Do not bump versions by hand. The version lives in `Cargo.toml`
(`[workspace.package]`), `app/package.json` and `app/src-tauri/tauri.conf.json`,
and `node scripts/check-version.mjs` checks they agree.

The pull request title decides the next version.
[release-please](https://github.com/googleapis/release-please) reads the merged
titles and keeps one release pull request open (`chore: release x.y.z`) with
the highest bump among them and the `CHANGELOG.md` entry:

| Title type | Bump | Example |
|---|---|---|
| `fix`, `perf` | patch | 0.1.0 → 0.1.1 |
| `feat` | minor | 0.1.1 → 0.2.0 |
| `feat!`, `fix!` or a `BREAKING CHANGE:` footer | major; minor while below 1.0 | 1.2.3 → 2.0.0 |
| `docs`, `refactor`, `test`, `build`, `ci`, `chore` | none | |

Use `feat` only for something new a user can reach: a command, a view, a
setting, a supported agent. A change to how an existing thing looks or behaves,
polish included, is a `fix`. To correct a bump once, add a `Release-As: x.y.z`
line to the body of a commit on `main`.

Merging the release pull request tags the release. The release workflow then
builds macOS (Apple silicon and Intel), Windows and Linux installers plus the
standalone `agendar` CLI, and uploads them with checksums to the GitHub release.

## Reporting bugs and security issues

Open an issue for bugs and ideas. Report security problems privately, as
described in [`SECURITY.md`](SECURITY.md).
