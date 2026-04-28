//! mtgish `Rule::SagaChapters` → engine Saga chapter triggers.
//!
//! CR 714: A Saga's chapter abilities are triggered abilities that fire
//! whenever a lore counter reaches the chapter's threshold. mtgish encodes
//! a saga as `SagaChapters(Vec<SagaChapter>)` where each `SagaChapter` is
//! `SagaChapter(Vec<i32>, Box<Actions>)` — the Vec lists chapter numbers
//! sharing the same effect (e.g., "I, II — Draw a card.").
//!
//! Engine encodes this as one `TriggerDefinition` per chapter number,
//! using `TriggerMode::CounterAdded` with a `CounterTriggerFilter` keyed
//! on the lore counter type and the chapter ordinal as `threshold`.
//! Note: the Saga ETB lore-counter replacement is built by the engine's
//! Oracle parser at runtime; mtgish-import intentionally emits only the
//! chapter triggers — the engine still ETBs the Saga via the existing
//! Saga handling pipeline.

use engine::types::ability::{AbilityKind, CounterTriggerFilter, TargetFilter, TriggerDefinition};
use engine::types::counter::CounterType as EngineCounterType;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::convert::action;
use crate::convert::build_ability_from_actions;
use crate::convert::result::{ConvResult, ConversionGap};
use crate::schema::types::SagaChapter;

/// CR 714.2: Convert a list of `SagaChapter` entries into one
/// `TriggerDefinition` per chapter ordinal. A single SagaChapter with
/// multiple ordinals (e.g., "I, II — Draw a card.") fans out into one
/// trigger per ordinal, each with its own `CounterTriggerFilter.threshold`.
///
/// Each chapter body delegates to `action::convert_actions` →
/// `build_ability_from_actions`, picking up Modal/Optional/OptionalWithCost/
/// Conditional/Branched/Scoped/LinearChain shapes that share their wiring
/// with the spell-body conversion path. Saga chapters are ordinary
/// triggered abilities (CR 714.2 + CR 113.3a) — there is no chapter-only
/// idiom that the keystone doesn't already cover.
pub fn convert(chapters: &[SagaChapter]) -> ConvResult<Vec<TriggerDefinition>> {
    let mut out = Vec::new();
    for chapter in chapters {
        let SagaChapter::SagaChapter(nums, actions) = chapter;
        // CR 714.2a + CR 113.3a: Build the chapter body via the shared
        // ActionsConversion pipeline so Modal / MayAction / MayCost / If /
        // Unless / IfElse / EachPlayerAction shapes lift through the same
        // ability-shaping code as a spell body.
        let conv = action::convert_actions(actions)?;
        let exec = build_ability_from_actions(AbilityKind::Spell, None, conv)?;
        for &n in nums {
            let ordinal = u32::try_from(n).map_err(|_| ConversionGap::MalformedIdiom {
                idiom: "SagaChapter/ordinal",
                path: String::new(),
                detail: format!("expected non-negative chapter ordinal, got {n}"),
            })?;
            let trigger = TriggerDefinition::new(TriggerMode::CounterAdded)
                .valid_card(TargetFilter::SelfRef)
                .counter_filter(CounterTriggerFilter {
                    counter_type: EngineCounterType::Lore,
                    threshold: Some(ordinal),
                })
                .execute(exec.clone())
                .trigger_zones(vec![Zone::Battlefield])
                .description(format!("Chapter {ordinal}"));
            out.push(trigger);
        }
    }
    Ok(out)
}
