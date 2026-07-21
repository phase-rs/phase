//! Issue #6092: per-ability activation-block read-out (`GameObject::blocked_abilities`).
//!
//! CR 602.5: The engine already ENFORCES three classes of activation
//! prohibition (CantBeActivated static / CantActivateDuring static / temporary
//! `ProhibitActivity`). This suite covers the DISPLAY read-out derived from those
//! same predicates: `derive_display_state` populates `blocked_abilities` with one
//! `AbilityBlockEntry { ability_index, reason: { sources, kind } }` per blocked
//! activated ability, in enforcement-gate order (CantBeActivated → CantActivateDuring →
//! Prohibited). Every test drives the production seam
//! (`derive_display_state` → `casting::activation_prohibition_reason` → the three
//! shared reason cores) and asserts on the resulting `blocked_abilities`; a revert
//! of the derive sweep or any core flips an assertion.
//!
//! Oracle text for The Immortal Sun, Pithing Needle, and City of Solitude is
//! verbatim from MTGJSON `AtomicCards.json` / `data/card-data.json`.

use std::sync::Arc;

use engine::game::derived::derive_display_state;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    AbilityBlockKind, AbilityBlockReason, AbilityCost, AbilityDefinition, AbilityKind, AbilityTag,
    ChosenAttribute, Effect, GameRestriction, ManaContribution, ManaProduction, ProhibitedActivity,
    QuantityExpr, RestrictionExpiry, RestrictionPlayerScope, TargetFilter,
};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::PublicStateDirty;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::statics::ActivationExemption;

const SUN_ORACLE: &str = "Players can't activate planeswalkers' loyalty abilities.\nAt the beginning of your draw step, draw an additional card.\nSpells you cast cost {1} less to cast.\nCreatures you control get +1/+1.";

// Pithing Needle's verbatim modern Oracle text (data/card-data.json).
const NEEDLE_ORACLE: &str = "As this artifact enters, choose a card name.\nActivated abilities of sources with the chosen name can't be activated unless they're mana abilities.";

// City of Solitude's verbatim Oracle text (data/card-data.json).
const CITY_ORACLE: &str =
    "Players can cast spells and activate abilities only during their own turns.";

/// A non-mana activated ability (`{T}: draw`) → kind == Activated, not a mana
/// ability, no keyword tag.
fn tap_nonmana_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Tap)
}

/// A mana ability (`{T}: Add`) — classified a mana ability by
/// `mana_abilities::is_mana_ability` (Effect::Mana, no target, no loyalty cost).
fn mana_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::Fixed {
                colors: vec![],
                contribution: ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(AbilityCost::Tap)
}

/// A loyalty activated ability (`[+1]`) → kind == Activated with a loyalty cost.
fn loyalty_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Loyalty { amount: 1 })
    .sorcery_speed()
}

/// A tag-carrying activated ability (Kang power-up class).
fn power_up_ability() -> AbilityDefinition {
    let mut ability = tap_nonmana_ability();
    ability.ability_tag = Some(AbilityTag::PowerUp);
    ability
}

/// Force the derive gate open and recompute display state (idempotent per call).
fn rederive(runner: &mut GameRunner) {
    runner.state_mut().public_state_dirty = PublicStateDirty::all_dirty();
    derive_display_state(runner.state_mut());
}

/// Populate the static-presence index / layers so the CR 604.1 O(1) presence
/// gate sees the seeded statics, then recompute the read-out.
fn refresh_statics_and_derive(runner: &mut GameRunner) {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    rederive(runner);
}

fn blocked(runner: &GameRunner, id: ObjectId) -> Vec<(usize, Vec<ObjectId>, AbilityBlockKind)> {
    runner.state().objects[&id]
        .blocked_abilities
        .iter()
        .map(|e| (e.ability_index, e.reason.sources.clone(), e.reason.kind))
        .collect()
}

/// Rows 1 + 2: Pithing Needle names a card. On a source with that name, the
/// NON-mana activated ability gets a `CantBeActivated` entry sourced to the
/// Needle; its MANA ability does NOT (CR 605.1a exemption). Row 1 (the non-mana
/// entry) is the reach-guard proving the Needle static is in force, so row 2's
/// negative (mana ability absent) is not vacuous.
#[test]
fn pithing_needle_blocks_nonmana_exempts_mana() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let needle = scenario
        .add_creature_from_oracle(P0, "Pithing Needle", 0, 0, NEEDLE_ORACLE)
        .as_artifact()
        .id();
    // Index 0 = non-mana activated, index 1 = mana ability. Both on a source
    // whose name matches the Needle's chosen name.
    let named = scenario
        .add_creature(P0, "Grim Monolith", 0, 0)
        .with_ability_definition(tap_nonmana_ability())
        .with_ability_definition(mana_ability())
        .id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&needle)
        .unwrap()
        .chosen_attributes = vec![ChosenAttribute::CardName("Grim Monolith".to_string())];
    refresh_statics_and_derive(&mut runner);

    assert_eq!(
        blocked(&runner, named),
        vec![(0, vec![needle], AbilityBlockKind::CantBeActivated)],
        "the non-mana ability (index 0) is blocked by the Needle; the mana ability (index 1) is exempt"
    );
}

/// Primary F2 discriminator (CR 602.5): TWO Pithing Needles both naming the same
/// source each independently prohibit its activated ability. The read-out records
/// BOTH carriers in `reason.sources`, sorted + deduped. Reach-guard: the
/// single-source rows above assert `len == 1`, so this multi-source assertion
/// cannot pass vacuously. REVERT-FAIL: a first-match core would yield `len == 1`.
#[test]
fn two_sources_of_same_kind_both_surface() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let needle_a = scenario
        .add_creature_from_oracle(P0, "Pithing Needle", 0, 0, NEEDLE_ORACLE)
        .as_artifact()
        .id();
    let needle_b = scenario
        .add_creature_from_oracle(P0, "Pithing Needle", 0, 0, NEEDLE_ORACLE)
        .as_artifact()
        .id();
    let named = scenario
        .add_creature(P0, "Grim Monolith", 0, 0)
        .with_ability_definition(tap_nonmana_ability())
        .id();
    let mut runner = scenario.build();
    for needle in [needle_a, needle_b] {
        runner
            .state_mut()
            .objects
            .get_mut(&needle)
            .unwrap()
            .chosen_attributes = vec![ChosenAttribute::CardName("Grim Monolith".to_string())];
    }
    refresh_statics_and_derive(&mut runner);

    let mut expected_sources = vec![needle_a, needle_b];
    expected_sources.sort();
    assert_eq!(
        blocked(&runner, named),
        vec![(0, expected_sources, AbilityBlockKind::CantBeActivated)],
        "both Needles surface in sources, sorted + deduped"
    );
    let entry_sources = &runner.state().objects[&named].blocked_abilities[0]
        .reason
        .sources;
    assert_eq!(
        entry_sources.len(),
        2,
        "two distinct prohibiting sources are both recorded, not just the first"
    );
}

/// MINOR-4 legacy decode (CR 602.5 read-out serde back-compat): a pre-change
/// on-disk blob carried a bare `source` int + the flattened `kind` tag. The new
/// shape defaults `sources` (absent → `vec![]`) and, having no
/// `deny_unknown_fields`, tolerates the stale `source` key (dropped by the flatten
/// buffer; the internally-tagged `kind` ignores it). REVERT-FAIL: removing
/// `#[serde(default)]` on `sources` makes this blob error (missing field).
#[test]
fn ability_block_reason_decodes_legacy_source_key() {
    // Pre-change on-disk shape carried a bare `source` int + the flattened kind tag.
    let legacy = serde_json::from_str::<AbilityBlockReason>(r#"{"source":5,"type":"Prohibited"}"#)
        .expect("legacy blob with stale `source` key decodes");
    assert_eq!(
        legacy,
        AbilityBlockReason {
            sources: vec![],
            kind: AbilityBlockKind::Prohibited,
        },
    );
    // Sibling: the missing-key blob decodes to the same value.
    let missing = serde_json::from_str::<AbilityBlockReason>(r#"{"type":"Prohibited"}"#)
        .expect("missing-`sources` blob decodes via the serde default");
    assert_eq!(missing, legacy);
}

/// Row 9 baseline: with no prohibition anywhere, `blocked_abilities` is empty.
/// This is the negative reach-guard for every positive test above/below.
#[test]
fn no_prohibition_leaves_read_out_empty() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let named = scenario
        .add_creature(P0, "Grim Monolith", 0, 0)
        .with_ability_definition(tap_nonmana_ability())
        .with_ability_definition(mana_ability())
        .id();
    let mut runner = scenario.build();
    refresh_statics_and_derive(&mut runner);
    assert!(
        blocked(&runner, named).is_empty(),
        "no static / restriction is present, so nothing is blocked"
    );
}

/// Row 10: when the prohibiting source leaves the battlefield, the next derive
/// clears the entry (the sweep assigns unconditionally). Reach-guard: the entry
/// is present BEFORE removal.
#[test]
fn read_out_clears_when_source_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let needle = scenario
        .add_creature_from_oracle(P0, "Pithing Needle", 0, 0, NEEDLE_ORACLE)
        .as_artifact()
        .id();
    let named = scenario
        .add_creature(P0, "Grim Monolith", 0, 0)
        .with_ability_definition(tap_nonmana_ability())
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&needle)
        .unwrap()
        .chosen_attributes = vec![ChosenAttribute::CardName("Grim Monolith".to_string())];
    refresh_statics_and_derive(&mut runner);
    assert_eq!(
        blocked(&runner, named),
        vec![(0, vec![needle], AbilityBlockKind::CantBeActivated)],
        "baseline: blocked while the Needle is on the battlefield"
    );

    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != needle);
        state.objects.remove(&needle);
    }
    refresh_statics_and_derive(&mut runner);
    assert!(
        blocked(&runner, named).is_empty(),
        "the entry is cleared once the prohibiting source leaves the battlefield"
    );
}

/// Row 11 (CR 603.2a): only ACTIVATED abilities are read out. A same-named
/// source whose sole ability is non-activated (kind != Activated) gets no entry,
/// while a sibling with an Activated ability DOES — proving the kind filter is
/// load-bearing (the non-activated result is not vacuous).
#[test]
fn non_activated_ability_is_not_read_out() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let needle = scenario
        .add_creature_from_oracle(P0, "Pithing Needle", 0, 0, NEEDLE_ORACLE)
        .as_artifact()
        .id();
    // A non-activated ability (kind == Spell stands in for a triggered/spell
    // ability that lives outside the activated-ability index space).
    let spell_kind = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    );
    let trigger_only = scenario
        .add_creature(P0, "Grim Monolith", 0, 0)
        .with_ability_definition(spell_kind)
        .id();
    let activated = scenario
        .add_creature(P0, "Grim Monolith", 0, 0)
        .with_ability_definition(tap_nonmana_ability())
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&needle)
        .unwrap()
        .chosen_attributes = vec![ChosenAttribute::CardName("Grim Monolith".to_string())];
    refresh_statics_and_derive(&mut runner);

    assert!(
        blocked(&runner, trigger_only).is_empty(),
        "a non-activated ability is never read out (CR 603.2a)"
    );
    assert_eq!(
        blocked(&runner, activated),
        vec![(0, vec![needle], AbilityBlockKind::CantBeActivated)],
        "positive control: the sibling's ACTIVATED ability IS read out, so the empty result above is not vacuous"
    );
}

/// Row 12: Needle + City both block the same non-mana ability on the
/// non-active player's source. Exactly ONE entry, and it is CantBeActivated
/// (gate order pins CantBeActivated ahead of CantActivateDuring).
#[test]
fn gate_order_pins_cant_be_activated_first() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // P0 is the active player (default). City blocks the non-active player.
    let needle = scenario
        .add_creature_from_oracle(P0, "Pithing Needle", 0, 0, NEEDLE_ORACLE)
        .as_artifact()
        .id();
    scenario.add_creature_from_oracle(P0, "City of Solitude", 0, 0, CITY_ORACLE);
    // A source controlled by the NON-active player (P1) named to match the Needle.
    let named = scenario
        .add_creature(P1, "Grim Monolith", 0, 0)
        .with_ability_definition(tap_nonmana_ability())
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&needle)
        .unwrap()
        .chosen_attributes = vec![ChosenAttribute::CardName("Grim Monolith".to_string())];
    refresh_statics_and_derive(&mut runner);

    assert_eq!(
        blocked(&runner, named),
        vec![(0, vec![needle], AbilityBlockKind::CantBeActivated)],
        "both statics block the ability, but the read-out records exactly one entry in gate order (CantBeActivated first)"
    );
}

/// Row 3: The Immortal Sun blocks a planeswalker's LOYALTY ability
/// (`CantBeActivated`, sourced to the Sun); a NON-loyalty activated ability on
/// another permanent is NOT blocked (kind axis).
#[test]
fn immortal_sun_blocks_loyalty_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sun = scenario
        .add_creature_from_oracle(P0, "The Immortal Sun", 0, 0, SUN_ORACLE)
        .as_artifact()
        .id();
    let walker = scenario.add_creature(P0, "Test Walker", 0, 0).id();
    let rock = scenario
        .add_creature(P0, "Mana Rock", 0, 0)
        .with_ability_definition(tap_nonmana_ability())
        .id();
    let mut runner = scenario.build();
    {
        let pw = runner.state_mut().objects.get_mut(&walker).unwrap();
        pw.card_types.core_types.push(CoreType::Planeswalker);
        pw.base_card_types = pw.card_types.clone();
        pw.loyalty = Some(5);
        pw.counters.insert(CounterType::Loyalty, 5);
        pw.abilities = Arc::new(vec![loyalty_ability()]);
        pw.base_abilities = Arc::new(vec![loyalty_ability()]);
    }
    refresh_statics_and_derive(&mut runner);

    assert_eq!(
        blocked(&runner, walker),
        vec![(0, vec![sun], AbilityBlockKind::CantBeActivated)],
        "the planeswalker's loyalty ability is blocked by the Sun"
    );
    assert!(
        blocked(&runner, rock).is_empty(),
        "a non-loyalty activated ability on another permanent is NOT blocked (kind axis)"
    );
}

/// Rows 4 + 5: City of Solitude blocks the NON-active player's activated ability
/// (`CantActivateDuring`, sourced to the City) but not the active player's own.
#[test]
fn city_of_solitude_blocks_non_active_players_ability() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain); // active_player = P0
    let city = scenario
        .add_creature_from_oracle(P0, "City of Solitude", 0, 0, CITY_ORACLE)
        .id();
    let their_source = scenario
        .add_creature(P1, "Foe Rock", 0, 0)
        .with_ability_definition(tap_nonmana_ability())
        .id();
    let own_source = scenario
        .add_creature(P0, "Own Rock", 0, 0)
        .with_ability_definition(tap_nonmana_ability())
        .id();
    let mut runner = scenario.build();
    refresh_statics_and_derive(&mut runner);

    assert_eq!(
        blocked(&runner, their_source),
        vec![(0, vec![city], AbilityBlockKind::CantActivateDuring)],
        "the non-active player's ability is blocked outside their own turn"
    );
    assert!(
        blocked(&runner, own_source).is_empty(),
        "the active player's own ability is not blocked on their turn"
    );
}

/// Rows 6 + 7 + 8: A Kang-class `ProhibitActivity` (only_tag PowerUp) yields a
/// `Prohibited` entry for a power-up ability in force (row 6), NOT while
/// pre-armed via `UntilEndOfNextTurnOf` (row 7), and NOT for a non-power-up
/// ability (row 8, tag miss).
#[test]
fn kang_prohibit_activity_tag_scoped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kang = scenario.add_creature(P0, "Kang", 0, 0).id();
    let subject = scenario
        .add_creature(P0, "Powered Up", 0, 0)
        .with_ability_definition(power_up_ability()) // index 0: tagged
        .with_ability_definition(tap_nonmana_ability()) // index 1: untagged
        .id();
    let mut runner = scenario.build();

    // Row 6: in-force prohibition (EndOfTurn expiry) → power-up ability blocked,
    // untagged ability not (row 8).
    runner
        .state_mut()
        .restrictions
        .push(GameRestriction::ProhibitActivity {
            source: kang,
            affected_players: RestrictionPlayerScope::AllPlayers,
            expiry: RestrictionExpiry::EndOfTurn,
            activity: ProhibitedActivity::ActivateAbilities {
                exemption: ActivationExemption::None,
                only_tag: Some(AbilityTag::PowerUp),
            },
        });
    rederive(&mut runner);
    assert_eq!(
        blocked(&runner, subject),
        vec![(0, vec![kang], AbilityBlockKind::Prohibited)],
        "row 6/8: the tagged power-up ability (index 0) is Prohibited; the untagged ability (index 1) is not"
    );

    // Row 7: replace with a PRE-ARMED restriction → not yet in force → cleared.
    {
        let state = runner.state_mut();
        state.restrictions.clear();
        state.restrictions.push(GameRestriction::ProhibitActivity {
            source: kang,
            affected_players: RestrictionPlayerScope::AllPlayers,
            expiry: RestrictionExpiry::UntilEndOfNextTurnOf { player: P0 },
            activity: ProhibitedActivity::ActivateAbilities {
                exemption: ActivationExemption::None,
                only_tag: Some(AbilityTag::PowerUp),
            },
        });
    }
    rederive(&mut runner);
    assert!(
        blocked(&runner, subject).is_empty(),
        "row 7: a pre-armed UntilEndOfNextTurnOf prohibition is not yet in force"
    );
}

/// Row 13: a runtime-granted equip ability (index past the printed list) is read
/// out under a blanket Prohibited restriction, while a printed sibling keeps its
/// `< printed_len` index. Reach-guard: `activated_ability_definitions` yields the
/// runtime index.
#[test]
fn runtime_granted_ability_index_is_read_out() {
    use engine::game::casting::activated_ability_definitions;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kang = scenario.add_creature(P0, "Kang", 0, 0).id();
    let obj = scenario
        .add_creature(P0, "Granted Gear", 0, 0)
        .with_ability_definition(tap_nonmana_ability()) // printed index 0
        .as_artifact()
        .id();
    let mut runner = scenario.build();
    let printed_len = runner.state().objects[&obj].abilities.len();
    // Grant an Equip keyword at RUNTIME (present in live keywords, absent from
    // base) so `runtime_granted_equip_abilities` synthesizes an activated ability
    // appended at `printed_len`.
    {
        let gear = runner.state_mut().objects.get_mut(&obj).unwrap();
        gear.keywords = vec![Keyword::Equip(ManaCost::generic(2))];
    }

    // Reach-guard: the runtime index really exists past the printed list.
    let defs = activated_ability_definitions(runner.state(), obj);
    assert!(
        defs.iter().any(|(idx, _)| *idx >= printed_len),
        "reach-guard: a runtime-granted ability must occupy an index >= printed_len ({printed_len}); got {:?}",
        defs.iter().map(|(i, _)| *i).collect::<Vec<_>>()
    );

    // Blanket prohibition (only_tag None) blocks every activation.
    runner
        .state_mut()
        .restrictions
        .push(GameRestriction::ProhibitActivity {
            source: kang,
            affected_players: RestrictionPlayerScope::AllPlayers,
            expiry: RestrictionExpiry::EndOfTurn,
            activity: ProhibitedActivity::ActivateAbilities {
                exemption: ActivationExemption::None,
                only_tag: None,
            },
        });
    rederive(&mut runner);

    let entries = blocked(&runner, obj);
    assert!(
        entries.iter().any(|(idx, src, kind)| *idx < printed_len
            && src.contains(&kang)
            && *kind == AbilityBlockKind::Prohibited),
        "the printed ability keeps its < printed_len index: {entries:?}"
    );
    assert!(
        entries.iter().any(|(idx, src, kind)| *idx >= printed_len
            && src.contains(&kang)
            && *kind == AbilityBlockKind::Prohibited),
        "the runtime-granted ability is read out at an index >= printed_len: {entries:?}"
    );
}

/// Row 14: a `Prohibited` restriction can outlive its source object. The entry
/// still records `Prohibited` with the departed id retained in `reason.sources`,
/// and that id is no longer present in `state.objects` (the frontend
/// departed-source guard is exercised in the component test).
#[test]
fn prohibited_entry_survives_departed_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kang = scenario.add_creature(P0, "Kang", 0, 0).id();
    let subject = scenario
        .add_creature(P0, "Powered Up", 0, 0)
        .with_ability_definition(power_up_ability())
        .id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .restrictions
        .push(GameRestriction::ProhibitActivity {
            source: kang,
            affected_players: RestrictionPlayerScope::AllPlayers,
            expiry: RestrictionExpiry::EndOfTurn,
            activity: ProhibitedActivity::ActivateAbilities {
                exemption: ActivationExemption::None,
                only_tag: Some(AbilityTag::PowerUp),
            },
        });
    // The Kang source leaves the battlefield, but the restriction persists.
    {
        let state = runner.state_mut();
        state.battlefield.retain(|&id| id != kang);
        state.objects.remove(&kang);
    }
    rederive(&mut runner);

    let entries = blocked(&runner, subject);
    assert_eq!(
        entries,
        vec![(0, vec![kang], AbilityBlockKind::Prohibited)],
        "the entry persists and still names the departed source"
    );
    assert!(
        runner.state().objects.get(&kang).is_none(),
        "the source object is genuinely gone — the departed-source id is dangling"
    );
}

/// Row 15: an ability that is merely unaffordable (no prohibition) is NOT read
/// out — the read-out reflects only prohibitions, not payability. Reach-guard:
/// the SAME ability is enumerated by `activated_ability_definitions` and gains an
/// entry once a prohibition is added.
#[test]
fn unaffordable_ability_without_prohibition_is_not_read_out() {
    use engine::game::casting::activated_ability_definitions;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let kang = scenario.add_creature(P0, "Kang", 0, 0).id();
    // A legal-but-unaffordable ability: a {5} mana cost with an empty pool.
    let costly = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Mana {
        cost: ManaCost::generic(5),
    });
    let obj = scenario
        .add_creature(P0, "Expensive Engine", 0, 0)
        .with_ability_definition(costly)
        .id();
    let mut runner = scenario.build();
    rederive(&mut runner);

    // Reach-guard: the ability IS enumerated (so the empty read-out is about the
    // absence of a prohibition, not the ability being invisible).
    let defs = activated_ability_definitions(runner.state(), obj);
    assert!(
        defs.iter().any(|(idx, _)| *idx == 0),
        "reach-guard: the unaffordable ability is enumerated at index 0"
    );
    assert!(
        blocked(&runner, obj).is_empty(),
        "an unaffordable (but un-prohibited) ability is not read out"
    );

    // Add a blanket prohibition → the very same ability now gains an entry.
    runner
        .state_mut()
        .restrictions
        .push(GameRestriction::ProhibitActivity {
            source: kang,
            affected_players: RestrictionPlayerScope::AllPlayers,
            expiry: RestrictionExpiry::EndOfTurn,
            activity: ProhibitedActivity::ActivateAbilities {
                exemption: ActivationExemption::None,
                only_tag: None,
            },
        });
    rederive(&mut runner);
    assert_eq!(
        blocked(&runner, obj),
        vec![(0, vec![kang], AbilityBlockKind::Prohibited)],
        "with a prohibition present, the same ability is now read out"
    );
}
