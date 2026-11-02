//! NBT read/write round trips. Run on wasm32 (a 32-bit target) with
//! `cargo test --target wasm32-unknown-unknown` and the wasm-bindgen test runner.

use std::io::{Cursor, Read};

use flate2::bufread::GzDecoder;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen_test::wasm_bindgen_test;

const FIXTURES: [(&[u8], bool); 5] = [
    (include_bytes!("hello_world.nbt"), false),
    (include_bytes!("hypixel.nbt"), false),
    (include_bytes!("bigtest.nbt"), true),
    (include_bytes!("complex_player.dat"), true),
    (include_bytes!("level.dat"), true),
];

fn decoded(src: &[u8], gzipped: bool) -> Vec<u8> {
    if !gzipped {
        return src.to_vec();
    }
    let mut out = Vec::new();
    GzDecoder::new(src)
        .read_to_end(&mut out)
        .expect("gzip fixture");
    out
}

#[cfg_attr(not(target_arch = "wasm32"), test)]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
fn borrowed_reader_agrees_with_the_owned_reader() {
    for (src, gzipped) in FIXTURES {
        let bytes = decoded(src, gzipped);
        let borrowed = simdnbt::borrow::read(&mut Cursor::new(bytes.as_slice()))
            .expect("borrow read")
            .unwrap()
            .as_compound()
            .to_owned();
        let owned = simdnbt::owned::read(&mut Cursor::new(bytes.as_slice()))
            .expect("owned read")
            .unwrap()
            .as_compound();
        assert_eq!(borrowed, owned);
    }
}

#[cfg_attr(not(target_arch = "wasm32"), test)]
#[cfg_attr(target_arch = "wasm32", wasm_bindgen_test)]
fn written_nbt_reads_back_equal() {
    for (src, gzipped) in FIXTURES {
        let bytes = decoded(src, gzipped);
        let first = simdnbt::owned::read(&mut Cursor::new(bytes.as_slice()))
            .expect("owned read")
            .unwrap();
        let mut written = Vec::new();
        first.write(&mut written);
        let second = simdnbt::owned::read(&mut Cursor::new(written.as_slice()))
            .expect("re-read")
            .unwrap();
        assert_eq!(first, second);
    }
}
