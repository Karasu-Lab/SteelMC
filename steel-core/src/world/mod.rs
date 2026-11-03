
use std::{
    io, mem,
    path::Path,
    sync::{
        Arc, LazyLock, Weak,
        atomic::{AtomicBool, AtomicI8, Ordering},
    },
    time::Duration,
};
use web_time::Instant;

use crate::chunk::chunk_ticket_storage::ChunkTicketStorage;
use crate::chunk::full_chunk::{FullChunkBlockSetResult, FullChunkRef};
use crate::chunk::gameplay_chunk_lookup_cache::GameplayChunkLookupCacheScope;
use crate::chunk::light::{
    LightLayer, LightSectionEmptinessChange, MAX_LIGHT_LEVEL, has_different_light_properties,
};
use crate::chunk::status::ChunkStatus;
use crate::entity::damage::DamageHistory;
use crate::poi::OccupationStatus;
use crate::portal::WorldChangeRequest;
use crate::world::game_event::{
    GameEventContext, GameEventDispatcher, GameEventListenerCount, GameEventListenerStorage,
    SharedGameEventListener,
};
use crate::{chunk::chunk_map::ChunkMapGameTickTimings, world::weather::Weather};
use steel_utils::saved_data::{SavedDataManager, names as saved_data_names};

use glam::DVec3;
use sha2::{Digest, Sha256};
use steel_protocol::packets::game::{
    CBlockDestruction, CChangeDifficulty, CGameEvent, CInitializeBorder, CLevelEvent,
    CLevelParticles, CPlayerChat, CSetBorderCenter, CSetBorderLerpSize, CSetBorderSize,
    CSetBorderWarningDelay, CSetBorderWarningDistance, CSetEntityData, CSetEntityLink,
    CSetEquipment, CSound, CSystemChat, CUpdateAttributes, GameEventType, RandomizationType,
    SoundSource,
};
use steel_protocol::utils::ConnectionProtocol;
use steel_protocol::{
    packet_traits::{ClientPacket, CompressionInfo, EncodedPacket},
    packets::game::CSetTime,
};

use rustc_hash::FxHashSet;
use simdnbt::owned::NbtCompound;
use steel_registry::biome::{BiomeRef, TemperatureModifier};
use steel_registry::blocks::block_state_ext::BlockStateExt;
use steel_registry::blocks::properties::{Axis, BlockStateProperties, Direction};
use steel_registry::blocks::shapes::{
    BooleanOp, OffsetVoxelShape, SupportType, VoxelShape, is_offset_face_full, is_shape_full_block,
    join_is_not_empty,
};
use steel_registry::fluid::{FluidRef, FluidState};
use steel_registry::game_events::GameEventRef;
use steel_registry::game_rules::{ErasedGameRuleRef, GameRule, GameRuleValue, GameRuleValueType};
use steel_registry::item_stack::ItemStack;
use steel_registry::level_events;
use steel_registry::loot_table::LootContext;
use steel_registry::particle_type::ParticleData;
use steel_registry::sound_event::SoundEventRef;
use steel_registry::vanilla_block_tags::BlockTag;
use steel_registry::vanilla_game_rules::{
    BLOCK_DROPS, GLOBAL_SOUND_EVENTS, PLAYERS_NETHER_PORTAL_DEFAULT_DELAY, RANDOM_TICK_SPEED,
};
use steel_registry::{REGISTRY, RegistryEntry, RegistryExt, dimension_type::DimensionTypeRef};
use steel_registry::{block_entity_type::BlockEntityTypeRef, vanilla_dimension_types};
use steel_registry::{
    blocks::BlockRef, vanilla_game_rules::ADVANCE_TIME, vanilla_game_rules::ADVANCE_WEATHER,
};
use steel_registry::{vanilla_blocks, vanilla_entities, vanilla_game_events, vanilla_poi_types};
use steel_utils::block_util::FoundRectangle;
use steel_utils::{
    Downcast as _,
    locks::{SyncMutex, SyncRwLock},
    random::{Random as _, RandomSource, legacy_random::LegacyRandom},
};
use steel_worldgen::{biomes::obfuscate_biome_seed, noise::PerlinSimplexNoise};

use steel_utils::{
    BlockLocalAabb, BlockPos, BlockStateId, ChunkPos, Identifier, PackedBlockPos, SectionPos,
    WorldAabb,
    types::{Difficulty, GameType, UpdateFlags},
};
use tokio::runtime::Runtime;

#[cfg(not(target_arch = "wasm32"))]
use crate::chunk_saver::RegionManager;
use crate::{
    ChunkMap,
    behavior::{BLOCK_BEHAVIORS, BlockCollisionContext, BlockLootContext, FLUID_BEHAVIORS},
    block_entity::{BlockEntity, SharedBlockEntity, entities::EndGatewayBlockEntity},
    chunk::{heightmap::HeightmapType, player_chunk_view::PlayerChunkView},
    chunk_saver::{ChunkStorage, RamOnlyStorage},
    entity::{
        AddEntityError, Entity, EntityChangeSenders, EntityChunkCallback, EntityLifecycleChanges,
        EntityMovementSyncPacket, EntityOwnership, EntityTracker, EntityVisibility,
        InactiveEntityCallback, MobEffectSyncPacket, RemovalReason, SharedEntity,
        WorldEntityManager,
        entities::{ExperienceOrbEntity, ItemEntity},
        entity_loot_ref,
    },
    fluid::{FluidStateExt as _, fluid_state_to_block},
    level_data::{
        GameTime, GameTimeSource, LevelDataManager, RespawnData, WorldGenerationSettings,
    },
    player::{LastSeen, Player, connection::NetworkConnection},
    poi::PointOfInterestStorage,
};

mod block_entity_ticker;
mod block_event;
mod block_region;
mod block_updates;
mod border;
mod broadcasts;
pub(crate) mod clock;
mod domain_entity_directory;
mod entity_management;
mod environment;
mod events;
pub mod explosion;
pub mod game_event;
mod level_effects;
mod level_reader;
mod player_index;
pub(crate) mod player_spawn_finder;
mod portals;
mod properties;
mod raycast;
mod redstone;
mod signal_getter;
mod sleep;
mod sleep_status;
mod spawn;
pub mod tick_scheduler;
mod weather;
mod world_entities;

#[cfg(test)]
mod tests;

pub use crate::config::WorldStorageConfig;
use crate::worldgen::generators::vanilla::fuzzed_biome_at_block;
use crate::worldgen::{ChunkGenerator, ChunkGeneratorType};
use block_event::BlockEventQueue;
pub(crate) use block_region::{BlockRegionBounds, BlockRegionRead, MAX_BLOCK_REGION_WORKSET_SLOTS};
use block_updates::CollectingNeighborUpdater;
pub use border::WorldBorderError;
pub(crate) use border::{MAX_CENTER_COORDINATE, MAX_SIZE};
use border::{WorldBorder, WorldBorderSnapshot};
pub(crate) use domain_entity_directory::DomainEntityDirectory;
use entity_management::NavigatingMobTracker;
#[cfg(test)]
use entity_management::nearest_player_distance_in_range;
pub use explosion::{
    BlockInteraction, DefaultExplosionDamageCalculator, EntityBasedExplosionDamageCalculator,
    Explosion, ExplosionDamageCalculator, ExplosionInteraction, ExplosionOptions, ExplosionOutcome,
};
pub(crate) use explosion::{ExplosionBlockReader, ImmutableExplosionBlockCalculator};
pub use level_reader::{LevelAccessor, LevelReader, ScheduledTickAccess};
pub use player_index::{PlayerAreaMap, PlayerMap};
pub use raycast::{ClipBlockShape, ClipFluid, ClipHitResult, RaytraceAction};
#[cfg(test)]
pub(crate) use signal_getter::get_best_neighbor_signal;
pub use signal_getter::{SignalGetter, SignalQueryContext};
pub(crate) use signal_getter::{get_control_input_signal, get_signal, is_redstone_conductor};
pub use tick_scheduler::ScheduledTick;

#[cfg(test)]
use level_effects::sound_is_within_range;
#[cfg(test)]
use portals::{
    closest_portal_candidate, nether_portal_creation_scan_origin, nether_portal_frame_offset_pos,
};

const fn initialize_border_packet(snapshot: WorldBorderSnapshot) -> CInitializeBorder {
    CInitializeBorder {
        new_center_x: snapshot.center_x,
        new_center_z: snapshot.center_z,
        old_size: snapshot.old_size,
        new_size: snapshot.new_size,
        lerp_time: snapshot.lerp_time,
        new_absolute_max_size: snapshot.absolute_max_size,
        warning_blocks: snapshot.warning_blocks,
        warning_time: snapshot.warning_time,
    }
}

#[derive(Debug)]
pub struct WorldGameTickTimings {
    pub elapsed: Duration,
    pub chunk_map: ChunkMapGameTickTimings,
    pub entity_tick: Duration,
}

#[must_use]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConditionalBlockSetResult {
    Changed,
    Unchanged,
    Stale(BlockStateId),
    Unavailable,
}

#[derive(Clone)]
pub struct WorldConfig {
    pub damage_history: Arc<DamageHistory>,
    pub game_time_source: GameTimeSource,
    pub storage: WorldStorageConfig,
    pub level_data_path: Option<String>,
    pub generator: Arc<ChunkGeneratorType>,
    pub generation_settings: WorldGenerationSettings,
    pub view_distance: u8,
    pub simulation_distance: u8,
    pub max_chained_neighbor_updates: i32,
    pub compression: Option<CompressionInfo>,
    pub is_flat: bool,
    pub sea_level: i32,
    pub default_gamemode: GameType,
    pub difficulty: Difficulty,
}

pub struct World {
    pub(crate) damage_history: Weak<DamageHistory>,
    pub chunk_map: Arc<ChunkMap>,
    pub players: PlayerMap,
    pub player_area_map: PlayerAreaMap,
    pub key: Identifier,
    pub dimension_type: DimensionTypeRef,
    pub level_data: SyncRwLock<LevelDataManager>,
    pub(crate) game_time: Arc<GameTime>,
    pub(crate) saved_data: SavedDataManager,
    world_border: SyncMutex<WorldBorder>,
    sleep_status: SyncMutex<sleep_status::SleepStatus>,
    pub view_distance: u8,
    pub simulation_distance: u8,
    pub compression: Option<CompressionInfo>,
    pub is_flat: bool,
    pub sea_level: i32,
    default_gamemode: AtomicI8,
    tick_runs_normally: AtomicBool,
    handling_tick: AtomicBool,
    block_events: SyncMutex<BlockEventQueue>,
    neighbor_updater: CollectingNeighborUpdater,
    entity_manager: WorldEntityManager,
    domain_entity_directory: SyncRwLock<Option<Arc<DomainEntityDirectory>>>,
    block_entity_tickers: block_entity_ticker::WorldBlockEntityTickers,
    game_event_listener_count: Arc<GameEventListenerCount>,
    entity_tracker: EntityTracker,
    navigating_mobs: NavigatingMobTracker,
    pub weather: SyncMutex<Weather>,
    redstone_torch_toggles: SyncMutex<redstone::RedstoneTorchToggleTracker>,
    scheduled_ticks: tick_scheduler::WorldTickScheduler,
    scheduled_block_ticks_this_tick:
        SyncMutex<Option<Arc<tick_scheduler::ScheduledTickRunBatch<BlockRef>>>>,
    scheduled_fluid_ticks_this_tick:
        SyncMutex<Option<Arc<tick_scheduler::ScheduledTickRunBatch<FluidRef>>>>,
    pub poi_storage: SyncMutex<PointOfInterestStorage>,
    pending_world_changes: SyncMutex<Vec<(SharedEntity, WorldChangeRequest)>>,
}

impl World {
    pub async fn new_with_config(
        chunk_runtime: Arc<Runtime>,
        key: Identifier,
        dimension_type: DimensionTypeRef,
        seed: i64,
        config: WorldConfig,
        generation_pool: Arc<rayon::ThreadPool>,
    ) -> io::Result<Arc<Self>> {
        let chunk_encoding_pool = Arc::clone(&generation_pool);
        Self::new_with_config_and_encoding_pool(
            chunk_runtime,
            key,
            dimension_type,
            seed,
            config,
            generation_pool,
            chunk_encoding_pool,
        )
        .await
    }

    #[expect(
        clippy::too_many_lines,
        reason = "world construction wiring reads better kept in one place"
    )]
    pub(crate) async fn new_with_config_and_encoding_pool(
        chunk_runtime: Arc<Runtime>,
        key: Identifier,
        dimension_type: DimensionTypeRef,
        seed: i64,
        config: WorldConfig,
        generation_pool: Arc<rayon::ThreadPool>,
        chunk_encoding_pool: Arc<rayon::ThreadPool>,
    ) -> io::Result<Arc<Self>> {
        let view_distance = config.view_distance;
        let simulation_distance = config.simulation_distance;
        let max_chained_neighbor_updates = config.max_chained_neighbor_updates;
        let compression = config.compression;
        let is_flat = config.is_flat;
        let sea_level = config.sea_level;
        let default_gamemode = config.default_gamemode;
        let storage: Arc<ChunkStorage> = match &config.storage {
            #[cfg(not(target_arch = "wasm32"))]
            WorldStorageConfig::Disk { path } => {
                Arc::new(ChunkStorage::Disk(RegionManager::new(path.clone())))
            }
            #[cfg(target_arch = "wasm32")]
            WorldStorageConfig::Disk { .. } => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "disk world storage is unavailable on wasm32",
                ));
            }
            WorldStorageConfig::RamOnly => {
                Arc::new(ChunkStorage::RamOnly(RamOnlyStorage::empty_world()))
            }
        };


        let path = config.level_data_path.as_deref().map(Path::new);
        let saved_data = SavedDataManager::new(path);
        let mut level_data = LevelDataManager::new(
            path,
            seed,
            config.difficulty,
            config.generation_settings,
            config.game_time_source,
        )
        .await?;
        if level_data.is_dirty() {
            level_data.save().await?;
        }
        let ticket_storage = ChunkTicketStorage::load(&saved_data, &key).await;
        let world_border = WorldBorder::new(level_data.data().world_border)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;

        let mut weather = Weather::default();
        if level_data.is_raining() {
            weather.rain_level = 1.0;
            if level_data.is_thundering() {
                weather.thunder_level = 1.0;
            }
        }

        Ok(Arc::new_cyclic(|weak_self: &Weak<World>| {
            let chunk_map = Arc::new(ChunkMap::new_with_storage_and_ticket_storage(
                chunk_runtime,
                Weak::clone(weak_self),
                dimension_type,
                sea_level,
                storage,
                config.generator,
                generation_pool,
                chunk_encoding_pool,
                view_distance,
                simulation_distance,
                ticket_storage,
            ));
            chunk_map.start_generation_refill_loop();

            Self {
                damage_history: Arc::downgrade(&config.damage_history),
                chunk_map,
                players: PlayerMap::new(),
                player_area_map: PlayerAreaMap::new(),
                key,
                dimension_type,
                game_time: level_data.game_time_handle(),
                level_data: SyncRwLock::new(level_data),
                saved_data,
                world_border: SyncMutex::new(world_border),
                sleep_status: SyncMutex::new(sleep_status::SleepStatus::default()),
                view_distance,
                simulation_distance,
                compression,
                is_flat,
                sea_level,
                default_gamemode: AtomicI8::new(default_gamemode.into()),
                tick_runs_normally: AtomicBool::new(true),
                handling_tick: AtomicBool::new(false),
                block_events: SyncMutex::new(BlockEventQueue::default()),
                neighbor_updater: CollectingNeighborUpdater::new(max_chained_neighbor_updates),
                entity_manager: WorldEntityManager::new(),
                domain_entity_directory: SyncRwLock::new(None),
                block_entity_tickers: block_entity_ticker::WorldBlockEntityTickers::new(),
                game_event_listener_count: GameEventListenerCount::shared(),
                entity_tracker: EntityTracker::new(),
                navigating_mobs: NavigatingMobTracker::new(),
                weather: SyncMutex::new(weather),
                redstone_torch_toggles: SyncMutex::new(
                    redstone::RedstoneTorchToggleTracker::default(),
                ),
                scheduled_ticks: tick_scheduler::WorldTickScheduler::new(),
                scheduled_block_ticks_this_tick: SyncMutex::new(None),
                scheduled_fluid_ticks_this_tick: SyncMutex::new(None),
                poi_storage: SyncMutex::new(PointOfInterestStorage::new()),
                pending_world_changes: SyncMutex::new(Vec::new()),
            }
        }))
    }

    #[expect(
        clippy::await_holding_lock,
        reason = "holding the write lock across await is safe here because it only happens during shutdown"
    )]
    pub async fn cleanup(&self, total_saved: &mut usize) {
        self.sync_world_border_to_level_data();
        match self.level_data.write().save().await {
            Ok(()) => log::info!("World {} level data saved successfully", self.key),
            Err(e) => log::error!("Failed to save world level data: {e}"),
        }

        let chunk_tickets = self.chunk_map.persistent_chunk_tickets();
        match self
            .saved_data
            .save(saved_data_names::CHUNK_TICKETS, &chunk_tickets)
            .await
        {
            Ok(()) => log::info!("World {} saved chunk ticket data successfully", self.key),
            Err(e) => log::error!("Failed to save world chunk ticket data: {e}"),
        }

        match self.save_all_chunks().await {
            Ok(count) => *total_saved += count,
            Err(e) => log::error!("Failed to save world chunks: {e}"),
        }
    }

    #[must_use]
    pub fn domain(&self) -> &str {
        self.key.namespace.as_ref()
    }

    pub(crate) fn set_domain_entity_directory(&self, directory: Arc<DomainEntityDirectory>) {
        *self.domain_entity_directory.write() = Some(directory);
    }

    #[must_use]
    pub fn get_entity_in_domain_by_uuid(&self, uuid: &uuid::Uuid) -> Option<SharedEntity> {
        self.get_entity_by_uuid(uuid).or_else(|| {
            self.domain_entity_directory
                .read()
                .as_ref()
                .and_then(|directory| directory.get_entity_by_uuid(uuid))
        })
    }

    #[tracing::instrument(level = "trace", skip(self), name = "world_game_tick")]
    #[expect(
        clippy::too_many_lines,
        reason = "world tick orchestration keeps vanilla subsystem order explicit"
    )]
    pub fn tick_game(
        self: &Arc<Self>,
        tick_count: u64,
        runs_normally: bool,
    ) -> WorldGameTickTimings {
        let world_start = Instant::now();
        let lookup_cache_scope = GameplayChunkLookupCacheScope::enter(&self.chunk_map);
        self.handling_tick.store(true, Ordering::Relaxed);
        self.set_tick_runs_normally(runs_normally);
        if runs_normally {
            self.tick_world_border();
            self.tick_weather();
        }
        self.tick_sleeping_players();
        if runs_normally {
            self.tick_time();
        }

        let random_tick_speed = self.get_game_rule(&RANDOM_TICK_SPEED) as u32;

        let early_lookup_stats = lookup_cache_scope.finish();
        let mut chunk_map_timings =
            self.chunk_map
                .tick_game(self, tick_count, random_tick_speed, runs_normally);
        chunk_map_timings.lookup_cache.merge(early_lookup_stats);
        let lookup_cache_scope = GameplayChunkLookupCacheScope::enter(&self.chunk_map);

        if runs_normally {
            let _span = tracing::trace_span!("block_events").entered();
            self.run_block_events();
        }

        self.handling_tick.store(false, Ordering::Relaxed);

        let entity_tick = {
            let _span = tracing::trace_span!("entity_tick").entered();
            let start = Instant::now();
            let dirty_chunks = self
                .entity_manager
                .tick_entities(tick_count as i32, runs_normally);
            for chunk in dirty_chunks {
                self.mark_chunk_dirty(chunk);
            }
            start.elapsed()
        };

        {
            let _span = tracing::trace_span!("block_entities").entered();
            let start = Instant::now();
            self.block_entity_tickers.tick(self, runs_normally);
            chunk_map_timings.tick_block_entities = start.elapsed();
        }

        {
            let _span = tracing::trace_span!("entity_tracker_send_changes").entered();
            self.entity_tracker.send_changes(
                |chunk| self.get_packet_tracking_players(chunk),
                |player_id| self.players.get_by_entity_id(player_id),
                EntityChangeSenders {
                    movement: |entity_id, packet| {
                        self.broadcast_movement_sync_to_entity_trackers(entity_id, packet, None);
                    },
                    self_movement: |player_id, packet| {
                        let Some(encoded) = self.encode_movement_sync_packet(packet) else {
                            return;
                        };
                        let Some(player) = self.players.get_by_entity_id(player_id) else {
                            return;
                        };
                        player.connection.send_encoded(encoded);
                    },
                    entity_data: |entity_id, dirty_entity_data| {
                        let packet = CSetEntityData::new(entity_id, dirty_entity_data);
                        let Ok(encoded) = EncodedPacket::from_bare(
                            packet,
                            self.compression,
                            ConnectionProtocol::Play,
                        ) else {
                            return;
                        };
                        self.broadcast_to_entity_trackers_encoded(entity_id, encoded.clone(), None);
                        if let Some(player) = self.players.get_by_entity_id(entity_id) {
                            player.connection.send_encoded(encoded);
                        }
                    },
                    attributes: |entity_id, dirty_attributes| {
                        let packet = CUpdateAttributes::new(entity_id, dirty_attributes);
                        let Ok(encoded) = EncodedPacket::from_bare(
                            packet,
                            self.compression,
                            ConnectionProtocol::Play,
                        ) else {
                            return;
                        };
                        self.broadcast_to_entity_trackers_encoded(entity_id, encoded.clone(), None);
                        if let Some(player) = self.players.get_by_entity_id(entity_id) {
                            player.connection.send_encoded(encoded);
                        }
                    },
                    mob_effects: |player_id, packet| {
                        let Some(player) = self.players.get_by_entity_id(player_id) else {
                            return;
                        };
                        match packet {
                            MobEffectSyncPacket::Update(packet) => player.send_packet(packet),
                            MobEffectSyncPacket::Remove(packet) => player.send_packet(packet),
                        }
                    },
                    equipment: |entity_id, packet: CSetEquipment| {
                        let Ok(encoded) = EncodedPacket::from_bare(
                            packet,
                            self.compression,
                            ConnectionProtocol::Play,
                        ) else {
                            return;
                        };
                        self.broadcast_to_entity_trackers_encoded(entity_id, encoded, None);
                    },
                    passengers: |player_id, packet| {
                        if let Some(player) = self.players.get_by_entity_id(player_id) {
                            player.send_packet(packet);
                        }
                    },
                    entity_link: |entity_id, packet: CSetEntityLink| {
                        let Ok(encoded) = EncodedPacket::from_bare(
                            packet,
                            self.compression,
                            ConnectionProtocol::Play,
                        ) else {
                            return;
                        };
                        self.broadcast_to_entity_trackers_encoded(entity_id, encoded, None);
                    },
                },
            );
        }

        chunk_map_timings
            .lookup_cache
            .merge(lookup_cache_scope.finish());
        WorldGameTickTimings {
            elapsed: world_start.elapsed(),
            chunk_map: chunk_map_timings,
            entity_tick,
        }
    }

    pub async fn save_all_chunks(&self) -> io::Result<usize> {
        self.chunk_map.save_all_chunks().await
    }
}

impl LevelReader for World {
    fn get_block_state(&self, pos: BlockPos) -> BlockStateId {
        Self::get_block_state(self, pos)
    }

    fn get_block_entity(&self, pos: BlockPos) -> Option<SharedBlockEntity> {
        Self::get_block_entity(self, pos)
    }

    fn is_face_sturdy_for(
        &self,
        state: BlockStateId,
        pos: BlockPos,
        direction: Direction,
        support_type: SupportType,
    ) -> bool {
        BLOCK_BEHAVIORS
            .get_behavior(state.get_block())
            .is_face_sturdy(state, self, pos, direction, support_type)
    }

    fn sky_darkening(&self) -> u8 {
        World::sky_darkening(self)
    }

    fn raw_brightness(&self, pos: BlockPos, sky_darkening: u8) -> u8 {
        let sky_light = if self.dimension_type.has_skylight {
            self.light_value_at(LightLayer::Sky, pos)
                .saturating_sub(sky_darkening)
        } else {
            0
        };

        if sky_light == MAX_LIGHT_LEVEL {
            return MAX_LIGHT_LEVEL;
        }

        sky_light.max(self.light_value_at(LightLayer::Block, pos))
    }

    fn can_see_sky(&self, pos: BlockPos) -> bool {
        Self::can_see_sky(self, pos)
    }

    fn ambient_light(&self) -> f32 {
        self.dimension_type.ambient_light
    }

    fn min_y(&self) -> i32 {
        self.get_min_y()
    }

    fn height(&self) -> i32 {
        self.get_height()
    }

    fn sea_level(&self) -> i32 {
        self.sea_level
    }
}

impl LevelReader for Arc<World> {
    fn get_block_state(&self, pos: BlockPos) -> BlockStateId {
        self.as_ref().get_block_state(pos)
    }

    fn get_block_entity(&self, pos: BlockPos) -> Option<SharedBlockEntity> {
        self.as_ref().get_block_entity(pos)
    }

    fn is_face_sturdy_for(
        &self,
        state: BlockStateId,
        pos: BlockPos,
        direction: Direction,
        support_type: SupportType,
    ) -> bool {
        self.as_ref()
            .is_face_sturdy_for(state, pos, direction, support_type)
    }

    fn sky_darkening(&self) -> u8 {
        self.as_ref().sky_darkening()
    }

    fn raw_brightness(&self, pos: BlockPos, sky_darkening: u8) -> u8 {
        self.as_ref().raw_brightness(pos, sky_darkening)
    }

    fn can_see_sky(&self, pos: BlockPos) -> bool {
        self.as_ref().can_see_sky(pos)
    }

    fn ambient_light(&self) -> f32 {
        self.as_ref().ambient_light()
    }

    fn height_at(&self, heightmap_type: HeightmapType, x: i32, z: i32) -> i32 {
        let mapped_type = match heightmap_type {
            HeightmapType::WorldSurfaceWg => HeightmapType::WorldSurface,
            HeightmapType::OceanFloorWg => HeightmapType::OceanFloor,
            other => other,
        };
        self.as_ref()
            .height_at(mapped_type, x, z)
            .unwrap_or_else(|| self.min_y())
    }

    fn min_y(&self) -> i32 {
        self.as_ref().get_min_y()
    }

    fn height(&self) -> i32 {
        self.as_ref().get_height()
    }

    fn sea_level(&self) -> i32 {
        self.as_ref().sea_level
    }
}

impl ScheduledTickAccess for Arc<World> {
    fn fluid_tick_delay(&self, fluid: FluidRef) -> i32 {
        FLUID_BEHAVIORS.get_behavior(fluid).tick_delay(self)
    }

    fn schedule_block_tick_default(&self, pos: BlockPos, block: BlockRef, delay: i32) -> bool {
        self.as_ref().schedule_block_tick_default(pos, block, delay);
        true
    }

    fn has_scheduled_block_tick(&self, pos: BlockPos, block: BlockRef) -> bool {
        self.as_ref().has_scheduled_block_tick(pos, block)
    }

    fn will_tick_block_this_tick(&self, pos: BlockPos, block: BlockRef) -> bool {
        self.as_ref().will_tick_block_this_tick(pos, block)
    }

    fn schedule_fluid_tick_default(&self, pos: BlockPos, fluid: FluidRef, delay: i32) -> bool {
        self.as_ref().schedule_fluid_tick_default(pos, fluid, delay);
        true
    }

    fn will_tick_fluid_this_tick(&self, pos: BlockPos, fluid: FluidRef) -> bool {
        self.as_ref().will_tick_fluid_this_tick(pos, fluid)
    }
}

impl LevelAccessor for Arc<World> {
    fn set_block_state(&self, pos: BlockPos, state: BlockStateId, flags: UpdateFlags) -> bool {
        self.set_block(pos, state, flags)
    }

    fn can_write_to_chunk(&self, chunk_x: i32, chunk_z: i32) -> bool {
        self.chunk_map
            .with_full_chunk(ChunkPos::new(chunk_x, chunk_z), |_| ())
            .is_some()
    }

    fn requires_live_write_preflight(&self) -> bool {
        true
    }

    fn destroy_block(&self, pos: BlockPos, drop_items: bool) -> bool {
        World::destroy_block(self, pos, drop_items)
    }

    fn play_block_sound(
        &self,
        sound: SoundEventRef,
        pos: BlockPos,
        volume: f32,
        pitch: f32,
        exclude: Option<i32>,
    ) {
        self.as_ref()
            .play_block_sound(sound, pos, volume, pitch, exclude);
    }

    fn game_event(&self, event: GameEventRef, pos: BlockPos, context: &GameEventContext<'_>) {
        World::game_event(self, event, pos, context);
    }
}
