//! Ebondeath, Dracolich (AFR 100) — "You may cast this card from your graveyard
//! if a creature not named Ebondeath, Dracolich died this turn."
//!
//! The gate is a NAME predicate (CR 201.2) over this turn's deaths (CR 700.4).
//! It reads the name the creature had while it was last on the battlefield: the
//! 2021-07-23 ruling says it "cares what the creature's name was while it was
//! last on the battlefield, not what it is in the graveyard". It is not a
//! self-exclusion: a different Ebondeath dying never opens it, and Ebondeath
//! itself dying doesn't close it when another creature also died.
//!
//! Every negative carries a positive reach guard in the same scenario, and every
//! offer is checked on both the offer authority (`spell_objects_available_to_cast`,
//! which the AI candidate generator reads) and `can_cast_object_now`.

use engine::game::casting::{can_cast_object_now, spell_objects_available_to_cast};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::parser::oracle::parse_oracle_text;
use engine::parser::oracle_nom::condition::parse_inner_condition;
use engine::types::ability::{
    Comparator, ControllerRef, FilterProp, QuantityExpr, QuantityRef, StaticCondition,
    TargetFilter, TypeFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::CastPaymentMode;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const EBONDEATH_NAME: &str = "Ebondeath, Dracolich";
const EBONDEATH: &str = "Flash\nFlying\nEbondeath enters tapped.\nYou may cast this card from your graveyard if a creature not named Ebondeath, Dracolich died this turn.";

fn strings(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| s.to_string()).collect()
}

fn ebondeath_in_graveyard(scenario: &mut GameScenario, owner: PlayerId) -> ObjectId {
    scenario
        .add_creature_to_graveyard(owner, EBONDEATH_NAME, 5, 2)
        .with_subtypes(vec!["Zombie", "Dragon"])
        .from_oracle_text(EBONDEATH)
        .with_mana_cost(ManaCost::zero())
        .id()
}

/// A synthetic self-granting graveyard permission gated by `gate` (the class
/// shape, for accepted grammar with no printed card).
fn gated_card_in_graveyard(
    scenario: &mut GameScenario,
    owner: PlayerId,
    name: &str,
    gate: &str,
) -> ObjectId {
    let text = format!("You may cast this card from your graveyard if {gate}.");
    scenario
        .add_creature_to_graveyard(owner, name, 2, 2)
        .from_oracle_text(&text)
        .with_mana_cost(ManaCost::zero())
        .id()
}

/// CR 700.4: kill a creature through the real zone-move primitive, which records
/// its battlefield→graveyard snapshot in `zone_changes_this_turn`.
fn dies(runner: &mut GameRunner, creature: ObjectId) {
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), creature, Zone::Graveyard, &mut events);
}

fn offered_to(runner: &GameRunner, player: PlayerId, card: ObjectId) -> bool {
    let on_offer = spell_objects_available_to_cast(runner.state(), player).contains(&card);
    assert_eq!(
        on_offer,
        can_cast_object_now(runner.state(), player, card),
        "offer list and can_cast_object_now must agree for {card:?}"
    );
    on_offer
}

fn offered(runner: &GameRunner, card: ObjectId) -> bool {
    offered_to(runner, P0, card)
}

/// CR 601.2e: an illegal proposed cast is rejected and the card stays put.
fn assert_cast_rejected(runner: &mut GameRunner, card: ObjectId) {
    let card_id = runner.state().objects[&card].card_id;
    let result = runner.act(GameAction::CastSpell {
        object_id: card,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::default(),
    });
    assert!(
        result.is_err(),
        "a gated-off graveyard cast must be rejected, got {result:?}"
    );
    assert_eq!(runner.state().objects[&card].zone, Zone::Graveyard);
}

fn graveyard_card_named(runner: &GameRunner, player: PlayerId, name: &str) -> ObjectId {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .unwrap()
        .graveyard
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == name)
        .unwrap_or_else(|| panic!("{name} is in the graveyard"))
}

// ── Runtime: Ebondeath ───────────────────────────────────────────────────────

/// A Grizzly Bears died this turn → Ebondeath is offered from the graveyard and
/// the cast completes (it enters tapped, per its own replacement).
#[test]
fn ebondeath_offered_and_cast_after_another_creature_died() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ebondeath = ebondeath_in_graveyard(&mut scenario, P0);
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    assert!(
        !offered(&runner, ebondeath),
        "reach: gated off before any death"
    );
    dies(&mut runner, bears);
    assert!(offered(&runner, ebondeath), "a non-Ebondeath creature died");

    let outcome = runner.cast(ebondeath).resolve();
    assert_eq!(outcome.zone_of(ebondeath), Zone::Battlefield);
    assert!(
        outcome.state().objects[&ebondeath].tapped,
        "Ebondeath enters tapped"
    );
}

/// CR 201.2: only a DIFFERENT object named Ebondeath died → not offered, and a
/// direct cast is rejected. The name filter is not a self-exclusion. Reach: a
/// Bears death in the same scenario opens it.
#[test]
fn ebondeath_not_offered_when_only_another_ebondeath_died() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ebondeath = ebondeath_in_graveyard(&mut scenario, P0);
    let other_ebondeath = scenario.add_creature(P0, EBONDEATH_NAME, 5, 2).id();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    dies(&mut runner, other_ebondeath);
    assert!(
        runner
            .state()
            .zone_changes_this_turn
            .iter()
            .any(|r| r.name == EBONDEATH_NAME && r.to_zone == Zone::Graveyard),
        "reach: the other Ebondeath's death was recorded"
    );
    assert!(
        !offered(&runner, ebondeath),
        "only a creature named Ebondeath, Dracolich died"
    );
    assert_cast_rejected(&mut runner, ebondeath);

    dies(&mut runner, bears);
    assert!(offered(&runner, ebondeath), "reach: a Bears death opens it");
}

/// Nothing died this turn → not offered, and a direct cast is rejected.
#[test]
fn ebondeath_not_offered_when_nothing_died() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ebondeath = ebondeath_in_graveyard(&mut scenario, P0);
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    assert!(runner.state().zone_changes_this_turn.is_empty());
    assert!(!offered(&runner, ebondeath));
    assert_cast_rejected(&mut runner, ebondeath);

    dies(&mut runner, bears);
    assert!(offered(&runner, ebondeath), "reach: a death opens it");
}

/// CR 514.2: a death on a PREVIOUS turn doesn't count. Reach guards: offered on
/// the turn the Bears died, and offered again on the new turn once a creature
/// dies there. Ebondeath has Flash, so the negative isn't a timing artifact.
#[test]
fn ebondeath_not_offered_for_a_death_on_a_previous_turn() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let ebondeath = ebondeath_in_graveyard(&mut scenario, P0);
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let later = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    let mut runner = scenario.build();

    dies(&mut runner, bears);
    assert!(
        offered(&runner, ebondeath),
        "reach: offered the turn it died"
    );

    let turn = runner.state().turn_number;
    to_opponents_turn_with_p0_priority(&mut runner);
    assert!(
        runner.state().turn_number > turn,
        "reach: the turn advanced"
    );
    assert!(
        !offered(&runner, ebondeath),
        "last turn's death must not open this turn's gate"
    );

    dies(&mut runner, later);
    assert!(
        offered(&runner, ebondeath),
        "reach: a death on the new turn opens it"
    );
}

/// CR 702.8a + CR 117.1a + CR 601.3: Flash lets Ebondeath be cast from the
/// graveyard on the OPPONENT's turn once a creature died there. A real
/// `CastSpell` goes through offer, admission and prepare, and resolves.
#[test]
fn ebondeath_cast_on_opponents_turn_after_a_death() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::End);
    let ebondeath = ebondeath_in_graveyard(&mut scenario, P0);
    let their_bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    to_opponents_turn_with_p0_priority(&mut runner);
    assert!(
        !offered(&runner, ebondeath),
        "reach: nothing died this turn yet"
    );
    dies(&mut runner, their_bears);
    assert!(
        offered(&runner, ebondeath),
        "offered on the opponent's turn"
    );

    let outcome = runner.cast(ebondeath).resolve();
    assert_eq!(outcome.zone_of(ebondeath), Zone::Battlefield);
}

/// Rulings: deaths count even if Ebondeath wasn't in the graveyard yet, and it
/// doesn't matter that Ebondeath itself also died. The Bears dies first, then
/// Ebondeath dies from the battlefield. It is offered, and the cast completes.
#[test]
fn ebondeath_offered_after_bears_then_itself_died() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let on_battlefield = scenario
        .add_creature(P0, EBONDEATH_NAME, 5, 2)
        .with_subtypes(vec!["Zombie", "Dragon"])
        .from_oracle_text(EBONDEATH)
        .with_mana_cost(ManaCost::zero())
        .id();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let mut runner = scenario.build();

    dies(&mut runner, bears);
    dies(&mut runner, on_battlefield);
    let ebondeath = graveyard_card_named(&runner, P0, EBONDEATH_NAME);

    assert!(offered(&runner, ebondeath));
    let outcome = runner.cast(ebondeath).resolve();
    assert_eq!(outcome.zone_of(ebondeath), Zone::Battlefield);
}

/// Ruling: a creature token that died still counts, although it then ceases to
/// exist (CR 111.7).
#[test]
fn ebondeath_offered_after_a_token_died() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ebondeath = ebondeath_in_graveyard(&mut scenario, P0);
    let token = scenario.add_creature(P0, "Zombie", 2, 2).id();
    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&token).unwrap().is_token = true;

    assert!(!offered(&runner, ebondeath), "reach: gated off before");
    dies(&mut runner, token);
    assert!(offered(&runner, ebondeath), "a dead token still counts");
}

/// Ruling (name at death): a creature that became a COPY of Ebondeath and died
/// was named Ebondeath while it was last on the battlefield, so it doesn't open
/// the gate. Uses the production copy effect (CR 707.2). Reach: a Hill Giant
/// death then opens it.
#[test]
fn ebondeath_not_offered_when_a_copy_of_ebondeath_died() {
    const COPY: &str =
        "Target creature you control becomes a copy of target creature until end of turn.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let ebondeath = ebondeath_in_graveyard(&mut scenario, P0);
    let copy_spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Copy Probe", true, COPY)
        .with_mana_cost(ManaCost::zero())
        .id();
    let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let donor = scenario
        .add_creature(P1, EBONDEATH_NAME, 5, 2)
        .with_subtypes(vec!["Zombie", "Dragon"])
        .id();
    let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let mut runner = scenario.build();

    let outcome = runner
        .cast(copy_spell)
        .target_objects(&[bears, donor])
        .resolve();
    assert_eq!(
        outcome.state().objects[&bears].name,
        EBONDEATH_NAME,
        "reach: the Bears is now a copy of Ebondeath"
    );
    dies(&mut runner, bears);
    assert!(
        !offered(&runner, ebondeath),
        "a creature named Ebondeath (a copy) died, so the gate stays closed"
    );

    dies(&mut runner, giant);
    assert!(
        offered(&runner, ebondeath),
        "reach: a Hill Giant death opens it"
    );
}

// ── Runtime: the accepted class ─────────────────────────────────────────────

/// Accepted synthetic shape on the under-control arm. Both the name exclusion
/// and the controller scope hold: own Bears → closed; opponent's Hill Giant →
/// closed; own Hill Giant → open, and the cast completes.
#[test]
fn not_named_under_your_control_gate_is_name_and_controller_scoped() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let revenant = gated_card_in_graveyard(
        &mut scenario,
        P0,
        "Test Revenant",
        "a creature not named Grizzly Bears died under your control this turn",
    );
    let own_bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let their_giant = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    let own_giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let mut runner = scenario.build();

    dies(&mut runner, own_bears);
    assert!(!offered(&runner, revenant), "only a Grizzly Bears died");
    dies(&mut runner, their_giant);
    assert!(
        !offered(&runner, revenant),
        "an opponent's creature died, not one under your control"
    );
    dies(&mut runner, own_giant);
    assert!(
        offered(&runner, revenant),
        "reach: own non-Bears death opens it"
    );
    let outcome = runner.cast(revenant).resolve();
    assert_eq!(outcome.zone_of(revenant), Zone::Battlefield);
}

/// Accepted synthetic shape on the under-OPPONENT's-control arm, 3 players, so
/// "an opponent" is any opponent.
#[test]
fn not_named_under_an_opponents_control_gate_covers_every_opponent() {
    const GATE: &str =
        "a creature not named Grizzly Bears died under an opponent's control this turn";
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let p2 = PlayerId(2);
    let revenant = gated_card_in_graveyard(&mut scenario, P0, "Test Revenant", GATE);
    let their_bears = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let own_giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let p1_giant = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    let mut runner = scenario.build();

    dies(&mut runner, their_bears);
    assert!(
        !offered(&runner, revenant),
        "the opponent's creature was named Grizzly Bears"
    );
    dies(&mut runner, own_giant);
    assert!(
        !offered(&runner, revenant),
        "my own creature died, not an opponent's"
    );
    dies(&mut runner, p1_giant);
    assert!(
        offered(&runner, revenant),
        "reach: a differently named creature an opponent (P1) controlled died"
    );

    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let revenant = gated_card_in_graveyard(&mut scenario, P0, "Test Revenant", GATE);
    let p2_giant = scenario.add_creature(p2, "Hill Giant", 3, 3).id();
    let mut runner = scenario.build();
    assert!(!offered(&runner, revenant), "reach: gated off before");
    dies(&mut runner, p2_giant);
    assert!(
        offered(&runner, revenant),
        "the second opponent (P2) counts as \"an opponent\""
    );
}

/// "Under your control" for a permission card that was STOLEN and died into its
/// owner's graveyard. Once it is in the graveyard it has no controller, so
/// "your" is its owner (CR 109.4 + CR 108.4a). Its own death under the thief's
/// control doesn't count for the owner.
#[test]
fn not_named_under_your_control_reads_the_owner_after_a_stolen_death() {
    const TEXT: &str = "You may cast this card from your graveyard if a creature not named Grizzly Bears died under your control this turn.";
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let stolen = scenario
        .add_creature(P0, "Test Revenant", 2, 2)
        .from_oracle_text(TEXT)
        .with_mana_cost(ManaCost::zero())
        .controlled_by(P1)
        .id();
    let their_giant = scenario.add_creature(P1, "Hill Giant", 3, 3).id();
    let own_bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let own_giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let mut runner = scenario.build();
    assert_eq!(
        runner.state().objects[&stolen].controller,
        P1,
        "reach: stolen"
    );

    dies(&mut runner, stolen);
    let revenant = graveyard_card_named(&runner, P0, "Test Revenant");
    assert_eq!(runner.state().objects[&revenant].controller, P0);
    assert!(
        !offered(&runner, revenant),
        "it died under P1's control, which doesn't count for P0"
    );
    dies(&mut runner, their_giant);
    assert!(!offered(&runner, revenant), "P1's creature doesn't count");
    dies(&mut runner, own_bears);
    assert!(!offered(&runner, revenant), "own Grizzly Bears is excluded");
    dies(&mut runner, own_giant);
    assert!(offered(&runner, revenant), "reach: own Hill Giant opens it");
}

/// CR 201.2a: objects sharing a name satisfy the Named filter. With only Éowyn,
/// Shieldmaiden dead, a "not named Éowyn, Shieldmaiden" gate stays CLOSED. An
/// ASCII-only compare would miss "É"/"é" and wrongly open it. Reach: a Hill
/// Giant death opens it.
#[test]
fn not_named_gate_compares_non_ascii_names() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let revenant = gated_card_in_graveyard(
        &mut scenario,
        P0,
        "Test Revenant",
        "a creature not named Éowyn, Shieldmaiden died this turn",
    );
    let eowyn = scenario.add_creature(P0, "Éowyn, Shieldmaiden", 5, 4).id();
    let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let mut runner = scenario.build();

    dies(&mut runner, eowyn);
    assert!(
        !offered(&runner, revenant),
        "only a creature named Éowyn, Shieldmaiden died"
    );
    dies(&mut runner, giant);
    assert!(
        offered(&runner, revenant),
        "reach: a Hill Giant death opens it"
    );
}

/// CR 201.2a: objects sharing a name satisfy the Named filter. The parser's
/// whole-string lowercase form of ΟΣ ends in final sigma (ος). A
/// character-by-character comparison yields οσ and would wrongly open the gate.
#[test]
fn not_named_gate_compares_contextual_unicode_lowercase() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let revenant = gated_card_in_graveyard(
        &mut scenario,
        P0,
        "Test Revenant",
        "a creature not named ΟΣ died this turn",
    );
    let excluded = scenario.add_creature(P0, "ΟΣ", 2, 2).id();
    let giant = scenario.add_creature(P0, "Hill Giant", 3, 3).id();
    let mut runner = scenario.build();

    dies(&mut runner, excluded);
    assert!(!offered(&runner, revenant), "only a creature named ΟΣ died");
    dies(&mut runner, giant);
    assert!(offered(&runner, revenant), "a Hill Giant death opens it");
}

/// CR 201.2: the POSITIVE "named <non-ASCII name>" gate opens when that creature
/// dies. Same comparison authority as the negated form.
#[test]
fn named_gate_matches_non_ascii_names() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let revenant = gated_card_in_graveyard(
        &mut scenario,
        P0,
        "Test Revenant",
        "a creature named Éowyn, Shieldmaiden died this turn",
    );
    let eowyn = scenario.add_creature(P0, "Éowyn, Shieldmaiden", 5, 4).id();
    let mut runner = scenario.build();

    assert!(!offered(&runner, revenant), "reach: gated off before");
    dies(&mut runner, eowyn);
    assert!(offered(&runner, revenant), "Éowyn, Shieldmaiden died");
}

// ── Parse ────────────────────────────────────────────────────────────────────

/// Ebondeath's permission carries the typed gate and no swallowed clause.
#[test]
fn ebondeath_permission_carries_the_not_named_gate() {
    let parsed = parse_oracle_text(
        EBONDEATH,
        EBONDEATH_NAME,
        &strings(&["Flash", "Flying"]),
        &strings(&["Creature"]),
        &strings(&["Zombie", "Dragon"]),
    );
    let permission = parsed
        .statics
        .iter()
        .find(|s| matches!(s.mode, StaticMode::GraveyardCastPermission { .. }))
        .expect("reach: the graveyard permission is parsed");
    let condition = permission
        .condition
        .as_ref()
        .expect("the died-this-turn gate must ride the permission");
    assert_not_named_died_gate(condition, "ebondeath, dracolich", &[TypeFilter::Creature]);
    assert!(
        !parsed
            .parse_warnings
            .iter()
            .any(|w| format!("{w:?}").contains("SwallowedClause")),
        "no swallowed clause: {:?}",
        parsed.parse_warnings
    );
}

/// SHAPE, building block: the died-this-turn subject accepts `not named <name>`
/// (comma-bearing names included) and composes with other type-phrase parts
/// and with the under-control arms.
#[test]
fn died_this_turn_subject_accepts_not_named_suffix() {
    let (rest, cond) =
        parse_inner_condition("a creature not named ebondeath, dracolich died this turn")
            .expect("not-named died gate must parse");
    assert!(rest.trim().is_empty(), "left: {rest:?}");
    assert_not_named_died_gate(&cond, "ebondeath, dracolich", &[TypeFilter::Creature]);

    let (rest, cond) = parse_inner_condition("a non-zombie creature not named foo died this turn")
        .expect("composes with a negated subtype");
    assert!(rest.trim().is_empty(), "left: {rest:?}");
    assert_not_named_died_gate(
        &cond,
        "foo",
        &[
            TypeFilter::Creature,
            TypeFilter::Non(Box::new(TypeFilter::Subtype("Zombie".to_string()))),
        ],
    );

    for (text, controller) in [
        (
            "a creature not named foo died under your control this turn",
            ControllerRef::You,
        ),
        (
            "a creature not named foo died under an opponent's control this turn",
            ControllerRef::Opponent,
        ),
    ] {
        let (rest, cond) = parse_inner_condition(text).expect("under-control arm parses");
        assert!(rest.trim().is_empty(), "left: {rest:?}");
        let StaticCondition::QuantityComparison {
            lhs:
                QuantityExpr::Ref {
                    qty:
                        QuantityRef::ZoneChangeCountThisTurn {
                            filter: TargetFilter::Typed(tf),
                            ..
                        },
                },
            ..
        } = &cond
        else {
            panic!("expected a died gate, got {cond:?}");
        };
        assert_eq!(tf.controller, Some(controller), "{text}");
        assert!(
            tf.properties.contains(&not_named("foo")),
            "{text}: {:?}",
            tf.properties
        );
    }
}

/// SHAPE, fail-closed: shapes the not-named suffix must not claim.
#[test]
fn died_this_turn_not_named_rejects_unmodelled_shapes() {
    // Positive reach in the same test: the plain form parses.
    assert!(parse_inner_condition("a creature not named foo died this turn").is_ok());
    for text in [
        // CR 201.5: a self-ref token is not a literal name.
        "a creature not named ~ died this turn",
        "a creature not named  died this turn",
        // A clause after the name must not be swallowed into it.
        "a creature not named foo with flying died this turn",
        // A name exclusion over a disjunctive subject isn't modelled.
        "an artifact or creature not named foo died this turn",
    ] {
        let parsed = parse_inner_condition(text);
        let claimed = matches!(
            &parsed,
            Ok((rest, StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref { qty: QuantityRef::ZoneChangeCountThisTurn { .. } },
                ..
            })) if rest.trim().is_empty()
        );
        assert!(
            !claimed,
            "{text:?} must not parse to a died gate: {parsed:?}"
        );
    }
}

fn not_named(name: &str) -> FilterProp {
    FilterProp::Not {
        prop: Box::new(FilterProp::Named {
            name: name.to_string(),
        }),
    }
}

fn assert_not_named_died_gate(cond: &StaticCondition, name: &str, types: &[TypeFilter]) {
    let StaticCondition::QuantityComparison {
        lhs:
            QuantityExpr::Ref {
                qty:
                    QuantityRef::ZoneChangeCountThisTurn {
                        from: Some(Zone::Battlefield),
                        to: Some(Zone::Graveyard),
                        filter: TargetFilter::Typed(tf),
                    },
            },
        comparator: Comparator::GE,
        rhs: QuantityExpr::Fixed { value: 1 },
    } = cond
    else {
        panic!("expected a died-this-turn gate, got {cond:?}");
    };
    assert_eq!(tf.type_filters, types, "subject types");
    assert_eq!(
        tf.properties,
        vec![not_named(name)],
        "the name exclusion is Not(Named), never Another"
    );
}

/// Advance from P0's end step to P1's turn and hand P0 priority there. P1 is
/// the active player and passes once (CR 117.3d).
pub(crate) fn to_opponents_turn_with_p0_priority(runner: &mut GameRunner) {
    for _ in 0..40 {
        if runner.state().active_player == P1
            && matches!(
                runner.state().waiting_for,
                engine::types::game_state::WaitingFor::Priority { player } if player == P1
            )
        {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority toward the opponent's turn");
    }
    assert_eq!(runner.state().active_player, P1, "reach: it is P1's turn");
    runner
        .act(GameAction::PassPriority)
        .expect("P1 passes priority");
    assert!(
        matches!(
            runner.state().waiting_for,
            engine::types::game_state::WaitingFor::Priority { player } if player == P0
        ),
        "reach: P0 holds priority on P1's turn, got {:?}",
        runner.state().waiting_for
    );
}
