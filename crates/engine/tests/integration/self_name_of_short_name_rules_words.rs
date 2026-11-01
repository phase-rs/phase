//! CR 201.5c: only instances of a card's shortened name *used to refer to the card* are
//! treated as its name. The "of"-derived short name of a card ("Life" for Life of Toshiro
//! Umezawa) must not rewrite the same word where it is ordinary rules text: a lower-case
//! rules word ("You gain 2 life.", "glyph counters"), a sentence-initial instruction verb
//! ("Return all …"), or a subtype noun after a determiner ("Sacrifice a Wall", "destroy
//! target Knight", "another Cleric you control"). Each was rewritten to `~` and either
//! became an `Unimplemented` no-op or silently lost its type restriction.
//!
//! Cards (Oracle verified against Scryfall):
//! - Life of Toshiro Umezawa (NEO, Saga front face): chapter I/II mode "You gain 2 life."
//! - Return of the Nightstalkers (P02): "Return all Nightstalker permanent cards from your
//!   graveyard to the battlefield. Then destroy all Swamps you control."
//! - Glyph of Delusion (LEG): "Put X glyph counters on target creature …"
//! - Wall of Mulch (Creature — Wall): "{G}, Sacrifice a Wall: Draw a card."
//! - Knight of the Mists (Creature — Human Knight): "When this creature enters, you may pay
//!   {U}. If you don't, destroy target Knight and it can't be regenerated."
//! - Cleric of Life's Bond (Creature — Vampire Cleric): "Whenever another Cleric you control
//!   enters, you gain 1 life."
//!
//! CR 700.2b + CR 714.2b (modal chapter ability), CR 714.3c (lore counter in main phase).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::parser::parse_oracle_text;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const TOSHIRO: &str = "(As this Saga enters and after your draw step, add a lore counter.)\nI, II \u{2014} Choose one \u{2014}\n\u{2022} Target creature gets +2/+2 until end of turn.\n\u{2022} Target creature gets -1/-1 until end of turn.\n\u{2022} You gain 2 life.\nIII \u{2014} Exile this Saga, then return it to the battlefield transformed under your control.";

/// Completion boundary: the operation under test finished with priority back and an empty
/// stack, rather than stopping at an unanswered prompt or at a driver loop's bound.
#[track_caller]
fn assert_settled(runner: &GameRunner, what: &str) {
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
            && runner.state().stack.is_empty(),
        "boundary: {what} must finish at Priority with an empty stack, got {:?} with {} stack item(s)",
        runner.state().waiting_for,
        runner.state().stack.len()
    );
}

const NIGHTSTALKERS: &str = "Return all Nightstalker permanent cards from your graveyard to the battlefield. Then destroy all Swamps you control.";

/// Mirror of the `modal_saga_chapter_runtime` driver: move P0 into its precombat main so
/// CR 714.3c adds lore counter I, pick `mode` at the CR 700.2b mode-choice window, take the
/// first legal target for any target prompt, and resolve the stack. Returns whether the
/// mode-choice window was offered.
fn fire_chapter_one(runner: &mut GameRunner, saga: ObjectId, mode: usize) -> bool {
    {
        let state = runner.state_mut();
        state.turn_number = 2;
        state.active_player = P0;
        state.phase = Phase::Upkeep;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }
    runner.advance_to_phase(Phase::PreCombatMain);
    let lore = runner.state().objects[&saga]
        .counters
        .get(&CounterType::Lore)
        .copied()
        .unwrap_or(0);
    assert_eq!(
        lore, 1,
        "reach: CR 714.3c must add lore counter I (chapter I fires)"
    );

    let mut chose_mode = false;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::ModeChoice { .. } | WaitingFor::AbilityModeChoice { .. } => {
                runner
                    .act(GameAction::SelectModes {
                        indices: vec![mode],
                    })
                    .expect("defect: the \"You gain 2 life.\" mode must be selectable");
                chose_mode = true;
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .choose_first_legal_target()
                    .expect("choose the first legal target");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt while resolving chapter I: {other:?}"),
        }
    }
    assert_settled(runner, "chapter I");
    chose_mode
}

/// CR 201.5c + CR 700.2b: "Life" in "You gain 2 life." is the rules word, not a shortened
/// name for Life of Toshiro Umezawa, so choosing that mode gains exactly 2 life.
#[test]
fn life_of_toshiro_umezawa_gain_two_life_mode_gains_life() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let saga = scenario
        .add_creature(P0, "Life of Toshiro Umezawa", 0, 0)
        .as_enchantment()
        .with_subtypes(vec!["Saga"])
        .from_oracle_text(TOSHIRO)
        .id();
    scenario.add_creature(P0, "Grizzly Bears", 2, 2);
    scenario.with_library_top(P0, &["Plains"; 10]);
    scenario.with_library_top(P1, &["Plains"; 10]);
    let mut runner = scenario.build();
    let life_before = runner.state().players[0].life;

    let chose_mode = fire_chapter_one(&mut runner, saga, 2);

    assert!(
        chose_mode,
        "reach: chapter I must offer the CR 700.2b mode-choice window"
    );
    assert_eq!(
        runner.state().players[0].life,
        life_before + 2,
        "defect: the chosen \"You gain 2 life.\" mode must gain 2 life \
         (baseline parses it as Unimplemented \"gain 2 ~\")"
    );
}

/// CR 201.5c: the sentence-initial verb "Return" is not a shortened name for Return of the
/// Nightstalkers, so the spell returns the Nightstalker card and then destroys the Swamp.
#[test]
fn return_of_the_nightstalkers_returns_nightstalker_cards() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Return of the Nightstalkers", false, NIGHTSTALKERS)
        .id();
    let stalker = scenario
        .add_creature_to_graveyard(P0, "Abyssal Nightstalker", 2, 2)
        .with_subtypes(vec!["Nightstalker"])
        .id();
    let swamp = scenario.add_basic_land(P0, ManaColor::Black);
    let mut runner = scenario.build();

    let outcome = runner.cast(spell).resolve();
    assert_settled(&runner, "Return of the Nightstalkers");

    assert_eq!(
        outcome.zone_of(spell),
        Zone::Graveyard,
        "reach: the spell was cast and resolved; final {:?}",
        outcome.final_waiting_for()
    );
    assert_eq!(
        outcome.zone_of(stalker),
        Zone::Battlefield,
        "defect: the Nightstalker permanent card must return from the graveyard \
         (baseline parses the clause as Unimplemented \"~ all Nightstalker …\")"
    );
    assert_eq!(
        outcome.zone_of(swamp),
        Zone::Graveyard,
        "defect: \"Then destroy all Swamps you control.\" must also apply"
    );
}

const GLYPH_OF_DELUSION: &str = "Put X glyph counters on target creature that target Wall blocked this turn, where X is the power of that blocked creature. The creature gains \"This creature doesn't untap during your untap step if it has a glyph counter on it\" and \"At the beginning of your upkeep, remove a glyph counter from this creature.\"";

// The activated-ability line only: the scenario parser has no MTGJSON keyword hints, so the
// printed "Defender" line would otherwise occupy ability index 0.
const WALL_OF_MULCH: &str = "{G}, Sacrifice a Wall: Draw a card.";

const KNIGHT_OF_THE_MISTS: &str = "Flanking (Whenever a creature without flanking blocks this creature, the blocking creature gets -1/-1 until end of turn.)\nWhen this creature enters, you may pay {U}. If you don't, destroy target Knight and it can't be regenerated.";

const CLERIC_OF_LIFES_BOND: &str = "Whenever another Cleric you control enters, you gain 1 life.\nWhenever you gain life for the first time each turn, put a +1/+1 counter on this creature.";

fn mana(units: &[ManaType]) -> Vec<ManaUnit> {
    units
        .iter()
        .map(|&mana_type| ManaUnit::new(mana_type, ObjectId(0), false, vec![]))
        .collect()
}

/// CR 201.5c: "glyph" in "glyph counters" is the counter's name, not a shortened name
/// for Glyph of Delusion. Every counter the card names stays a glyph counter. (The spell's
/// "that target Wall blocked this turn" clause is still unsupported and stays an honest
/// `Unimplemented`; this test covers only the name normalization.)
#[test]
fn glyph_of_delusion_keeps_glyph_counter_name() {
    let parsed = parse_oracle_text(
        GLYPH_OF_DELUSION,
        "Glyph of Delusion",
        &[],
        &["Instant".to_owned()],
        &[],
    );
    let rendered = format!("{parsed:?}");
    // The granted bodies' "This creature" is a self-reference of the creature that gains
    // them, so `~` is expected there; only the counter name is under test.
    for counter_phrase in [
        "Put X glyph counters",
        "if it has a glyph counter",
        "remove a glyph counter",
    ] {
        assert!(
            rendered.contains(counter_phrase),
            "defect: {counter_phrase:?} must keep the glyph counter name, got {rendered}"
        );
    }
}

/// CR 201.5c + CR 205.3a: "Sacrifice a Wall" names any Wall, not Wall of Mulch itself.
/// The cost prompt offers every Wall the controller has and no non-Wall; paying with the
/// other Wall draws a card and leaves Wall of Mulch on the battlefield.
#[test]
fn wall_of_mulch_sacrifices_any_wall_but_no_non_wall() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mulch = scenario
        .add_creature_from_oracle(P0, "Wall of Mulch", 0, 4, WALL_OF_MULCH)
        .with_subtypes(vec!["Wall"])
        .id();
    let other_wall = scenario
        .add_creature(P0, "Wall of Wood", 0, 3)
        .with_subtypes(vec!["Wall"])
        .id();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    // Hostile: shares the source's name but is not a Wall. "Sacrifice a Wall" is a type
    // filter, so the name match must not make it payable.
    let namesake = scenario.add_creature(P0, "Wall of Mulch", 0, 4).id();
    scenario.with_mana_pool(P0, mana(&[ManaType::Green]));
    scenario.with_library_top(P0, &["Plains"; 3]);
    let mut runner = scenario.build();
    let hand_before = runner.state().players[0].hand.len();

    runner
        .act(GameAction::ActivateAbility {
            source_id: mulch,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate Wall of Mulch");
    let WaitingFor::PayCost { choices, .. } = runner.state().waiting_for.clone() else {
        panic!(
            "reach: the sacrifice cost must prompt for a Wall, got {:?}",
            runner.state().waiting_for
        );
    };
    assert!(
        choices.contains(&mulch) && choices.contains(&other_wall),
        "every Wall the controller has is a legal sacrifice, got {choices:?}"
    );
    assert!(
        !choices.contains(&bears),
        "defect: a non-Wall creature must not pay \"Sacrifice a Wall\", got {choices:?}"
    );
    assert!(
        !choices.contains(&namesake),
        "a non-Wall sharing the source's name must not pay \"Sacrifice a Wall\", got {choices:?}"
    );

    runner
        .act(GameAction::SelectCards {
            cards: vec![other_wall],
        })
        .expect("sacrifice the other Wall");
    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } | WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected prompt while resolving Wall of Mulch: {other:?}"),
        }
    }
    assert_settled(&runner, "the Wall of Mulch activation");

    assert_eq!(runner.state().objects[&other_wall].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&mulch].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before + 1,
        "the ability draws a card"
    );
}

/// Cast Knight of the Mists, take the ETB trigger's target slot through `choose`, decline
/// the optional {U} payment, and resolve the stack. Returns the slot's legal targets.
fn cast_knight_of_the_mists(
    runner: &mut GameRunner,
    knight: ObjectId,
    choose: impl Fn(&[TargetRef]) -> TargetRef,
) -> Vec<TargetRef> {
    let card_id = runner.state().objects[&knight].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: knight,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Knight of the Mists");
    let mut legal = None;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } | WaitingFor::ManaPayment { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let slot = &target_slots[selection.current_slot];
                let target = choose(&slot.legal_targets);
                legal = Some(slot.legal_targets.clone());
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(target),
                    })
                    .expect("choose the trigger target");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: false })
                    .expect("decline the optional {U} payment");
            }
            other => panic!("unexpected prompt while resolving Knight of the Mists: {other:?}"),
        }
    }
    assert_settled(runner, "Knight of the Mists and its trigger");
    legal.expect("reach: the ETB trigger must prompt for its \"target Knight\"")
}

/// CR 201.5c + CR 205.3a: "destroy target Knight" targets a Knight, not Knight of the
/// Mists by name. An opponent's Knight is a legal target and is destroyed; a non-Knight
/// creature is never offered.
#[test]
fn knight_of_the_mists_targets_any_knight_but_no_non_knight() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let knight = scenario
        .add_creature_to_hand_from_oracle(P0, "Knight of the Mists", 2, 2, KNIGHT_OF_THE_MISTS)
        .with_subtypes(vec!["Human", "Knight"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 2,
        })
        .id();
    let rival_knight = scenario
        .add_creature(P1, "White Knight", 2, 2)
        .with_subtypes(vec!["Human", "Knight"])
        .id();
    let bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    // Hostile: shares the source's name but is not a Knight. "target Knight" is a type
    // filter, so the name match must not make it legal.
    let namesake = scenario.add_creature(P1, "Knight of the Mists", 2, 2).id();
    scenario.with_mana_pool(
        P0,
        mana(&[ManaType::Blue, ManaType::Colorless, ManaType::Colorless]),
    );
    let mut runner = scenario.build();

    let legal = cast_knight_of_the_mists(&mut runner, knight, |_| TargetRef::Object(rival_knight));

    assert!(
        legal.contains(&TargetRef::Object(rival_knight)),
        "an opponent's Knight is a legal target, got {legal:?}"
    );
    assert!(
        !legal.contains(&TargetRef::Object(bears)),
        "defect: a non-Knight creature must not be a legal \"target Knight\", got {legal:?}"
    );
    assert!(
        !legal.contains(&TargetRef::Object(namesake)),
        "a non-Knight sharing the source's name must not be a legal \"target Knight\", got {legal:?}"
    );
    assert_eq!(runner.state().objects[&rival_knight].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&bears].zone, Zone::Battlefield);
}

/// CR 201.5c + CR 205.3a: "another Cleric you control" matches any other Cleric, so a
/// Cleric entering gains 1 life and a non-Cleric entering gains none.
#[test]
fn cleric_of_lifes_bond_triggers_on_another_cleric_only() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Cleric of Life's Bond", 2, 2, CLERIC_OF_LIFES_BOND)
        .with_subtypes(vec!["Vampire", "Cleric"]);
    let bears = scenario
        .add_creature_to_hand(P0, "Grizzly Bears", 2, 2)
        .id();
    // Hostile: shares the source's name but is not a Cleric.
    let namesake = scenario
        .add_creature_to_hand(P0, "Cleric of Life's Bond", 2, 2)
        .id();
    let cleric = scenario
        .add_creature_to_hand(P0, "Acolyte", 1, 1)
        .with_subtypes(vec!["Human", "Cleric"])
        .id();
    let mut runner = scenario.build();
    let life_before = runner.state().players[0].life;

    let outcome = runner.cast(bears).resolve();
    assert_settled(&runner, "casting bears and its triggers");
    assert_eq!(
        outcome.zone_of(bears),
        Zone::Battlefield,
        "reach: the non-Cleric entered"
    );
    assert_eq!(
        runner.state().players[0].life,
        life_before,
        "a non-Cleric entering must not trigger \"another Cleric you control\""
    );

    let outcome = runner.cast(namesake).resolve();
    assert_settled(&runner, "casting namesake and its triggers");
    assert_eq!(
        outcome.zone_of(namesake),
        Zone::Battlefield,
        "reach: the non-Cleric namesake entered"
    );
    assert_eq!(
        runner.state().players[0].life,
        life_before,
        "a non-Cleric sharing the source's name must not trigger \"another Cleric\""
    );

    let outcome = runner.cast(cleric).resolve();
    assert_settled(&runner, "casting cleric and its triggers");
    assert_eq!(
        outcome.zone_of(cleric),
        Zone::Battlefield,
        "reach: the Cleric entered"
    );
    assert_eq!(
        runner.state().players[0].life,
        life_before + 1,
        "defect: another Cleric entering must gain 1 life"
    );
}

const FEAR_OF_FEAR_ITSELF: &str = "{2}: Target creature gains fear until end of turn. (It can't be blocked except by artifact creatures and/or black creatures.)";

const MINN: &str = "Whenever you draw your second card each turn, create a 1/1 blue Illusion creature token with \"This token gets +1/+0 for each other Illusion you control.\"\nWhenever an Illusion you control dies, you may put a permanent card with mana value less than or equal to that creature's power from your hand onto the battlefield.";

/// CR 201.5c + CR 702.36a: "gains fear" grants the Fear keyword; "fear" is not a shortened
/// name for Fear of Fear Itself.
#[test]
fn fear_of_fear_itself_grants_fear() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Fear of Fear Itself", FEAR_OF_FEAR_ITSELF)
        .id();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    scenario.with_mana_pool(P0, mana(&[ManaType::Colorless, ManaType::Colorless]));
    let mut runner = scenario.build();
    assert!(
        !runner.state().objects[&bears]
            .keywords
            .contains(&Keyword::Fear),
        "reach: the target starts without fear"
    );

    runner.activate(source, 0).target_object(bears).resolve();
    assert_settled(&runner, "the Fear of Fear Itself activation");

    assert!(
        runner.state().objects[&bears]
            .keywords
            .contains(&Keyword::Fear),
        "defect: the target creature must gain fear"
    );
}

/// Cast a "Draw two cards." sorcery so Minn's second-draw trigger creates its Illusion token,
/// and return the token's power.
fn minn_token_power(other_illusions: usize) -> i32 {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Minn, Wily Illusionist", 1, 4, MINN)
        .as_legendary()
        .with_subtypes(vec!["Gnome", "Wizard"]);
    for _ in 0..other_illusions {
        scenario
            .add_creature(P0, "Phantasmal Bear", 2, 2)
            .with_subtypes(vec!["Illusion", "Bear"]);
    }
    // An opponent's Illusion is not "you control".
    scenario
        .add_creature(P1, "Phantasmal Bear", 2, 2)
        .with_subtypes(vec!["Illusion", "Bear"]);
    let draw_two = scenario
        .add_spell_to_hand_from_oracle(P0, "Divination", false, "Draw two cards.")
        .id();
    scenario.with_library_top(P0, &["Plains"; 5]);
    let mut runner = scenario.build();

    runner.cast(draw_two).resolve();
    assert_settled(&runner, "Divination and Minn's trigger");

    let tokens: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|object| object.is_token && object.zone == Zone::Battlefield)
        .collect();
    assert_eq!(
        tokens.len(),
        1,
        "reach: the second draw must create exactly one Illusion token"
    );
    tokens[0].power.expect("the token is a creature")
}

/// CR 201.5c + CR 205.3a: inside the token named "Illusion", "each other Illusion you
/// control" counts Illusions, not the token by name. Two other Illusions make it 3/1.
#[test]
fn minn_illusion_token_counts_other_illusions() {
    assert_eq!(
        minn_token_power(2),
        3,
        "defect: the token gets +1/+0 for each of the two other Illusions you control"
    );
}

/// Control for the dynamic count: with no other Illusion you control, the token stays 1/1.
#[test]
fn minn_illusion_token_without_other_illusions_stays_one_one() {
    assert_eq!(
        minn_token_power(0),
        1,
        "defect: no other Illusion you control means no bonus"
    );
}
