pub(crate) mod activation;
mod aggro_pressure;
mod anthem_priority;
mod anti_self_harm;
mod blight_value;
mod board_development;
mod board_wipe_telegraph;
mod card_advantage;
mod combat_tax;
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
mod reactive_self_protection;
mod recursion_awareness;
mod redundancy_avoidance;
pub mod registry;
mod sacrifice_value;
mod spellslinger_casting;
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
