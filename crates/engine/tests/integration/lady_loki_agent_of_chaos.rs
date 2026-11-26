//! Lady Loki, Agent of Chaos ({5}{R}, Legendary Creature — God Sorcerer Villain,
//! 5/5). Verbatim Oracle text (Scryfall / data/card-data.json):
//!
//!   "Whenever you cast your first instant, sorcery, or Villain spell each turn,
//!    exile it, then exile cards from the top of your library until you exile a
//!    nonland card. Lady Loki deals damage to each opponent equal to the
//!    difference between that spell's mana value and that nonland card's mana
//!    value. You may cast that card without paying its mana cost."
//!
//! The parser already assembled the four-clause chain correctly
//!   ChangeZone{Exile, TriggeringSource}  (exile it)
//!     → ExileFromTopUntil{NextMatches{nonland}}  (dig)
//!       → DamageEachPlayer{Difference{..}, Opponent}  (the payoff)
//!         → CastFromZone{ParentTarget, without_paying, optional}  (free cast)
//! but the damage-clause amount fell to `Effect::Unimplemented` because
//! "that nonland card's mana value" had no object-scope arm, and even once it
//! parses `DamageEachPlayer`'s resolver dropped the injected exile-until hit
//! (`ObjectManaValue { scope: Target }` → 0). This module drives the real
//! cast/apply pipeline and asserts the measured per-opponent life deltas — each
//! is revert-failing against those two fixes.

use engine::game::scenario::{GameScenario, P0};
use engine::parser::parse_oracle_text;
use engine::types::ability::{
    Effect, ObjectScope, PlayerFilter, QuantityExpr, QuantityRef, TargetFilter, TriggerConstraint,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::ObjectId;

const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

const LADY_LOKI: &str = "Whenever you cast your first instant, sorcery, or Villain spell each turn, exile it, then exile cards from the top of your library until you exile a nonland card. Lady Loki deals damage to each opponent equal to the difference between that spell's mana value and that nonland card's mana value. You may cast that card without paying its mana cost.";

fn red_pool(n: usize) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]))
        .collect()
}

// ---------------------------------------------------------------------------
// Runtime: core payoff (3-player, non-X)
// ---------------------------------------------------------------------------

/// PRIMARY discriminating test. Spell MV = 4, exile-until hit MV = 1, so each
/// opponent takes |4 − 1| = 3. The difference is non-degenerate (≠ spell MV = 4,
/// ≠ 0), so every failure mode yields a DISTINCT wrong number:
///   * targets dropped (`ObjectManaValue{Target}` → 0): |4 − 0| = 4
///   * EventSource fails to read the exiled spell: |0 − 1| = 1
///   * both broken: 0 (no damage)
///
/// Only the correct wiring produces exactly −3 to each opponent and 0 to P0.
#[test]
fn lady_loki_deals_mv_difference_to_each_opponent() {
    let mut scenario = GameScenario::new_n_player(3, 7);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, red_pool(4));

    scenario.add_creature_from_oracle(P0, "Lady Loki, Agent of Chaos", 5, 5, LADY_LOKI);

    // Triggering instant, mana value 4 ({3}{R}).
    let spell = {
        let mut b =
            scenario.add_spell_to_hand_from_oracle(P0, "Chaos Instant", true, "Draw a card.");
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 3,
        });
        b.id()
    };

    // Library top-first: [Dig Land, Nonland Hit(MV 1)] — the land is dug through,
    // the nonland with mana value 1 is the hit.
    let hit = {
        let mut b = scenario.add_spell_to_library_top(P0, "Nonland Hit", false);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 1,
        });
        b.id()
    };
    let land = scenario.add_card_to_library_top(P0, "Dig Land");

    let mut runner = scenario.build();
    // The generic library card carries no core types; make it a real land so the
    // "until you exile a nonland card" dig exiles it BEFORE reaching the hit.
    {
        let obj = runner.state_mut().objects.get_mut(&land).unwrap();
        obj.card_types.core_types = vec![CoreType::Land];
        obj.base_card_types = obj.card_types.clone();
    }

    let outcome = runner.cast(spell).resolve();

    // CR 120.3 + CR 202.3e: each opponent takes |MV(spell) − MV(hit)| = |4 − 1| = 3.
    assert_eq!(
        outcome.life_delta(P1),
        -3,
        "P1 must take |4 − 1| = 3 (reverting the Target-threading fix gives −4; a \
         broken EventSource read gives −1)"
    );
    assert_eq!(
        outcome.life_delta(P2),
        -3,
        "P2 must take |4 − 1| = 3 — same per-opponent amount"
    );
    assert_eq!(
        outcome.life_delta(P0),
        0,
        "Lady Loki damages each OPPONENT only; the caster is unaffected"
    );

    // The dig exiled both the land and the nonland hit (CR 400.7 via ExileFromTopUntil).
    // The free cast was not accepted here, so the hit remains in Exile.
    assert_eq!(
        outcome.zone_of(land),
        engine::types::zones::Zone::Exile,
        "the dug-through land is exiled"
    );
    assert_eq!(
        outcome.zone_of(hit),
        engine::types::zones::Zone::Exile,
        "the nonland hit is exiled by the dig"
    );
}

// ---------------------------------------------------------------------------
// Runtime: X spell — the triggering spell keeps its last stack mana value
// ---------------------------------------------------------------------------

/// CR 202.3e + CR 608.2h: Torment of Hailfire with X=2 has a last-known stack
/// mana value of 4. Living Laser's mana value in exile is 5. Each opponent
/// takes |4 − 5| = 1. The exiled Torment card itself has mana value 2.
#[test]
fn lady_loki_x_spell_reads_its_last_stack_mana_value() {
    let mut scenario = GameScenario::new_n_player(3, 11);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        (0..4)
            .map(|_| ManaUnit::new(ManaType::Black, ObjectId(0), false, vec![]))
            .collect(),
    );
    scenario.add_creature_from_oracle(P0, "Lady Loki, Agent of Chaos", 5, 5, LADY_LOKI);

    let spell = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Torment of Hailfire",
            false,
            "Repeat the following process X times. Each opponent loses 3 life unless that player sacrifices a nonland permanent of their choice or discards a card.",
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::X, ManaCostShard::Black, ManaCostShard::Black],
            generic: 0,
        })
        .id();
    let hit = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Living Laser",
            4,
            4,
            "Haste\nWhenever Living Laser attacks, for each card you've discarded this turn, create a token that's a copy of Living Laser, except the token isn't legendary. The tokens enter tapped and attacking. Exile the tokens at the beginning of the next end step.",
        )
        .as_legendary()
        .with_subtypes(vec!["Elemental", "Villain"])
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 4,
        })
        .id();

    let mut runner = scenario.build();
    engine::game::zones::move_to_library_position(runner.state_mut(), hit, true, &mut Vec::new());
    let outcome = runner
        .cast(spell)
        .x(2)
        .commit()
        .stop_at_optional()
        .resolve();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice {
            player: P0,
            decision_subject_id: Some(subject),
            ..
        } if subject == hit
    ));
    assert_eq!(outcome.zone_of(spell), Zone::Exile);
    assert_eq!(outcome.zone_of(hit), Zone::Exile);

    runner
        .act(GameAction::DecideOptionalEffect { accept: false })
        .expect("decline casting the found card");
    assert_eq!(runner.life(P1), 19, "use Torment's last stack value of 4");
    assert_eq!(runner.life(P2), 19, "damage reaches each opponent");
    assert_eq!(runner.life(P0), 20, "Lady Loki's controller is unaffected");
    assert_eq!(runner.state().objects[&hit].zone, Zone::Exile);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

// ---------------------------------------------------------------------------
// Runtime: optional free cast addresses the exile-until hit
// ---------------------------------------------------------------------------

/// After the damage clause, "you may cast that card without paying its mana cost"
/// must address the exile-until hit (via propagated `ParentTarget`). Accepting
/// the offer casts the hit — it leaves Exile — proving the anaphor bound to the
/// hit and not to nothing. (If `ParentTarget` failed to resolve, the offer would
/// have nothing to cast and the hit would stay in Exile.)
#[test]
fn lady_loki_free_cast_addresses_the_hit() {
    let mut scenario = GameScenario::new_n_player(2, 5);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, red_pool(4));

    scenario.add_creature_from_oracle(P0, "Lady Loki, Agent of Chaos", 5, 5, LADY_LOKI);

    let spell = {
        let mut b =
            scenario.add_spell_to_hand_from_oracle(P0, "Chaos Instant", true, "Draw a card.");
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 3,
        });
        b.id()
    };

    // Nonland hit on top so the dig immediately exiles it.
    let hit = {
        let mut b = scenario.add_spell_to_library_top(P0, "Free Cast Hit", false);
        b.with_mana_cost(ManaCost::Cost {
            shards: vec![],
            generic: 1,
        });
        b.id()
    };

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).accept_optional().resolve();

    // Positive reach-guard: the opponent took the payoff damage, proving the
    // chain resolved through the damage clause into the free-cast tail.
    assert_eq!(
        outcome.life_delta(P1),
        -3,
        "the payoff damage must have resolved before the free-cast tail"
    );
    // The accepted free cast addressed the hit, so it left Exile.
    assert_ne!(
        outcome.zone_of(hit),
        engine::types::zones::Zone::Exile,
        "accepting 'you may cast that card' must cast the hit (ParentTarget bound \
         to the exile-until hit), moving it out of Exile"
    );
}

// ---------------------------------------------------------------------------
// Parser SHAPE: trigger constraint + strictly nested payoff chain (Finding 4)
// ---------------------------------------------------------------------------

/// SHAPE test. Parses Lady Loki's verbatim body and asserts:
///   * the trigger is `NthSpellThisTurn { n: 1, Or[Instant, Sorcery, Villain] }`;
///   * `ExileFromTopUntil`'s DIRECT sub is
///     `DamageEachPlayer { Difference{ EventSource, Target }, Opponent }`
///     (not a sibling — that is what lets the injected hit reach the Target leaf);
///   * `DamageEachPlayer`'s DIRECT sub is `CastFromZone { ParentTarget }`.
///
/// The two mana-value operands must resolve to DISTINCT scopes (EventSource vs
/// Target) — the anti-collapse assertion that guards against a sibling misparse
/// silently resolving damage to the spell's own mana value.
#[test]
fn lady_loki_parses_nested_payoff_chain() {
    let parsed = parse_oracle_text(
        LADY_LOKI,
        "Lady Loki, Agent of Chaos",
        &[],
        &["Legendary".to_string(), "Creature".to_string()],
        &[
            "God".to_string(),
            "Sorcerer".to_string(),
            "Villain".to_string(),
        ],
    );

    assert_eq!(parsed.triggers.len(), 1, "one triggered ability");
    let trigger = &parsed.triggers[0];

    // Trigger constraint: first instant/sorcery/Villain spell each turn.
    match trigger.constraint.as_ref() {
        Some(TriggerConstraint::NthSpellThisTurn { n, filter, .. }) => {
            assert_eq!(*n, 1, "fires on the FIRST matching spell each turn");
            let Some(TargetFilter::Or { filters }) = filter else {
                panic!("expected an Or filter, got {filter:?}");
            };
            assert_eq!(filters.len(), 3, "instant / sorcery / Villain: {filters:?}");
        }
        other => panic!("expected NthSpellThisTurn constraint, got {other:?}"),
    }

    // Chain: head is "exile it" (ChangeZone → Exile of the triggering source).
    let head = trigger
        .execute
        .as_ref()
        .expect("trigger has an execute chain");
    assert!(
        matches!(
            head.effect.as_ref(),
            Effect::ChangeZone {
                destination: engine::types::zones::Zone::Exile,
                target: TargetFilter::TriggeringSource,
                ..
            }
        ),
        "head must exile the triggering spell: {:?}",
        head.effect
    );

    // head.sub = ExileFromTopUntil.
    let dig = head.sub_ability.as_deref().expect("dig sub-ability");
    assert!(
        matches!(dig.effect.as_ref(), Effect::ExileFromTopUntil { .. }),
        "second clause must be the exile-until dig: {:?}",
        dig.effect
    );

    // ExileFromTopUntil's DIRECT sub = DamageEachPlayer{Difference{EventSource, Target}}.
    let damage = dig
        .sub_ability
        .as_deref()
        .expect("damage clause is ExileFromTopUntil's direct sub");
    let Effect::DamageEachPlayer {
        amount,
        player_filter,
    } = damage.effect.as_ref()
    else {
        panic!(
            "ExileFromTopUntil's direct sub must be DamageEachPlayer (not a sibling), got {:?}",
            damage.effect
        );
    };
    assert_eq!(
        *player_filter,
        PlayerFilter::Opponent,
        "damage to each opponent"
    );
    let QuantityExpr::Difference { left, right } = amount else {
        panic!("amount must be a Difference, got {amount:?}");
    };
    assert_eq!(
        left.as_ref(),
        &QuantityExpr::Ref {
            qty: QuantityRef::ObjectManaValue {
                scope: ObjectScope::EventSource,
            },
        },
        "left operand = that spell's mana value (EventSource)"
    );
    assert_eq!(
        right.as_ref(),
        &QuantityExpr::Ref {
            qty: QuantityRef::ObjectManaValue {
                scope: ObjectScope::Target,
            },
        },
        "right operand = that nonland card's mana value (Target = the injected hit)"
    );

    // DamageEachPlayer's DIRECT sub = CastFromZone{ParentTarget}, optional.
    let cast = damage
        .sub_ability
        .as_deref()
        .expect("free-cast tail is DamageEachPlayer's direct sub");
    assert!(
        matches!(
            cast.effect.as_ref(),
            Effect::CastFromZone {
                target: TargetFilter::ParentTarget,
                without_paying_mana_cost: true,
                ..
            }
        ),
        "free-cast tail must be CastFromZone{{ParentTarget, without_paying}}: {:?}",
        cast.effect
    );
    assert!(cast.optional, "the free cast is optional ('you may cast')");

    // No clause fell to Unimplemented.
    for (label, def) in [
        ("head", &**head),
        ("dig", dig),
        ("damage", damage),
        ("cast", cast),
    ] {
        assert!(
            !matches!(def.effect.as_ref(), Effect::Unimplemented { .. }),
            "{label} clause must not be Unimplemented: {:?}",
            def.effect
        );
    }
}
