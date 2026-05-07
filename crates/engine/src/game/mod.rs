pub mod ability_utils;
pub mod arithmetic;
pub mod bending;
pub mod casting;
pub(crate) mod casting_costs;
pub(crate) mod casting_targets;
pub mod combat;
pub mod combat_damage;
pub mod commander;
pub mod companion;
pub mod cost_payability;
pub mod coverage;
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
pub(crate) mod engine_stack;
pub mod filter;
pub mod functioning_abilities;
pub mod game_object;
pub mod gap_analysis;
pub mod keywords;
pub mod layers;
pub mod life_costs;
pub mod log;
pub mod mana_abilities;
pub mod mana_payment;
pub mod mana_sources;
pub mod match_flow;
pub mod morph;
pub mod mulligan;
pub(crate) mod off_zone_characteristics;
pub mod phasing;
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
pub mod speed;
pub mod stack;
pub mod static_abilities;
pub mod targeting;
pub mod transform;
pub(crate) mod trigger_matchers;
pub mod triggers;
pub mod turn_control;
pub mod turns;
pub mod visibility;
pub mod zones;

#[cfg(test)]
pub(crate) mod test_fixtures;

pub use deck_loading::{
    create_commander_from_card_face, load_and_hydrate_decks, load_deck_into_state,
    resolve_deck_list, resolve_player_deck_list, DeckEntry, DeckList, DeckPayload, PlayerDeckList,
};
pub use deck_validation::{
    evaluate_deck_compatibility, is_commander_eligible, validate_deck_for_format,
    validate_name_deck_for_format, CompatibilityCheck, DeckCompatibilityRequest,
    DeckCompatibilityResult, DeckCoverage, UnsupportedCard,
};
pub use engine::{
    apply, apply_as_current, new_game, start_game, start_game_skip_mulligan,
    start_game_with_starting_player, EngineError,
};
pub use game_object::{BackFaceData, GameObject, PhaseOutCause, PhaseStatus};
pub use keywords::parse_keywords;
pub use layers::evaluate_layers;
pub use mana_payment::{can_pay, pay_cost, produce_mana, PaymentError};
pub use printed_cards::rehydrate_game_from_card_db;
pub use public_state::finalize_public_state;
pub use triggers::process_triggers;
pub use visibility::filter_state_for_viewer;
pub use zones::{
    add_to_zone, create_object, move_to_library_at_index, move_to_library_position, move_to_zone,
    remove_from_zone,
};
