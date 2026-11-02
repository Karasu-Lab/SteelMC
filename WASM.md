# Building Steel's terrain generation for wasm32

`steel-core`'s generation runs on `wasm32-unknown-unknown` through
`steel_core::headless::HeadlessWorld`: one world, no players, no tick loop. The
server itself (`Server`, sockets, the run loop) still compiles there but is not
meant to run; online mode and on-disk storage are not available on wasm32.

## What differs on wasm32

- **Storage is RAM only.** `WorldStorageConfig::Disk`, the file player-data
  backend and `SavedDataManager` persistence are compiled out or return
  `Unsupported`.
- **No online services.** `server/service_keys_offline.rs` and
  `player/profile/lookup_offline.rs` replace the HTTP-backed versions, so there
  is no `reqwest` call on wasm32.
- **zstd** goes through `steel_utils::compression`, which uses `ruzstd` on wasm32
  (the C library cannot be built there).
- **Time** uses `web-time`, since `std::time::Instant`/`SystemTime` panic on
  `wasm32-unknown-unknown`.
- **tokio** is limited to `macros`, `rt`, `sync`, `io-util` and `time`; the
  OS-backed features are added per crate for non-wasm targets only. The
  headless world uses a current-thread runtime with no drivers.
- **simdnbt** is `vendor/simdnbt`, a copy without the 64-bit-only guard (see
  `vendor/simdnbt/VENDORED.md`). Workspaces that depend on Steel need
  `[patch.crates-io] simdnbt = { path = "<path to>/vendor/simdnbt" }` in their
  own root manifest.

## Build settings

```sh
RUSTFLAGS='--cfg getrandom_backend="wasm_js" -C target-feature=+atomics,+bulk-memory,+mutable-globals'
cargo build --target wasm32-unknown-unknown -Zbuild-std=std,panic_abort ...
```

`--cfg getrandom_backend="wasm_js"` is needed for every build for this target.
The atomics flags and `-Zbuild-std` are needed for the worker threads below;
`rust-src` must be in the toolchain. The linker also needs shared memory
(`-C link-arg=--shared-memory`, `--import-memory`, `--max-memory=...`), and the
page must be cross-origin isolated (COOP/COEP) for `SharedArrayBuffer`.

## Threads

`rayon` pools are built through `steel_utils::threading::build_thread_pool`.
On wasm32 it starts each worker with the spawner the host installs once, before
the first world is opened:

```rust
steel_utils::threading::install_thread_spawner(|name, task| {
    // Run `task` on a new Web Worker sharing this module's memory.
    start_worker(name, task)
});
```

`thread` supplies the Web Worker side and calls
`pomme_singleplayer::generated::install_thread_spawner` (a re-export). Without a
spawner, opening a world fails with "no thread spawner installed".

Generation blocks (`HeadlessWorld::generate` waits on the pool), so it must run
on a worker, never the browser's main thread.

## Entry points

- `steel_core::headless::HeadlessWorld`: open a generator with a seed, then
  `block_states(pos)` or `chunk_packet(pos)`.
- `pomme_singleplayer::generated::GeneratedWorld`: the overworld, with block
  state descriptions and the chunk packet body that
  `pomme-client::world::generated::decode` reads.
- `worldgen::SteelChunkGenerator`: `worldgen::ChunkGenerator` over the
  above.

Generation is deterministic for a seed: `HeadlessWorld` runs each stage one
chunk at a time in a fixed order (see `steel-core/tests/headless.rs`).
