//! Discriminating integration coverage for **Sandswirl Wanderglyph**'s static:
//!   "Each opponent who attacked you or a planeswalker you control this turn
//!    can't cast spells."
//!
//! This is a continuous casting prohibition (`StaticMode::CantBeCast { who:
//! Opponents }`) gated by the per-affected-CASTER predicate
//! `ParsedCondition::YouAttackedSourceControllerThisTurn` (CR 101.2 + CR 508.6 +
//! CR 109.5), evaluated as `has_attacked(caster, source.controller)` against the
//! CR-508.5-collapsed `attacked_defenders_this_turn` ledger.
//!
//! The load-bearing distinction vs. the sibling `YouAttackedThisTurn` (Angelic
//! Arbiter, "attacked ANYONE"): an opponent who attacked a DIFFERENT opponent
//! this turn — but not you — is NOT prohibited. The 3-player test below is the
//! revert-probe: swapping the eval to `YouAttackedThisTurn` would read
//! `players_attacked_this_turn` (which contains BOTH opponents here) and wrongly
//! gate the opponent who only attacked the other opponent → the `p2_can_cast`
//! assertion fails. The per-caster eval IS the "each opponent who …" quantifier.

use std::collections::HashSet;

use engine::game::casting::can_cast_object_now;
use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const P2: PlayerId = PlayerId(2);

const SANDSWIRL: &str = "Flying\nWhenever an opponent casts a spell during their turn, they can't attack you or planeswalkers you control this turn.\nEach opponent who attacked you or a planeswalker you control this turn can't cast spells.";

fn zero_instant(scenario: &mut GameScenario, owner: PlayerId, name: &str) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(owner, name, true, "Draw a card.")
        .with_mana_cost(ManaCost::generic(0))
        .id()
}

/// CR 101.2 + CR 508.6 + CR 109.5: in a 3-player game with Sandswirl under P0,
/// directly model the turn's combat ledger — P1 attacked P0 (you), P2 attacked
/// only P1 (the OTHER opponent) — and prove the prohibition discriminates by
/// DEFENDER. P1 and P2 are symmetric (both opponents of P0, both non-active), so
/// the only difference in castability is the attacked-you predicate.
#[test]
fn only_opponents_who_attacked_you_are_prohibited() {
    let mut scenario = GameScenario::new_n_player(3, 17);
    scenario.at_phase(Phase::PreCombatMain);

    let sandswirl = scenario
        .add_creature_from_oracle(P0, "Sandswirl Wanderglyph", 5, 3, SANDSWIRL)
        .id();

    // {0} instants for every player — identical castability except the static.
    let p0_spell = zero_instant(&mut scenario, P0, "P0 Spell");
    let p1_spell = zero_instant(&mut scenario, P1, "P1 Spell");
    let p2_spell = zero_instant(&mut scenario, P2, "P2 Spell");

    let mut runner = scenario.build();

    // Model the turn's combat history (CR 508.5-collapsed defending players):
    //   P1 attacked P0 (you).        P2 attacked P1 (the other opponent), NOT you.
    // `players_attacked_this_turn` holds BOTH so the revert-probe is live: an
    // attacked-ANYONE eval would gate P2 too.
    {
        let st = runner.state_mut();
        st.attacked_defenders_this_turn
            .insert(P1, HashSet::from([P0]));
        st.attacked_defenders_this_turn
            .insert(P2, HashSet::from([P1]));
        st.players_attacked_this_turn.insert(P1);
        st.players_attacked_this_turn.insert(P2);
    }

    // Sanity: the static must actually be on the battlefield source.
    assert!(
        runner.state().objects.contains_key(&sandswirl),
        "Sandswirl must be on the battlefield"
    );

    // PRIMARY discrimination:
    assert!(
        !can_cast_object_now(runner.state(), P1, p1_spell),
        "P1 attacked you this turn → must be prohibited from casting"
    );
    assert!(
        can_cast_object_now(runner.state(), P2, p2_spell),
        "P2 attacked only the OTHER opponent (not you) → must NOT be prohibited \
         (revert-probe: an attacked-anyone eval wrongly gates P2 here)"
    );
    // Control: the source's own controller is never an "opponent" → never gated.
    assert!(
        can_cast_object_now(runner.state(), P0, p0_spell),
        "P0 (the controller / 'you') is never prohibited by its own static"
    );
}

/// End-to-end via the REAL combat pipeline: on P1's turn, P1 attacks P0, which
/// populates `attacked_defenders_this_turn` through `declare_attackers`. The
/// static then prohibits P1 from casting — proving the predicate reads genuine
/// combat history, not just a hand-seeded ledger.
#[test]
fn real_attack_on_you_prohibits_casting() {
    let mut scenario = GameScenario::new_n_player(3, 23);
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Sandswirl Wanderglyph", 5, 3, SANDSWIRL)
        .id();
    let attacker = scenario
        .add_creature_from_oracle(P1, "Raging Bear", 2, 2, "")
        .id();
    let p1_spell = zero_instant(&mut scenario, P1, "P1 Spell");

    let mut runner = scenario.build();

    // Make it P1's turn (escape hatch) so P1 can legally declare attackers vs P0.
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P0))])
        .expect("P1 declaring an attacker against P0 must succeed");
    // Drain any priority so combat-declaration state settles.
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() || runner.act(GameAction::PassPriority).is_err()
                {
                    break;
                }
            }
            _ => break,
        }
    }

    assert!(
        runner.state().has_attacked(P1, P0),
        "declare_attackers must record P1 as having attacked P0 this turn"
    );
    assert!(
        !can_cast_object_now(runner.state(), P1, p1_spell),
        "P1 really attacked you this turn → Sandswirl prohibits P1 from casting"
    );
}

/// Parse-level gap=0 proof: the whole card lowers with NO `Unimplemented` clause —
/// Gap A (the SpellCast trigger's child) becomes a player can't-attack restriction
/// and Gap B becomes a `CantBeCast` static gated on the attacked-you predicate.
#[test]
fn full_card_parses_both_clauses_no_unimplemented() {
    use engine::parser::oracle::parse_oracle_text;
    use engine::types::ability::{
        Effect, GameRestriction, ParsedCondition, ProhibitedActivity, RestrictionExpiry,
    };
    use engine::types::statics::{ProhibitionScope, StaticMode};
    use engine::types::triggers::{AttackTargetFilter, TriggerMode};

    let parsed = parse_oracle_text(
        SANDSWIRL,
        "Sandswirl Wanderglyph",
        &["Flying".to_string()],
        &["Artifact".to_string(), "Creature".to_string()],
        &["Golem".to_string()],
    );

    // Gap A — SpellCast trigger child is a real player can't-attack restriction
    // (defended = you or your planeswalkers), NOT Unimplemented.
    let spell_trigger = parsed
        .triggers
        .iter()
        .find(|t| matches!(t.mode, TriggerMode::SpellCast))
        .expect("SpellCast trigger must parse");
    let child = spell_trigger
        .execute
        .as_ref()
        .expect("SpellCast trigger must carry an effect");
    assert!(
        matches!(
            child.effect.as_ref(),
            Effect::AddRestriction {
                restriction: GameRestriction::ProhibitActivity {
                    activity: ProhibitedActivity::Attack {
                        defended: AttackTargetFilter::PlayerOrPlaneswalker,
                    },
                    // CR 514.2: "… this turn" expires at the CURRENT turn's cleanup
                    // (`RestrictionExpiry::EndOfTurn`), NOT a permanent restriction and
                    // NOT the next-turn `UntilEndOfNextTurnOf` (Willie Lumpkin) path —
                    // `RestrictionExpiry` has no never-expires variant, so EndOfTurn is
                    // provably current-turn cleanup. Probe #3 separately excludes the
                    // next-turn duration; this pins the expiry to the current turn.
                    expiry: RestrictionExpiry::EndOfTurn,
                    ..
                }
            }
        ),
        "trigger child must be a you-or-planeswalkers can't-attack restriction expiring \
         at the CURRENT turn's end (EndOfTurn), got {:?}",
        child.effect
    );

    // Gap B — CantBeCast static gated on the source-controller attacked-you predicate.
    let cant_cast = parsed
        .statics
        .iter()
        .find(|s| {
            matches!(
                s.mode,
                StaticMode::CantBeCast {
                    who: ProhibitionScope::Opponents
                }
            )
        })
        .expect("CantBeCast static must parse");
    assert_eq!(
        cant_cast.per_player_condition,
        Some(ParsedCondition::YouAttackedSourceControllerThisTurn),
    );

    // Gap=0 proxy: no clause anywhere lowered to Unimplemented.
    let blob = format!("{parsed:?}");
    assert!(
        !blob.contains("Unimplemented"),
        "no clause may remain Unimplemented (gap=0)"
    );
}
