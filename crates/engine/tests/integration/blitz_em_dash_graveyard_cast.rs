//! CR 702.152 Blitz — em-dash (compound) blitz costs cast from the graveyard.
//!
//! Covers the complete em-dash blitz class, which is exactly two cards:
//!   * Sabin, Master Monk   — "Blitz—{2}{R}{R}, Discard a card."
//!   * Tenacious Underdog   — "Blitz—{2}{B}{B}, Pay 2 life."
//!
//! Both also carry "You may cast this card from your graveyard using its blitz
//! ability.", so the graveyard route is the only route that matters for them.
//!
//! CR 702.152a: "Blitz [cost]" means "You may cast this card by paying [cost]
//! rather than its mana cost". CR 118.9 governs the alternative cost, and
//! CR 601.2h governs paying the non-mana residual (discard / pay life) as part
//! of the total cost.
//!
//! These tests drive the real parser output into a scenario, so they exercise
//! parser -> keyword extraction -> casting-variant selection -> cost payment.

use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::parser::oracle::parse_oracle_text;
use engine::types::actions::{AlternativeCastDecision, GameAction};
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const SABIN: &str = "Double strike\nBlitz\u{2014}{2}{R}{R}, Discard a card. (If you cast this spell for its blitz cost, it gains haste and \"When this creature dies, draw a card.\" Sacrifice it at the beginning of the next end step.)\nYou may cast this card from your graveyard using its blitz ability.";

const UNDERDOG: &str = "Blitz\u{2014}{2}{B}{B}, Pay 2 life. (If you cast this spell for its blitz cost, it gains haste and \"When this creature dies, draw a card.\" Sacrifice it at the beginning of the next end step.)\nYou may cast this card from your graveyard using its blitz ability.";

const CALDAIA: &str = "Whenever this creature or another creature you control with mana value 4 or greater dies, create two 1/1 green and white Citizen creature tokens.\nBlitz {2}{G} (If you cast this spell for its blitz cost, it gains haste and \"When this creature dies, draw a card.\" Sacrifice it at the beginning of the next end step.)";

/// Fill the active player's pool with 8 mana of one color — enough to pay any
/// cost in these tests, so an over-charge shows up as a pool delta rather than
/// as an affordability failure.
fn fill_mana(runner: &mut GameRunner, mana: ManaType) {
    let dummy = ObjectId(0);
    let pool = &mut runner.state_mut().players[0].mana_pool;
    for _ in 0..8 {
        pool.add(ManaUnit::new(mana, dummy, false, vec![]));
    }
}

fn blitz_keyword(parsed: &engine::parser::oracle::ParsedAbilities) -> Keyword {
    parsed
        .extracted_keywords
        .iter()
        .find(|k| matches!(k, Keyword::Blitz(_)))
        .unwrap_or_else(|| {
            panic!(
                "blitz keyword must be extracted, got {:?}",
                parsed.extracted_keywords
            )
        })
        .clone()
}

/// CR 702.152a + CR 118.9 + CR 601.2h: Sabin's graveyard blitz charges the
/// blitz cost ({2}{R}{R} = 4 mana), not the printed cost ({4}{R} = 5 mana), and
/// the discard is a mandatory additional cost.
///
/// Before the em-dash branch existed the whole blitz line parsed to
/// `Effect::Unimplemented` and Sabin had no blitz keyword at all, which also
/// made its graveyard-permission static (gated on `HasKeywordKind(Blitz)`) dead.
#[test]
fn sabin_graveyard_blitz_charges_blitz_cost_and_discards() {
    let parsed = parse_oracle_text(
        SABIN,
        "Sabin, Master Monk",
        &[],
        &["Legendary".into(), "Creature".into()],
        &["Human".into(), "Noble".into(), "Monk".into()],
    );
    let kw = blitz_keyword(&parsed);
    let gy_static = parsed
        .statics
        .first()
        .expect("graveyard-cast permission static must parse")
        .clone();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sabin = scenario
        .add_creature_to_graveyard(P0, "Sabin, Master Monk", 4, 3)
        .with_static_definition(gy_static)
        // Printed cost {4}{R} = 5 mana; blitz cost is 4. The two differ, so the
        // pool delta discriminates which cost was actually charged.
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::Red],
        })
        .with_keyword(kw)
        .id();
    scenario.add_card_to_hand(P0, "Filler Card");
    let mut runner = scenario.build();
    fill_mana(&mut runner, ManaType::Red);

    let card_id = runner.state().objects[&sabin].card_id;
    let waiting = runner
        .act(GameAction::CastSpell {
            object_id: sabin,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("graveyard blitz cast must be legal");

    // CR 118.9a: only one alternative cost applies, and the permission reads
    // "using its blitz ability" — so the gate must NOT also offer a printed-cost
    // graveyard cast. Reaching a bare cost payment (not a variant choice) is the
    // positive reach guard that the blitz variant was selected outright.
    assert!(
        !matches!(waiting.waiting_for, WaitingFor::CastingVariantChoice { .. }),
        "a \"using its blitz ability\" permission must not offer a printed-cost \
         graveyard cast alongside blitz; got {:?}",
        waiting.waiting_for
    );
    assert!(
        matches!(waiting.waiting_for, WaitingFor::PayCost { .. }),
        "expected the blitz discard cost prompt, got {:?}",
        waiting.waiting_for
    );

    // CR 601.2h: the discard is part of the total cost, so it cannot be declined.
    assert!(
        runner
            .act(GameAction::SelectCards { cards: vec![] })
            .is_err(),
        "blitz's discard is an additional cost and must be mandatory"
    );

    let filler = runner.state().players[0].hand[0];
    runner
        .act(GameAction::SelectCards {
            cards: vec![filler],
        })
        .expect("paying the blitz discard must succeed");

    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        4,
        "blitz cost {{2}}{{R}}{{R}} = 4 mana must be charged, not the printed {{4}}{{R}} = 5"
    );
    assert_eq!(
        runner.state().players[0].hand.len(),
        0,
        "the discarded card must leave hand"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Sabin must be on the stack after a successful blitz cast"
    );
}

/// CR 702.152a: Tenacious Underdog — the other member of the em-dash blitz
/// class, with a pay-life residual instead of a discard. This card carried NO
/// `Unimplemented` marker before the fix: it silently charged its printed cost.
///
/// Its printed cost ({1}{B} = 2) is CHEAPER than its blitz cost ({2}{B}{B} = 4),
/// so a pool delta of 4 cannot be produced by accidentally charging the printed
/// cost — the direction of the difference makes this assertion discriminating.
#[test]
fn underdog_graveyard_blitz_charges_blitz_cost_and_pays_life() {
    let parsed = parse_oracle_text(
        UNDERDOG,
        "Tenacious Underdog",
        &[],
        &["Creature".into()],
        &["Human".into(), "Warrior".into()],
    );
    let kw = blitz_keyword(&parsed);
    let gy_static = parsed
        .statics
        .first()
        .expect("graveyard-cast permission static must parse")
        .clone();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dog = scenario
        .add_creature_to_graveyard(P0, "Tenacious Underdog", 3, 2)
        .with_static_definition(gy_static)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Black],
        })
        .with_keyword(kw)
        .id();
    let mut runner = scenario.build();
    fill_mana(&mut runner, ManaType::Black);
    let life_before = runner.state().players[0].life;

    let card_id = runner.state().objects[&dog].card_id;
    let waiting = runner
        .act(GameAction::CastSpell {
            object_id: dog,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("graveyard blitz cast must be legal");

    assert!(
        !matches!(waiting.waiting_for, WaitingFor::CastingVariantChoice { .. }),
        "a \"using its blitz ability\" permission must not offer a printed-cost \
         graveyard cast alongside blitz; got {:?}",
        waiting.waiting_for
    );

    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        4,
        "blitz cost {{2}}{{B}}{{B}} = 4 mana must be charged, not the printed {{1}}{{B}} = 2"
    );
    assert_eq!(
        runner.state().players[0].life,
        life_before - 2,
        "CR 601.2h: blitz's \"Pay 2 life\" residual must actually be paid"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Tenacious Underdog must be on the stack after a successful blitz cast"
    );
}

/// CR 118.9b: alternative costs are OPTIONAL. When a separate, unconstrained
/// permission authorizes casting from the graveyard (Advanced Floral
/// Invocations' "You may play lands and cast creature spells from your
/// graveyard.", and the Muldrotha / Lurrus class generally), the printed-cost
/// graveyard cast IS on offer — so a blitz creature there must present the
/// CHOICE rather than being force-routed into blitz.
///
/// This is distinct from the two class tests above, where the only permission is
/// the card's own "using its blitz ability" rider and blitz is the sole legal
/// cast.
#[test]
fn unconstrained_graveyard_permission_still_offers_printed_cost_choice() {
    const INVOCATIONS: &str = "You may play lands and cast creature spells from your graveyard.";

    let parsed = parse_oracle_text(
        SABIN,
        "Sabin, Master Monk",
        &[],
        &["Legendary".into(), "Creature".into()],
        &["Human".into(), "Noble".into(), "Monk".into()],
    );
    let kw = blitz_keyword(&parsed);
    let own_rider = parsed
        .statics
        .first()
        .expect("graveyard-cast permission static must parse")
        .clone();

    let enabler = parse_oracle_text(
        INVOCATIONS,
        "Advanced Floral Invocations",
        &[],
        &["Enchantment".into()],
        &[],
    );
    let unconstrained = enabler
        .statics
        .iter()
        .find(|s| {
            format!("{s:?}").contains("GraveyardCastPermission")
                && !format!("{s:?}").contains("HasKeywordKind")
        })
        .expect("an unconstrained GraveyardCastPermission must parse")
        .clone();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_enchantment_from_oracle(P0, "Advanced Floral Invocations", INVOCATIONS)
        .with_static_definition(unconstrained);
    let sabin = scenario
        .add_creature_to_graveyard(P0, "Sabin, Master Monk", 4, 3)
        .with_static_definition(own_rider)
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::Red],
        })
        .with_keyword(kw)
        .id();
    scenario.add_card_to_hand(P0, "Filler Card");
    let mut runner = scenario.build();
    fill_mana(&mut runner, ManaType::Red);

    let card_id = runner.state().objects[&sabin].card_id;
    let waiting = runner
        .act(GameAction::CastSpell {
            object_id: sabin,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("graveyard cast must be legal under an unconstrained permission");

    assert!(
        matches!(
            waiting.waiting_for,
            WaitingFor::AlternativeCastChoice {
                keyword: engine::types::game_state::AlternativeCastKeyword::Blitz,
                ..
            }
        ),
        "with a printed-cost graveyard cast also legal, blitz must be OFFERED, \
         not forced; got {:?}",
        waiting.waiting_for
    );
}

/// Anti-widening control: Caldaia Guardian has space-form `Blitz {2}{G}` and NO
/// graveyard-cast permission. Blitz alone must never make a card castable from
/// the graveyard — only the separate permission static does that.
///
/// This is what stops the new `blitz_castable_zone` gate widening across the 14
/// space-form blitz cards. Note the two class tests above supply their own
/// permission static, so this control is not measuring assertion order: it is
/// the same runtime path with the permission removed.
#[test]
fn space_form_blitz_alone_does_not_allow_graveyard_cast() {
    let parsed = parse_oracle_text(
        CALDAIA,
        "Caldaia Guardian",
        &["Blitz".into()],
        &["Creature".into()],
        &["Human".into(), "Soldier".into()],
    );
    let kw = blitz_keyword(&parsed);
    assert!(
        parsed.statics.is_empty(),
        "Caldaia Guardian must have no graveyard-cast permission; got {:?}",
        parsed.statics
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let guardian = scenario
        .add_creature_to_graveyard(P0, "Caldaia Guardian", 4, 3)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Green],
        })
        .with_keyword(kw)
        .id();
    let mut runner = scenario.build();
    fill_mana(&mut runner, ManaType::Green);

    let card_id = runner.state().objects[&guardian].card_id;
    let res = runner.act(GameAction::CastSpell {
        object_id: guardian,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    });
    assert!(
        res.is_err(),
        "blitz without a graveyard-cast permission must not be castable from the \
         graveyard, but the cast was accepted: {:?}",
        res.map(|w| w.waiting_for)
    );
    assert_eq!(
        runner.state().stack.len(),
        0,
        "no spell may reach the stack from this illegal cast"
    );
}

const MULDROTHA: &str = "During each of your turns, you may play a land and cast a permanent spell of each permanent type from your graveyard. (If a card has multiple permanent types, choose one as you play it.)";

const LEONARDO: &str = "Sneak {2}{W}{W}\nDouble strike\nDuring your turn, you may cast creature spells with power or toughness 1 or less from your graveyard. If you cast a spell this way, that creature enters with a finality counter on it. (If a creature with a finality counter on it would die, exile it instead.)";

/// Put a permanent on P0's battlefield carrying every static its Oracle text
/// parses to, so the graveyard permission under test is the parser's own.
fn add_permission_source(
    scenario: &mut GameScenario,
    name: &str,
    oracle: &str,
    subtypes: &[&str],
) -> ObjectId {
    let parsed = parse_oracle_text(
        oracle,
        name,
        &[],
        &["Legendary".into(), "Creature".into()],
        &subtypes
            .iter()
            .map(|s| (*s).to_string())
            .collect::<Vec<_>>(),
    );
    assert!(
        parsed
            .statics
            .iter()
            .any(|s| format!("{s:?}").contains("GraveyardCastPermission")),
        "{name} must parse to a GraveyardCastPermission, got {:?}",
        parsed.statics
    );
    let mut source = scenario.add_creature(P0, name, 4, 4);
    for s in parsed.statics {
        source.with_static_definition(s);
    }
    source.id()
}

fn caldaia_blitz() -> Keyword {
    blitz_keyword(&parse_oracle_text(
        CALDAIA,
        "Caldaia Guardian",
        &["Blitz".into()],
        &["Creature".into()],
        &["Human".into(), "Soldier".into()],
    ))
}

fn cast_from_graveyard(runner: &mut GameRunner, id: ObjectId) -> Result<WaitingFor, String> {
    let card_id = runner.state().objects[&id].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: id,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .map(|r| r.waiting_for)
        .map_err(|e| format!("{e:?}"))
}

/// CR 601.2a + CR 118.9a: a card cast from the graveyard for its own
/// alternative cost is still cast under the permission that let it leave the
/// graveyard. Muldrotha allows one permanent spell of each permanent type per
/// turn, so a creature blitzed from the graveyard spends Muldrotha's creature
/// slot, and a second creature can't follow it that turn.
///
/// Before the fix, choosing blitz replaced the `GraveyardPermission` casting
/// variant, so finalization never spent the slot and Muldrotha allowed one
/// graveyard creature after another.
#[test]
fn blitz_from_graveyard_under_muldrotha_spends_its_creature_slot() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let muldrotha = add_permission_source(
        &mut scenario,
        "Muldrotha, the Gravetide",
        MULDROTHA,
        &["Elemental", "Avatar"],
    );
    let guardian = scenario
        .add_creature_to_graveyard(P0, "Caldaia Guardian", 4, 3)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Green],
        })
        .with_keyword(caldaia_blitz())
        .id();
    let bears = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Green],
        })
        .id();
    let mut runner = scenario.build();
    fill_mana(&mut runner, ManaType::Green);

    // Muldrotha is unconstrained, so the printed cost is also on offer and
    // blitz must be chosen (CR 118.9b).
    let waiting = cast_from_graveyard(&mut runner, guardian).expect("graveyard cast must be legal");
    assert!(
        matches!(
            waiting,
            WaitingFor::AlternativeCastChoice {
                keyword: engine::types::game_state::AlternativeCastKeyword::Blitz,
                ..
            }
        ),
        "expected the blitz choice, got {waiting:?}"
    );
    runner
        .act(GameAction::ChooseAlternativeCast {
            choice: AlternativeCastDecision::Alternative,
        })
        .expect("choosing blitz must complete the cast");

    // Positive reach guard: the blitz cast completed and charged blitz's
    // {2}{G} = 3 mana rather than the printed {3}{G} = 4.
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Caldaia must be on the stack"
    );
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        5,
        "blitz {{2}}{{G}} = 3 of the 8 mana must be spent, not the printed 4"
    );

    assert!(
        runner
            .state()
            .graveyard_cast_permissions_used_per_type
            .contains(&(muldrotha, CoreType::Creature)),
        "the blitz cast must spend Muldrotha's creature slot, used: {:?}",
        runner.state().graveyard_cast_permissions_used_per_type
    );

    runner.resolve_top();
    assert_eq!(
        runner.state().objects[&guardian].zone,
        Zone::Battlefield,
        "Caldaia must resolve, so the stack is empty for the next sorcery-speed cast"
    );
    assert!(runner.state().stack.is_empty());

    let second = cast_from_graveyard(&mut runner, bears);
    assert!(
        second.is_err(),
        "Muldrotha's creature slot is spent, so a second graveyard creature \
         must be refused this turn, but the cast was accepted: {second:?}"
    );
    assert!(
        runner.state().stack.is_empty(),
        "the refused cast must not reach the stack"
    );
}

/// CR 601.2a: when an unlimited permission also admits the blitz cast, the
/// bounded one is not spent. Sabin's own rider ("You may cast this card from
/// your graveyard using its blitz ability.") needs no slot, so blitzing Sabin
/// beside Muldrotha leaves Muldrotha's creature slot for another creature.
#[test]
fn blitz_prefers_the_cards_own_rider_over_a_bounded_graveyard_permission() {
    let parsed = parse_oracle_text(
        SABIN,
        "Sabin, Master Monk",
        &[],
        &["Legendary".into(), "Creature".into()],
        &["Human".into(), "Noble".into(), "Monk".into()],
    );
    let own_rider = parsed
        .statics
        .first()
        .expect("graveyard-cast permission static must parse")
        .clone();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let muldrotha = add_permission_source(
        &mut scenario,
        "Muldrotha, the Gravetide",
        MULDROTHA,
        &["Elemental", "Avatar"],
    );
    let sabin = scenario
        .add_creature_to_graveyard(P0, "Sabin, Master Monk", 4, 3)
        .with_static_definition(own_rider)
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::Red],
        })
        .with_keyword(blitz_keyword(&parsed))
        .id();
    let bears = scenario
        .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Red],
        })
        .id();
    scenario.add_card_to_hand(P0, "Filler Card");
    let mut runner = scenario.build();
    fill_mana(&mut runner, ManaType::Red);

    cast_from_graveyard(&mut runner, sabin).expect("graveyard cast must be legal");
    runner
        .act(GameAction::ChooseAlternativeCast {
            choice: AlternativeCastDecision::Alternative,
        })
        .expect("choosing blitz must be legal");
    let filler = runner.state().players[0].hand[0];
    runner
        .act(GameAction::SelectCards {
            cards: vec![filler],
        })
        .expect("paying the blitz discard must complete the cast");

    // Positive reach guard: the blitz cast completed for {2}{R}{R} = 4.
    assert_eq!(runner.state().stack.len(), 1, "Sabin must be on the stack");
    assert_eq!(runner.state().players[0].mana_pool.total(), 4);

    assert!(
        !runner
            .state()
            .graveyard_cast_permissions_used_per_type
            .contains(&(muldrotha, CoreType::Creature)),
        "Sabin's own unlimited rider authorizes this cast, so Muldrotha's \
         creature slot must stay unspent, used: {:?}",
        runner.state().graveyard_cast_permissions_used_per_type
    );

    runner.resolve_top();
    assert!(runner.state().stack.is_empty());
    cast_from_graveyard(&mut runner, bears)
        .expect("Muldrotha's creature slot is still free, so this cast must be legal");
}

/// CR 601.2a + CR 122.1: a permission's "if you cast a spell this way, that
/// creature enters with a counter on it" rider applies to a blitz cast it
/// admits, because blitz changes the cost, not the permission. Leonardo admits
/// creature spells with power or toughness 1 or less from the graveyard, so the
/// fixture is a 1/1 blitz creature.
#[test]
fn blitz_from_graveyard_keeps_the_permissions_enters_with_counter_rider() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_permission_source(
        &mut scenario,
        "Leonardo, Sewer Samurai",
        LEONARDO,
        &["Mutant", "Ninja", "Turtle", "Samurai"],
    );
    let blitzer = scenario
        .add_creature_to_graveyard(P0, "Blitz Test Creature", 1, 1)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Green],
        })
        .with_keyword(caldaia_blitz())
        .id();
    let mut runner = scenario.build();
    fill_mana(&mut runner, ManaType::Green);

    let waiting = cast_from_graveyard(&mut runner, blitzer).expect("graveyard cast must be legal");
    assert!(
        matches!(waiting, WaitingFor::AlternativeCastChoice { .. }),
        "expected the blitz choice, got {waiting:?}"
    );
    runner
        .act(GameAction::ChooseAlternativeCast {
            choice: AlternativeCastDecision::Alternative,
        })
        .expect("choosing blitz must complete the cast");
    // Positive reach guard: blitz's {2}{G} = 3 was charged, not the printed 4.
    assert_eq!(runner.state().players[0].mana_pool.total(), 5);

    runner.resolve_top();
    let entered = &runner.state().objects[&blitzer];
    assert_eq!(entered.zone, Zone::Battlefield);
    assert_eq!(
        entered.counters.get(&CounterType::Finality).copied(),
        Some(1),
        "Leonardo's finality-counter rider must apply to a blitz cast it admits, \
         counters: {:?}",
        entered.counters
    );
}

const RIVETEERS_DECOY: &str = "This creature must be blocked if able.\nBlitz {3}{G} (If you cast this spell for its blitz cost, it gains haste and \"When this creature dies, draw a card.\" Sacrifice it at the beginning of the next end step.)";

const BOON_SATYR: &str = "Flash\nBestow {3}{G}{G} (If you cast this card for its bestow cost, it's an Aura spell with enchant creature. It becomes a creature again if it's not attached.)\nEnchanted creature gets +4/+2.";

/// CR 601.2a + CR 110.4 + CR 702.103b: the fix applies to every alternative
/// cost that is the card's own, not only Blitz. Bestow is the other one that can
/// be cast from the graveyard.
///
/// A bestowed spell is an Aura, not a creature (CR 702.103b), so under Muldrotha
/// it is cast as an enchantment spell and spends the ENCHANTMENT slot, leaving
/// the creature slot free (Muldrotha ruling, 2020-11-10: "you can cast a card
/// with bestow as an enchantment spell"). Before the fix it spent neither.
#[test]
fn bestow_from_graveyard_under_muldrotha_spends_its_enchantment_slot() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let muldrotha = add_permission_source(
        &mut scenario,
        "Muldrotha, the Gravetide",
        MULDROTHA,
        &["Elemental", "Avatar"],
    );
    let mut builder = scenario.add_creature_to_graveyard(P0, "Boon Satyr", 4, 2);
    builder.with_mana_cost(ManaCost::Cost {
        generic: 1,
        shards: vec![ManaCostShard::Green, ManaCostShard::Green],
    });
    builder.with_subtypes(vec!["Satyr"]);
    builder.from_oracle_text_with_keywords(&["Flash", "Bestow"], BOON_SATYR);
    let satyr = builder.id();
    let mut runner = scenario.build();
    // Boon Satyr is an Enchantment Creature. The graveyard builder seeds only
    // Creature, so add Enchantment to both the current and base type lines.
    {
        let obj = runner.state_mut().objects.get_mut(&satyr).unwrap();
        for types in [
            &mut obj.card_types.core_types,
            &mut obj.base_card_types.core_types,
        ] {
            if !types.contains(&CoreType::Enchantment) {
                types.push(CoreType::Enchantment);
            }
        }
    }
    fill_mana(&mut runner, ManaType::Green);

    // CR 118.9b + CR 702.103a: Muldrotha also authorizes the printed creature
    // cast, so bestow is offered as a CHOICE, not forced.
    let waiting =
        cast_from_graveyard(&mut runner, satyr).expect("graveyard bestow cast must be legal");
    assert!(
        matches!(
            waiting,
            WaitingFor::AlternativeCastChoice {
                keyword: engine::types::game_state::AlternativeCastKeyword::Bestow,
                ..
            }
        ),
        "with Muldrotha authorizing the printed cast too, bestow must be offered \
         as a choice, got {waiting:?}"
    );
    runner
        .act(GameAction::ChooseAlternativeCast {
            choice: AlternativeCastDecision::Alternative,
        })
        .expect("choosing bestow must be legal");
    if let WaitingFor::TargetSelection { .. } = runner.state().waiting_for {
        runner
            .choose_first_legal_target()
            .expect("Muldrotha is a legal creature to enchant");
    }

    // Positive reach guard: the bestow cast completed, charging bestow's
    // {3}{G}{G} = 5 rather than the printed {1}{G}{G} = 3.
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Boon Satyr must be on the stack"
    );
    assert_eq!(runner.state().players[0].mana_pool.total(), 3);

    let used = &runner.state().graveyard_cast_permissions_used_per_type;
    assert!(
        used.contains(&(muldrotha, CoreType::Enchantment)),
        "a bestowed spell is an enchantment spell, so it must spend Muldrotha's \
         enchantment slot, used: {used:?}"
    );
    assert!(
        !used.contains(&(muldrotha, CoreType::Creature)),
        "a bestowed spell is not a creature spell, so the creature slot must stay \
         free, used: {used:?}"
    );
}

/// CR 601.2a: when several permissions admit a graveyard cast, the player
/// announces which one they use (Muldrotha, 2020-11-10 ruling). The engine
/// picks only when the choice is strictly dominant, so an unlimited permission
/// that brings a rider is NOT preferred over a bounded one.
///
/// Leonardo is unlimited but gives the creature a finality counter, so blitz's
/// end-step sacrifice would exile Riveteers Decoy instead of returning it to the
/// graveyard. Spending Muldrotha's slot instead is a real alternative, so the
/// engine keeps the same source-order first match a printed-cost cast gets.
/// Muldrotha is added first, so that first match is Muldrotha.
#[test]
fn blitz_does_not_prefer_an_unlimited_permission_that_brings_a_rider() {
    let decoy_blitz = blitz_keyword(&parse_oracle_text(
        RIVETEERS_DECOY,
        "Riveteers Decoy",
        &["Blitz".into()],
        &["Creature".into()],
        &["Human".into(), "Warrior".into()],
    ));

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let muldrotha = add_permission_source(
        &mut scenario,
        "Muldrotha, the Gravetide",
        MULDROTHA,
        &["Elemental", "Avatar"],
    );
    add_permission_source(
        &mut scenario,
        "Leonardo, Sewer Samurai",
        LEONARDO,
        &["Mutant", "Ninja", "Turtle", "Samurai"],
    );
    let decoy = scenario
        .add_creature_to_graveyard(P0, "Riveteers Decoy", 3, 1)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Green],
        })
        .with_keyword(decoy_blitz)
        .id();
    let mut runner = scenario.build();
    fill_mana(&mut runner, ManaType::Green);

    cast_from_graveyard(&mut runner, decoy).expect("graveyard cast must be legal");
    runner
        .act(GameAction::ChooseAlternativeCast {
            choice: AlternativeCastDecision::Alternative,
        })
        .expect("choosing blitz must complete the cast");
    // Positive reach guard: blitz's {3}{G} = 4 was charged, not the printed 2.
    assert_eq!(runner.state().players[0].mana_pool.total(), 4);

    assert!(
        runner
            .state()
            .graveyard_cast_permissions_used_per_type
            .contains(&(muldrotha, CoreType::Creature)),
        "with no strictly dominant permission, the source-order first match \
         (Muldrotha) authorizes the cast and spends its slot, used: {:?}",
        runner.state().graveyard_cast_permissions_used_per_type
    );

    runner.resolve_top();
    let entered = &runner.state().objects[&decoy];
    assert_eq!(entered.zone, Zone::Battlefield);
    assert_eq!(
        entered.counters.get(&CounterType::Finality).copied(),
        None,
        "Leonardo's permission was not the one used, so its finality rider must \
         not apply, counters: {:?}",
        entered.counters
    );
}

const EXPLORATION_BROODSHIP: &str = "Station (Tap another creature you control: Put charge counters equal to its power on this Spacecraft. Station only as a sorcery. It's an artifact creature at 8+.)\n3+ | You may play an additional land on each of your turns.\n8+ | Flying\nOnce during each of your turns, you may cast a permanent spell from your graveyard by sacrificing a land in addition to paying its other costs.";

const ENCROACHING_MYCOSYNTH: &str = "Nonland permanents you control are artifacts in addition to their other types. The same is true for permanent spells you control and nonland permanent cards you own that aren't on the battlefield.";

/// Put Exploration Broodship on P0's battlefield as the Spacecraft it is, with
/// `charge` charge counters. CR 721.2a: its graveyard permission is printed in
/// the 8+ striation, so it functions only with 8 or more charge counters. The
/// subtype must be set before the Oracle text is parsed, so the parser sees a
/// Spacecraft and gates the striation. Flush layers after `build()`.
fn add_exploration_broodship(scenario: &mut GameScenario, charge: u32) -> ObjectId {
    let broodship = scenario
        .add_artifact_from_oracle(P0, "Exploration Broodship", EXPLORATION_BROODSHIP)
        .with_subtypes(vec!["Spacecraft"])
        .from_oracle_text(EXPLORATION_BROODSHIP)
        .id();
    scenario.with_counter(
        broodship,
        CounterType::Generic("charge".to_string()),
        charge,
    );
    broodship
}

/// Charge counters that switch on Exploration Broodship's graveyard permission.
const BROODSHIP_STATIONED: u32 = 8;

/// Answer every `PayCost` prompt with the first card it offers, until priority
/// returns or a prompt this helper does not handle comes up.
fn pay_offered_costs(runner: &mut GameRunner) {
    for _ in 0..4 {
        let choice = match &runner.state().waiting_for {
            WaitingFor::PayCost { choices, .. } => choices.first().copied(),
            _ => return,
        };
        runner
            .act(GameAction::SelectCards {
                cards: choice.into_iter().collect(),
            })
            .expect("paying an offered cost must be legal");
    }
}

/// CR 601.2a + CR 601.2f: the permission that sets a graveyard cast's cost is
/// the one it commits to. Sabin's own blitz rider needs no slot and has no
/// extra cost, so it is elected over Exploration Broodship, and Broodship's
/// "by sacrificing a land" rider must NOT be charged. Broodship is on the
/// battlefield, so it is scanned before Sabin's own rider.
///
/// Before the election was shared, payment read Broodship's rider (source-order
/// first match) while finalization spent Sabin's own rider: the land was
/// sacrificed and Broodship's slot was left unspent.
#[test]
fn blitz_charges_only_the_elected_permissions_extra_cost() {
    let parsed = parse_oracle_text(
        SABIN,
        "Sabin, Master Monk",
        &[],
        &["Legendary".into(), "Creature".into()],
        &["Human".into(), "Noble".into(), "Monk".into()],
    );
    let own_rider = parsed
        .statics
        .first()
        .expect("graveyard-cast permission static must parse")
        .clone();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let broodship = add_exploration_broodship(&mut scenario, BROODSHIP_STATIONED);
    let land = scenario.add_basic_land(P0, ManaColor::Red);
    let sabin = scenario
        .add_creature_to_graveyard(P0, "Sabin, Master Monk", 4, 3)
        .with_static_definition(own_rider)
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::Red],
        })
        .with_keyword(blitz_keyword(&parsed))
        .id();
    let filler = scenario.add_card_to_hand(P0, "Filler Card");
    let mut runner = scenario.build();
    engine::game::layers::flush_layers(runner.state_mut());
    fill_mana(&mut runner, ManaType::Red);

    cast_from_graveyard(&mut runner, sabin).expect("graveyard cast must be legal");
    runner
        .act(GameAction::ChooseAlternativeCast {
            choice: AlternativeCastDecision::Alternative,
        })
        .expect("choosing blitz must be legal");
    pay_offered_costs(&mut runner);

    // Positive reach guard: the blitz cast completed, and its own discard cost
    // was paid, so the cost pipeline really ran.
    assert_eq!(runner.state().stack.len(), 1, "Sabin must be on the stack");
    assert_eq!(runner.state().objects[&filler].zone, Zone::Graveyard);

    assert_eq!(
        runner.state().objects[&land].zone,
        Zone::Battlefield,
        "Broodship is not the permission this cast uses, so its land-sacrifice \
         rider must not be charged"
    );
    assert!(
        !runner
            .state()
            .graveyard_cast_permissions_used
            .contains(&broodship),
        "Broodship's once-per-turn slot must stay unspent"
    );
}

/// Control for the test above: with Broodship as the ONLY permission, it is
/// the elected authority, so its land sacrifice IS charged and its slot IS
/// spent. Caldaia Guardian has no graveyard permission of its own.
#[test]
fn blitz_under_broodship_alone_charges_its_land_sacrifice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let broodship = add_exploration_broodship(&mut scenario, BROODSHIP_STATIONED);
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    let guardian = scenario
        .add_creature_to_graveyard(P0, "Caldaia Guardian", 4, 3)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Green],
        })
        .with_keyword(caldaia_blitz())
        .id();
    let mut runner = scenario.build();
    engine::game::layers::flush_layers(runner.state_mut());
    fill_mana(&mut runner, ManaType::Green);

    cast_from_graveyard(&mut runner, guardian).expect("graveyard cast must be legal");
    runner
        .act(GameAction::ChooseAlternativeCast {
            choice: AlternativeCastDecision::Alternative,
        })
        .expect("choosing blitz must be legal");
    pay_offered_costs(&mut runner);

    assert_eq!(
        runner.state().stack.len(),
        1,
        "Caldaia must be on the stack"
    );
    assert_eq!(
        runner.state().objects[&land].zone,
        Zone::Graveyard,
        "Broodship admits this cast, so its land-sacrifice rider must be paid"
    );
    assert!(
        runner
            .state()
            .graveyard_cast_permissions_used
            .contains(&broodship),
        "Broodship's once-per-turn slot must be spent"
    );
}

/// Muldrotha, Encroaching Mycosynth and a graveyard Boon Satyr: a board where
/// a card cast for its own alternative cost would need a permanent-type slot
/// choice the engine can't prompt for yet.
fn mycosynth_muldrotha_board(scenario: &mut GameScenario) -> ObjectId {
    scenario.at_phase(Phase::PreCombatMain);
    add_permission_source(
        scenario,
        "Muldrotha, the Gravetide",
        MULDROTHA,
        &["Elemental", "Avatar"],
    );
    scenario.add_artifact_from_oracle(P0, "Encroaching Mycosynth", ENCROACHING_MYCOSYNTH);
    let mut builder = scenario.add_creature_to_graveyard(P0, "Boon Satyr", 4, 2);
    builder.with_mana_cost(ManaCost::Cost {
        generic: 1,
        shards: vec![ManaCostShard::Green, ManaCostShard::Green],
    });
    builder.with_subtypes(vec!["Satyr"]);
    builder.from_oracle_text_with_keywords(&["Flash", "Bestow"], BOON_SATYR);
    builder.id()
}

/// CR 110.4 + CR 702.103b + CR 118.9b: under Encroaching Mycosynth a bestowed
/// Boon Satyr is an artifact AND an enchantment spell, so Muldrotha offers two
/// slots and the player would have to choose one. That choice has no prompt on
/// the alternative-cost path yet, so bestow is not offered through Muldrotha,
/// rather than finalizing a cast that spends no slot (or tripping the
/// finalize-time slot assertion). Bestow is optional, though, so Muldrotha's
/// ordinary creature cast is still legal and goes to the existing slot prompt.
#[test]
fn bestow_needing_a_permanent_type_choice_leaves_the_printed_cast() {
    let mut scenario = GameScenario::new();
    let satyr = mycosynth_muldrotha_board(&mut scenario);
    let mut runner = scenario.build();
    // Boon Satyr is an Enchantment Creature. The graveyard builder seeds only
    // Creature, so add Enchantment to both the current and base type lines.
    {
        let obj = runner.state_mut().objects.get_mut(&satyr).unwrap();
        for types in [
            &mut obj.card_types.core_types,
            &mut obj.base_card_types.core_types,
        ] {
            if !types.contains(&CoreType::Enchantment) {
                types.push(CoreType::Enchantment);
            }
        }
    }
    engine::game::layers::flush_layers(runner.state_mut());
    fill_mana(&mut runner, ManaType::Green);

    // Positive reach guard: Mycosynth really made the graveyard card an artifact,
    // which is what gives the bestowed spell its second slot.
    assert!(
        runner.state().objects[&satyr]
            .card_types
            .core_types
            .contains(&CoreType::Artifact),
        "Encroaching Mycosynth must make the graveyard Satyr an artifact, types: {:?}",
        runner.state().objects[&satyr].card_types.core_types
    );

    let waiting = cast_from_graveyard(&mut runner, satyr)
        .expect("Muldrotha's printed creature cast is legal, so the cast must go ahead");
    match &waiting {
        WaitingFor::ChoosePermanentTypeSlot {
            available_slots, ..
        } => {
            for slot in [
                CoreType::Artifact,
                CoreType::Creature,
                CoreType::Enchantment,
            ] {
                assert!(
                    available_slots.contains(&slot),
                    "the printed Artifact Enchantment Creature cast offers every slot, \
                     missing {slot:?} in {available_slots:?}"
                );
            }
        }
        other => panic!(
            "bestow can't be used through Muldrotha here, so the printed cast must go \
             to the existing slot prompt, not {other:?}"
        ),
    }
    assert!(
        runner.state().stack.is_empty(),
        "nothing is on the stack yet"
    );
    let obj = &runner.state().objects[&satyr];
    assert_eq!(obj.zone, Zone::Graveyard);
    assert!(
        obj.card_types.core_types.contains(&CoreType::Creature),
        "no bestow form was applied, so the card keeps its creature type, types: {:?}",
        obj.card_types.core_types
    );
}

/// CR 110.4 + CR 118.9b: choosing the printed cast over bestow, from the
/// graveyard under Muldrotha, still takes the permanent-type slot choice. Boon
/// Satyr's printed cast is an enchantment creature spell, so there are two
/// slots to choose between.
#[test]
fn bestow_declined_from_graveyard_takes_the_slot_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_permission_source(
        &mut scenario,
        "Muldrotha, the Gravetide",
        MULDROTHA,
        &["Elemental", "Avatar"],
    );
    let mut builder = scenario.add_creature_to_graveyard(P0, "Boon Satyr", 4, 2);
    builder.with_mana_cost(ManaCost::Cost {
        generic: 1,
        shards: vec![ManaCostShard::Green, ManaCostShard::Green],
    });
    builder.with_subtypes(vec!["Satyr"]);
    builder.from_oracle_text_with_keywords(&["Flash", "Bestow"], BOON_SATYR);
    let satyr = builder.id();
    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&satyr).unwrap();
        for types in [
            &mut obj.card_types.core_types,
            &mut obj.base_card_types.core_types,
        ] {
            if !types.contains(&CoreType::Enchantment) {
                types.push(CoreType::Enchantment);
            }
        }
    }
    fill_mana(&mut runner, ManaType::Green);

    let waiting = cast_from_graveyard(&mut runner, satyr).expect("graveyard cast must be legal");
    assert!(
        matches!(waiting, WaitingFor::AlternativeCastChoice { .. }),
        "expected the bestow choice, got {waiting:?}"
    );
    let waiting = runner
        .act(GameAction::ChooseAlternativeCast {
            choice: AlternativeCastDecision::Normal,
        })
        .expect("choosing the printed cast must be legal")
        .waiting_for;
    assert!(
        matches!(
            &waiting,
            WaitingFor::ChoosePermanentTypeSlot { available_slots, .. }
                if available_slots.contains(&CoreType::Creature)
                    && available_slots.contains(&CoreType::Enchantment)
        ),
        "the printed enchantment creature cast must go to the slot prompt, got {waiting:?}"
    );
}

/// CR 110.4 + CR 702.103d + CR 118.9b: with Muldrotha's ENCHANTMENT slot already
/// spent this turn, a bestowed Boon Satyr (an enchantment spell) can't use
/// Muldrotha, but the printed creature cast can still take the free creature
/// slot. Bestow is optional, so that printed cast must go ahead instead of the
/// whole cast being refused.
#[test]
fn bestow_blocked_by_a_spent_slot_still_leaves_the_printed_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let muldrotha = add_permission_source(
        &mut scenario,
        "Muldrotha, the Gravetide",
        MULDROTHA,
        &["Elemental", "Avatar"],
    );
    let mut builder = scenario.add_creature_to_graveyard(P0, "Boon Satyr", 4, 2);
    builder.with_mana_cost(ManaCost::Cost {
        generic: 1,
        shards: vec![ManaCostShard::Green, ManaCostShard::Green],
    });
    builder.with_subtypes(vec!["Satyr"]);
    builder.from_oracle_text_with_keywords(&["Flash", "Bestow"], BOON_SATYR);
    let satyr = builder.id();
    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&satyr).unwrap();
        for types in [
            &mut obj.card_types.core_types,
            &mut obj.base_card_types.core_types,
        ] {
            if !types.contains(&CoreType::Enchantment) {
                types.push(CoreType::Enchantment);
            }
        }
    }
    runner
        .state_mut()
        .graveyard_cast_permissions_used_per_type
        .insert((muldrotha, CoreType::Enchantment));
    fill_mana(&mut runner, ManaType::Green);

    let waiting = cast_from_graveyard(&mut runner, satyr)
        .expect("the printed creature cast through the free creature slot is legal");
    assert!(
        !matches!(waiting, WaitingFor::AlternativeCastChoice { .. }),
        "bestow can't use Muldrotha's spent enchantment slot, so it must not be offered, \
         got {waiting:?}"
    );
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the printed cast must reach the stack"
    );
    assert!(
        runner.state().objects[&satyr]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "this must be the printed creature cast, not a bestowed Aura"
    );
    assert!(
        runner
            .state()
            .graveyard_cast_permissions_used_per_type
            .contains(&(muldrotha, CoreType::Creature)),
        "the printed cast must spend Muldrotha's creature slot, used: {:?}",
        runner.state().graveyard_cast_permissions_used_per_type
    );
}

/// CR 110.4 + CR 118.9b: the Blitz counterpart of the test above. Under
/// Encroaching Mycosynth Sabin is an artifact creature card, so its printed cast
/// through Muldrotha has two slots. Sabin's own rider still admits blitz, so the
/// choice is offered, and declining blitz goes to the slot prompt.
#[test]
fn blitz_declined_from_graveyard_takes_the_slot_prompt() {
    let parsed = parse_oracle_text(
        SABIN,
        "Sabin, Master Monk",
        &[],
        &["Legendary".into(), "Creature".into()],
        &["Human".into(), "Noble".into(), "Monk".into()],
    );
    let own_rider = parsed
        .statics
        .first()
        .expect("graveyard-cast permission static must parse")
        .clone();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_permission_source(
        &mut scenario,
        "Muldrotha, the Gravetide",
        MULDROTHA,
        &["Elemental", "Avatar"],
    );
    scenario.add_artifact_from_oracle(P0, "Encroaching Mycosynth", ENCROACHING_MYCOSYNTH);
    let sabin = scenario
        .add_creature_to_graveyard(P0, "Sabin, Master Monk", 4, 3)
        .with_static_definition(own_rider)
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::Red],
        })
        .with_keyword(blitz_keyword(&parsed))
        .id();
    scenario.add_card_to_hand(P0, "Filler Card");
    let mut runner = scenario.build();
    engine::game::layers::flush_layers(runner.state_mut());
    fill_mana(&mut runner, ManaType::Red);

    let waiting = cast_from_graveyard(&mut runner, sabin).expect("graveyard cast must be legal");
    assert!(
        matches!(waiting, WaitingFor::AlternativeCastChoice { .. }),
        "expected the blitz choice, got {waiting:?}"
    );
    let waiting = runner
        .act(GameAction::ChooseAlternativeCast {
            choice: AlternativeCastDecision::Normal,
        })
        .expect("choosing the printed cast must be legal")
        .waiting_for;
    assert!(
        matches!(
            &waiting,
            WaitingFor::ChoosePermanentTypeSlot { available_slots, .. }
                if available_slots.contains(&CoreType::Artifact)
                    && available_slots.contains(&CoreType::Creature)
        ),
        "the printed artifact creature cast must go to the slot prompt, got {waiting:?}"
    );
}

/// CR 110.4: the Blitz counterpart. Under Encroaching Mycosynth, Caldaia Guardian
/// is an artifact creature, so a blitz cast under Muldrotha would need the
/// slot choice too. Blitz is therefore not offered through Muldrotha, and the
/// printed-cost cast goes to the existing permanent-type slot prompt instead.
#[test]
fn blitz_needing_a_permanent_type_choice_is_not_offered() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_permission_source(
        &mut scenario,
        "Muldrotha, the Gravetide",
        MULDROTHA,
        &["Elemental", "Avatar"],
    );
    scenario.add_artifact_from_oracle(P0, "Encroaching Mycosynth", ENCROACHING_MYCOSYNTH);
    let guardian = scenario
        .add_creature_to_graveyard(P0, "Caldaia Guardian", 4, 3)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Green],
        })
        .with_keyword(caldaia_blitz())
        .id();
    let mut runner = scenario.build();
    engine::game::layers::flush_layers(runner.state_mut());
    fill_mana(&mut runner, ManaType::Green);
    assert!(
        runner.state().objects[&guardian]
            .card_types
            .core_types
            .contains(&CoreType::Artifact),
        "Encroaching Mycosynth must make the graveyard Caldaia an artifact creature"
    );

    let waiting = cast_from_graveyard(&mut runner, guardian)
        .expect("the printed-cost graveyard cast must still be legal");
    assert!(
        matches!(waiting, WaitingFor::ChoosePermanentTypeSlot { .. }),
        "blitz must not be offered through Muldrotha here, so the printed cast \
         goes to the slot prompt, got {waiting:?}"
    );
}

const KRARK_CLAN_IRONWORKS: &str = "Sacrifice an artifact: Add {C}{C}.";

/// CR 601.2a: a graveyard cast commits to one permission as its costs begin, and
/// that permission is the one spent, even if its source leaves during payment.
///
/// Exploration Broodship is scanned before Muldrotha, so it is elected and its
/// land sacrifice is charged. Broodship is then sacrificed to Krark-Clan
/// Ironworks for mana mid-payment. Re-electing at finalization would find only
/// Muldrotha and spend Muldrotha's creature slot for a cast Broodship paid for.
#[test]
fn blitz_keeps_its_elected_permission_when_the_source_leaves_mid_payment() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let broodship = add_exploration_broodship(&mut scenario, BROODSHIP_STATIONED);
    let muldrotha = add_permission_source(
        &mut scenario,
        "Muldrotha, the Gravetide",
        MULDROTHA,
        &["Elemental", "Avatar"],
    );
    let ironworks = scenario
        .add_artifact_from_oracle(P0, "Krark-Clan Ironworks", KRARK_CLAN_IRONWORKS)
        .id();
    let land = scenario.add_basic_land(P0, ManaColor::Green);
    let guardian = scenario
        .add_creature_to_graveyard(P0, "Caldaia Guardian", 4, 3)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Green],
        })
        .with_keyword(caldaia_blitz())
        .id();
    let mut runner = scenario.build();
    engine::game::layers::flush_layers(runner.state_mut());
    // Blitz {2}{G} is affordable from the pool alone, so it is offered; the
    // Ironworks activation below is the mid-payment board change.
    for _ in 0..3 {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Green,
            ObjectId(0),
            false,
            vec![],
        ));
    }

    let card_id = runner.state().objects[&guardian].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: guardian,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Manual,
        })
        .expect("graveyard cast must be legal");
    runner
        .act(GameAction::ChooseAlternativeCast {
            choice: AlternativeCastDecision::Alternative,
        })
        .expect("choosing blitz must be legal");
    // Broodship's rider: sacrifice a land.
    runner
        .act(GameAction::SelectCards { cards: vec![land] })
        .expect("paying Broodship's land sacrifice must be legal");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ManaPayment { .. }
    ));
    // Mid-payment: sacrifice Broodship to Krark-Clan Ironworks for mana.
    runner
        .act(GameAction::ActivateAbility {
            source_id: ironworks,
            ability_index: 0,
        })
        .expect("Krark-Clan Ironworks must be activatable during payment");
    runner
        .act(GameAction::SelectCards {
            cards: vec![broodship],
        })
        .expect("sacrificing Broodship for mana must be legal");
    runner
        .act(GameAction::PassPriority)
        .expect("committing the payment must complete the cast");

    // Positive reach guard: the cast completed, Broodship's rider was charged,
    // and Broodship really left mid-payment.
    assert_eq!(
        runner.state().stack.len(),
        1,
        "Caldaia must be on the stack"
    );
    assert_eq!(runner.state().objects[&land].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&broodship].zone, Zone::Graveyard);

    assert!(
        runner
            .state()
            .graveyard_cast_permissions_used
            .contains(&broodship),
        "Broodship admitted and was charged for this cast, so its slot is the one spent"
    );
    assert!(
        !runner
            .state()
            .graveyard_cast_permissions_used_per_type
            .contains(&(muldrotha, CoreType::Creature)),
        "Muldrotha did not admit this cast, so its creature slot must stay free, used: {:?}",
        runner.state().graveyard_cast_permissions_used_per_type
    );
}

const DEFILER_OF_INSTINCT: &str = "First strike\nAs an additional cost to cast red permanent spells, you may pay 2 life. Those spells cost {R} less to cast if you paid life this way. This effect reduces only the amount of red mana you pay.\nWhenever you cast a red permanent spell, this creature deals 1 damage to any target.";

/// Put Defiler of Instinct on P0's battlefield with the statics its Oracle text
/// parses to (not its cast trigger, which would add an unrelated target prompt).
fn add_defiler_of_instinct(scenario: &mut GameScenario) -> ObjectId {
    let parsed = parse_oracle_text(
        DEFILER_OF_INSTINCT,
        "Defiler of Instinct",
        &["First strike".into()],
        &["Creature".into()],
        &["Phyrexian".into(), "Kavu".into()],
    );
    assert!(
        parsed
            .statics
            .iter()
            .any(|s| format!("{s:?}").contains("DefilerCostReduction")),
        "Defiler of Instinct must parse to a Defiler cost reduction, got {:?}",
        parsed.statics
    );
    let mut defiler = scenario.add_creature(P0, "Defiler of Instinct", 3, 3);
    for s in parsed.statics {
        defiler.with_static_definition(s);
    }
    defiler.id()
}

/// Sabin in the graveyard with its own blitz rider, Defiler of Instinct out,
/// a card in hand for the discard, and `red` red mana in the pool.
fn sabin_with_defiler(red: usize) -> (GameRunner, ObjectId, ObjectId) {
    let parsed = parse_oracle_text(
        SABIN,
        "Sabin, Master Monk",
        &[],
        &["Legendary".into(), "Creature".into()],
        &["Human".into(), "Noble".into(), "Monk".into()],
    );
    let own_rider = parsed
        .statics
        .first()
        .expect("graveyard-cast permission static must parse")
        .clone();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_defiler_of_instinct(&mut scenario);
    let sabin = scenario
        .add_creature_to_graveyard(P0, "Sabin, Master Monk", 4, 3)
        .with_static_definition(own_rider)
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::Red],
        })
        .with_keyword(blitz_keyword(&parsed))
        .with_color(vec![ManaColor::Red])
        .id();
    let filler = scenario.add_card_to_hand(P0, "Filler Card");
    let mut runner = scenario.build();
    for _ in 0..red {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Red,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    (runner, sabin, filler)
}

/// CR 601.2b + CR 118.9d: a matching Defiler's reduction applies to Sabin's
/// blitz cost. Blitz {2}{R}{R} less {R} is 3 mana, and only 3 red mana is
/// available, so the cast is affordable ONLY with the Defiler. The Defiler is
/// offered before the discard residual, and the discard is still paid.
#[test]
fn blitz_with_a_non_mana_rider_offers_a_matching_defiler() {
    let (mut runner, sabin, filler) = sabin_with_defiler(3);
    let life_before = runner.state().players[0].life;

    let card_id = runner.state().objects[&sabin].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: sabin,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("blitz affordable with the Defiler reduction must be castable");
    // Positive reach guard: the Defiler choice is offered.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DefilerPayment { .. }
        ),
        "the matching Defiler must be offered, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("paying the Defiler's life must be legal");
    pay_offered_costs(&mut runner);

    assert_eq!(runner.state().objects[&sabin].zone, Zone::Stack);
    assert_eq!(
        runner.state().players[0].life,
        life_before - 2,
        "the Defiler's 2 life must be paid"
    );
    assert_eq!(
        runner.state().objects[&filler].zone,
        Zone::Graveyard,
        "blitz's discard residual must still be paid"
    );
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "blitz {{2}}{{R}}{{R}} less the Defiler's {{R}} costs exactly the 3 mana available"
    );
}

/// Control: declining the Defiler pays the full blitz cost and the discard.
#[test]
fn blitz_with_a_declined_defiler_pays_its_full_cost_and_rider() {
    let (mut runner, sabin, filler) = sabin_with_defiler(4);
    let life_before = runner.state().players[0].life;

    let card_id = runner.state().objects[&sabin].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: sabin,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("graveyard blitz must be castable");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::DefilerPayment { .. }
    ));
    runner
        .act(GameAction::DecideOptionalCost { pay: false })
        .expect("declining the Defiler must be legal");
    pay_offered_costs(&mut runner);

    assert_eq!(runner.state().objects[&sabin].zone, Zone::Stack);
    assert_eq!(runner.state().players[0].life, life_before);
    assert_eq!(runner.state().objects[&filler].zone, Zone::Graveyard);
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "the full blitz {{2}}{{R}}{{R}} = 4 must be paid"
    );
}

const PHOENIX_ORACLE: &str = "Bestow\u{2014}{R}, Collect evidence 6. (To pay this bestow cost, pay {R} and exile cards with total mana value 6 or greater from your graveyard.)\nFlying, haste\nEnchanted creature gets +2/+2 and has flying and haste.\nYou may cast this card from your graveyard using its bestow ability.";

/// CR 601.2b + CR 118.9d: the same Defiler composition on a residual branch
/// this change did not introduce: Detective's Phoenix's compound bestow
/// ("{R}, Collect evidence 6"). Defiler of Instinct removes the {R}, so with
/// no mana at all the bestow is affordable ONLY with the Defiler, and Collect
/// evidence is still paid.
#[test]
fn compound_bestow_offers_a_matching_defiler_and_still_collects_evidence() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_defiler_of_instinct(&mut scenario);
    let mut builder = scenario.add_creature_to_graveyard(P0, "Detective's Phoenix", 2, 2);
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::Red, ManaCostShard::Red],
        generic: 3,
    });
    builder.with_subtypes(vec!["Phoenix"]);
    builder.with_color(vec![ManaColor::Red]);
    builder.from_oracle_text_with_keywords(&["Flying", "Haste"], PHOENIX_ORACLE);
    let phoenix = builder.id();
    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&phoenix).unwrap();
        for types in [
            &mut obj.card_types.core_types,
            &mut obj.base_card_types.core_types,
        ] {
            if !types.contains(&CoreType::Enchantment) {
                types.push(CoreType::Enchantment);
            }
        }
    }
    let fodder: Vec<ObjectId> = (0..2)
        .map(|i| {
            let card_id = engine::types::identifiers::CardId(runner.state().next_object_id);
            let id = engine::game::zones::create_object(
                runner.state_mut(),
                card_id,
                P0,
                format!("Evidence {i}"),
                Zone::Graveyard,
            );
            runner.state_mut().objects.get_mut(&id).unwrap().mana_cost = ManaCost::generic(3);
            id
        })
        .collect();
    let life_before = runner.state().players[0].life;

    let card_id = runner.state().objects[&phoenix].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: phoenix,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("bestow affordable with the Defiler reduction must be castable");
    // The Aura target may be chosen first; the Defiler must come before the
    // Collect evidence residual.
    if let WaitingFor::TargetSelection { .. } = runner.state().waiting_for {
        runner
            .choose_first_legal_target()
            .expect("Defiler of Instinct is a legal creature to enchant");
    }
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::DefilerPayment { .. }
        ),
        "the matching Defiler must be offered, got {:?}",
        runner.state().waiting_for
    );
    runner
        .act(GameAction::DecideOptionalCost { pay: true })
        .expect("paying the Defiler's life must be legal");
    match runner.state().waiting_for.clone() {
        WaitingFor::CollectEvidenceChoice { .. } => {
            runner
                .act(GameAction::SelectCards {
                    cards: fodder.clone(),
                })
                .expect("collecting evidence with two MV-3 cards must be legal");
        }
        other => panic!("Collect evidence must still be asked for, got {other:?}"),
    }
    if let WaitingFor::TargetSelection { .. } = runner.state().waiting_for {
        runner
            .choose_first_legal_target()
            .expect("Defiler of Instinct is a legal creature to enchant");
    }

    assert_eq!(runner.state().objects[&phoenix].zone, Zone::Stack);
    assert_eq!(runner.state().players[0].life, life_before - 2);
    for id in &fodder {
        assert_eq!(
            runner.state().objects[id].zone,
            Zone::Exile,
            "Collect evidence must still be paid"
        );
    }
}

const YAWGMOTHS_WILL: &str = "Until end of turn, you may play lands and cast spells from your graveyard.\nIf a card would be put into your graveyard from anywhere this turn, exile that card instead.";

/// CR 601.2a + CR 611.2c: a resolution-created graveyard permission (Yawgmoth's
/// Will) admits a blitz cast too. The single election reads the same permission
/// scan castability does, including its transient (resolution-created) sources,
/// so the recorded authority is found and the cast is not refused by the
/// fail-closed check at finalization.
///
/// Runs on a 256 MB stack because `parse_oracle_text` overflows the default test
/// stack on Yawgmoth's Will (see `will_cycle_delivery.rs`).
#[test]
fn blitz_from_graveyard_under_a_resolution_created_permission_is_not_refused() {
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(|| {
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let guardian = scenario
                .add_creature_to_graveyard(P0, "Caldaia Guardian", 4, 3)
                .with_mana_cost(ManaCost::Cost {
                    generic: 3,
                    shards: vec![ManaCostShard::Green],
                })
                .with_keyword(caldaia_blitz())
                .id();
            let will = scenario
                .add_spell_to_hand_from_oracle(P0, "Yawgmoth's Will", false, YAWGMOTHS_WILL)
                .id();
            let mut runner = scenario.build();
            let _ = runner.cast(will).resolve();
            fill_mana(&mut runner, ManaType::Green);

            // Positive reach guard: the Will resolved, so the graveyard cast is
            // admitted by its resolution-created permission alone.
            let waiting = cast_from_graveyard(&mut runner, guardian)
                .expect("Yawgmoth's Will must admit the graveyard cast");
            assert!(
                matches!(
                    waiting,
                    WaitingFor::AlternativeCastChoice {
                        keyword: engine::types::game_state::AlternativeCastKeyword::Blitz,
                        ..
                    }
                ),
                "the Will authorizes the printed cast too, so blitz is a choice, got {waiting:?}"
            );
            runner
                .act(GameAction::ChooseAlternativeCast {
                    choice: AlternativeCastDecision::Alternative,
                })
                .expect("the blitz cast under the Will must not be refused at finalization");
            assert_eq!(runner.state().objects[&guardian].zone, Zone::Stack);
            assert_eq!(
                runner.state().players[0].mana_pool.total(),
                5,
                "blitz {{2}}{{G}} = 3 of the 8 mana must be spent"
            );
        })
        .expect("spawn 256MB test thread")
        .join()
        .unwrap_or_else(|payload| std::panic::resume_unwind(payload));
}

/// Set `enters_with_counter` on every graveyard-cast permission a spell's
/// ability grants: the permission rides a `GrantStaticAbility` modification
/// on one of its `GenericEffect` statics. Returns how many were set.
fn set_granted_permission_counter(
    ability: &mut engine::types::ability::AbilityDefinition,
    counter: Option<CounterType>,
) -> usize {
    fn set_on_static(
        definition: &mut engine::types::ability::StaticDefinition,
        counter: &Option<CounterType>,
    ) -> usize {
        let mut set = 0;
        if let engine::types::statics::StaticMode::GraveyardCastPermission {
            enters_with_counter,
            ..
        } = &mut definition.mode
        {
            *enters_with_counter = counter.clone();
            set += 1;
        }
        for modification in definition.modifications.iter_mut() {
            if let engine::types::ability::ContinuousModification::GrantStaticAbility {
                definition,
            } = modification
            {
                set += set_on_static(definition, counter);
            }
        }
        set
    }
    let mut set = 0;
    if let engine::types::ability::Effect::GenericEffect {
        static_abilities, ..
    } = &mut *ability.effect
    {
        for definition in static_abilities.iter_mut() {
            set += set_on_static(definition, &counter);
        }
    }
    if let Some(sub) = ability.sub_ability.as_deref_mut() {
        set += set_granted_permission_counter(sub, counter);
    }
    set
}

/// Resolve Yawgmoth's Will with its granted graveyard permission's
/// enters-with rider set to `counter`, cast a creature from the graveyard
/// through it, and return the counters the creature entered with.
fn counters_after_casting_under_a_transient_permission(
    counter: Option<CounterType>,
) -> std::collections::HashMap<CounterType, u32> {
    std::thread::Builder::new()
        .stack_size(256 * 1024 * 1024)
        .spawn(move || {
            let parsed = parse_oracle_text(
                YAWGMOTHS_WILL,
                "Yawgmoth's Will",
                &[],
                &["Sorcery".into()],
                &[],
            );
            let mut scenario = GameScenario::new();
            scenario.at_phase(Phase::PreCombatMain);
            let bears = scenario
                .add_creature_to_graveyard(P0, "Grizzly Bears", 2, 2)
                .with_mana_cost(ManaCost::Cost {
                    generic: 1,
                    shards: vec![ManaCostShard::Green],
                })
                .id();
            let mut will = scenario.add_spell_to_hand(P0, "Yawgmoth's Will", false);
            let mut granted = 0;
            for mut ability in parsed.abilities {
                granted += set_granted_permission_counter(&mut ability, counter.clone());
                will.with_ability_definition(ability);
            }
            // Reach guard: the rider really landed on the Will's granted permission.
            assert_eq!(
                granted, 1,
                "Yawgmoth's Will must grant exactly one graveyard-cast permission"
            );
            let will = will.id();
            let mut runner = scenario.build();
            let _ = runner.cast(will).resolve();
            fill_mana(&mut runner, ManaType::Green);

            let card_id = runner.state().objects[&bears].card_id;
            runner
                .act(GameAction::CastSpell {
                    object_id: bears,
                    card_id,
                    targets: vec![],
                    payment_mode: CastPaymentMode::Auto,
                })
                .expect("the resolution-created permission must admit the graveyard cast");
            runner.resolve_top();
            let entered = &runner.state().objects[&bears];
            assert_eq!(entered.zone, Zone::Battlefield, "the creature must resolve");
            entered.counters.clone()
        })
        .expect("spawn 256MB test thread")
        .join()
        .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
}

/// CR 611.2a + CR 614.1c: a resolution-created graveyard permission's
/// "enters with a counter" rider applies to a creature cast through it. No
/// printed card grants a transient graveyard permission with such a rider, so
/// the rider is set on Yawgmoth's Will's own grant; the grant still resolves
/// through the engine exactly as the card's does.
#[test]
fn a_resolution_created_permissions_counter_rider_applies() {
    let counters = counters_after_casting_under_a_transient_permission(Some(CounterType::Finality));
    assert_eq!(
        counters.get(&CounterType::Finality).copied(),
        Some(1),
        "the transient permission's finality rider must apply, counters: {counters:?}"
    );
}

/// Control: with no rider on the transient permission, no counter is added.
#[test]
fn a_resolution_created_permission_without_a_rider_adds_no_counter() {
    let counters = counters_after_casting_under_a_transient_permission(None);
    assert!(
        counters.is_empty(),
        "no rider, so no counter, counters: {counters:?}"
    );
}

/// Put exactly `count` mana of one color in the active player's pool.
fn add_mana(runner: &mut GameRunner, mana: ManaType, count: usize) {
    let dummy = ObjectId(0);
    let pool = &mut runner.state_mut().players[0].mana_pool;
    for _ in 0..count {
        pool.add(ManaUnit::new(mana, dummy, false, vec![]));
    }
}

/// The `CastSpell` action `legal_actions` offers for `id`, if any.
fn offered_cast(runner: &GameRunner, id: ObjectId) -> Option<GameAction> {
    engine::ai_support::legal_actions(runner.state())
        .into_iter()
        .find(
            |action| matches!(action, GameAction::CastSpell { object_id, .. } if *object_id == id),
        )
}

/// Sabin in the graveyard with its own "using its blitz ability" permission, a
/// card in hand to discard, and exactly `red` red mana.
fn sabin_in_graveyard_with(red: usize) -> (GameRunner, ObjectId) {
    let parsed = parse_oracle_text(
        SABIN,
        "Sabin, Master Monk",
        &[],
        &["Legendary".into(), "Creature".into()],
        &["Human".into(), "Noble".into(), "Monk".into()],
    );
    let kw = blitz_keyword(&parsed);
    let gy_static = parsed
        .statics
        .first()
        .expect("graveyard-cast permission static must parse")
        .clone();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let sabin = scenario
        .add_creature_to_graveyard(P0, "Sabin, Master Monk", 4, 3)
        .with_static_definition(gy_static)
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::Red],
        })
        .with_keyword(kw)
        .id();
    scenario.add_card_to_hand(P0, "Filler Card");
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Red, red);
    (runner, sabin)
}

/// CR 702.152a + CR 601.2f + CR 601.2a: with exactly the four mana Sabin's
/// blitz costs ({2}{R}{R}) and a card to discard, the graveyard blitz cast is a
/// legal action even though the printed {4}{R} is unaffordable, and the offered
/// action completes. `legal_actions` and the cast handler must agree.
#[test]
fn graveyard_blitz_affordable_only_for_its_blitz_cost_is_a_legal_action() {
    let (mut runner, sabin) = sabin_in_graveyard_with(4);
    let action = offered_cast(&runner, sabin)
        .expect("the graveyard blitz cast must be offered with exactly its blitz mana");
    runner
        .act(action)
        .expect("the offered graveyard blitz cast must be accepted");
    let filler = runner.state().players[0].hand[0];
    runner
        .act(GameAction::SelectCards {
            cards: vec![filler],
        })
        .expect("paying the blitz discard must succeed");
    assert_eq!(runner.state().objects[&sabin].zone, Zone::Stack);
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "the whole blitz cost was paid"
    );
}

/// Control: one mana short of the blitz cost, the cast is not offered.
#[test]
fn graveyard_blitz_one_mana_short_is_not_a_legal_action() {
    let (runner, sabin) = sabin_in_graveyard_with(3);
    assert!(
        offered_cast(&runner, sabin).is_none(),
        "three mana cannot pay {{2}}{{R}}{{R}}"
    );
}

/// Detective's Phoenix in the graveyard with `red` red mana, a creature to
/// enchant, and two mana-value-3 cards to collect as evidence.
fn phoenix_in_graveyard_with(red: usize) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    phoenix_in_graveyard(red, true, true)
}

/// Detective's Phoenix in the graveyard with `red` red mana, optionally a
/// creature to enchant and two mana-value-3 cards to collect as evidence.
fn phoenix_in_graveyard(
    red: usize,
    with_target: bool,
    with_evidence: bool,
) -> (GameRunner, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    if with_target {
        scenario.add_creature(P0, "Grizzly Bears", 2, 2);
    }
    let mut builder = scenario.add_creature_to_graveyard(P0, "Detective's Phoenix", 2, 2);
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 2,
    });
    builder.with_subtypes(vec!["Phoenix"]);
    builder.with_color(vec![ManaColor::Red]);
    builder.from_oracle_text_with_keywords(&["Flying", "Haste"], PHOENIX_ORACLE);
    let phoenix = builder.id();
    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&phoenix).unwrap();
        for types in [
            &mut obj.card_types.core_types,
            &mut obj.base_card_types.core_types,
        ] {
            if !types.contains(&CoreType::Enchantment) {
                types.push(CoreType::Enchantment);
            }
        }
    }
    let fodder: Vec<ObjectId> = (0..if with_evidence { 2 } else { 0 })
        .map(|i| {
            let card_id = engine::types::identifiers::CardId(runner.state().next_object_id);
            let id = engine::game::zones::create_object(
                runner.state_mut(),
                card_id,
                P0,
                format!("Evidence {i}"),
                Zone::Graveyard,
            );
            runner.state_mut().objects.get_mut(&id).unwrap().mana_cost = ManaCost::generic(3);
            id
        })
        .collect();
    add_mana(&mut runner, ManaType::Red, red);
    (runner, phoenix, fodder)
}

/// CR 702.103a + CR 601.2f + CR 601.2a: the Bestow sibling. With only the {R}
/// of Detective's Phoenix's bestow cost (its printed {2}{R} is unaffordable), a
/// legal creature to enchant and evidence to collect, the graveyard bestow cast
/// is a legal action, and the offered action completes.
#[test]
fn graveyard_bestow_affordable_only_for_its_bestow_cost_is_a_legal_action() {
    let (mut runner, phoenix, fodder) = phoenix_in_graveyard_with(1);
    let action = offered_cast(&runner, phoenix)
        .expect("the graveyard bestow cast must be offered with only its bestow mana");
    runner
        .act(action)
        .expect("the offered graveyard bestow cast must be accepted");
    for _ in 0..3 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .choose_first_legal_target()
                    .expect("Grizzly Bears is a legal creature to enchant");
            }
            WaitingFor::CollectEvidenceChoice { .. } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: fodder.clone(),
                    })
                    .expect("collecting evidence with two MV-3 cards must be legal");
            }
            _ => break,
        }
    }
    assert_eq!(runner.state().objects[&phoenix].zone, Zone::Stack);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
    for id in &fodder {
        assert_eq!(runner.state().objects[id].zone, Zone::Exile);
    }
}

/// Control: with no mana at all the bestow {R} is unaffordable, so the cast is
/// not offered.
#[test]
fn graveyard_bestow_without_its_mana_is_not_a_legal_action() {
    let (runner, phoenix, _) = phoenix_in_graveyard_with(0);
    assert!(
        offered_cast(&runner, phoenix).is_none(),
        "no mana cannot pay the bestow {{R}}"
    );
}

/// Tenacious Underdog in the graveyard under only its own "using its blitz
/// ability" permission, with `black` black mana and P0 at `life`.
fn underdog_in_graveyard_with(black: usize, life: i32) -> (GameRunner, ObjectId) {
    let parsed = parse_oracle_text(
        UNDERDOG,
        "Tenacious Underdog",
        &[],
        &["Creature".into()],
        &["Human".into(), "Warrior".into()],
    );
    let kw = blitz_keyword(&parsed);
    let gy_static = parsed
        .statics
        .first()
        .expect("graveyard-cast permission static must parse")
        .clone();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let dog = scenario
        .add_creature_to_graveyard(P0, "Tenacious Underdog", 3, 2)
        .with_static_definition(gy_static)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Black],
        })
        .with_keyword(kw)
        .id();
    let mut runner = scenario.build();
    runner.state_mut().players[0].life = life;
    add_mana(&mut runner, ManaType::Black, black);
    (runner, dog)
}

/// CR 601.2a + CR 118.9a: a "using its blitz ability" permission admits only the
/// blitz cast. With two black mana and 1 life, Underdog's printed {1}{B} is
/// affordable but its blitz ({2}{B}{B}, Pay 2 life) is not, so the card is
/// neither offered nor castable: castability must not fall back to the printed
/// cost the permission never granted.
#[test]
fn graveyard_blitz_only_permission_does_not_admit_an_affordable_printed_cost() {
    let (mut runner, dog) = underdog_in_graveyard_with(2, 1);
    assert!(
        offered_cast(&runner, dog).is_none(),
        "only the printed cost is affordable, and the permission doesn't grant it"
    );
    assert!(
        cast_from_graveyard(&mut runner, dog).is_err(),
        "the cast handler must refuse the same cast"
    );
    assert_eq!(runner.state().objects[&dog].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[0].mana_pool.total(), 2);
}

/// Control: once blitz itself is payable (four black mana, 3 life), the same
/// permission offers the cast.
#[test]
fn graveyard_blitz_only_permission_offers_a_payable_blitz() {
    let (runner, dog) = underdog_in_graveyard_with(4, 3);
    assert!(
        offered_cast(&runner, dog).is_some(),
        "a payable blitz is offered"
    );
}

/// CR 601.2a + CR 118.9a + CR 702.103a + CR 303.4a: the Bestow sibling. Detective's
/// Phoenix's "using its bestow ability" permission admits only the bestow cast.
/// With three red mana its printed {2}{R} is affordable, but bestow needs a
/// creature to enchant AND evidence to collect. Remove either, and the cast is
/// neither offered nor accepted.
#[test]
fn graveyard_bestow_only_permission_does_not_admit_an_affordable_printed_cost() {
    for (with_target, with_evidence) in [(false, true), (true, false)] {
        let (mut runner, phoenix, _) = phoenix_in_graveyard(3, with_target, with_evidence);
        let case = format!("target={with_target} evidence={with_evidence}");
        assert!(
            offered_cast(&runner, phoenix).is_none(),
            "{case}: bestow is unpayable and the permission doesn't grant the printed cast"
        );
        assert!(
            cast_from_graveyard(&mut runner, phoenix).is_err(),
            "{case}: the cast handler must refuse the same cast"
        );
        assert_eq!(runner.state().objects[&phoenix].zone, Zone::Graveyard);
    }
}

/// Control: with both the target and the evidence, the three-mana board offers
/// the cast (bestow's {R} is payable).
#[test]
fn graveyard_bestow_only_permission_offers_a_payable_bestow() {
    let (runner, phoenix, _) = phoenix_in_graveyard(3, true, true);
    assert!(offered_cast(&runner, phoenix).is_some());
}

const BROKKOS: &str = "Mutate {2}{U/B}{G}{G} (If you cast this spell for its mutate cost, put it over or under target non-Human creature you own. They mutate into the creature on top plus all abilities from under it.)\nTrample\nYou may cast this card from your graveyard using its mutate ability.";

const FLORAL_INVOCATIONS: &str = "You may play lands and cast creature spells from your graveyard.";

/// Brokkos, Apex of Forever in the graveyard under its own "using its mutate
/// ability" permission, with its printed {2}{B}{G}{U} affordable and no creature
/// to mutate onto. With `unconstrained`, an Advanced Floral Invocations-style
/// permission that leaves the casting method open is added too.
fn brokkos_in_graveyard(unconstrained: bool) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    if unconstrained {
        let enabler = parse_oracle_text(
            FLORAL_INVOCATIONS,
            "Advanced Floral Invocations",
            &[],
            &["Enchantment".into()],
            &[],
        );
        let permission = enabler
            .statics
            .iter()
            .find(|s| format!("{s:?}").contains("GraveyardCastPermission"))
            .expect("an unconstrained GraveyardCastPermission must parse")
            .clone();
        scenario
            .add_enchantment_from_oracle(P0, "Advanced Floral Invocations", FLORAL_INVOCATIONS)
            .with_static_definition(permission);
    }
    let mut builder = scenario.add_creature_to_graveyard(P0, "Brokkos, Apex of Forever", 6, 6);
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![
            ManaCostShard::Black,
            ManaCostShard::Green,
            ManaCostShard::Blue,
        ],
        generic: 2,
    });
    builder.with_subtypes(vec!["Nightmare", "Beast", "Elemental"]);
    builder.from_oracle_text_with_keywords(&["Mutate", "Trample"], BROKKOS);
    let brokkos = builder.id();
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Black, 2);
    add_mana(&mut runner, ManaType::Green, 1);
    add_mana(&mut runner, ManaType::Blue, 1);
    add_mana(&mut runner, ManaType::Colorless, 1);
    (runner, brokkos)
}

/// CR 118.9b: Brokkos's graveyard permission requires its mutate cast, and the
/// engine has no graveyard mutate route, so the permission is an honest gap:
/// the clause is not modeled as a graveyard permission, and Brokkos is neither
/// offered nor cast from the graveyard (before, it was cast for its printed
/// {2}{B}{G}{U}, measured).
#[test]
fn graveyard_mutate_only_permission_is_an_honest_gap() {
    let (mut runner, brokkos) = brokkos_in_graveyard(false);
    assert!(
        !runner.state().objects[&brokkos]
            .static_definitions
            .as_slice()
            .iter()
            .any(|s| format!("{s:?}").contains("GraveyardCastPermission")),
        "the mutate-only permission must not be modeled"
    );
    assert!(
        offered_cast(&runner, brokkos).is_none(),
        "no modeled permission, so no graveyard cast is offered"
    );
    assert!(
        cast_from_graveyard(&mut runner, brokkos).is_err(),
        "the cast handler must refuse the printed cost too"
    );
    assert_eq!(runner.state().objects[&brokkos].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[0].mana_pool.total(), 5);
}

/// Reach guard: the same board plus a permission that leaves the method open
/// offers the printed cast and completes it for {2}{B}{G}{U}.
#[test]
fn graveyard_printed_cost_is_admitted_by_an_unconstrained_permission() {
    let (mut runner, brokkos) = brokkos_in_graveyard(true);
    let action = offered_cast(&runner, brokkos)
        .expect("an unconstrained permission admits the printed cast");
    runner
        .act(action)
        .expect("the offered printed cast completes");
    assert_eq!(runner.state().objects[&brokkos].zone, Zone::Stack);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
}
/// CR 721.2a: Exploration Broodship's graveyard permission is printed in its 8+
/// striation, so with seven charge counters it grants nothing, and Caldaia
/// Guardian (no permission of its own) can't be cast from the graveyard.
#[test]
fn broodship_below_its_station_threshold_admits_no_graveyard_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_exploration_broodship(&mut scenario, BROODSHIP_STATIONED - 1);
    scenario.add_basic_land(P0, ManaColor::Green);
    let guardian = scenario
        .add_creature_to_graveyard(P0, "Caldaia Guardian", 4, 3)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Green],
        })
        .with_keyword(caldaia_blitz())
        .id();
    let mut runner = scenario.build();
    engine::game::layers::flush_layers(runner.state_mut());
    fill_mana(&mut runner, ManaType::Green);
    assert!(offered_cast(&runner, guardian).is_none());
    assert!(cast_from_graveyard(&mut runner, guardian).is_err());
}

// --- CR 118.9b: the permission's required casting method -----------------

use engine::types::ability::{
    CardPlayMode, FilterProp, StaticDefinition, TargetFilter, TypedFilter,
};
use engine::types::keywords::KeywordKind;
use engine::types::statics::{CastFrequency, StaticMode};

const UNDERWORLD_BREACH: &str = "Each nonland card in your graveyard has escape. The escape cost is equal to the card's mana cost plus exile three other cards from your graveyard. (You may cast cards from your graveyard for their escape cost.)\nAt the beginning of the end step, sacrifice this enchantment.";

const LURRUS: &str = "Lifelink\nOnce during each of your turns, you may cast a permanent spell with mana value 2 or less from your graveyard.";

/// A creature-card graveyard permission built directly: `frequency`, the
/// casting method it requires, and any extra card-selection properties.
fn creature_permission(
    frequency: CastFrequency,
    required_cast_keyword: Option<KeywordKind>,
    selection: Vec<FilterProp>,
) -> StaticDefinition {
    let mut filter = TypedFilter::creature();
    filter.properties.extend(selection);
    StaticDefinition::new(StaticMode::GraveyardCastPermission {
        frequency,
        play_mode: CardPlayMode::Cast,
        graveyard_destination_replacement: None,
        extra_cost: None,
        enters_with_counter: None,
        required_cast_keyword,
    })
    .affected(TargetFilter::Typed(filter))
}

fn add_permission_host(scenario: &mut GameScenario, name: &str, def: StaticDefinition) -> ObjectId {
    scenario
        .add_creature(P0, name, 1, 1)
        .with_static_definition(def)
        .id()
}

fn add_bears_to_graveyard(scenario: &mut GameScenario, name: &str) -> ObjectId {
    scenario
        .add_creature_to_graveyard(P0, name, 2, 2)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Green],
        })
        .id()
}

/// CR 601.2a + CR 118.9b: a printed-cost graveyard cast commits to a permission
/// that leaves the method open, even when a method-restricted one is scanned
/// first. A sneak-only permission (Ninja Teen's shape, built directly because
/// the parser declines Ninja Teen) sits before Muldrotha; the printed Grizzly
/// Bears cast spends Muldrotha's creature slot, so a second creature is refused.
#[test]
fn printed_cast_commits_to_an_open_permission_scanned_after_a_restricted_one() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_permission_host(
        &mut scenario,
        "Sneak Permission",
        creature_permission(CastFrequency::Unlimited, Some(KeywordKind::Sneak), vec![]),
    );
    let muldrotha = add_permission_source(
        &mut scenario,
        "Muldrotha, the Gravetide",
        MULDROTHA,
        &["Elemental", "Avatar"],
    );
    let bears = add_bears_to_graveyard(&mut scenario, "Grizzly Bears");
    let second = add_bears_to_graveyard(&mut scenario, "Second Bears");
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Green, 4);

    cast_from_graveyard(&mut runner, bears).expect("the printed cast is legal under Muldrotha");
    assert_eq!(runner.state().objects[&bears].zone, Zone::Stack);
    assert!(
        runner
            .state()
            .graveyard_cast_permissions_used_per_type
            .contains(&(muldrotha, CoreType::Creature)),
        "the printed cast must spend Muldrotha's creature slot, used: {:?}",
        runner.state().graveyard_cast_permissions_used_per_type
    );
    runner.resolve_top();
    assert!(
        cast_from_graveyard(&mut runner, second).is_err(),
        "Muldrotha's creature slot is spent, and the sneak-only permission can't \
         authorize a printed cast"
    );
}

/// CR 118.9b: a permission that requires one method can't authorize another.
/// Sabin, with no permission of its own, beside a sneak-only permission: its
/// payable blitz is neither offered nor cast through that permission, and
/// neither is its printed cost.
#[test]
fn blitz_is_not_cast_through_a_permission_requiring_another_method() {
    let parsed = parse_oracle_text(
        SABIN,
        "Sabin, Master Monk",
        &[],
        &["Legendary".into(), "Creature".into()],
        &["Human".into(), "Noble".into(), "Monk".into()],
    );
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_permission_host(
        &mut scenario,
        "Sneak Permission",
        creature_permission(CastFrequency::Unlimited, Some(KeywordKind::Sneak), vec![]),
    );
    let sabin = scenario
        .add_creature_to_graveyard(P0, "Sabin, Master Monk", 4, 3)
        .with_mana_cost(ManaCost::Cost {
            generic: 4,
            shards: vec![ManaCostShard::Red],
        })
        .with_keyword(blitz_keyword(&parsed))
        .id();
    scenario.add_card_to_hand(P0, "Filler Card");
    let mut runner = scenario.build();
    fill_mana(&mut runner, ManaType::Red);

    assert!(
        offered_cast(&runner, sabin).is_none(),
        "a sneak-only permission must not offer Sabin's blitz or printed cast"
    );
    assert!(cast_from_graveyard(&mut runner, sabin).is_err());
    assert_eq!(runner.state().objects[&sabin].zone, Zone::Graveyard);
}

/// CR 601.2a: between method-open permissions, the printed cast prefers a
/// strictly dominant one (unlimited, no rider). Muldrotha is scanned before an
/// unlimited creature permission, and the printed cast spends no Muldrotha slot.
#[test]
fn printed_cast_prefers_a_strictly_dominant_open_permission() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_permission_source(
        &mut scenario,
        "Muldrotha, the Gravetide",
        MULDROTHA,
        &["Elemental", "Avatar"],
    );
    add_permission_host(
        &mut scenario,
        "Open Permission",
        creature_permission(CastFrequency::Unlimited, None, vec![]),
    );
    let bears = add_bears_to_graveyard(&mut scenario, "Grizzly Bears");
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Green, 2);

    cast_from_graveyard(&mut runner, bears).expect("the printed cast is legal");
    assert_eq!(runner.state().objects[&bears].zone, Zone::Stack);
    assert!(
        runner
            .state()
            .graveyard_cast_permissions_used_per_type
            .is_empty(),
        "the unlimited permission authorizes it, so Muldrotha's slot stays free, used: {:?}",
        runner.state().graveyard_cast_permissions_used_per_type
    );
}

/// CR 601.2a: two bounded open permissions (Muldrotha and Lurrus) are a real
/// choice the player would announce, which the engine does not model. The
/// printed cast takes the first in source order, a documented policy: this test
/// pins it rather than claiming it is the rules result.
#[test]
fn competing_bounded_open_permissions_follow_source_order() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let muldrotha = add_permission_source(
        &mut scenario,
        "Muldrotha, the Gravetide",
        MULDROTHA,
        &["Elemental", "Avatar"],
    );
    let lurrus = add_permission_source(
        &mut scenario,
        "Lurrus of the Dream-Den",
        LURRUS,
        &["Cat", "Nightmare"],
    );
    let bears = add_bears_to_graveyard(&mut scenario, "Grizzly Bears");
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Green, 2);

    cast_from_graveyard(&mut runner, bears).expect("the printed cast is legal");
    assert!(
        runner
            .state()
            .graveyard_cast_permissions_used_per_type
            .contains(&(muldrotha, CoreType::Creature)),
        "the first bounded permission (Muldrotha) is spent"
    );
    assert!(
        !runner
            .state()
            .graveyard_cast_permissions_used
            .contains(&lurrus),
        "Lurrus's once-per-turn slot is left unspent"
    );
}

/// Negative control: a keyword that only SELECTS cards stays a selector. A
/// method-open permission for creature cards with flying admits a flier's
/// printed cast; `required_cast_keyword` is what restricts a method, not a
/// `HasKeywordKind` in the filter.
#[test]
fn a_keyword_selector_permission_admits_the_printed_cast() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_permission_host(
        &mut scenario,
        "Flier Permission",
        creature_permission(
            CastFrequency::Unlimited,
            None,
            vec![FilterProp::HasKeywordKind {
                value: KeywordKind::Flying,
            }],
        ),
    );
    let crow = scenario
        .add_creature_to_graveyard(P0, "Storm Crow", 1, 2)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Blue],
        })
        .with_keyword(Keyword::Flying)
        .id();
    let mut runner = scenario.build();
    add_mana(&mut runner, ManaType::Blue, 2);

    let action = offered_cast(&runner, crow).expect("the selector admits the flier's printed cast");
    runner.act(action).expect("the printed cast completes");
    assert_eq!(runner.state().objects[&crow].zone, Zone::Stack);
    assert_eq!(runner.state().players[0].mana_pool.total(), 0);
}

/// Tenacious Underdog in the graveyard under its own blitz-only permission and
/// Underworld Breach, with `fodder` other cards in the graveyard, `black` mana
/// and `life`.
fn underdog_under_breach(fodder: usize, black: usize, life: i32) -> (GameRunner, ObjectId) {
    let parsed = parse_oracle_text(
        UNDERDOG,
        "Tenacious Underdog",
        &[],
        &["Creature".into()],
        &["Human".into(), "Warrior".into()],
    );
    let own_rider = parsed.statics.first().expect("the rider parses").clone();
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_enchantment_from_oracle(P0, "Underworld Breach", UNDERWORLD_BREACH);
    let dog = scenario
        .add_creature_to_graveyard(P0, "Tenacious Underdog", 3, 2)
        .with_static_definition(own_rider)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Black],
        })
        .with_keyword(blitz_keyword(&parsed))
        .id();
    for i in 0..fodder {
        scenario.add_creature_to_graveyard(P0, &format!("Fodder {i}"), 1, 1);
    }
    let mut runner = scenario.build();
    runner.state_mut().players[0].life = life;
    add_mana(&mut runner, ManaType::Black, black);
    (runner, dog)
}

/// CR 601.2b + CR 118.9b: under Underworld Breach, Underdog's options are its
/// escape cast and its blitz cast. Its blitz-only permission contributes the
/// Blitz option, never a printed-cost `GraveyardPermission` one, and choosing
/// Blitz pays {2}{B}{B} and 2 life.
#[test]
fn breach_offers_escape_and_blitz_but_no_printed_permission_option() {
    let (mut runner, dog) = underdog_under_breach(3, 4, 20);
    let waiting = cast_from_graveyard(&mut runner, dog).expect("the cast starts");
    let WaitingFor::CastingVariantChoice { options, .. } = waiting else {
        panic!("expected the casting menu, got {waiting:?}");
    };
    let variants: Vec<String> = options.iter().map(|o| format!("{:?}", o.variant)).collect();
    assert_eq!(
        variants,
        vec!["Escape".to_string(), "Blitz".to_string()],
        "{options:?}"
    );
    assert!(
        options[1].additional_cost.is_some(),
        "the Blitz option carries its 2-life residual for display: {options:?}"
    );
    runner
        .act(GameAction::ChooseCastingVariant { index: 1 })
        .expect("choosing blitz is legal");
    pay_offered_costs(&mut runner);
    assert_eq!(runner.state().objects[&dog].zone, Zone::Stack);
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "{{2}}{{B}}{{B}} paid"
    );
    assert_eq!(runner.state().players[0].life, 18, "blitz's 2 life paid");
}

/// With escape unpayable (no other cards to exile) the menu has one option, so
/// it is taken directly: the blitz, never the printed cost.
#[test]
fn breach_with_unpayable_escape_auto_routes_to_blitz() {
    let (mut runner, dog) = underdog_under_breach(0, 4, 20);
    cast_from_graveyard(&mut runner, dog).expect("the cast starts");
    pay_offered_costs(&mut runner);
    assert_eq!(runner.state().objects[&dog].zone, Zone::Stack);
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "{{2}}{{B}}{{B}} paid"
    );
    assert_eq!(runner.state().players[0].life, 18, "blitz's 2 life paid");
}

/// CR 601.2a + CR 118.9b: a stale or hand-built menu option naming Underdog's
/// blitz-only permission as a printed-cost `GraveyardPermission` is refused
/// when chosen, and nothing moves or is paid. This pins the menu's
/// revalidation against a fresh option set. The preparation backstop behind it
/// (`prepare_spell_cast` refusing a `GraveyardPermission` cast through a
/// method-restricted source) is reached in production by castability's
/// default preparation and is pinned by the printed-cost negatives
/// (`graveyard_blitz_only_permission_does_not_admit_an_affordable_printed_cost`
/// and its Bestow sibling), which fail without it.
#[test]
fn a_printed_option_through_a_blitz_only_permission_is_refused() {
    let (mut runner, dog) = underdog_under_breach(3, 4, 20);
    let waiting = cast_from_graveyard(&mut runner, dog).expect("the cast starts");
    let WaitingFor::CastingVariantChoice {
        player,
        object_id,
        card_id,
        payment_mode,
        mut options,
    } = waiting
    else {
        panic!("expected the casting menu, got {waiting:?}");
    };
    options.push(engine::types::game_state::CastingVariantChoiceOption {
        variant: engine::types::game_state::CastingVariant::GraveyardPermission {
            source: dog,
            frequency: CastFrequency::Unlimited,
            slot_type: None,
            graveyard_destination_replacement: None,
        },
        face: engine::types::game_state::CastingVariantFace::Current,
        mana_cost: ManaCost::Cost {
            generic: 1,
            shards: vec![ManaCostShard::Black],
        },
        additional_cost: None,
    });
    let stale = options.len() - 1;
    runner.state_mut().waiting_for = WaitingFor::CastingVariantChoice {
        player,
        object_id,
        card_id,
        payment_mode,
        options,
    };
    assert!(
        runner
            .act(GameAction::ChooseCastingVariant { index: stale })
            .is_err(),
        "a printed-cost option through a blitz-only permission must be refused"
    );
    assert_eq!(runner.state().objects[&dog].zone, Zone::Graveyard);
    assert_eq!(runner.state().players[0].mana_pool.total(), 4);
}

/// CR 118.9b: Timeline Culler ("using its warp ability") and Ninja Teen
/// ("using their sneak abilities") require methods the engine can't cast from
/// the graveyard, so their permissions are honest gaps: not modeled as a
/// graveyard permission, and left unimplemented.
#[test]
fn warp_and_sneak_graveyard_permissions_are_honest_gaps() {
    let culler = parse_oracle_text(
        "Haste\nYou may cast this card from your graveyard using its warp ability.\nWarp\u{2014}{B}, Pay 2 life. (You may cast this card from your hand or graveyard for its warp cost. If you do, exile this creature at the beginning of the next end step, then you may cast it from exile on a later turn.)",
        "Timeline Culler",
        &["Haste".into(), "Warp".into()],
        &["Creature".into()],
        &["Drix".into(), "Warlock".into()],
    );
    let teen = parse_oracle_text(
        "(Gain the next level as a sorcery to add its ability.)\nWhenever a creature you control leaves the battlefield, each opponent loses 1 life.\n{1}{B}: Level 2\nCreatures you control get +1/+0 and have menace.\n{B}: Level 3\nCreature cards in your graveyard have sneak {3}{B}.\nYou may cast creature spells from your graveyard using their sneak abilities.",
        "Ninja Teen",
        &[],
        &["Enchantment".into()],
        &["Class".into()],
    );
    for (name, parsed) in [("Timeline Culler", &culler), ("Ninja Teen", &teen)] {
        let rendered = format!("{parsed:?}");
        assert!(
            !rendered.contains("GraveyardCastPermission"),
            "{name}: the method-restricted permission must not be modeled"
        );
        assert!(
            rendered.contains("Unimplemented"),
            "{name}: the declined clause must stay an explicit gap"
        );
    }
}

/// CR 118.9b: the method survives persistence. Underdog's blitz-only permission
/// round-trips through the persisted-state chokepoint with
/// `required_cast_keyword == Some(Blitz)`, and the restored game still refuses
/// its affordable printed cost.
#[test]
fn a_required_cast_method_survives_persistence() {
    let (runner, dog) = underdog_in_graveyard_with(2, 1);
    let saved = serde_json::to_string(&engine::types::game_state::PersistedGameState::capture(
        runner.state().clone(),
    ))
    .expect("the state serializes");
    let restored: engine::types::game_state::PersistedGameState =
        serde_json::from_str(&saved).expect("the state deserializes");
    let mut runner = GameRunner::from_state(
        restored
            .into_game_state()
            .expect("the persisted state satisfies the restore contract"),
    );
    let required = runner.state().objects[&dog]
        .static_definitions
        .as_slice()
        .iter()
        .find_map(|def| match def.mode {
            StaticMode::GraveyardCastPermission {
                required_cast_keyword,
                ..
            } => Some(required_cast_keyword),
            _ => None,
        });
    assert_eq!(required, Some(Some(KeywordKind::Blitz)));
    assert!(offered_cast(&runner, dog).is_none());
    assert!(cast_from_graveyard(&mut runner, dog).is_err());
}
const TERROR_OF_THE_PEAKS: &str = "Flying\nSpells your opponents cast that target this creature cost an additional 3 life to cast.\nWhenever another creature you control enters, this creature deals damage equal to that creature's power to any target.";

/// Detective's Phoenix in the graveyard with evidence and no red mana; Defiler
/// of Instinct (given shroud, so it is not an Aura target itself); the
/// opponent's Terror of the Peaks as the only legal creature to enchant; P0 at
/// `life`.
fn phoenix_with_only_terror_to_enchant(life: i32) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let defiler_statics = parse_oracle_text(
        DEFILER_OF_INSTINCT,
        "Defiler of Instinct",
        &["First strike".into()],
        &["Creature".into()],
        &["Phyrexian".into(), "Kavu".into()],
    )
    .statics;
    let mut defiler = scenario.add_creature(P0, "Defiler of Instinct", 3, 3);
    for s in defiler_statics {
        defiler.with_static_definition(s);
    }
    defiler.with_keyword(Keyword::Shroud);
    let terror = scenario
        .add_creature(engine::game::scenario::P1, "Terror of the Peaks", 5, 4)
        .from_oracle_text_with_keywords(&["Flying"], TERROR_OF_THE_PEAKS)
        .id();
    let mut builder = scenario.add_creature_to_graveyard(P0, "Detective's Phoenix", 2, 2);
    builder.with_mana_cost(ManaCost::Cost {
        shards: vec![ManaCostShard::Red],
        generic: 2,
    });
    builder.with_subtypes(vec!["Phoenix"]);
    builder.with_color(vec![ManaColor::Red]);
    builder.from_oracle_text_with_keywords(&["Flying", "Haste"], PHOENIX_ORACLE);
    let phoenix = builder.id();
    let mut runner = scenario.build();
    {
        let obj = runner.state_mut().objects.get_mut(&phoenix).unwrap();
        for types in [
            &mut obj.card_types.core_types,
            &mut obj.base_card_types.core_types,
        ] {
            if !types.contains(&CoreType::Enchantment) {
                types.push(CoreType::Enchantment);
            }
        }
    }
    for i in 0..2 {
        let card_id = engine::types::identifiers::CardId(runner.state().next_object_id);
        let id = engine::game::zones::create_object(
            runner.state_mut(),
            card_id,
            P0,
            format!("Evidence {i}"),
            Zone::Graveyard,
        );
        runner.state_mut().objects.get_mut(&id).unwrap().mana_cost = ManaCost::generic(3);
    }
    engine::game::layers::flush_layers(runner.state_mut());
    runner.state_mut().players[0].life = life;
    (runner, phoenix, terror)
}

/// Reach guard for the deferred case below: at 5 life the board reaches the
/// offer, because the Defiler's 2 life and Terror's 3-life tax (5) are payable.
#[test]
fn bestow_onto_terror_with_a_defiler_is_offered_when_its_total_life_is_payable() {
    let (runner, phoenix, _terror) = phoenix_with_only_terror_to_enchant(5);
    assert!(offered_cast(&runner, phoenix).is_some());
}

/// CR 601.2h + CR 119.4: the REQUIRED behavior, not today's. At 4 life the only
/// Aura target is Terror of the Peaks, whose tax ("cost an additional 3 life")
/// applies to a spell targeting it; with no red mana the bestow {R} needs the
/// Defiler's 2 life too, and 5 life can't be paid from 4, so the cast must not
/// be offered.
///
/// Currently fails: the offer reads imposed taxes with no targets chosen, so a
/// target-dependent tax is not in its life total (a stated limitation of this
/// change; follow-up logged).
#[test]
#[ignore = "target-dependent imposed tax (Terror of the Peaks) is not in the alternative-cost offer's life total; follow-up"]
fn bestow_onto_terror_with_a_defiler_is_not_offered_when_its_total_life_is_unpayable() {
    let (runner, phoenix, _terror) = phoenix_with_only_terror_to_enchant(4);
    assert!(
        offered_cast(&runner, phoenix).is_none(),
        "4 life can't pay the Defiler's 2 and Terror's 3"
    );
}

/// Sabin with Defiler of Instinct, `red` red mana, a card to discard, P0 at
/// `life`, and a required "pay 2 life" additional cost on Sabin itself. No
/// printed card combines a graveyard blitz or bestow with a required
/// additional cost, so the cost is set directly.
fn sabin_with_defiler_and_a_required_life_cost(
    red: usize,
    life: i32,
) -> (GameRunner, ObjectId, ObjectId) {
    let (mut runner, sabin, filler) = sabin_with_defiler(red);
    runner.state_mut().players[0].life = life;
    runner
        .state_mut()
        .objects
        .get_mut(&sabin)
        .unwrap()
        .additional_cost = Some(engine::types::ability::AdditionalCost::Required(
        engine::types::ability::AbilityCost::PayLife {
            amount: engine::types::ability::QuantityExpr::Fixed { value: 2 },
        },
    ));
    (runner, sabin, filler)
}

fn assert_not_offered_and_refused(runner: &mut GameRunner, sabin: ObjectId, why: &str) {
    assert!(
        engine::game::casting::current_casting_variant_choice_options(runner.state(), P0, sabin)
            .is_empty(),
        "{why}: the engine's casting options must not include the blitz"
    );
    assert!(offered_cast(runner, sabin).is_none(), "{why}");
    assert!(
        cast_from_graveyard(runner, sabin).is_err(),
        "{why}: the handler refuses it"
    );
    assert_eq!(runner.state().objects[&sabin].zone, Zone::Graveyard);
}

/// CR 601.2h + CR 119.4: with only the Defiler-reduced blitz mana (three red),
/// Sabin's own required 2 life and the Defiler's 2 can't both be paid at 3
/// life, so the blitz is not offered, and the handler refuses it. Before, the
/// engine's casting options still listed it (measured).
#[test]
fn blitz_with_a_defiler_and_a_required_life_cost_is_not_offered_when_unpayable() {
    let (mut runner, sabin, _) = sabin_with_defiler_and_a_required_life_cost(3, 3);
    assert_not_offered_and_refused(&mut runner, sabin, "3 life, 3 mana");
}

/// The limitation, pinned so offer and payment keep agreeing: at 4 life the
/// rules allow the reduced blitz (2 + 2 life), but the payment pipeline pays an
/// object's own required additional cost before the Defiler prompt and can't
/// complete it (measured: "Cannot pay mana cost"). The offer doesn't credit
/// the Defiler in that case, so the cast isn't offered rather than offered and
/// refused. No printed card has this combination.
#[test]
fn blitz_with_a_defiler_and_a_required_life_cost_is_not_offered_through_the_defiler() {
    let (mut runner, sabin, _) = sabin_with_defiler_and_a_required_life_cost(3, 4);
    assert_not_offered_and_refused(&mut runner, sabin, "4 life, 3 mana");
}

/// Control: the required cost is priced, not refused. Caldaia Guardian (blitz
/// {2}{G}, no non-mana residual) under Muldrotha with a required "pay 2 life"
/// additional cost, the full blitz mana and 3 life: the blitz is offered and
/// completes, paying three mana and the 2 life. (A residual-free blitz keeps
/// this control off a separate, pre-existing gap: an object's own required
/// additional cost currently suppresses an alternative cost's non-mana
/// residual, which no printed card combines.)
#[test]
fn blitz_with_a_required_life_cost_completes_without_the_defiler() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_permission_source(
        &mut scenario,
        "Muldrotha, the Gravetide",
        MULDROTHA,
        &["Elemental", "Avatar"],
    );
    let guardian = scenario
        .add_creature_to_graveyard(P0, "Caldaia Guardian", 4, 3)
        .with_mana_cost(ManaCost::Cost {
            generic: 3,
            shards: vec![ManaCostShard::Green],
        })
        .with_keyword(caldaia_blitz())
        .id();
    let mut runner = scenario.build();
    runner.state_mut().players[0].life = 3;
    runner
        .state_mut()
        .objects
        .get_mut(&guardian)
        .unwrap()
        .additional_cost = Some(engine::types::ability::AdditionalCost::Required(
        engine::types::ability::AbilityCost::PayLife {
            amount: engine::types::ability::QuantityExpr::Fixed { value: 2 },
        },
    ));
    add_mana(&mut runner, ManaType::Green, 3);

    let action = offered_cast(&runner, guardian).expect("three mana and 3 life pay the blitz");
    runner.act(action).expect("the offered cast is accepted");
    if let WaitingFor::AlternativeCastChoice { .. } = runner.state().waiting_for {
        runner
            .act(GameAction::ChooseAlternativeCast {
                choice: AlternativeCastDecision::Alternative,
            })
            .expect("choosing blitz");
    }
    assert_eq!(runner.state().objects[&guardian].zone, Zone::Stack);
    assert_eq!(runner.state().players[0].life, 1, "the required 2 life");
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "blitz {{2}}{{G}}"
    );
}

/// Underdog under Underworld Breach, as in `underdog_under_breach`, with its
/// blitz "Pay N life" amount replaced by `amount`.
fn underdog_under_breach_paying(
    amount: engine::types::ability::QuantityExpr,
) -> (GameRunner, ObjectId) {
    use engine::types::ability::AbilityCost;
    use engine::types::keywords::BlitzCost;
    let (mut runner, dog) = underdog_under_breach(3, 4, 20);
    let obj = runner.state_mut().objects.get_mut(&dog).unwrap();
    for keyword in obj.keywords.iter_mut() {
        if let Keyword::Blitz(BlitzCost::NonMana(AbilityCost::Composite { costs })) = keyword {
            for cost in costs.iter_mut() {
                if let AbilityCost::PayLife { amount: life } = cost {
                    *life = amount.clone();
                }
            }
        }
    }
    obj.base_keywords = obj.keywords.clone();
    engine::game::layers::flush_layers(runner.state_mut());
    (runner, dog)
}

/// The Blitz option's displayed non-mana cost, from the production menu builder.
fn blitz_option_life(runner: &GameRunner, dog: ObjectId) -> engine::types::ability::QuantityExpr {
    let options =
        engine::game::casting::current_casting_variant_choice_options(runner.state(), P0, dog);
    let blitz = options
        .iter()
        .find(|option| format!("{:?}", option.variant) == "Blitz")
        .unwrap_or_else(|| panic!("reach guard: the Blitz option is on the menu, got {options:?}"));
    match &blitz.additional_cost {
        Some(engine::types::ability::AbilityCost::PayLife { amount }) => amount.clone(),
        other => panic!("the Blitz option shows its life payment, got {other:?}"),
    }
}

/// CR 601.2f-h: a life amount the engine can already know is shown resolved.
/// "Pay life equal to your starting life total" (20) shows 20.
#[test]
fn menu_resolves_a_previewable_life_amount() {
    use engine::types::ability::{QuantityExpr, QuantityRef};
    let (runner, dog) = underdog_under_breach_paying(QuantityExpr::Ref {
        qty: QuantityRef::StartingLifeTotal,
    });
    assert_eq!(
        blitz_option_life(&runner, dog),
        QuantityExpr::Fixed { value: 20 }
    );
}

/// CR 601.2f-h: an amount that depends on a choice not made yet (an unannounced
/// X) keeps its expression, so the menu shows the unquantified cost rather than
/// "+ Pay 0 life".
#[test]
fn menu_keeps_an_unresolved_life_amount_as_an_expression() {
    use engine::types::ability::{QuantityExpr, QuantityRef};
    let x = QuantityExpr::Ref {
        qty: QuantityRef::Variable {
            name: "X".to_string(),
        },
    };
    let (runner, dog) = underdog_under_breach_paying(x.clone());
    assert_eq!(blitz_option_life(&runner, dog), x);
}
