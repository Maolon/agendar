# Agendar: manage your coding agents like a product manager

An agenda for your AI coding agents. It lists only what needs you. Your answer wakes the agent session that asked.

[Download](https://github.com/Maolon/agendar/releases/latest) · [View on GitHub](https://github.com/Maolon/agendar)

Free and open source. macOS, Windows and Linux. Works with Claude Code, Codex, Cursor, Gemini CLI, Antigravity, opencode and pi.

A product manager does not watch the team type. They agree on the plan, answer the questions no one else can, review the demo, and accept it or send it back. With coding agents, you do that job by reading a wall of terminals. **Agendar is the desk for it: agents report, ask, and deliver in one place, and you see only what needs you.**

## The job, part by part

Run your agents wherever you like. Agendar does not start them or assign their work. Agents declare the plan and report on it. You decide and review.

- **Roadmap: the plan.** Agents declare the project and its steps, with dependencies. You see one square per step, a plan graph, and a finish forecast adjusted by how long past steps took.
- **Standup: the board.** Shows what is moving, stalled, or untouched. If a session stops, its step is freed for another agent. You never chase updates.
- **Your desk: decision cards.** Each card shows the question, two to four options with what each costs, a recommendation, and what your answer unblocks. Ranked by cost of delay.
- **Sign-off: deliveries.** Finished work arrives with demos and a disclosure of what is proven. Accept it, or send it back with comments on the demo.
- **Reading: reports.** Proposals to read before a choice, or findings from a test or an audit. Read the summary in the app and the full document in your browser.
- **Guardrails: budgets and policy.** Work with no demo after four hours is flagged. A policy sets which questions reach you and which an agent may assume.

## A question reaches your desk

The agent asks with one command. It first does the steps that do not depend on the answer, then waits. You get a card. Pick an option or write your own. The session that asked gets a receipt with *done when* criteria and continues.

```sh
agendar ask --question "Delete the build cache?" \
  --option "Delete: frees 49 GB" \
  --option "Keep: next build is fast" --recommend 1
agendar wait <ask-id>      # blocks until you answer, then prints the receipt
agendar ack <answer-id>
```

![A decision card in Agendar: the question, three options with consequences and expected finish times, the recommended one marked, and the answer button](https://mxlo.top/agendar/images/decision-light.webp)

## Done means you saw the demo

An agent's "done" is a claim. A delivery shows the work: a screenshot, a CLI transcript, the few diff hunks that matter, and what is not proven yet. Accept closes the step. Send back reopens it and sends your comments to the same session.

```sh
agendar deliver --node sync/conflicts --attempt <attempt-id> --json delivery.json
agendar wait --delivery <delivery-id>   # blocks until you accept or send back
agendar settle                          # what this session still owes the board
```

In a review, each claim is labeled *Seen* (the app observed it), *Agent says*, or *Not proven yet*.

## Step away from the terminals

The menu bar shows what is waiting for you. Notifications bring questions and finished milestones. Approve the recommendation in one click. Choices that are hard to undo require confirmation in the main window.

![A project page: progress by step, what waits for you, and each agent's expected finish](https://mxlo.top/agendar/images/project-light.webp)

![The menu bar popover: projects, the top question and Approve recommended](https://mxlo.top/agendar/images/tray-light.webp)

## Works with your agents

Agent environments like Paseo and Conductor, or the Claude and Codex desktop apps, are the workshop where agents run. Agendar is the desk beside them, and it works with any of them.

| Agent | How it connects |
|---|---|
| Claude Code | Hooks. A background hook wakes the idle session with your answer or verdict |
| Codex | Hooks. The thread gets your answer at its next turn |
| Cursor, Gemini CLI, Antigravity, opencode, pi | MCP server (`agendar mcp`) |
| Anything that runs a shell | The `agendar` CLI and the Agent Skill |

## Built for agents to drive

You use the app. Agents use one of these.

- **CLI.** Output is JSON, one envelope per command. A repeated `--request-id` replays instead of writing twice. `agendar board`, `agendar progress started --node core/wake --eta 30m..1h30m`.
- **MCP.** The same commands as typed tools: `board`, `start`, `pulse`, `ask`, `wait`, `ack`, `report`, `settle`. `agendar mcp`, `agendar install-mcp --harness cursor`.
- **Hooks.** For Claude Code and Codex. Answers and verdicts reach the session without the agent polling. `agendar bind`, `agendar bindings`.
- **Skill.** The [Agent Skill](https://github.com/Maolon/agendar/blob/main/skills/agendar/SKILL.md) teaches the loop: read the board, report, ask, deliver, settle. `agendar skill print`.

```sh
npx skills add Maolon/agendar -g
```

## Principles

- **Local.** Records are plain files in a folder you choose: iCloud Drive, OneDrive, Dropbox, Syncthing, or one disk. No server, no account.
- **Private.** No telemetry. The only network request checks the GitHub releases API for updates, and you can turn it off.
- **Independent.** Not tied to one model or one agent tool. Seven harnesses connect today, and anything with a shell can use the CLI.
- **Honest.** What an agent claims is labeled *Agent says*. Only what the app observed is labeled *Seen*.
- **Quiet.** Agents that make progress stay out of your way. The list holds only what needs you.
- **Open.** MIT license. Setup installs nothing until you click its button.

## Install

Download the app for your platform from the [latest release](https://github.com/Maolon/agendar/releases/latest). The CLI is bundled inside. On first launch, Setup asks where to store your agenda and offers to link `agendar` onto your PATH.

| Platform | File |
|---|---|
| macOS, Apple silicon | `Agendar_<version>_aarch64.dmg` |
| macOS, Intel | `Agendar_<version>_x64.dmg` |
| Windows | `Agendar_<version>_x64-setup.exe` |
| Linux | `.deb` or `.AppImage` |

Then connect your agents in **Settings > Agents**, or let an agent run `agendar bind`. Each [release](https://github.com/Maolon/agendar/releases/latest) also includes the standalone `agendar` CLI for machines that only run agents, and a `.sha256` checksum for every file.

- **macOS:** The app is not notarized yet. On first launch, open it, then go to System Settings > Privacy & Security and choose Open Anyway. Or run `xattr -dr com.apple.quarantine /Applications/Agendar.app` once.
- **Windows:** SmartScreen may warn about an unknown publisher. Choose More info > Run anyway.
- **Status:** Agendar is young and used every day to track its own development. macOS is the platform it is tested on. Windows and Linux build and pass the tests in CI, but have seen little real use.

## Frequently asked questions

### What is Agendar?

A desktop app and CLI for managing AI coding agents the way a product manager manages a team. Agents report their plan, ask you for decisions, and deliver their work in one place. It lists only what needs you. Your answer wakes the agent session that asked.

### How is it different from an agent environment like Paseo, Conductor, or the Claude and Codex desktop apps?

Those run agents. They give each one a worktree, a terminal, and a chat. They are the workshop. Agendar does not run agents. It is the board and decision loop beside whichever environment you use, and it covers all of them at once. If you use one environment and one agent at a time, its built-in chat may be all you need.

### Does Agendar start agents or assign work to them?

No. Agents declare the plan and take steps themselves. You answer, review, edit the goal, pause a step, or drop it. Agendar records that intent. It never starts, resumes, or stops a session.

### Which coding agents does it work with?

Claude Code and Codex connect through hooks. Cursor, Gemini CLI, Antigravity, opencode, and pi connect through the MCP server (`agendar mcp`). Anything that runs a shell can use the `agendar` CLI and the Agent Skill.

### How does my answer reach the agent?

In Claude Code, a background hook waits for the answer and wakes the idle session with it. Codex gets it through its hooks at the thread's next turn. MCP agents call the `wait` tool. Any other agent runs `agendar wait` or reads `agendar inbox`. The agent applies the receipt and acknowledges it.

### What happens if I do not answer?

A card can carry a default. When its timer expires, the agent proceeds with the recommendation, and the answer appears as an assumption for you to confirm. Without a default, that step holds while other steps keep going.

### Does it need a server? Where is my data kept?

No server and no account. Each machine appends to its own plain files in a folder you choose: iCloud Drive, OneDrive, Dropbox, Syncthing, or a local folder. The app works offline, sends no telemetry, and lets several machines share one agenda.

### Which platforms does it run on, and what does it cost?

macOS (Apple silicon and Intel), Windows, and Linux (`.deb` or `.AppImage`). macOS is the platform it is tested on. Agendar is free and open source under the MIT license.

---

[GitHub](https://github.com/Maolon/agendar) · [Releases](https://github.com/Maolon/agendar/releases) · [Changelog](https://github.com/Maolon/agendar/blob/main/CHANGELOG.md) · [Contributing](https://github.com/Maolon/agendar/blob/main/CONTRIBUTING.md) · [简体中文](https://github.com/Maolon/agendar/blob/main/README.zh-CN.md) · Open source under the MIT license.
