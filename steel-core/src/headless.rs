//! Terrain generation without a server: one world, no players, no tick loop.
//!
//! The chunks come out of the same generation pyramid the server runs
//! (`Empty` through `Full`), so a fixed seed gives the same terrain, and each
//! one can be read back as the packet payload a client would receive. This is
//! what runs on wasm32, where there are no sockets to talk to a server over.

use std::io;
use std::sync::Arc;

use steel_protocol::packets::game::CLevelChunkWithLight;
use steel_registry::dimension_type::DimensionType;
use steel_utils::serial::WriteTo;
use steel_utils::types::{Difficulty, GameType};
use steel_utils::{BlockPos, BlockStateId, ChunkPos, Identifier, threading};
use tokio::runtime::{Builder as RuntimeBuilder, Runtime};
use toml::map::Map;

use crate::bootstrap::init_globals;
use crate::chunk::chunk_generation_task::StaticCache2D;
use crate::chunk::chunk_holder::ChunkHolder;
use crate::chunk::chunk_map::ChunkMap;
use crate::chunk::chunk_pyramid::GENERATION_PYRAMID;
use crate::chunk::chunk_ticket_manager::ChunkTicketLevel;
use crate::chunk::status::ChunkStatus;
use crate::entity::damage::DamageHistory;
use crate::level_data::{GameTimeSource, WorldGenerationSettings};
use crate::world::{World, WorldConfig, WorldStorageConfig};
use crate::worldgen::WorldGeneratorRegistry;

/// The statuses a chunk passes through on its way to `Full`, in order.
const PIPELINE_STATUSES: [ChunkStatus; 10] = [
    ChunkStatus::Empty,
    ChunkStatus::StructureStarts,
    ChunkStatus::StructureReferences,
    ChunkStatus::Biomes,
    ChunkStatus::Terrain,
    ChunkStatus::Features,
    ChunkStatus::InitializeLight,
    ChunkStatus::Light,
    ChunkStatus::Spawn,
    ChunkStatus::Full,
];

/// A world that generates chunks on demand and never ticks.
pub struct HeadlessWorld {
    runtime: Arc<Runtime>,
    generation_pool: Arc<rayon::ThreadPool>,
    world: Arc<World>,
    dimension: &'static DimensionType,
    // Keeps the damage history the world holds a weak reference to alive.
    _damage_history: Arc<DamageHistory>,
}

impl HeadlessWorld {
    /// Opens `generator` (for example `minecraft:overworld`) with `seed`, using
    /// `threads` generation workers (`None` for rayon's default).
    ///
    /// # Errors
    /// Returns an error if the generator is unknown or the world cannot be built.
    pub fn open(
        generator_key: &Identifier,
        seed: i64,
        threads: Option<usize>,
    ) -> Result<Self, String> {
        init_globals();

        let runtime = Arc::new(
            RuntimeBuilder::new_current_thread()
                .build()
                .map_err(|error| format!("failed to build the chunk runtime: {error}"))?,
        );
        let generation_pool = Arc::new({
            let mut builder = rayon::ThreadPoolBuilder::new()
                .thread_name(|index| format!("headless-gen-{index}"));
            if let Some(threads) = threads {
                builder = builder.num_threads(threads);
            }
            if cfg!(debug_assertions) {
                builder = builder.stack_size(threading::DEBUG_STACK_SIZE);
            }
            threading::build_thread_pool(builder)
                .map_err(|error| format!("failed to build the generation pool: {error}"))?
        });

        let registry = WorldGeneratorRegistry::new_with_builtins()?;
        let config = registry.validate_config(generator_key, &toml::Value::Table(Map::new()))?;
        let output = registry.create(None, &config, seed, Arc::clone(&generation_pool))?;
        let dimension = output.dimension_type;
        let generator = Arc::new(output.generator);
        let generation_settings = WorldGenerationSettings::from_generator_config(
            generator_key.clone(),
            &output.config,
            dimension.key.clone(),
            dimension.min_y,
            dimension.height,
        );
        let damage_history = Arc::new(DamageHistory::default());
        let world_config = WorldConfig {
            damage_history: Arc::clone(&damage_history),
            game_time_source: GameTimeSource::Primary,
            storage: WorldStorageConfig::RamOnly,
            level_data_path: None,
            generator,
            generation_settings,
            view_distance: 10,
            simulation_distance: 10,
            max_chained_neighbor_updates: 1_000_000,
            compression: None,
            is_flat: false,
            sea_level: output.sea_level,
            default_gamemode: GameType::Spectator,
            difficulty: Difficulty::Peaceful,
        };
        let world_key = Identifier::new("headless", generator_key.path.to_string());
        let world = runtime
            .block_on(World::new_with_config(
                Arc::clone(&runtime),
                world_key,
                dimension,
                seed,
                world_config,
                Arc::clone(&generation_pool),
            ))
            .map_err(|error: io::Error| format!("failed to build the world: {error}"))?;

        Ok(Self {
            runtime,
            generation_pool,
            world,
            dimension,
            _damage_history: damage_history,
        })
    }

    /// The lowest block y of the dimension.
    #[must_use]
    pub const fn min_y(&self) -> i32 {
        self.dimension.min_y
    }

    /// The dimension's height in blocks.
    #[must_use]
    pub const fn height(&self) -> i32 {
        self.dimension.height
    }

    /// Whether the dimension has a sky-light layer.
    #[must_use]
    pub const fn has_skylight(&self) -> bool {
        self.dimension.has_skylight
    }

    /// Generates `pos` (and the neighbors the pipeline needs) up to `Full`.
    ///
    /// Blocks until done; call it from a worker, never a UI thread.
    ///
    /// # Errors
    /// Returns an error if a pipeline stage does not complete.
    pub fn generate(&self, pos: ChunkPos) -> Result<(), String> {
        let chunk_map = &self.world.chunk_map;
        let target_step = GENERATION_PYRAMID.get_step_to(ChunkStatus::Full);
        let radius = target_step.get_accumulated_radius_of(ChunkStatus::Empty) as i32;
        let (min_y, height) = (self.dimension.min_y, self.dimension.height);

        let cache = Arc::new(StaticCache2D::create(pos.0.x, pos.0.y, radius, {
            let chunk_map = Arc::clone(chunk_map);
            move |x, z| holder_at(&chunk_map, ChunkPos::new(x, z), min_y, height)
        }));

        for status in PIPELINE_STATUSES {
            let stage_radius = target_step.get_accumulated_radius_of(status) as i32;
            let step = GENERATION_PYRAMID.get_step_to(status);
            let holders: Vec<Arc<ChunkHolder>> = (pos.0.y - stage_radius..=pos.0.y + stage_radius)
                .flat_map(|z| {
                    (pos.0.x - stage_radius..=pos.0.x + stage_radius).map(move |x| (x, z))
                })
                .map(|(x, z)| Arc::clone(cache.get(x, z)))
                .collect();

            // One holder at a time, in a fixed order: a stage such as `Features`
            // writes into its neighbors, which the server's scheduler keeps
            // apart, and running them together would make the result depend on
            // timing.
            let completed = self.runtime.block_on(async {
                for holder in &holders {
                    let Some(task) = holder.apply_step(
                        step,
                        chunk_map,
                        &cache,
                        Arc::clone(&self.generation_pool),
                    ) else {
                        continue;
                    };
                    if task.await.is_none() {
                        return false;
                    }
                }
                true
            });
            if !completed {
                return Err(format!(
                    "chunk stage {status:?} did not complete at {pos:?}"
                ));
            }
        }
        Ok(())
    }

    /// Generates `pos` and returns the body of the level-chunk packet a client
    /// would be sent for it: position, section data, heightmaps and light.
    ///
    /// # Errors
    /// Returns an error if generation fails or the packet cannot be written.
    pub fn chunk_packet(&self, pos: ChunkPos) -> Result<Vec<u8>, String> {
        self.generate(pos)?;
        let holder = self
            .world
            .chunk_map
            .tracked_chunk_holder(pos)
            .ok_or_else(|| format!("no holder for {pos:?} after generation"))?;
        let chunk = holder
            .try_full_chunk()
            .ok_or_else(|| format!("{pos:?} is not a full chunk"))?;
        let packet = CLevelChunkWithLight {
            x: pos.0.x,
            z: pos.0.y,
            chunk_data: chunk.extract_chunk_data(),
            light_data: chunk.extract_light_data(self.dimension.has_skylight),
        };
        let mut out = Vec::new();
        packet
            .write(&mut out)
            .map_err(|error| format!("failed to write the chunk packet: {error}"))?;
        Ok(out)
    }
}

impl HeadlessWorld {
    /// Generates `pos` and returns every block state in it, indexed x fastest,
    /// then z, then y upwards from [`Self::min_y`].
    ///
    /// # Errors
    /// Returns an error if generation fails.
    pub fn block_states(&self, pos: ChunkPos) -> Result<Vec<BlockStateId>, String> {
        self.generate(pos)?;
        let holder = self
            .world
            .chunk_map
            .tracked_chunk_holder(pos)
            .ok_or_else(|| format!("no holder for {pos:?} after generation"))?;
        let chunk = holder
            .try_full_chunk()
            .ok_or_else(|| format!("{pos:?} is not a full chunk"))?;
        let (min_y, height) = (self.dimension.min_y, self.dimension.height);
        let mut states = Vec::with_capacity(16 * 16 * height as usize);
        for y in min_y..min_y + height {
            for z in 0..16 {
                for x in 0..16 {
                    states.push(chunk.get_block_state(BlockPos::new(
                        pos.0.x * 16 + x,
                        y,
                        pos.0.y * 16 + z,
                    )));
                }
            }
        }
        Ok(states)
    }
}

/// The holder the map already tracks for `pos`, or a fresh detached one.
fn holder_at(
    chunk_map: &Arc<ChunkMap>,
    pos: ChunkPos,
    min_y: i32,
    height: i32,
) -> Arc<ChunkHolder> {
    if let Some(holder) = chunk_map.tracked_chunk_holder(pos) {
        return holder;
    }
    let holder = Arc::new(ChunkHolder::new(
        pos,
        ChunkTicketLevel::STRONGEST,
        None,
        min_y,
        height,
    ));
    chunk_map.insert_detached_chunk_holder(pos, Arc::clone(&holder));
    holder
}
