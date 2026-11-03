# Vendored simdnbt

Source: `simdnbt` 0.10.0 from crates.io (<https://github.com/azalea-rs/simdnbt>),
licensed MIT as declared in `Cargo.toml`.

## Changes from upstream

- `src/lib.rs`: removed `compile_error!("simdnbt only supports 64-bit platforms")`.
  Steel's worldgen has to run on `wasm32-unknown-unknown`, a 32-bit target. The
  tape stores pointers in `u64` slots and reads them back with `as *const`, and the
  rest of the crate uses `usize` only for in-memory lengths, so nothing depends on a
  64-bit address space.

Everything else is unmodified. The workspace uses this copy through
`[patch.crates-io]` in the root `Cargo.toml`.
