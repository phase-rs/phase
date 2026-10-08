//! Standard "become-color / set-color" batch — continuous color-setting effects
//! and statics route through the existing `ContinuousModification::SetColor` /
//! `AddColor` / `AddChosenColor` Layer-5 seam (CR 105.3 / CR 613.1e). These were
//! parser gaps: the effect/static phrasings never reached those modifications.
//!
//! Each test drives the PRODUCTION pipeline — `parse_oracle_text` →
//! resolve_ability_chain / static install → `evaluate_layers` — and asserts the
//! affected object's effective color after the layer evaluator runs. Every
//! assertion FLIPS on a revert of the parser arm: without it, parsing emits
//! `Effect::Unimplemented` (no color modification), so the layer pass leaves the
//! printed color untouched and the equality checks fail.
//!
//! Cards: Tam, Mindful First-Year (become all colors); Possessed Goat (becomes a
//! black ... in addition to its other colors and types); Puca's Eye (becomes the
//! chosen color); Mondo Gecko (becomes the color of your choice and gains
//! hexproof from that color); Leyline of the Guildpact (each nonland permanent
//! you control is all colors); Shimmerwilds Growth (enchanted land is the chosen
//! color).

use engine::game::ability_utils::{build_resolved_from_def, build_resolved_from_def_with_targets};
use engine::game::effects::resolve_ability_chain;
use engine::game::game_object::AttachTarget;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, ChosenAttribute, ContinuousModification, Effect, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{HexproofFilter, Keyword};
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;

const WUBRG: [ManaColor; 5] = [
    ManaColor::White,
    ManaColor::Blue,
    ManaColor::Black,
    ManaColor::Red,
    ManaColor::Green,
];

fn creature_types() -> Vec<String> {
    vec!["Creature".to_string()]
}

/// Parse a card and assert it has zero residual `Unimplemented` nodes — the
/// per-card 0-unimpl gate.
fn assert_zero_unimplemented(oracle: &str, name: &str, types: &[String], subtypes: &[String]) {
    let parsed = parse_oracle_text(oracle, name, &[], types, subtypes);
    let dbg = format!("{parsed:#?}");
    assert!(
        !dbg.contains("Unimplemented"),
        "{name}: expected zero Unimplemented nodes, parse was:\n{dbg}"
    );
}

// ---------------------------------------------------------------------------
// Foraging Wickermaw — "{1}: Add one mana of any color. This creature becomes
// that color until end of turn." The "that color" anaphor refers to the color
// the player produces with the mana ability. It now routes through the existing
// `AddChosenColor` Layer-5 seam: the mana producer records the produced color as
// `ChosenAttribute::Color` on the source (see `produce_mana_from_ability`), and
// this `AddChosenColor` reads it live at Layer 5. This is the parser-shape half;
// the runtime color-follows-choice / staleness / gate discrimination lives in
// `mana_abilities.rs` tests. CR 105.3 + CR 106.1a + CR 613.1e.
// ---------------------------------------------------------------------------

#[test]
fn foraging_wickermaw_becomes_that_color_maps_to_add_chosen_color() {
    const ORACLE: &str = "When this creature enters, surveil 1.\n{1}: Add one mana of any color. This creature becomes that color until end of turn. Activate only once each turn.";
    // The "becomes that color" clause now parses — zero residual Unimplemented.
    assert_zero_unimplemented(
        ORACLE,
        "Foraging Wickermaw",
        &creature_types(),
        &["Lizard".to_string()],
    );

    let parsed = parse_oracle_text(
        ORACLE,
        "Foraging Wickermaw",
        &[],
        &creature_types(),
        &["Lizard".to_string()],
    );
    // The {1} mana ability carries the become-that-color apply-half.
    let mana_ability = parsed
        .abilities
        .iter()
        .find(|a| format!("{:?}", a.effect).contains("AnyOneColor"))
        .expect("the '{1}: Add one mana of any color' mana ability must parse");
    let sub = mana_ability
        .sub_ability
        .as_ref()
        .expect("the 'becomes that color' clause must hang off the mana ability");
    assert!(
        generic_effect_has_chosen_color(&sub.effect),
        "'becomes that color' must map to AddChosenColor; got {:?}",
        sub.effect
    );
    // The ETB surveil trigger must still parse.
    assert!(
        format!("{parsed:#?}").contains("Surveil"),
        "the ETB surveil trigger must still parse"
    );
}

// ---------------------------------------------------------------------------
// Tam, Mindful First-Year — "{T}: Target creature you control becomes all
// colors until end of turn." CR 105.2 + CR 105.3 + CR 613.1e.
// ---------------------------------------------------------------------------

#[test]
fn tam_target_creature_becomes_all_colors() {
    const ORACLE: &str = "Each other creature you control has hexproof from each of its colors.\n{T}: Target creature you control becomes all colors until end of turn.";
    assert_zero_unimplemented(ORACLE, "Tam, Mindful First-Year", &creature_types(), &[]);

    let parsed = parse_oracle_text(ORACLE, "Tam", &[], &creature_types(), &[]);
    // The activated ability is the one carrying the become-all-colors effect.
    let activated = parsed
        .abilities
        .iter()
        .find(|a| {
            let dbg = format!("{:?}", a.effect);
            dbg.contains("SetColor")
        })
        .expect("Tam's activated ability must carry a SetColor modification");

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Tam", 1, 3).id();
    let target = scenario.add_creature(P0, "Mono-Green Beast", 3, 3).id();
    let mut runner = scenario.build();
    // Printed target color: mono-green.
    {
        let obj = runner.state_mut().objects.get_mut(&target).unwrap();
        obj.color = vec![ManaColor::Green];
        obj.base_color = vec![ManaColor::Green];
    }

    let ability = build_resolved_from_def_with_targets(
        activated,
        source,
        P0,
        vec![TargetRef::Object(target)],
    );
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("become-all-colors must resolve");

    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    let target_colors = &runner.state().objects[&target].color;
    for color in WUBRG {
        assert!(
            target_colors.contains(&color),
            "target must be all five colors after 'becomes all colors'; got {target_colors:?}"
        );
    }
    assert_eq!(
        target_colors.len(),
        5,
        "all-colors SETS the color set to exactly WUBRG (CR 105.3 replacement)"
    );
}

// ---------------------------------------------------------------------------
// Possessed Goat — "Put three +1/+1 counters on this creature and it becomes a
// black Demon in addition to its other colors and types." The "and it becomes"
// conjunct must split off (sequence dispatch) and the color must be ADDITIVE
// (CR 105.3 "in addition to its other colors") — AddColor(Black), not SetColor.
// ---------------------------------------------------------------------------

#[test]
fn possessed_goat_adds_black_in_addition_to_existing_colors() {
    const ORACLE: &str = "{3}, Discard a card: Put three +1/+1 counters on this creature and it becomes a black Demon in addition to its other colors and types. Activate only once.";
    assert_zero_unimplemented(
        ORACLE,
        "Possessed Goat",
        &creature_types(),
        &["Goat".to_string()],
    );

    let parsed = parse_oracle_text(
        ORACLE,
        "Possessed Goat",
        &[],
        &creature_types(),
        &["Goat".to_string()],
    );
    let activated = &parsed.abilities[0];

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Possessed Goat", 0, 1).id();
    let mut runner = scenario.build();
    // Printed: a white Goat. The additive color must PRESERVE white.
    {
        let obj = runner.state_mut().objects.get_mut(&source).unwrap();
        obj.color = vec![ManaColor::White];
        obj.base_color = vec![ManaColor::White];
    }

    let ability = build_resolved_from_def(activated, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("counters + become must resolve");

    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    let obj = &runner.state().objects[&source];
    assert!(
        obj.color.contains(&ManaColor::Black),
        "Possessed Goat must gain black; got {:?}",
        obj.color
    );
    assert!(
        obj.color.contains(&ManaColor::White),
        "'in addition to its other colors' must PRESERVE the printed white; got {:?}",
        obj.color
    );
    assert!(
        obj.card_types.subtypes.iter().any(|s| s == "Demon"),
        "Possessed Goat must gain the Demon subtype; got {:?}",
        obj.card_types.subtypes
    );
    // The +1/+1 counters from the sibling conjunct must still land (proves the
    // split kept the counter clause intact rather than swallowing it).
    assert_eq!(
        *obj.counters
            .get(&engine::types::counter::CounterType::Plus1Plus1)
            .unwrap_or(&0),
        3,
        "the 'put three +1/+1 counters' sibling conjunct must still resolve"
    );
}

// ---------------------------------------------------------------------------
// Puca's Eye — "draw a card, then choose a color. This artifact becomes the
// chosen color." The apply-half carries `AddChosenColor`, which reads the
// source's `ChosenAttribute::Color` at Layer 5 (CR 105.3).
// ---------------------------------------------------------------------------

/// Find the apply-half `AbilityDefinition` (the `GenericEffect` carrying the
/// color modification) hanging off a `Choose` effect anywhere in the parse.
fn find_apply_half_with(
    def: &AbilityDefinition,
    pred: impl Fn(&Effect) -> bool + Copy,
) -> Option<AbilityDefinition> {
    if matches!(def.effect.as_ref(), Effect::Choose { .. }) {
        if let Some(sub) = &def.sub_ability {
            if pred(&sub.effect) {
                return Some(sub.as_ref().clone());
            }
            if let Some(found) = find_apply_half_with(sub, pred) {
                return Some(found);
            }
        }
    }
    if let Some(sub) = &def.sub_ability {
        if let Some(found) = find_apply_half_with(sub, pred) {
            return Some(found);
        }
    }
    None
}

fn generic_effect_has_chosen_color(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::GenericEffect { static_abilities, .. }
            if static_abilities.iter().any(|s| s
                .modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::AddChosenColor { .. })))
    )
}

#[test]
fn pucas_eye_becomes_the_chosen_color() {
    const ORACLE: &str = "When this artifact enters, draw a card, then choose a color. This artifact becomes the chosen color.\n{3}, {T}: Draw a card. Activate only if there are five colors among permanents you control.";
    assert_zero_unimplemented(ORACLE, "Puca's Eye", &["Artifact".to_string()], &[]);

    let parsed = parse_oracle_text(ORACLE, "Puca's Eye", &[], &["Artifact".to_string()], &[]);
    let trigger_exec = parsed.triggers[0]
        .execute
        .clone()
        .expect("Puca's Eye ETB trigger must have an execute chain");
    let apply_half = find_apply_half_with(&trigger_exec, generic_effect_has_chosen_color)
        .expect("the ETB chain must carry a Choose{Color} -> AddChosenColor apply-half");

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Puca's Eye", 0, 0).id();
    let mut runner = scenario.build();
    // Make the source a colorless artifact and stamp the made choice as red.
    {
        let obj = runner.state_mut().objects.get_mut(&source).unwrap();
        obj.card_types.core_types = vec![CoreType::Artifact];
        obj.base_card_types = obj.card_types.clone();
        obj.color = vec![];
        obj.base_color = vec![];
        obj.chosen_attributes
            .push(ChosenAttribute::Color(ManaColor::Red));
    }

    let ability = build_resolved_from_def(&apply_half, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("apply-half (AddChosenColor) must resolve");

    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    assert_eq!(
        runner.state().objects[&source].color,
        vec![ManaColor::Red],
        "Puca's Eye must become exactly the chosen color (red)"
    );
}

// ---------------------------------------------------------------------------
// Mondo Gecko — "becomes the color of your choice and gains hexproof from that
// color." The apply-half must carry BOTH AddChosenColor AND
// AddKeyword(HexproofFrom(ChosenColor)), and the protection must resolve to the
// chosen color at Layer 5/6 (CR 105.3 + CR 702.11d).
// ---------------------------------------------------------------------------

fn generic_effect_has_hexproof_chosen(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::GenericEffect { static_abilities, .. }
            if static_abilities.iter().any(|s| s.modifications.iter().any(|m| matches!(
                m,
                ContinuousModification::AddKeyword {
                    keyword: Keyword::HexproofFrom(HexproofFilter::ChosenColor)
                }
            )))
    )
}

#[test]
fn mondo_gecko_becomes_chosen_color_and_gains_hexproof_from_it() {
    const ORACLE: &str = "{1}, Discard a card: Until end of turn, Mondo Gecko becomes the color of your choice and gains hexproof from that color.\nWhenever Mondo Gecko deals combat damage to a player, draw a card for each color among permanents you control.";
    assert_zero_unimplemented(
        ORACLE,
        "Mondo Gecko",
        &creature_types(),
        &["Lizard".to_string()],
    );

    let parsed = parse_oracle_text(
        ORACLE,
        "Mondo Gecko",
        &[],
        &creature_types(),
        &["Lizard".to_string()],
    );
    let activated = &parsed.abilities[0];
    // The activated ability is a Choose{Color} whose apply-half carries both mods.
    let apply_half = find_apply_half_with(activated, generic_effect_has_chosen_color)
        .expect("Mondo Gecko's ability must carry a Choose -> AddChosenColor apply-half");
    assert!(
        generic_effect_has_hexproof_chosen(&apply_half.effect),
        "the apply-half must ALSO grant hexproof from the chosen color; got {:?}",
        apply_half.effect
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario.add_creature(P0, "Mondo Gecko", 3, 2).id();
    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&source).unwrap();
        obj.color = vec![ManaColor::Green];
        obj.base_color = vec![ManaColor::Green];
        obj.chosen_attributes
            .push(ChosenAttribute::Color(ManaColor::Blue));
    }

    let ability = build_resolved_from_def(&apply_half, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("apply-half must resolve");

    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    let obj = &runner.state().objects[&source];
    assert_eq!(
        obj.color,
        vec![ManaColor::Blue],
        "Mondo Gecko must become exactly the chosen color (blue)"
    );
    // The granted hexproof's color was baked to the chosen color (blue) at apply.
    assert!(
        obj.keywords.iter().any(|k| matches!(
            k,
            Keyword::HexproofFrom(HexproofFilter::Color(ManaColor::Blue))
        )),
        "hexproof from that color must resolve to the chosen color (blue); got {:?}",
        obj.keywords
    );
}

// ---------------------------------------------------------------------------
// Leyline of the Guildpact — "Each nonland permanent you control is all colors."
// A global color-defining static (Layer 5) over a controller-scoped nonland
// permanent filter. CR 105.2 + CR 613.1e.
// ---------------------------------------------------------------------------

#[test]
fn leyline_of_the_guildpact_makes_nonland_permanents_all_colors() {
    const ORACLE: &str = "If this card is in your opening hand, you may begin the game with it on the battlefield.\nEach nonland permanent you control is all colors.\nLands you control are every basic land type in addition to their other types.";
    assert_zero_unimplemented(
        ORACLE,
        "Leyline of the Guildpact",
        &["Enchantment".to_string()],
        &[],
    );

    let parsed = parse_oracle_text(
        ORACLE,
        "Leyline of the Guildpact",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    let color_static = parsed
        .statics
        .iter()
        .find(|s| {
            s.modifications.iter().any(
                |m| matches!(m, ContinuousModification::SetColor { colors } if colors.len() == 5),
            )
        })
        .expect("Leyline must produce an all-colors SetColor static")
        .clone();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let leyline = scenario.add_creature(P0, "Leyline", 0, 0).id();
    let my_creature = scenario.add_creature(P0, "Mono-White Cleric", 1, 1).id();
    let my_land = scenario.add_basic_land(P0, ManaColor::Green);
    let opp_creature = scenario.add_creature(P1, "Opp Mono-Red Goblin", 1, 1).id();
    let mut runner = scenario.build();
    {
        // Make the enchantment a noncreature permanent and seed printed colors.
        let obj = runner.state_mut().objects.get_mut(&leyline).unwrap();
        obj.card_types.core_types = vec![CoreType::Enchantment];
        obj.base_card_types = obj.card_types.clone();
    }
    {
        let obj = runner.state_mut().objects.get_mut(&my_creature).unwrap();
        obj.color = vec![ManaColor::White];
        obj.base_color = vec![ManaColor::White];
    }
    {
        let obj = runner.state_mut().objects.get_mut(&opp_creature).unwrap();
        obj.color = vec![ManaColor::Red];
        obj.base_color = vec![ManaColor::Red];
    }

    // Install the static on the Leyline source (both live + base lists).
    {
        let src = runner.state_mut().objects.get_mut(&leyline).unwrap();
        src.static_definitions.push(color_static.clone());
        let base = std::sync::Arc::make_mut(&mut src.base_static_definitions);
        base.push(color_static);
    }
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    // The controlled nonland permanent is now all five colors.
    let mine = &runner.state().objects[&my_creature].color;
    for color in WUBRG {
        assert!(
            mine.contains(&color),
            "controlled nonland permanent must be all colors; got {mine:?}"
        );
    }
    // The land is excluded (nonland filter) — its color is unchanged (colorless).
    assert!(
        runner.state().objects[&my_land].color.is_empty(),
        "a land must NOT be recolored by the nonland-permanent static"
    );
    // The opponent's creature is excluded (you-control filter) — stays mono-red.
    assert_eq!(
        runner.state().objects[&opp_creature].color,
        vec![ManaColor::Red],
        "an opponent's permanent must NOT be recolored"
    );
}

// ---------------------------------------------------------------------------
// Shimmerwilds Growth — Aura: "Enchanted land is the chosen color." A
// `AddChosenColor` static on the EnchantedBy land, reading the Aura's chosen
// color attribute. CR 105.3 + CR 613.1e.
// ---------------------------------------------------------------------------

#[test]
fn shimmerwilds_growth_makes_enchanted_land_the_chosen_color() {
    const ORACLE: &str = "Enchant land\nAs this Aura enters, choose a color.\nEnchanted land is the chosen color.\nWhenever enchanted land is tapped for mana, its controller adds an additional one mana of the chosen color.";
    assert_zero_unimplemented(
        ORACLE,
        "Shimmerwilds Growth",
        &["Enchantment".to_string()],
        &["Aura".to_string()],
    );

    let parsed = parse_oracle_text(
        ORACLE,
        "Shimmerwilds Growth",
        &[],
        &["Enchantment".to_string()],
        &["Aura".to_string()],
    );
    let color_static = parsed
        .statics
        .iter()
        .find(|s| {
            s.modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::AddChosenColor { .. }))
        })
        .expect("Shimmerwilds Growth must produce an AddChosenColor static")
        .clone();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let aura = scenario.add_creature(P0, "Shimmerwilds Growth", 0, 0).id();
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();
    {
        // Mark the Aura as enchanting the land, stamp its chosen color = black.
        let obj = runner.state_mut().objects.get_mut(&aura).unwrap();
        obj.card_types.core_types = vec![CoreType::Enchantment];
        obj.base_card_types = obj.card_types.clone();
        obj.attached_to = Some(AttachTarget::Object(land));
        obj.chosen_attributes
            .push(ChosenAttribute::Color(ManaColor::Black));
        obj.static_definitions.push(color_static.clone());
        let base = std::sync::Arc::make_mut(&mut obj.base_static_definitions);
        base.push(color_static);
    }
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());

    assert!(
        runner.state().objects[&land]
            .color
            .contains(&ManaColor::Black),
        "the enchanted land must become the Aura's chosen color (black); got {:?}",
        runner.state().objects[&land].color
    );
}

// ---------------------------------------------------------------------------
// Questing Druid — "Whenever you cast a spell that's white, blue, black, or red,
// put a +1/+1 counter on this creature." A spell-color disjunction on a SpellCast
// trigger `valid_card` (CR 105.2 + CR 601.2a). Pre-fix the condition/effect split
// cut the color list at its first comma, so `valid_card` captured only white and
// the rest of the colors leaked into the effect as an Unimplemented node.
// ---------------------------------------------------------------------------

const QUESTING_DRUID: &str =
    "Whenever you cast a spell that's white, blue, black, or red, put a +1/+1 counter on this creature.";

fn colorless_mana(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit {
            color: ManaType::Colorless,
            source_id: ObjectId(0),
            pip_id: engine::types::mana::ManaPipId(0),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        })
        .collect()
}

/// Drive the pipeline to stack-empty, accepting any default targets and passing
/// priority. Mirrors the cast-pipeline harness used by the Taigam regression.
fn drive(runner: &mut GameRunner) {
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::TargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let t = target_slots[selection.current_slot]
                    .legal_targets
                    .first()
                    .cloned();
                runner
                    .act(GameAction::ChooseTarget { target: t })
                    .expect("choose cast target");
            }
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let t = target_slots[selection.current_slot]
                    .legal_targets
                    .first()
                    .cloned();
                runner
                    .act(GameAction::ChooseTarget { target: t })
                    .expect("choose trigger target");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() || runner.act(GameAction::PassPriority).is_err()
                {
                    break;
                }
            }
            _ => break,
        }
    }
}

/// Cast `spell` through the real pipeline and run to stack-empty.
fn cast_spell(runner: &mut GameRunner, spell: ObjectId) {
    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: Default::default(),
        })
        .expect("cast spell");
    drive(runner);
}

fn counters_on(runner: &GameRunner, id: ObjectId) -> u32 {
    *runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .unwrap_or(&0)
}

/// Casting a RED spell fires Questing Druid (red is one of the listed colors);
/// casting a GREEN spell does NOT (green is excluded). Both halves discriminate
/// the fix: pre-fix `valid_card` was only `HasColor(White)`, so the red spell
/// would not have matched and the counter would never land.
#[test]
fn questing_druid_triggers_only_on_listed_spell_colors() {
    assert_zero_unimplemented(QUESTING_DRUID, "Questing Druid", &creature_types(), &[]);

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let druid = scenario
        .add_creature_from_oracle(P0, "Questing Druid", 2, 2, QUESTING_DRUID)
        .id();
    // Two colorless-typed instants whose color we stamp directly; the trigger
    // reads the spell object's color, not its mana cost.
    let red_spell = scenario.add_spell_to_hand(P0, "Red Instant", true).id();
    let green_spell = scenario.add_spell_to_hand(P0, "Green Instant", true).id();
    scenario.with_mana_pool(P0, colorless_mana(6));
    let mut runner = scenario.build();
    {
        let r = runner.state_mut().objects.get_mut(&red_spell).unwrap();
        r.color = vec![ManaColor::Red];
        r.base_color = vec![ManaColor::Red];
    }
    {
        let g = runner.state_mut().objects.get_mut(&green_spell).unwrap();
        g.color = vec![ManaColor::Green];
        g.base_color = vec![ManaColor::Green];
    }

    assert_eq!(counters_on(&runner, druid), 0, "starts with no counters");

    // Green spell: NOT one of the listed colors — no trigger, no counter.
    cast_spell(&mut runner, green_spell);
    assert_eq!(
        counters_on(&runner, druid),
        0,
        "a green spell must NOT trigger Questing Druid (green is not listed)"
    );

    // Red spell: a listed color — the trigger fires and adds one +1/+1 counter.
    cast_spell(&mut runner, red_spell);
    assert_eq!(
        counters_on(&runner, druid),
        1,
        "a red spell must trigger Questing Druid (red is one of white/blue/black/red)"
    );
}
