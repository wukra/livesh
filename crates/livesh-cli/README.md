# livesh-cli

A persistent live shell. `livesh` runs your default shell inside a
daemon-owned PTY so you can detach from it, reconnect later, and let other
tools (terminal multiplexers, editors, IDE panes) attach to the same session
by id.

This crate provides three binaries:

| Binary | Purpose |
|----------|---------|
| `livesh` | Create or reattach a live shell and bridge your terminal to it |
| `liveshd` | The session-owning daemon (auto-spawned; not run by hand) |
| `liveshctl` | List, rename, kill, and GC sessions; hot-upgrade the daemon |

## Install

```bash
cargo install livesh-cli
```

## Requirements

- Rust 1.85+ (edition 2024)
- Unix-like OS (uses PTYs via `nix` / `portable-pty`)

See the [workspace README](https://github.com/xiaocang/livesh) for usage,
the cmux integration story, and the full wire contract.

## License

MIT
