//! CR 710: Kamigawa flip cards, end to end.
//!
//! Every test here builds its permanent from the card's VERBATIM Oracle text
//! and resolves the flip instruction through the real stack (`push_to_stack`
//! plus `GameAction::PassPriority` submitted through `apply()`), so the parser
//! dispatch, the `Effect` lowering, the effects dispatcher, and
//! `flip::flip_permanent` all run in production order.
//!
//! The rules-bearing assertion is CR 710.1c (`flip_keeps_color_and_mana_cost`):
//! a flip card's color and mana cost don't change when the permanent flips.
//! Reverting `flip::apply_flipped_face_to_object` to the double-faced
//! applicator (`printed_cards::apply_back_face_to_object`) blanks both and
//! fails exactly that test while every other assertion here still passes.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::game_object::BackFaceData;
use engine::game::layers::flush_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::stack;
use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, EffectKind};
use engine::types::actions::GameAction;
use engine::types::card::LayoutKind;
use engine::types::card_type::{CardType, CoreType, Supertype};
use engine::types::events::GameEvent;
use engine::types::game_state::{StackEntry, StackEntryKind};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
use engine::types::zones::Zone;

/// Bushi Tenderfoot's verbatim Oracle text (MTGJSON `text`, face a).
const BUSHI_TENDERFOOT_ORACLE: &str =
    "When a creature dealt damage by this creature this turn dies, flip this creature.";

/// `{W}` — Bushi Tenderfoot's printed mana cost (CR 202.1).
fn white_mana_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::White],
        generic: 0,
    }
}

/// Kenzo the Hardhearted — Bushi Tenderfoot's alternative half (CR 710.1b).
/// A real flip card's bottom half has NO printed mana cost, which is why
/// reusing the double-faced applicator would violate CR 710.1c.
fn kenzo_alternative_face() -> BackFaceData {
    BackFaceData {
        name: "Kenzo the Hardhearted".to_string(),
        power: Some(3),
        toughness: Some(4),
        loyalty: None,
        printed_loyalty: None,
        defense: None,
        card_types: CardType {
            supertypes: vec![Supertype::Legendary],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Human".to_string(), "Samurai".to_string()],
        },
        mana_cost: ManaCost::default(),
        keywords: vec![Keyword::DoubleStrike, Keyword::Bushido(2)],
        abilities: Vec::new(),
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: Vec::new(),
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: vec![],
        casting_options: vec![],
        layout_kind: Some(LayoutKind::Flip),
    }
}

/// Build a battlefield Bushi Tenderfoot from its verbatim Oracle text, with the
/// Kenzo half stashed exactly as `printed_cards::populate_back_face_if_dfc`
/// stores it for a `CardLayout::Flip` card.
fn bushi_tenderfoot_on_battlefield() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    let id = scenario
        .add_creature_from_oracle(P0, "Bushi Tenderfoot", 1, 1, BUSHI_TENDERFOOT_ORACLE)
        .id();
    let mut runner = scenario.build();
    {
        let object = runner.state_mut().objects.get_mut(&id).unwrap();
        object.mana_cost = white_mana_cost();
        object.base_mana_cost = object.mana_cost.clone();
        object.color = vec![ManaColor::White];
        object.base_color = object.color.clone();
        object.back_face = Some(kenzo_alternative_face());
    }
    (runner, id)
}

/// Akki Lavarunner's verbatim Oracle text (MTGJSON `text`, face a). Its flip
/// instruction uses the BARE OBJECT PRONOUN form ("flip it"), which 8 of the 19
/// corpus cards carrying a flip instruction use — the single most common
/// surface form, and the one that lowers to `TargetFilter::ParentTarget`.
const AKKI_LAVARUNNER_ORACLE: &str = "Haste
Whenever this creature deals damage to an opponent, flip it.";

/// Tok-Tok, Volcano Born — Akki Lavarunner's alternative half (CR 710.1b).
/// A 2/2 Legendary Creature — Goblin Shaman with a damage-prevention static.
fn tok_tok_alternative_face() -> BackFaceData {
    BackFaceData {
        name: "Tok-Tok, Volcano Born".to_string(),
        power: Some(2),
        toughness: Some(2),
        loyalty: None,
        printed_loyalty: None,
        defense: None,
        card_types: CardType {
            supertypes: vec![Supertype::Legendary],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Goblin".to_string(), "Shaman".to_string()],
        },
        mana_cost: ManaCost::default(),
        keywords: Vec::new(),
        abilities: Vec::new(),
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: Vec::new(),
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: vec![],
        casting_options: vec![],
        layout_kind: Some(LayoutKind::Flip),
    }
}

/// `{1}{R}` — Akki Lavarunner's printed mana cost (CR 202.1).
fn one_red_mana_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 1,
    }
}

/// Build a battlefield Akki Lavarunner from its verbatim Oracle text.
fn akki_lavarunner_on_battlefield() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    let id = scenario
        .add_creature_from_oracle(P0, "Akki Lavarunner", 1, 1, AKKI_LAVARUNNER_ORACLE)
        .id();
    let mut runner = scenario.build();
    {
        let object = runner.state_mut().objects.get_mut(&id).unwrap();
        object.mana_cost = one_red_mana_cost();
        object.base_mana_cost = object.mana_cost.clone();
        object.color = vec![ManaColor::Red];
        object.base_color = object.color.clone();
        object.back_face = Some(tok_tok_alternative_face());
    }
    (runner, id)
}

/// The parsed body of `source`'s first trigger — for Bushi Tenderfoot, the
/// "flip this creature" instruction.
///
/// Captured separately from resolution because flipping REPLACES the
/// permanent's text box (CR 710.1b): after the flip, Kenzo the Hardhearted has
/// no triggered ability at all, so the repeat-flip test must re-push the body
/// it captured before the flip.
fn captured_trigger_body(
    runner: &GameRunner,
    source: ObjectId,
    source_name: &str,
) -> AbilityDefinition {
    runner.state().objects[&source]
        .trigger_definitions
        .as_slice()
        .first()
        .and_then(|entry| entry.definition.execute.as_deref().cloned())
        .unwrap_or_else(|| panic!("{source_name} must parse a trigger with an execute body"))
}

/// Push `execute` onto the stack as a triggered ability of `source` and resolve
/// it by submitting real `GameAction::PassPriority` actions through `apply()`,
/// returning every `GameEvent` the engine emitted along the way.
fn resolve_trigger_body(
    runner: &mut GameRunner,
    source: ObjectId,
    source_name: &str,
    execute: &AbilityDefinition,
) -> Vec<GameEvent> {
    let ability = build_resolved_from_def(execute, source, P0);
    let entry_id = ObjectId(runner.state().next_object_id);
    runner.state_mut().next_object_id += 1;
    stack::push_to_stack(
        runner.state_mut(),
        StackEntry {
            id: entry_id,
            source_id: source,
            controller: P0,
            kind: StackEntryKind::TriggeredAbility {
                source_id: source,
                ability: Box::new(ability),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: source_name.to_string(),
                subject_match_count: None,
                die_result: None,
            },
        },
        &mut vec![],
    );

    let mut emitted = Vec::new();
    let initial_stack_len = runner.state().stack.len();
    for _ in 0..10 {
        if runner.state().stack.len() < initial_stack_len {
            break;
        }
        match runner.act(GameAction::PassPriority) {
            Ok(result) => emitted.extend(result.events),
            Err(_) => break,
        }
    }
    flush_layers(runner.state_mut());
    emitted
}

/// CR 710.1b: once the permanent is flipped, the alternative name, type line,
/// power, toughness, and text box apply instead of the normal ones.
#[test]
fn flip_applies_the_alternative_name_type_line_pt_and_text_box() {
    let (mut runner, bushi) = bushi_tenderfoot_on_battlefield();
    assert_eq!(runner.state().objects[&bushi].name, "Bushi Tenderfoot");
    assert!(!runner.state().objects[&bushi].flipped);

    let execute = captured_trigger_body(&runner, bushi, "Bushi Tenderfoot");
    let _ = resolve_trigger_body(&mut runner, bushi, "Bushi Tenderfoot", &execute);

    let object = &runner.state().objects[&bushi];
    assert!(object.flipped, "CR 710.4: the permanent is now flipped");
    assert_eq!(object.name, "Kenzo the Hardhearted");
    assert_eq!(
        (object.power, object.toughness),
        (Some(3), Some(4)),
        "CR 710.1b: the alternative power and toughness apply"
    );
    assert!(
        object.card_types.supertypes.contains(&Supertype::Legendary),
        "CR 710.1b: the alternative type line applies"
    );
    assert!(
        object
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Samurai"),
        "CR 710.1b: the alternative type line applies"
    );
    assert!(
        object.keywords.contains(&Keyword::DoubleStrike),
        "CR 710.1b: the alternative text box applies"
    );
    assert!(
        object.trigger_definitions.as_slice().is_empty(),
        "CR 710.1b + CR 710.2: the normal half's triggered ability no longer          applies once the permanent is flipped"
    );
}

/// CR 710.1c: a flip card's color and mana cost DON'T change if the permanent
/// is flipped.
///
/// This is the assertion that fails if `flip::apply_flipped_face_to_object` is
/// replaced by the double-faced applicator: Kenzo's half has no printed mana
/// cost and no color, so the permanent would become a colorless {0} object.
#[test]
fn flip_keeps_color_and_mana_cost() {
    let (mut runner, bushi) = bushi_tenderfoot_on_battlefield();

    let execute = captured_trigger_body(&runner, bushi, "Bushi Tenderfoot");
    let _ = resolve_trigger_body(&mut runner, bushi, "Bushi Tenderfoot", &execute);

    let object = &runner.state().objects[&bushi];
    assert!(
        object.flipped,
        "reach guard: the permanent actually flipped"
    );
    assert_eq!(object.name, "Kenzo the Hardhearted", "reach guard");
    assert_eq!(
        object.mana_cost,
        white_mana_cost(),
        "CR 710.1c: a flip card's mana cost doesn't change when it flips"
    );
    assert_eq!(
        object.base_mana_cost,
        white_mana_cost(),
        "CR 710.1c: the printed baseline keeps the normal half's mana cost"
    );
    assert_eq!(
        object.color,
        vec![ManaColor::White],
        "CR 710.1c: a flip card's color doesn't change when it flips"
    );
    assert_eq!(object.base_color, vec![ManaColor::White]);
}

/// CR 710.4: flipping a permanent is a one-way process. A second flip
/// instruction does nothing and emits no second `Flipped` event.
#[test]
fn flipping_an_already_flipped_permanent_is_a_no_op() {
    let (mut runner, bushi) = bushi_tenderfoot_on_battlefield();

    let execute = captured_trigger_body(&runner, bushi, "Bushi Tenderfoot");
    let first = resolve_trigger_body(&mut runner, bushi, "Bushi Tenderfoot", &execute);
    assert!(runner.state().objects[&bushi].flipped, "reach guard");
    assert_eq!(
        first
            .iter()
            .filter(
                |event| matches!(event, GameEvent::Flipped { object_id } if *object_id == bushi)
            )
            .count(),
        1,
        "reach guard: the first instruction really did flip the permanent"
    );
    let name_after_first = runner.state().objects[&bushi].name.clone();

    let second = resolve_trigger_body(&mut runner, bushi, "Bushi Tenderfoot", &execute);

    // Reach guard: the second flip effect actually resolved (got past dispatch
    // into `flip_permanent::resolve`), so the absence of a second `Flipped`
    // event below is the CR 710.4 one-way no-op — not an upstream short-circuit
    // that skipped the effect entirely.
    assert!(
        second.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::FlipPermanent,
                ..
            }
        )),
        "the repeat FlipPermanent effect must reach the resolver even when it no-ops"
    );

    let state = runner.state();
    assert!(state.objects[&bushi].flipped);
    assert_eq!(
        state.objects[&bushi].name, name_after_first,
        "CR 710.4: the permanent cannot flip back or flip again"
    );
    assert_eq!(
        (state.objects[&bushi].power, state.objects[&bushi].toughness),
        (Some(3), Some(4))
    );
    assert_eq!(
        second
            .iter()
            .filter(
                |event| matches!(event, GameEvent::Flipped { object_id } if *object_id == bushi)
            )
            .count(),
        0,
        "CR 710.4: the repeat instruction emits no second Flipped event"
    );
}

/// CR 710.2 + CR 710.4 + CR 110.5: in every zone other than the battlefield a
/// flip card has only the normal characteristics of the card, and a flipped
/// permanent that leaves the battlefield retains no memory of its status.
#[test]
fn a_flipped_permanent_leaving_the_battlefield_shows_only_normal_characteristics() {
    let (mut runner, bushi) = bushi_tenderfoot_on_battlefield();

    let execute = captured_trigger_body(&runner, bushi, "Bushi Tenderfoot");
    let _ = resolve_trigger_body(&mut runner, bushi, "Bushi Tenderfoot", &execute);
    assert!(runner.state().objects[&bushi].flipped, "reach guard");
    assert_eq!(runner.state().objects[&bushi].name, "Kenzo the Hardhearted");

    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), bushi, Zone::Graveyard, &mut events);

    let object = &runner.state().objects[&bushi];
    assert_eq!(object.zone, Zone::Graveyard);
    assert!(
        !object.flipped,
        "CR 110.5b + CR 710.4: the card in the graveyard is not flipped"
    );
    assert_eq!(
        object.name, "Bushi Tenderfoot",
        "CR 710.2: only the normal characteristics apply off the battlefield"
    );
    assert_eq!((object.power, object.toughness), (Some(1), Some(1)));
    assert!(!object.card_types.supertypes.contains(&Supertype::Legendary));
    assert_eq!(
        object.mana_cost,
        white_mana_cost(),
        "CR 710.1c: the mana cost was never changed, so the revert restores it unchanged"
    );
}

/// CR 710.1b + CR 710.2: a flip card sitting in a non-battlefield zone cannot
/// be flipped at all — the alternative characteristics exist only for a
/// battlefield permanent.
#[test]
fn a_flip_card_off_the_battlefield_cannot_flip() {
    let (mut runner, bushi) = bushi_tenderfoot_on_battlefield();
    let mut events = Vec::new();
    engine::game::zones::move_to_zone(runner.state_mut(), bushi, Zone::Graveyard, &mut events);

    let execute = captured_trigger_body(&runner, bushi, "Bushi Tenderfoot");
    let events = resolve_trigger_body(&mut runner, bushi, "Bushi Tenderfoot", &execute);

    // Reach guard: the flip effect actually resolved (got past dispatch into
    // `flip_permanent::resolve`), so the unchanged object below reflects the CR
    // 710.2 off-battlefield no-op, not an upstream short-circuit that never ran.
    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::EffectResolved {
                kind: EffectKind::FlipPermanent,
                ..
            }
        )),
        "the FlipPermanent effect must reach the resolver even when it no-ops"
    );

    let object = &runner.state().objects[&bushi];
    assert!(!object.flipped);
    assert_eq!(object.name, "Bushi Tenderfoot");
    assert_eq!((object.power, object.toughness), (Some(1), Some(1)));
}

/// The verbatim Oracle text (MTGJSON `text`, face a) of every printed flip card
/// that carries a flip INSTRUCTION — 19 of the 21 CR 710 flip cards.
///
/// The two excluded cards carry no flip instruction and are documented
/// out-of-scope in `game::flip`'s module docs:
/// - Homura, Human Ascendant — "return it to the battlefield flipped" is an
///   entry-time rider (CR 110.5b), not a flip instruction.
/// - Curse of the Fire Penguin — an Un-set Aura with no flip verb at all.
const FLIP_INSTRUCTION_CORPUS: &[(&str, &str)] = &[
    ("Akki Lavarunner", "Haste
Whenever this creature deals damage to an opponent, flip it."),
    ("Budoka Gardener", "{T}: You may put a land card from your hand onto the battlefield. If you control ten or more lands, flip this creature."),
    ("Budoka Pupil", "Whenever you cast a Spirit or Arcane spell, you may put a ki counter on this creature.
At the beginning of the end step, if there are two or more ki counters on this creature, you may flip it."),
    ("Bushi Tenderfoot", "When a creature dealt damage by this creature this turn dies, flip this creature."),
    ("Callow Jushi", "Whenever you cast a Spirit or Arcane spell, you may put a ki counter on this creature.
At the beginning of the end step, if there are two or more ki counters on this creature, you may flip it."),
    ("Cunning Bandit", "Whenever you cast a Spirit or Arcane spell, you may put a ki counter on this creature.
At the beginning of the end step, if there are two or more ki counters on this creature, you may flip it."),
    ("Erayo, Soratami Ascendant", "Flying
Whenever the fourth spell of a turn is cast, flip Erayo."),
    ("Faithful Squire", "Whenever you cast a Spirit or Arcane spell, you may put a ki counter on this creature.
At the beginning of the end step, if there are two or more ki counters on this creature, you may flip it."),
    ("Hired Muscle", "Whenever you cast a Spirit or Arcane spell, you may put a ki counter on this creature.
At the beginning of the end step, if there are two or more ki counters on this creature, you may flip it."),
    ("Initiate of Blood", "{T}: This creature deals 1 damage to target creature that was dealt damage this turn. When that creature dies this turn, flip this creature."),
    ("Jushi Apprentice", "{2}{U}, {T}: Draw a card. If you have nine or more cards in hand, flip this creature."),
    ("Kitsune Mystic", "At the beginning of the end step, if this creature is enchanted by two or more Auras, flip it."),
    ("Kuon, Ogre Ascendant", "At the beginning of the end step, if three or more creatures died this turn, flip Kuon."),
    ("Nezumi Graverobber", "{1}{B}: Exile target card from an opponent's graveyard. If no cards are in that graveyard, flip this creature."),
    ("Nezumi Shortfang", "{1}{B}, {T}: Target opponent discards a card. Then if that player has no cards in hand, flip this creature."),
    ("Orochi Eggwatcher", "{2}{G}, {T}: Create a 1/1 green Snake creature token. If you control ten or more creatures, flip this creature."),
    ("Rune-Tail, Kitsune Ascendant", "When you have 30 or more life, flip Rune-Tail."),
    ("Sasaya, Orochi Ascendant", "Reveal your hand: If you have seven or more land cards in your hand, flip Sasaya."),
    ("Student of Elements", "When this creature has flying, flip it."),
];

/// True when any effect anywhere in `definition`'s chain is `FlipPermanent`,
/// including inside a delayed-trigger body (Initiate of Blood's "When that
/// creature dies this turn, flip this creature" rider, CR 603.7a).
fn chain_contains_flip_permanent(definition: &AbilityDefinition) -> bool {
    if matches!(*definition.effect, Effect::FlipPermanent { .. }) {
        return true;
    }
    if let Effect::CreateDelayedTrigger { effect, .. } = definition.effect.as_ref() {
        if chain_contains_flip_permanent(effect) {
            return true;
        }
    }
    definition
        .sub_ability
        .iter()
        .chain(definition.else_ability.iter())
        .map(|boxed| boxed.as_ref())
        .chain(definition.mode_abilities.iter())
        .any(chain_contains_flip_permanent)
}

/// CR 710.4: EVERY printed flip card's flip instruction lowers to
/// `Effect::FlipPermanent`, across all three surface forms — "flip this
/// creature" (7 cards), "flip it" (8 cards), and the by-name "flip &lt;name&gt;"
/// (4 cards, normalized to `~` upstream) — and across both carriers (activated
/// abilities and triggered abilities). This is the build-for-the-class proof:
/// the recognizer is not tuned to one card's phrasing.
#[test]
fn every_printed_flip_card_lowers_its_instruction_to_flip_permanent() {
    let mut missing = Vec::new();
    for (card_name, oracle) in FLIP_INSTRUCTION_CORPUS {
        let parsed = engine::parser::oracle::parse_oracle_text(oracle, card_name, &[], &[], &[]);
        let found = parsed.abilities.iter().any(chain_contains_flip_permanent)
            || parsed.triggers.iter().any(|trigger| {
                trigger
                    .execute
                    .as_deref()
                    .is_some_and(chain_contains_flip_permanent)
            });
        if !found {
            missing.push(*card_name);
        }
    }
    assert!(
        missing.is_empty(),
        "these flip cards did not lower their flip instruction to Effect::FlipPermanent: {missing:?}"
    );
}

/// CR 710.4 + CR 608.2k: the BARE OBJECT PRONOUN form ("flip it") — an
/// untargeted back-reference to the object named by the trigger condition —
/// resolves and flips the ability's own source, end to end.
///
/// This is the surface form 8 of the 19 corpus cards use (Akki Lavarunner, the
/// five ki-counter Ascendants, Kitsune Mystic, Student of Elements), and it
/// lowers to a different `TargetFilter` than the self-deictic "flip this
/// creature" form: `resolve_pronoun_target` yields `TargetFilter::ParentTarget`
/// whenever the parse context has no non-self trigger subject, which is true for
/// all eight. It reaches `flip_permanent` only through
/// `effects::flip_permanent`'s empty-`targets` arm (`[] => ability.source_id`).
///
/// Discriminating: break that arm (return an error, or resolve `ParentTarget`
/// to anything other than the source) and the permanent never flips — the
/// `flipped` / name / power assertions below all fail. The parse assertion
/// alone would not catch it; this drives the real stack via
/// `GameAction::PassPriority` through `apply()`.
#[test]
fn the_flip_it_pronoun_form_flips_its_own_source_end_to_end() {
    let (mut runner, akki) = akki_lavarunner_on_battlefield();
    assert_eq!(runner.state().objects[&akki].name, "Akki Lavarunner");
    assert!(!runner.state().objects[&akki].flipped);

    let execute = captured_trigger_body(&runner, akki, "Akki Lavarunner");
    // Reach guard: the "flip it" form really does take the ParentTarget branch
    // (not the SelfRef branch the "flip this creature" cards take), so this test
    // exercises the arm the majority of the corpus depends on.
    assert!(
        matches!(
            *execute.effect,
            Effect::FlipPermanent {
                target: engine::types::ability::TargetFilter::ParentTarget
            }
        ),
        "reach guard: 'flip it' must lower to ParentTarget, got {:?}",
        execute.effect
    );

    let emitted = resolve_trigger_body(&mut runner, akki, "Akki Lavarunner", &execute);

    let object = &runner.state().objects[&akki];
    assert!(object.flipped, "CR 710.4: the permanent is now flipped");
    assert_eq!(object.name, "Tok-Tok, Volcano Born");
    assert_eq!(
        (object.power, object.toughness),
        (Some(2), Some(2)),
        "CR 710.1b: the alternative power and toughness apply"
    );
    assert!(
        object.card_types.supertypes.contains(&Supertype::Legendary),
        "CR 710.1b: the alternative type line applies"
    );
    assert_eq!(
        object.mana_cost,
        one_red_mana_cost(),
        "CR 710.1c: a flip card's mana cost doesn't change when it flips"
    );
    assert_eq!(object.color, vec![ManaColor::Red]);
    assert_eq!(
        emitted
            .iter()
            .filter(|event| matches!(event, GameEvent::Flipped { object_id } if *object_id == akki))
            .count(),
        1,
        "exactly one Flipped event for the source"
    );
}

/// CR 705.1 vs CR 710.4: the coin-flip mechanic is untouched — "flip a coin"
/// still lowers to `Effect::FlipCoin`, not to the new flip-permanent effect.
#[test]
fn flip_a_coin_still_parses_to_flip_coin() {
    let definition =
        engine::parser::oracle_effect::parse_effect_chain("Flip a coin.", AbilityKind::Spell);
    assert!(
        matches!(*definition.effect, Effect::FlipCoin { .. }),
        "CR 705.1: 'flip a coin' must stay a coin flip, got {:?}",
        definition.effect
    );

    let until_lose = engine::parser::oracle_effect::parse_effect_chain(
        "Flip a coin until you lose a flip.",
        AbilityKind::Spell,
    );
    assert!(
        matches!(*until_lose.effect, Effect::FlipCoinUntilLose { .. }),
        "CR 705.1: the Krark-style repeat coin flip is unaffected, got {:?}",
        until_lose.effect
    );
}
