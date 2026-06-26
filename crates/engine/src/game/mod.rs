pub mod ability_utils;
pub mod arithmetic;
pub mod attractions;
pub mod augment;
pub mod bending;
pub mod blitz;
// Tests for `blitz` live in a sibling file (declared here, not in `blitz.rs`,
// so `blitz.rs` stays implementation-only).
#[cfg(test)]
#[path = "blitz_tests.rs"]
mod blitz_tests;
pub mod bracket_estimate;
pub mod card_subset;
pub mod casting;
pub(crate) mod casting_costs;
pub(crate) mod casting_targets;
pub mod cipher;
// Tests for `cipher` live in a sibling file (declared here, not in `cipher.rs`,
// so `cipher.rs` stays implementation-only).
#[cfg(test)]
#[path = "cipher_tests.rs"]
mod cipher_tests;
pub mod combat;
pub mod combat_damage;
pub mod commander;
pub mod companion;
pub(crate) mod conditions;
pub mod contraptions;
#[cfg(test)]
#[path = "contraptions_tests.rs"]
mod contraptions_tests;
pub mod cost_payability;
pub(crate) mod costs;
pub mod coverage;
pub mod dash;
#[cfg(test)]
#[path = "dash_tests.rs"]
mod dash_tests;
pub mod day_night;
pub mod deck_loading;
pub mod deck_validation;
pub mod derived;
pub mod derived_views;
pub mod devotion;
pub mod dungeon;
pub mod effects;
pub mod elimination;
pub mod engine;
pub(crate) mod engine_casting;
pub(crate) mod engine_combat;
pub(crate) mod engine_debug;
pub(crate) mod engine_modes;
pub(crate) mod engine_payment_choices;
pub(crate) mod engine_priority;
pub(crate) mod engine_replacement;
pub(crate) mod engine_resolution_choices;
pub mod engine_resolve_batch;
pub(crate) mod engine_stack;
pub(crate) mod exile_links;
pub mod filter;
pub mod functioning_abilities;
pub mod game_object;
pub mod gap_analysis;
pub mod haunt;
// Tests for `haunt` live in a sibling file (declared here, not in `haunt.rs`,
// so `haunt.rs` stays implementation-only).
#[cfg(test)]
#[path = "haunt_tests.rs"]
mod haunt_tests;
pub mod keywords;
pub mod layers;
pub mod life_costs;
pub mod log;
pub mod mana_abilities;
pub mod mana_payment;
pub mod mana_sources;
pub mod match_flow;
pub mod meld;
// Tests for `meld` live in a sibling file (declared here, not in `meld.rs`,
// so `meld.rs` stays implementation-only).
#[cfg(test)]
#[path = "meld_tests.rs"]
mod meld_tests;
pub mod merge;
// Tests for `merge` live in a sibling file (declared here, not in `merge.rs`,
// so `merge.rs` stays implementation-only).
pub mod archenemy;
pub mod conspiracy;
// Tests for `conspiracy` live in a sibling file (declared here, not in
// `conspiracy.rs`, so `conspiracy.rs` stays implementation-only).
#[cfg(test)]
#[path = "conspiracy_tests.rs"]
mod conspiracy_tests;
#[cfg(test)]
#[path = "merge_tests.rs"]
mod merge_tests;
pub mod morph;
pub mod mulligan;
pub(crate) mod off_zone_characteristics;
pub mod pairing;
pub mod perf_counters;
// Tests for `archenemy` live in a sibling file (declared here, not in
// `archenemy.rs`, so `archenemy.rs` stays implementation-only).
#[cfg(test)]
#[path = "archenemy_tests.rs"]
mod archenemy_tests;
#[cfg(test)]
#[path = "augment_tests.rs"]
mod augment_tests;
pub mod phasing;
pub mod planechase;
// Tests for `planechase` live in a sibling file (declared here, not in
// `planechase.rs`, so `planechase.rs` stays implementation-only).
#[cfg(test)]
#[path = "planechase_tests.rs"]
mod planechase_tests;
pub mod planeswalker;
pub mod players;
pub mod printed_cards;
pub mod priority;
pub mod public_state;
pub mod quantity;
pub mod replacement;
pub mod restrictions;
pub mod room;
pub(crate) mod sacrifice;
pub mod sba;
pub mod scenario;
pub mod scenario_db;
pub mod specialize;
pub mod speed;
pub mod splice;
#[cfg(test)]
mod splice_tests;
pub mod stack;
pub mod static_abilities;
pub mod static_source_index;
pub mod stickers;
#[cfg(test)]
#[path = "stickers_tests.rs"]
mod stickers_tests;
pub mod targeting;
pub mod token_presets;
pub mod transform;
pub mod trigger_index;
pub(crate) mod trigger_matchers;
pub mod triggers;
pub mod turn_control;
pub mod turns;
pub mod visibility;
pub mod zone_pipeline;
pub mod zones;

#[cfg(test)]
pub(crate) mod test_fixtures;

pub use bracket_estimate::{
    estimate_bracket, BracketAxis, BracketAxisCounts, BracketContributingCards, BracketEstimate,
    BracketViolation, CommanderBracketTier,
};
pub use deck_loading::{
    create_commander_from_card_face, load_and_hydrate_decks, load_deck_into_state,
    resolve_deck_list, resolve_player_deck_list, DeckEntry, DeckList, DeckPayload, PlayerDeckList,
};
pub use deck_validation::{
    can_pair_commanders, deck_copy_limit_for, evaluate_deck_compatibility,
    is_brawl_commander_eligible, is_commander_eligible, is_tiny_leader_eligible,
    validate_deck_for_format, validate_name_deck_for_format, CompatibilityCheck,
    DeckCompatibilityRequest, DeckCompatibilityResult, DeckCoverage, UnsupportedCard,
};
pub use engine::{
    apply, apply_as_current, new_game, start_game, start_game_skip_mulligan,
    start_game_with_starting_player, EngineError,
};
pub use engine_debug::route_debug_create_to_battlefield;
pub use engine_resolve_batch::{
    resolve_all_fast_forward, ResolveAllCallbackDecision, ResolveAllFastForwardResult,
};
pub use game_object::{BackFaceData, GameObject, PhaseOutCause, PhaseStatus};
pub use keywords::parse_keywords;
pub use mana_payment::{can_pay, pay_from_pool, produce_mana, PaymentError};
pub use printed_cards::rehydrate_game_from_card_db;
pub use public_state::finalize_public_state;
pub use triggers::process_triggers;
pub use visibility::filter_state_for_viewer;
pub use zones::{
    add_to_zone, create_object, move_to_library_at_index, move_to_library_position, move_to_zone,
    remove_from_zone,
};
