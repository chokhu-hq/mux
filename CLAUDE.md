# Mux

Mux is a native Rust and egui harness for the Codex CLI, Claude Code and OpenCode. A background server owns the sessions (agent chats and terminals), and the app is a client that connects to it over a local socket. It runs on Linux, macOS and Windows. It's experimental and changes a lot.

## Layout

- `crates/core`: shared code. `protocol.rs` holds the client and server messages and `PROTOCOL` (bump it whenever a message changes). `lib.rs` has `build_id()`, `login_shell()` and the Windows helpers (`windows_build()`, `windows_path()`).
- `crates/server`: the background server. `agent.rs` drives Claude Code, `agent/opencode.rs` drives OpenCode, and Codex goes through its app-server JSON-RPC. `models.rs` lists models, `store.rs` saves state, `tray.rs` is the Windows tray.
- `crates/ui`: reusable widgets in `components/`, `theme.rs` (palettes, visuals, the `SelectionWatch` plugin), `highlight.rs` (syntax colours taken from the theme), `capture.rs` (screenshots).
- `crates/gui`: the app. `main.rs` is the window, sidebar and chat. `sessions.rs` tracks sessions, `settings.rs` is the settings page, `themes.rs` loads themes, `update.rs` and `install.rs` are the self-updater.
- `crates/gui/themes/*.json`: the built-in themes, embedded at build time. Custom themes go in `<data dir>/themes/*.json` and can `extends` a built-in one.
- `packaging/windows/mux.iss`: the Inno Setup installer. Never change its `AppId`.
- `.github/workflows/release.yml`: builds, signs and publishes every push to `main` as the Experimental release.

## Commands

- `cargo build`, `cargo run -p mux-gui`, `cargo test`, `cargo clippy --all-targets`, `cargo fmt`
- `cargo run -p mux-ui --example gallery` shows every widget
- Screenshots: set `MUX_SCREENSHOT=target/shots/<name>.png` (and optionally `MUX_SCREENSHOT_SIZE=WxH`, `MUX_SCREENSHOT_THEME`). The app settles its animations, saves a PNG and quits. Keep shots under `target/`.

## Working rules

- Commit straight to `main`. Never push without asking first, because every push publishes a release.
- Commit messages are one-line conventional commits, lowercase and imperative, with no body and no attribution.
- Make the smallest correct change. Comments are rare, one line, and say why.
- No semicolons in prose (comments, docs, commits, replies).
- Use `pnpm` if anything JavaScript ever comes up.
- After any UI change, take a screenshot and look at it before calling the work done.

## Testing against a sandbox

Never touch the real server or state while testing. Point a separate server and app at a sandbox:

```sh
export MUX_SOCKET=/tmp/mux-probe/sandbox/mux.sock MUX_DATA_DIR=/tmp/mux-probe/sandbox
```

Copy `state.json` in if you need real sessions. Stop the sandbox server by its exact PID (check `/proc/<pid>/environ` for the sandbox variables) and never with `pkill -f`, since that has killed the working shell before. Sandbox agents are real agents, so don't type into them anything you wouldn't want run. Any temporary input injection added to `main.rs` for testing must be reverted before committing.

## Things that bite

- The app holds outgoing messages until it knows the server's build. It restarts a stale server only for release builds, only when nothing is running, and at most once a run. Dev builds show a "Restart server" row instead.
- egui starts a new text selection on any mouse button, right included, and a hover-sense widget over text steals the hover and clears the selection. `context_menu::area_menu` and `SelectionWatch` work around both, so reuse them for menus over selectable text.
- `Id::new` is deprecated in this egui, so use `Id::unique`.
- Claude Code reports a stopped turn as `error_during_execution` with an `aborted_*` reason, which isn't a real error. Its thinking blocks arrive empty. Messages sent mid-turn are delivered after the next tool result, for Claude and Codex. OpenCode waits for the turn to end.
- `claude auth status` and `codex login status` exit 1 when signed out, and Settings shows that as signed out rather than missing.
- On Windows the CLIs can be `.exe`, `.cmd` or `.bat`, and PATH is merged from the registry at startup. The renderer is DX12 with a composition swapchain for transparency, and a marker file falls back to the standard renderer if the last run never drew (`MUX_RENDERER=standard` forces it). The system draws corners and borders from build 22000, and Acrylic needs 22621.
- Releases are signed with Ed25519 in CI (the `MUX_SIGNING_KEY` secret). The public key is `PUBLIC_KEY` in `install.rs`. The private key isn't on this machine, so never try to recreate or print it.

## What's next

- A scripted test mode (`MUX_SCRIPT`) that drives the app through a list of actions and captures screenshots, probably as an egui plugin using `input_hook` and `output_hook`. It was asked for and not started.
- A server stress test: many clients and sessions at once, clients dropping mid-stream, and big outputs.
- Things still to check on real Windows and macOS: dark Acrylic, the tray, the installer, the updater swap, the renderer fallback, the PATH merge, sign-in detection, and a light strip at the top left of the window on Windows.
- Ideas offered but not taken up yet: Segoe UI on Windows, speech to text.
