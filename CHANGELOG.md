# Changelog

## [0.2.1](https://github.com/Maolon/agendar/compare/v0.2.0...v0.2.1) (2026-11-19)


### Fixes

* an iCloud Vault reads files macOS moved off this Mac instead of failing with E_IO ([#7](https://github.com/Maolon/agendar/issues/7)) ([0c6ac95](https://github.com/Maolon/agendar/commit/0c6ac9515d5d1b0799c3ce0a31ac69b8d936dbce))

## [0.2.0](https://github.com/Maolon/agendar/compare/v0.1.0...v0.2.0) (2026-11-11)


### Features

* delivery review, a self-correcting board, search and update checks ([#4](https://github.com/Maolon/agendar/issues/4)) ([3cbd1ea](https://github.com/Maolon/agendar/commit/3cbd1eabef5cbf07050711cad4c9417d89247729))

## 0.1.0

First public release.

- Agents report steps, progress and pulses, and ask decision cards, through the
  `agendar` CLI, its MCP server, or Claude Code and Codex hooks.
- The desktop app lists what needs you, ranked by cost of delay; answer from the
  app, the menu bar popover or a notification, and the agent continues.
- Projects show progress per step, what each agent is doing, expected finish
  times and a calibrated project forecast, plus a plan graph.
- A Vault in any synced folder, written per machine, with no server.
- English, Simplified Chinese and Traditional Chinese; light and dark.
