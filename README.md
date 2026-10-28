# Mux

A native desktop app for running coding agents, shells and tasks side by side. Mux drives the Codex and Claude Code CLIs, keeps every session running in a background server, and gives each one a fast native UI built with Rust and egui.

## Features

- Codex and Claude Code agents with streaming output, tool calls, diffs and subagents
- Plan mode, agent questions, task lists, rewind and a queue for messages sent mid-turn
- Skills and slash commands, with `/` and `$` autocomplete in the composer
- A worktree per agent, a changes summary and a diff panel
- Real terminals for shells and tasks
- Sessions grouped by project, with search, unread markers and desktop notifications
- Token usage read from the CLIs' own transcripts, plus context compaction
- Built-in image and file viewers, light and dark themes

## Requirements

- Rust (edition 2024)
- The `codex` and/or `claude` CLIs on your `PATH`, signed in
- Linux (X11 or Wayland) or macOS

## Running

```sh
cargo run --release -p mux-gui
```

The app starts its server on first launch and reconnects to it afterwards, so sessions keep running when the window is closed. The server is the same binary run with `--server`.

To add Mux to your app launcher on Linux:

```sh
cargo build --release -p mux-gui
scripts/install-desktop.sh
```

## Layout

- `crates/core` holds the protocol shared by the app and server
- `crates/server` runs agents, terminals and session storage
- `crates/ui` holds the egui components and theme
- `crates/gui` is the app itself

Data lives in `~/.local/share/mux` (or `~/Library/Application Support/mux` on macOS). Set `MUX_DATA_DIR` and `MUX_SOCKET` to run a separate instance.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).
