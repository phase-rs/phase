pub(crate) mod activation;
mod aggro_pressure;
mod anthem_priority;
mod anti_self_harm;
mod board_development;
mod board_wipe_telegraph;
mod card_advantage;
pub(crate) mod context;
mod copy_value;
mod downside_awareness;
pub(crate) mod effect_classify;
mod effect_timing;
mod etb_value;
mod evasion_removal_priority;
mod free_outlet_activation;
pub(crate) mod hand_disruption;
mod hold_mana_up;
mod interaction_reservation;
mod landfall_timing;
mod lethality_awareness;
mod life_total_resource;
mod mana_efficiency;
pub mod mulligan;
mod plus_one_counters;
mod ramp_timing;
mod recursion_awareness;
pub mod registry;
mod sacrifice_value;
pub(crate) mod stack_awareness;
pub(crate) mod strategy_helpers;
mod sweeper_timing;
mod synergy_casting;
mod tempo_curve;
mod tokens_wide;
mod tribal_lord_priority;
pub(crate) mod tutor;

#[cfg(test)]
pub mod tests;

pub use registry::{
    DecisionKind, PolicyId, PolicyReason, PolicyRegistry, PolicyVerdict, TacticalPolicy,
};
