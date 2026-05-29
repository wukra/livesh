# livesh-core

Core session model for [livesh](https://github.com/xiaocang/livesh): session
metadata, on-disk paths, garbage collection of dead sessions, the terminal
model, and scrollback / snapshot / event-log size limits.

This is the shared library behind the `livesh` / `liveshd` / `liveshctl`
binaries (see `livesh-cli`). It builds on the wire types in `livesh-protocol`.

See the [workspace README](https://github.com/xiaocang/livesh) for the full
picture.

## License

MIT
