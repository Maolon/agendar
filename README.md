<p align="center">
  <img src="app/src-tauri/icons/128x128@2x.png" width="96" height="96" alt="Agendar icon">
</p>

<h1 align="center">Agendar</h1>

<p align="center">
  <b>An agenda for your AI coding agents.</b><br>
  It lists only what needs you. Your answer wakes the agent session that asked.
</p>

<p align="center">
  <a href="https://github.com/Maolon/agendar/actions/workflows/check.yml"><img src="https://github.com/Maolon/agendar/actions/workflows/check.yml/badge.svg" alt="check"></a>
  <a href="https://github.com/Maolon/agendar/releases/latest"><img src="https://img.shields.io/github/v/release/Maolon/agendar?sort=semver" alt="latest release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-MIT-blue" alt="MIT license"></a>
  <img src="https://img.shields.io/badge/macOS%20%C2%B7%20Windows%20%C2%B7%20Linux-lightgrey" alt="macOS, Windows, Linux">
</p>

<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/images/decision-dark.webp">
    <img src="docs/images/decision-light.webp" alt="A decision card: the question, three options with consequences and ETAs, the recommended one marked, and the answer button" width="820">
  </picture>
</p>

You run several coding agents across repositories. Most of the time they do not
need you. When one hits a decision that is yours, the question sits in a
terminal tab you are not looking at. The agent waits in silence while dependent
steps stall behind it. Or it guesses, and spends its next steps building on a
choice that is hard to undo.

When agents are making progress, they should stay out of your way. Agendar is a
desktop app and CLI that lists only what needs your call.

Every agent gets one place to report its plan and one way to ask. When an agent
needs a decision, it sends a card with the question, the options and what each
one costs, its recommendation, and what the answer unblocks. You get a single
list of what needs you, ranked by how many agents are stuck on it and for how
long. Answer from the app, the menu bar, or the notification, and the same
agent session continues where it stopped.

## How it works

1. **Agents report.** Through the `agendar` CLI or its MCP server, an agent
   declares the steps it plans, says when it starts and finishes each one, and
   sends a short pulse as it goes.
2. **Agents ask.** When a decision is yours, the agent sends a card. It
   includes the question, two to four options with what each one costs, its
   recommendation and why, and what the answer unblocks. It also states when
   the agent may go ahead with the recommendation if you stay silent.
3. **You answer.** The card waits under *Needs you*. Pick an option or write
   your own; the answer becomes a receipt with *done when* criteria.
4. **The same session continues.** Hooks hold Claude Code and Codex sessions
   open until you answer. Other agents read their inbox. The agent applies the
   receipt, acknowledges it, and reports the result back to the plan.

## What you see

<table>
  <tr>
    <td width="68%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="docs/images/project-dark.webp">
        <img src="docs/images/project-light.webp" alt="A project page: progress by step, what waits for you, and each agent's expected finish">
      </picture>
    </td>
    <td width="32%">
      <picture>
        <source media="(prefers-color-scheme: dark)" srcset="docs/images/tray-dark.webp">
        <img src="docs/images/tray-light.webp" alt="The menu bar popover: projects, the top question and Approve recommended">
      </picture>
    </td>
  </tr>
</table>

- **Needs you**, ranked by cost of delay. Questions are ordered by the number
  of waiting agents times how long they have waited, weighted by how hard the
  choice is to undo.
- **Projects** with one square per step. See what each agent is doing now, when
  it expects to finish, and a forecast for the whole project calibrated on how
  long past steps really took.
- **A plan graph** of steps and their dependencies. A question shows which
  steps it unblocks.
- **The menu bar** shows the top question with *Approve recommended*. Easy
  questions can be answered straight from the notification.
- **No server.** Each machine appends to its own files in a folder you choose
  (iCloud Drive, Dropbox, Syncthing, or a local folder). The app works offline,
  and several machines can share one agenda.
- **English, 简体中文 and 繁體中文**, light and dark.

## Works with

| Agent | How it connects |
|---|---|
| Claude Code | hooks: the session waits for your answer and continues |
| Codex | hooks |
| Cursor, Gemini CLI, Antigravity, opencode, pi | MCP server (`agendar mcp`) |
| Anything that runs a shell | the `agendar` CLI and the Agent Skill |

Connect them in **Settings > Agents**, or let an agent run `agendar bind`.
Agents learn the loop from the [Agent Skill](skills/agendar/SKILL.md):

```sh
npx skills add Maolon/agendar -g
```

## Install

Download the app for your platform from the
[latest release](https://github.com/Maolon/agendar/releases/latest):

| Platform | File |
|---|---|
| macOS, Apple silicon | `Agendar_<version>_aarch64.dmg` |
| macOS, Intel | `Agendar_<version>_x64.dmg` |
| Windows | `Agendar_<version>_x64-setup.exe` |
| Linux | `.deb` or `.AppImage` |

Each release also has the `agendar` CLI on its own (`agendar-cli-*`) for
machines that only run agents, and a `.sha256` checksum for every file.

On first launch, Setup picks the folder for your agenda and links `agendar` onto
your PATH.

> **macOS:** the app is not notarized yet. The first time, open it, then go to
> **System Settings > Privacy & Security** and choose **Open Anyway**. Or run
> `xattr -dr com.apple.quarantine /Applications/Agendar.app` once.
> **Windows:** SmartScreen may warn about an unknown publisher; choose
> **More info > Run anyway**.

## Status

Agendar is young and used every day to track its own development. macOS is the
platform it is tested on. Windows and Linux build and pass the tests in CI but
have seen little real use. Sharing one agenda across machines is built in and
tested with simulated hosts; real multi-machine use is still thin.

## Build from source

Rust 1.94.1 (pinned in `rust-toolchain.toml`), Node 26 and `pnpm@10.33.2`.

```sh
pnpm -C app install --frozen-lockfile
cargo test --workspace
pnpm -C app test

# Run the app with sample data and no setup
cd app && VITE_AGENDAR_FIXTURES=1 pnpm dev

# Build the installable app (the CLI is bundled inside it)
cd app && node scripts/bundle-cli.mjs && pnpm exec tauri build --bundles app
```

The project layout:

- `crates/agendar-core`: the event model, the append-only store, the SQLite
  index and the reducer that turns events into plans, progress, decisions and
  forecasts.
- `crates/agendar-cli`: the `agendar` command agents run, including the MCP
  server and the harness hooks.
- `app/`: the desktop app (Tauri 2, React and TypeScript).
- `skills/agendar/`: the Agent Skill.

## Documentation

- [`CONTRIBUTING.md`](CONTRIBUTING.md): how changes and releases work.
- [`AGENTS.md`](AGENTS.md): rules for people and coding agents changing the code.

## License

[MIT](LICENSE)
