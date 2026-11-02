//! Whole-buffer zstd for Steel's persistence formats.
//!
//! Native builds use the C library. wasm32 cannot build it, so it uses the pure
//! Rust `ruzstd`; both read and write standard zstd frames.

use std::io;

/// Compresses `data` in one frame at roughly zstd level `level`.
#[cfg(not(target_arch = "wasm32"))]
pub fn encode_all(data: &[u8], level: i32) -> io::Result<Vec<u8>> {
    zstd::encode_all(data, level)
}

/// Compresses `data` in one frame at roughly zstd level `level`.
#[cfg(target_arch = "wasm32")]
pub fn encode_all(data: &[u8], level: i32) -> io::Result<Vec<u8>> {
    use ruzstd::encoding::{CompressionLevel, compress_to_vec};

    let level = match level {
        i32::MIN..=0 => CompressionLevel::Uncompressed,
        1..=2 => CompressionLevel::Fastest,
        3..=4 => CompressionLevel::Default,
        5..=9 => CompressionLevel::Better,
        _ => CompressionLevel::Best,
    };
    Ok(compress_to_vec(data, level))
}

/// Decompresses every frame in `data`.
#[cfg(not(target_arch = "wasm32"))]
pub fn decode_all(data: &[u8]) -> io::Result<Vec<u8>> {
    zstd::decode_all(data)
}

/// Decompresses every frame in `data`.
#[cfg(target_arch = "wasm32")]
pub fn decode_all(data: &[u8]) -> io::Result<Vec<u8>> {
    use std::io::Read;

    let mut out = Vec::new();
    let mut decoder = ruzstd::decoding::StreamingDecoder::new(data)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    decoder.read_to_end(&mut out)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{decode_all, encode_all};

    #[test]
    fn round_trips() {
        let data: Vec<u8> = (0..4096u32).flat_map(|i| (i % 37).to_le_bytes()).collect();
        for level in [1, 3, 9] {
            let packed = encode_all(&data, level).expect("encode");
            assert_eq!(decode_all(&packed).expect("decode"), data);
        }
    }
}
