//! Dragon Egg's token clause is an ACTIVATED ability, not a free static pump.
//!
//! Card text (verbatim, Scryfall/MTGJSON): `Defender` /
//! `When this creature dies, create a 2/2 red Dragon creature token with flying
//! and "{R}: This token gets +1/+0 until end of turn."`
//!
//! CR 602.1 + CR 602.1a: an activated ability is written `[Cost]: [Effect]` and
//! the whole activation cost (`{R}`) is written before the colon. CR 604.1: a
//! static ability is written as a statement, never in that shape. CR 611.2a:
//! "until end of turn" gives the granted pump a duration that ends during the
//! cleanup step.
//!
//! Before the fix, the static-line parser's "~ has/gets …" arm claimed the token's
//! derived line `{R}: ~ gets +1/+0 until end of turn.` on a subject heuristic and
//! flattened it to `Continuous { SelfRef, [AddPower 1, AddToughness 0] }` — the
//! token entered as a 3/2 with a FREE +1/+0 and no ability at all; the `{R}` cost
//! and the duration were dropped outright. After the fix the token enters 2/2 and
//! carries a granted ACTIVATED `{R}` pump.
//!
//! Foot-guns avoided (see the `card-test` recipe): the card is built from its
//! verbatim Oracle text with its real MTGJSON keyword list, the ability is driven
//! through `GameRunner::activate(..).resolve()` (announcement → payment → stack)
//! rather than a hand-built action sequence, and every assertion is a runtime
//! delta paired with a positive reach guard.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{AbilityCost, AbilityKind, Duration, Effect, PtValue, TargetFilter};
use engine::types::actions::{DebugAction, GameAction};
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const DRAGON_EGG: &str = "Defender\nWhen this creature dies, create a 2/2 red Dragon creature token with flying and \"{R}: This token gets +1/+0 until end of turn.\"";

fn red() -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![])]
}

/// The single Dragon token the dies trigger created.
fn dragon_token(runner: &GameRunner) -> ObjectId {
    let tokens: Vec<_> = runner
        .state()
        .objects
        .iter()
        .filter(|(_, object)| {
            object.zone == Zone::Battlefield && object.is_token && object.name == "Dragon"
        })
        .map(|(id, _)| *id)
        .collect();
    assert_eq!(tokens.len(), 1, "exactly one Dragon token: {tokens:?}");
    tokens[0]
}

/// Sacrifice a Dragon Egg and resolve its dies trigger, leaving the created token.
fn dragon_token_on_battlefield() -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(P0, red());
    // CR 104.3c: both players need a library. Without one, crossing into the next
    // turn's draw step ends the game instead of exercising cleanup, and the
    // end-of-turn assertion below could not be reached at all.
    scenario.with_library_top(P0, &["Mountain", "Mountain", "Mountain"]);
    scenario.with_library_top(P1, &["Island", "Island", "Island"]);
    // Dragon Egg's MTGJSON keyword list is exactly ["Defender"], so pass it —
    // without it the "Defender" line would parse as an unimplemented residual and
    // the fixture would take a different branch than production.
    let egg = scenario
        .add_creature(P0, "Dragon Egg", 0, 2)
        .from_oracle_text_with_keywords(&["Defender"], DRAGON_EGG)
        .id();
    let mut runner = scenario.build();
    // CR 701.21: route through the single sacrifice authority so the dies trigger
    // fires (a bare RemoveObject would delete the object with no trigger). Debug
    // actions are gated on the scenario flag, as in
    // `abundance_optional_draw_replacement.rs:66`.
    runner.state_mut().debug_mode = true;
    runner
        .act(GameAction::Debug(DebugAction::Sacrifice { object_id: egg }))
        .expect("the sacrifice is accepted");
    runner.advance_until_stack_empty();
    let token = dragon_token(&runner);
    (runner, token)
}

#[test]
fn dragon_egg_token_enters_as_a_2_2() {
    let (runner, token) = dragon_token_on_battlefield();
    let object = &runner.state().objects[&token];
    // Revert-failing: the flattened `AddPower { 1 }` static made this 3/2 on entry
    // — a free pump with no activation.
    assert_eq!(object.power, Some(2), "the Dragon token enters 2/2");
    assert_eq!(object.toughness, Some(2), "the Dragon token enters 2/2");
}

#[test]
fn dragon_egg_token_activation_pays_red_and_pumps_until_end_of_turn() {
    let (mut runner, token) = dragon_token_on_battlefield();

    // Reach guard: the token carries a REAL activated ability (CR 602.1). Revert-
    // failing — on the flattened AST there is no granted ability at all, so this
    // `.expect` panics before any behaviour assertion below can pass vacuously.
    let ability_index = runner.state().objects[&token]
        .abilities
        .iter()
        .position(|ability| ability.cost.is_some())
        .unwrap_or_else(|| {
            panic!(
                "the token must carry a granted activated ability: {:#?}",
                runner.state().objects[&token].abilities
            )
        });
    let ability = &runner.state().objects[&token].abilities[ability_index];
    assert_eq!(ability.kind, AbilityKind::Activated);
    // CR 602.1a: the whole activation cost is written before the colon.
    assert_eq!(
        ability.cost,
        Some(AbilityCost::Mana {
            cost: ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            },
        }),
        "the {{R}} activation cost must be carried, not dropped"
    );
    let Effect::Pump {
        power,
        toughness,
        target,
    } = &*ability.effect
    else {
        panic!("expected a granted Pump, got {:?}", ability.effect);
    };
    assert_eq!(
        (power, toughness),
        (&PtValue::Fixed(1), &PtValue::Fixed(0)),
        "expected +1/+0"
    );
    assert_eq!(target, &TargetFilter::SelfRef, "the token pumps itself");
    assert_eq!(
        ability.duration,
        Some(Duration::UntilEndOfTurn),
        "the duration must be carried, not dropped"
    );

    let mana_before = runner.state().players[0].mana_pool.total();
    assert_eq!(mana_before, 1, "one staged red mana funds the activation");

    // CR 602.2a + CR 602.2b: announce, pay, and commit to the stack. Drive the
    // prompt loop by hand (mirroring `activated_self_reference_new_object.rs`'s
    // `activate_onto_stack`, plus the mana-payment arm this {R} cost needs) so the
    // test can assert the ability is ON THE STACK and has not resolved yet —
    // `AbilityActivation::resolve()` would run it all the way to resolution and
    // lose that intermediate state.
    runner
        .act(GameAction::ActivateAbility {
            source_id: token,
            ability_index,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("the activation is accepted");
    for _ in 0..8 {
        match &runner.state().waiting_for {
            // CR 602.1b: finalize the mana payment from the staged pool.
            WaitingFor::ManaPayment { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("finalizing the mana payment is accepted");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt while activating: {other:?}"),
        }
    }
    assert!(
        !runner.state().stack.is_empty(),
        "the ability is on the stack, not resolved"
    );
    assert_eq!(
        runner.state().objects[&token].power,
        Some(2),
        "the pump is not a static — nothing happens before resolution"
    );

    runner.advance_until_stack_empty();
    // CR 602.1a + CR 611.2a: the cost was paid and the pump resolved.
    assert_eq!(
        runner.state().players[0].mana_pool.total(),
        0,
        "the {{R}} activation cost is spent"
    );
    assert_eq!(
        runner.state().players[0]
            .mana_pool
            .count_color(ManaType::Red),
        0,
        "no red mana is left over"
    );
    assert_eq!(runner.state().objects[&token].power, Some(3));
    assert_eq!(runner.state().objects[&token].toughness, Some(2));

    // CR 611.2a + CR 514.2: the pump lasts only until end of turn — after cleanup
    // it is gone. Reach guard: the turn actually advanced past cleanup.
    let turn = runner.state().turn_number;
    runner.advance_to_end_step();
    runner.advance_to_phase(Phase::PreCombatMain);
    assert!(
        runner.state().turn_number > turn,
        "the reach guard must cross a cleanup step: turn {} -> {}",
        turn,
        runner.state().turn_number
    );
    assert_eq!(
        runner.state().objects[&token].power,
        Some(2),
        "the pump ended with the turn"
    );
    assert_eq!(runner.state().objects[&token].toughness, Some(2));
    assert!(
        runner.state().objects[&token].zone == Zone::Battlefield,
        "the token itself survives (control: the assertion above is about the pump)"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "the pipeline settled back at a priority window: {:?}",
        runner.state().waiting_for
    );
}
