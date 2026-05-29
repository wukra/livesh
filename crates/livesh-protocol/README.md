# livesh-protocol

Wire protocol types shared between the [livesh](https://github.com/xiaocang/livesh)
client and daemon.

This crate defines the serializable message and state types exchanged over the
livesh control socket. It carries no runtime logic — it is the contract that
`livesh-core` and the `livesh` / `liveshd` / `liveshctl` binaries build on.

See the [workspace README](https://github.com/xiaocang/livesh) for the full
picture.

## License

MIT
