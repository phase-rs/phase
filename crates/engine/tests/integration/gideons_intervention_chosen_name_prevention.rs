//! Gideon's Intervention — runtime witness for the whole printed card:
//! "As this enchantment enters, choose a card name.
//! Your opponents can't cast spells with the chosen name.
//! Prevent all damage that would be dealt to you and permanents you control by
//! sources with the chosen name."
//!
//! CR 614.1c: "As [this permanent] enters ... choose" is a replacement effect.
//! CR 201.2 + CR 201.4: a chosen card name is public and is checked by name.
//! CR 101.2: a "can't" restriction takes precedence over permission.
//! CR 604.1: the static abilities are simply true while the enchantment is on
//!   the battlefield.
//! CR 609.7c + CR 615.1a + CR 615.2: the prevention effect is scoped to damage
//!   from sources with the chosen name, rechecked when the damage would be dealt.
//! CR 120.3a: damage dealt to a player causes that player to lose that much life.
//!
//! Verbatim printed Oracle text (MTGJSON AtomicCards), as above.

use engine::database::card_db::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::engine::EngineError;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::ChoiceType;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// The printed creature named by every test in this file.
const NAMED_CARD: &str = "Grizzly Bears";

fn db() -> &'static CardDatabase {
    load_db().expect("the committed integration card fixture is present")
}

fn mana(kind: ManaType, count: usize) -> Vec<ManaUnit> {
    (0..count)
        .map(|_| ManaUnit::new(kind, ObjectId(0), false, Vec::new()))
        .collect()
}

/// Pass priority (or drain a trigger-ordering prompt) until `stop` holds.
fn drive_to(runner: &mut GameRunner, stop: impl Fn(&GameRunner) -> bool) {
    for _ in 0..64 {
        if stop(runner) {
            return;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("order triggers");
            }
            other => panic!("unexpected prompt while advancing: {other:?}"),
        }
    }
    panic!("the game never reached the expected step");
}

fn advance_to_declare_attackers(runner: &mut GameRunner) {
    drive_to(runner, |r| {
        matches!(r.state().waiting_for, WaitingFor::DeclareAttackers { .. })
    });
}

fn advance_to_declare_blockers(runner: &mut GameRunner) {
    drive_to(runner, |r| {
        matches!(r.state().waiting_for, WaitingFor::DeclareBlockers { .. })
    });
}

/// Make P1 the active player at its precombat main, so its creatures can be
/// declared as attackers (CR 508.1).
fn begin_p1_attack_step(runner: &mut GameRunner) {
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
}

/// CR 614.1c + CR 201.2: cast Gideon's Intervention from P0's hand, assert the
/// as-enters card-name prompt (actor and legal-name domain), name
/// [`NAMED_CARD`], and return once the enchantment is on the battlefield.
fn cast_naming_grizzly_bears(runner: &mut GameRunner, intervention: ObjectId) {
    let card_id = runner.state().objects[&intervention].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: intervention,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Gideon's Intervention is castable");
    runner.advance_until_stack_empty();

    let WaitingFor::NamedChoice {
        player,
        choice_type,
        options: prompt_options,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "the as-enters card-name choice must be offered, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, P0, "the enchantment's controller names the card");
    let ChoiceType::CardName {
        options: domain, ..
    } = &choice_type
    else {
        panic!("expected a card-name choice, got {choice_type:?}");
    };
    if domain.is_empty() && prompt_options.is_empty() {
        assert!(
            runner
                .state()
                .all_card_names
                .iter()
                .any(|legal| legal.eq_ignore_ascii_case(NAMED_CARD)),
            "CR 201.4: the open prompt's legal domain is the Oracle card reference, \
             which must include {NAMED_CARD}"
        );
    } else {
        assert!(
            domain
                .iter()
                .chain(prompt_options.iter())
                .any(|legal| legal.eq_ignore_ascii_case(NAMED_CARD)),
            "the offered card-name options must include {NAMED_CARD}"
        );
    }
    assert!(
        !runner.state().battlefield.contains(&intervention),
        "CR 614.1c: the choice is made before the permanent enters"
    );

    runner
        .act(GameAction::ChooseOption {
            choice: NAMED_CARD.to_string(),
        })
        .expect("naming a printed card must be accepted");

    assert!(
        runner.state().battlefield.contains(&intervention),
        "the enchantment enters after the name is chosen"
    );
    assert_eq!(
        runner.state().objects[&intervention].chosen_card_name(),
        Some(NAMED_CARD),
        "the chosen card name persists on the enchantment"
    );
}

/// Completion boundary: combat damage was dealt and the game settled back into
/// a clean priority window with an empty stack.
fn assert_combat_settled(runner: &GameRunner) {
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "combat must settle into a priority window, got {:?}",
        state.waiting_for
    );
    assert!(
        state.stack.is_empty(),
        "nothing may remain on the stack after combat damage"
    );
    assert!(
        matches!(state.phase, Phase::EndCombat | Phase::PostCombatMain),
        "combat damage must have been dealt, phase is {:?}",
        state.phase
    );
}

/// CR 609.7c + CR 615.1a + CR 120.3a: damage from a source with the chosen name
/// is prevented when the recipient is P0 or a permanent P0 controls. A source
/// with a different name damages P0 in the same combat.
#[test]
fn gideons_intervention_prevents_chosen_name_damage_to_its_controller() {
    let db = db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let intervention = scenario.add_real_card(P0, "Gideon's Intervention", Zone::Hand, db);
    let blocker = scenario.add_vanilla(P0, 2, 2);
    let named_one = scenario.add_real_card(P1, NAMED_CARD, Zone::Battlefield, db);
    let named_two = scenario.add_real_card(P1, NAMED_CARD, Zone::Battlefield, db);
    let other = scenario.add_real_card(P1, "Hill Giant", Zone::Battlefield, db);
    let mut pool = mana(ManaType::White, 2);
    pool.extend(mana(ManaType::Colorless, 2));
    scenario.with_mana_pool(P0, pool);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    cast_naming_grizzly_bears(&mut runner, intervention);

    begin_p1_attack_step(&mut runner);
    advance_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[
            (named_one, AttackTarget::Player(P0)),
            (named_two, AttackTarget::Player(P0)),
            (other, AttackTarget::Player(P0)),
        ])
        .expect("P1 attacks with both Grizzly Bears and Hill Giant");
    advance_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(blocker, named_one)])
        .expect("P0 blocks one Grizzly Bears with the 2/2");
    runner.combat_damage();
    assert_combat_settled(&runner);

    assert_eq!(
        runner.life(P0),
        17,
        "only Hill Giant (a non-matching source) damages P0; both Grizzly Bears' \
         combat damage is prevented"
    );
    assert_eq!(
        runner.state().objects[&blocker].damage_marked,
        0,
        "the named attacker's combat damage to a permanent P0 controls is prevented"
    );
    assert_eq!(
        runner.state().objects[&blocker].zone,
        Zone::Battlefield,
        "reach: the 2/2 blocker survives"
    );
    assert_eq!(
        runner.state().objects[&named_one].zone,
        Zone::Graveyard,
        "reach guard: the shield is one-way — the blocker's damage to the named \
         attacker is not prevented"
    );
    assert_eq!(
        runner.state().objects[&named_two].zone,
        Zone::Battlefield,
        "reach guard: the unblocked named attacker survives its prevented attack"
    );
    assert_eq!(
        runner.life(P1),
        20,
        "reach guard: P1 takes no combat damage in this combat"
    );
}

/// CR 609.7c: the prevention's recipient scope is "you and permanents you
/// control". Damage dealt by the named source to P1 (life) and to P1's own
/// creature is dealt in full.
#[test]
fn gideons_intervention_does_not_prevent_chosen_name_damage_to_opponents() {
    let db = db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let intervention = scenario.add_real_card(P0, "Gideon's Intervention", Zone::Hand, db);
    let named_one = scenario.add_real_card(P0, NAMED_CARD, Zone::Battlefield, db);
    let named_two = scenario.add_real_card(P0, NAMED_CARD, Zone::Battlefield, db);
    let p1_blocker = scenario.add_vanilla(P1, 0, 2);
    let mut pool = mana(ManaType::White, 2);
    pool.extend(mana(ManaType::Colorless, 2));
    scenario.with_mana_pool(P0, pool);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    cast_naming_grizzly_bears(&mut runner, intervention);

    advance_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[
            (named_one, AttackTarget::Player(P1)),
            (named_two, AttackTarget::Player(P1)),
        ])
        .expect("P0 attacks with both Grizzly Bears");
    advance_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(p1_blocker, named_one)])
        .expect("P1 blocks one Grizzly Bears");
    runner.combat_damage();
    assert_combat_settled(&runner);

    assert_eq!(
        runner.life(P1),
        18,
        "damage dealt by the chosen-name source to its controller's opponent is \
         not a protected recipient, so the unblocked 2 is dealt"
    );
    assert_eq!(
        runner.state().objects[&p1_blocker].zone,
        Zone::Graveyard,
        "damage dealt by the chosen-name source to P1's own creature is not prevented: \
         the 0/2 blocker took 2 damage and died"
    );
    assert_eq!(
        runner.life(P0),
        20,
        "reach guard: P0's own creatures are the only sources here"
    );
}

/// CR 101.2 + CR 201.2 + CR 604.1: the second line prohibits P1 from casting a
/// spell with the chosen name, while a differently named creature spell is
/// still castable.
#[test]
fn gideons_intervention_blocks_the_named_spell_but_not_another() {
    let db = db();

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let intervention = scenario.add_real_card(P0, "Gideon's Intervention", Zone::Hand, db);
    let named_spell = scenario.add_real_card(P1, NAMED_CARD, Zone::Hand, db);
    let other_spell = scenario.add_real_card(P1, "Centaur Courser", Zone::Hand, db);
    let mut p0_pool = mana(ManaType::White, 2);
    p0_pool.extend(mana(ManaType::Colorless, 2));
    scenario.with_mana_pool(P0, p0_pool);
    let mut p1_pool = mana(ManaType::Green, 1);
    p1_pool.extend(mana(ManaType::Colorless, 3));
    scenario.with_mana_pool(P1, p1_pool);

    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    cast_naming_grizzly_bears(&mut runner, intervention);

    // P1's own main phase so the creature cards are castable at sorcery speed.
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };

    let named_card_id = runner.state().objects[&named_spell].card_id;
    let rejected = runner.act(GameAction::CastSpell {
        object_id: named_spell,
        card_id: named_card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    });
    let Err(EngineError::ActionNotAllowed(message)) = &rejected else {
        panic!("CR 101.2: the chosen-name spell must be prohibited, got {rejected:?}");
    };
    assert_eq!(
        message, "A static ability prevents you from casting this spell",
        "CR 101.2: the prohibition, not another prerequisite, must reject the cast"
    );
    assert_eq!(
        runner.state().objects[&named_spell].zone,
        Zone::Hand,
        "the prohibited spell stays in hand"
    );
    assert!(
        runner.state().stack.is_empty(),
        "the prohibited cast puts nothing on the stack"
    );
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P1),
        "the rejection leaves P1 holding priority, got {:?}",
        runner.state().waiting_for
    );

    let other_card_id = runner.state().objects[&other_spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: other_spell,
            card_id: other_card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("a differently named creature spell is not prohibited");
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&other_spell].zone,
        Zone::Battlefield,
        "the differently named creature spell resolves onto the battlefield"
    );
    assert_eq!(
        runner.state().objects[&other_spell].controller,
        P1,
        "the cast spell is controlled by its caster"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ),
        "the game settles with priority for the active player, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().stack.is_empty(),
        "nothing may remain on the stack"
    );
}
