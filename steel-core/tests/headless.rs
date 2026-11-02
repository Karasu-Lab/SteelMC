//! A fixed seed generates the same terrain every time, without a server.

use std::thread;

use steel_core::headless::HeadlessWorld;
use steel_utils::threading::DEBUG_STACK_SIZE;
use steel_utils::{ChunkPos, Identifier};

fn packet(seed: i64, pos: ChunkPos) -> Vec<u8> {
    // Debug builds have deep density-function call chains, as the server's own
    // worker threads allow for.
    thread::Builder::new()
        .stack_size(DEBUG_STACK_SIZE)
        .spawn(move || {
            let world =
                HeadlessWorld::open(&Identifier::vanilla_static("overworld"), seed, Some(2))
                    .expect("the overworld should open");
            assert_eq!(world.min_y(), -64);
            assert_eq!(world.height(), 384);
            world.chunk_packet(pos).expect("the chunk should generate")
        })
        .expect("the generation thread should start")
        .join()
        .expect("generation should not panic")
}

#[test]
fn same_seed_gives_the_same_chunk_and_other_seeds_differ() {
    let pos = ChunkPos::new(0, 0);
    let first = packet(8_675_309, pos);
    assert!(!first.is_empty());
    let second = packet(8_675_309, pos);
    let diff = first.iter().zip(&second).position(|(a, b)| a != b);
    assert_eq!(
        (first.len(), diff),
        (second.len(), None),
        "packets differ (lengths {} and {}, first differing byte {diff:?})",
        first.len(),
        second.len()
    );
    assert_ne!(first, packet(1, pos));
}
