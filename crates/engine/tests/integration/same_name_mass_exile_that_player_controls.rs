//! "<verb> target <object> an opponent controls and all <X> that player
//! controls with the same name as that <object>" — the mass conjunct's "that
//! player" is the targeted object's controller (CR 608.2c), read through
//! last-known information once the target has already been exiled (CR 608.2h).
//!
//! Before the fix the conjunct bound `ControllerRef::You`, so Legion's End and
//! Deputy of Detention exiled the CASTER's same-named permanents, and Legions
//! to Ashes ("all tokens …") did not parse at all. Deputy's "until this
//! creature leaves the battlefield" must also cover the mass conjunct (CR 610.3).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::{DurationEvent, Effect, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const BANISHMENT: &str = "Flash\nWhen this enchantment enters, exile target nonland permanent an \
opponent controls and all other nonland permanents your opponents control with the same name as \
that permanent until this enchantment leaves the battlefield.";

const CLONE: &str =
    "You may have this creature enter as a copy of any creature on the battlefield.";

const RITE_OF_REPLICATION: &str = "Kicker {5} (You may pay an additional {5} as you cast this \
spell.)\nCreate a token that's a copy of target creature. If this spell was kicked, create five \
of those tokens instead.";

/// A typed mass-only "until" fixture: the bounded zone move is the ROOT
/// `ChangeZoneAll` (no single-target `ChangeZone` above it), followed by an
/// "if you do" rider.
const MASS_UNTIL_HOST: &str = "When this creature enters, exile all creatures your opponents \
control until this creature leaves the battlefield. If you do, draw a card.";

const LEGIONS_TO_ASHES: &str = "Exile target nonland permanent an opponent controls and all \
tokens that player controls with the same name as that permanent.";

const LEGIONS_END: &str = "Exile target creature an opponent controls with mana value 2 or less \
and all other creatures that player controls with the same name as that creature. Then that \
player reveals their hand and exiles all cards with that name from their hand and graveyard.";

const DEPUTY_OF_DETENTION: &str = "When this creature enters, exile target nonland permanent an \
opponent controls and all other nonland permanents that player controls with the same name as \
that permanent until this creature leaves the battlefield.";

fn free() -> ManaCost {
    ManaCost::Cost {
        generic: 0,
        shards: vec![],
    }
}

fn make_token(runner: &mut GameRunner, id: ObjectId) {
    runner.state_mut().objects.get_mut(&id).unwrap().is_token = true;
}

fn on_battlefield(state: &GameState, id: ObjectId) -> bool {
    state.battlefield.contains(&id)
}

fn named_on_battlefield(state: &GameState, player: PlayerId, name: &str) -> usize {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|o| o.controller == player && o.name == name)
        .count()
}

/// CR 111.1 + CR 201.2a + CR 608.2c: Legions to Ashes exiles the target and
/// every TOKEN its controller controls with that name — not that player's
/// same-named nontoken permanents, not other-named tokens, and never the
/// caster's tokens.
#[test]
fn legions_to_ashes_exiles_only_that_players_same_name_tokens() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let victim = scenario.add_creature(P1, "Soldier", 1, 1).id();
    let twin_token = scenario.add_creature(P1, "Soldier", 1, 1).id();
    let other_name_token = scenario.add_creature(P1, "Spirit", 1, 1).id();
    let nontoken_twin = scenario.add_creature(P1, "Soldier", 1, 1).id();
    let my_token = scenario.add_creature(P0, "Soldier", 1, 1).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Legions to Ashes", false, LEGIONS_TO_ASHES)
        .with_mana_cost(free())
        .id();

    let mut runner = scenario.build();
    for token in [victim, twin_token, other_name_token, my_token] {
        make_token(&mut runner, token);
    }

    let outcome = runner.cast(spell).target_objects(&[victim]).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Legions to Ashes must resolve cleanly, got {:?}",
        outcome.final_waiting_for()
    );

    let state = runner.state();
    assert!(!on_battlefield(state, victim), "the target must be exiled");
    // Reach guard: the mass conjunct ran and exiled the opponent's same-name token.
    assert!(
        !on_battlefield(state, twin_token),
        "that player's same-name token must be exiled"
    );
    assert!(
        on_battlefield(state, my_token),
        "the caster's same-name token must stay: \"that player\" is the target's controller"
    );
    assert!(
        on_battlefield(state, nontoken_twin),
        "a same-name NONTOKEN permanent must stay (\"all tokens\")"
    );
    assert!(
        on_battlefield(state, other_name_token),
        "a token with a different name must stay"
    );
}

/// CR 608.2c: Legion's End exiles the target and that player's same-name
/// creatures; the caster's same-name creature is untouched.
#[test]
fn legions_end_spares_the_casters_same_name_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let twin = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let mine = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Legion's End", false, LEGIONS_END)
        .with_mana_cost(free())
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_objects(&[victim]).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Legion's End must resolve cleanly, got {:?}",
        outcome.final_waiting_for()
    );

    outcome.assert_zone(&[victim], Zone::Exile);
    // Reach guard: the mass conjunct exiled the opponent's other copy.
    outcome.assert_zone(&[twin], Zone::Exile);
    assert!(
        on_battlefield(runner.state(), mine),
        "the caster's same-name creature must stay on the battlefield"
    );
}

/// CR 610.3: Deputy of Detention's "until this creature leaves the
/// battlefield" covers the whole exile instruction, so the same-name mass
/// exile returns with the target when Deputy leaves.
#[test]
fn deputy_of_detention_returns_mass_exiled_permanents_when_it_leaves() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let victim = scenario.add_creature(P1, "Soldier", 1, 1).id();
    let twin = scenario.add_creature(P1, "Soldier", 1, 1).id();
    let mine = scenario.add_creature(P0, "Soldier", 1, 1).id();

    let deputy = scenario
        .add_creature_to_hand_from_oracle(P0, "Deputy of Detention", 1, 3, DEPUTY_OF_DETENTION)
        .with_mana_cost(free())
        .id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", false, "Destroy target creature.")
        .with_mana_cost(free())
        .id();

    let mut runner = scenario.build();
    let outcome = runner.cast(deputy).target_objects(&[victim]).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Deputy's ETB must resolve cleanly, got {:?}",
        outcome.final_waiting_for()
    );
    outcome.assert_zone(&[victim, twin], Zone::Exile);
    assert!(
        on_battlefield(runner.state(), mine),
        "the caster's same-name permanent must stay on the battlefield"
    );

    runner.cast(removal).target_objects(&[deputy]).resolve();
    assert!(
        !on_battlefield(runner.state(), deputy),
        "Deputy must have left the battlefield"
    );
    // CR 610.3: both exiled permanents return (as new objects, CR 400.7).
    assert_eq!(
        named_on_battlefield(runner.state(), P1, "Soldier"),
        2,
        "the target AND the mass-exiled same-name permanent must return"
    );
}

// --- Round 2: CR 610.3b refusal, copied names, controls -------------------

/// Cast `host` and drive it until its own ETB trigger waits on the stack
/// (choosing `target` for it when the trigger targets).
fn stage_etb(runner: &mut GameRunner, host: ObjectId, target: Option<ObjectId>) {
    let _ = runner.cast(host).commit();
    for _ in 0..48 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: target.map(TargetRef::Object),
                    })
                    .expect("choose the ETB target");
            }
            WaitingFor::Priority { .. } => {
                if host_trigger_on_stack(runner.state(), host) {
                    assert!(on_battlefield(runner.state(), host), "host entered");
                    return;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("resolve the host");
            }
            other => panic!("staging halted at {other:?}"),
        }
    }
    panic!("ETB staging bound exceeded");
}

fn host_trigger_on_stack(state: &GameState, host: ObjectId) -> bool {
    state.stack.iter().any(|entry| {
        entry.source_id == host && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
    })
}

fn settle(runner: &mut GameRunner) {
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass");
            }
            other => panic!("resolution halted at {other:?}"),
        }
    }
    panic!("resolution bound exceeded");
}

fn give_priority(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..4 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { player: p } if p == player) {
            return;
        }
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
    panic!("{player:?} did not receive priority");
}

/// With the host's ETB waiting on the stack, `caster` removes the host in
/// response; the removal resolves first.
fn remove_host_in_response(
    runner: &mut GameRunner,
    caster: PlayerId,
    removal: ObjectId,
    host: ObjectId,
) {
    give_priority(runner, caster);
    let _ = runner.cast(removal).target_object(host).commit();
    runner.resolve_top();
    assert_eq!(
        runner.state().objects[&host].zone,
        Zone::Graveyard,
        "the removal must resolve before the ETB"
    );
    assert!(
        host_trigger_on_stack(runner.state(), host),
        "ETB still pending"
    );
}

/// The pending ETB's node latches for `SourceLeftBattlefield`, root first.
fn pending_etb_latches(state: &GameState, host: ObjectId) -> Vec<bool> {
    let ability = state
        .stack
        .iter()
        .find(|entry| entry.source_id == host)
        .and_then(|entry| entry.ability())
        .expect("pending ETB ability");
    std::iter::successors(Some(ability), |node| node.sub_ability.as_deref())
        .map(|node| {
            node.context
                .duration_events
                .contains(&DurationEvent::SourceLeftBattlefield)
        })
        .collect()
}

fn links_from(state: &GameState, host: ObjectId) -> usize {
    state
        .exile_links
        .iter()
        .filter(|link| link.source_id == host)
        .count()
}

fn hand_size(state: &GameState, player: PlayerId) -> usize {
    state
        .objects
        .values()
        .filter(|o| o.zone == Zone::Hand && o.owner == player)
        .count()
}

/// CR 610.3b: Deputy of Detention leaves before its ETB resolves, so neither
/// the target nor the same-name mass conjunct moves, and no return link is
/// installed. Each bounded node refuses on its OWN latch.
#[test]
fn deputy_of_detention_moves_nothing_when_it_left_before_its_etb_resolved() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario.add_creature(P1, "Soldier", 1, 1).id();
    let twin = scenario.add_creature(P1, "Soldier", 1, 1).id();
    let deputy = scenario
        .add_creature_to_hand_from_oracle(P0, "Deputy of Detention", 1, 3, DEPUTY_OF_DETENTION)
        .with_mana_cost(free())
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P1, "Murder", true, "Destroy target creature.")
        .with_mana_cost(free())
        .id();
    let mut runner = scenario.build();

    stage_etb(&mut runner, deputy, Some(target));
    remove_host_in_response(&mut runner, P1, murder, deputy);
    // Reach guard: both bounded nodes (root exile, same-name mass exile)
    // latched the departure.
    assert_eq!(
        pending_etb_latches(runner.state(), deputy),
        vec![true, true]
    );

    settle(&mut runner);
    assert!(
        on_battlefield(runner.state(), target),
        "the target must stay"
    );
    assert!(
        on_battlefield(runner.state(), twin),
        "the same-name copy must stay"
    );
    assert_eq!(links_from(runner.state(), deputy), 0, "no return links");
}

/// CR 610.3b, three players: Banishment is destroyed before its ETB resolves,
/// so none of the opponents' same-name permanents moves.
#[test]
fn banishment_moves_nothing_in_three_player_when_it_left_first() {
    let mut scenario = GameScenario::new_n_player(3, 71);
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario.add_creature(P1, "Soldier", 1, 1).id();
    let twin = scenario.add_creature(P1, "Soldier", 1, 1).id();
    let other_opponent = scenario.add_creature(P2, "Soldier", 1, 1).id();
    let mine = scenario.add_creature(P0, "Soldier", 1, 1).id();
    let banishment = scenario
        .add_spell_to_hand(P0, "Banishment", false)
        .as_enchantment()
        .from_oracle_text(BANISHMENT)
        .with_mana_cost(free())
        .id();
    let disenchant = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Disenchant",
            true,
            "Destroy target artifact or enchantment.",
        )
        .with_mana_cost(free())
        .id();
    let mut runner = scenario.build();

    stage_etb(&mut runner, banishment, Some(target));
    remove_host_in_response(&mut runner, P0, disenchant, banishment);
    assert_eq!(
        pending_etb_latches(runner.state(), banishment),
        vec![true, true]
    );

    settle(&mut runner);
    for id in [target, twin, other_opponent, mine] {
        assert!(on_battlefield(runner.state(), id), "{id:?} must stay");
    }
    assert_eq!(links_from(runner.state(), banishment), 0, "no return links");
}

/// Control (CR 610.3 + CR 610.3c), three players: with Banishment alive, both
/// opponents' same-name permanents are exiled, the caster's is spared, and all
/// of them return under their owners' control when Banishment leaves.
#[test]
fn banishment_three_player_exiles_both_opponents_and_all_return() {
    let mut scenario = GameScenario::new_n_player(3, 71);
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario.add_creature(P1, "Soldier", 1, 1).id();
    let twin = scenario.add_creature(P1, "Soldier", 1, 1).id();
    let other_opponent = scenario.add_creature(P2, "Soldier", 1, 1).id();
    let mine = scenario.add_creature(P0, "Soldier", 1, 1).id();
    let banishment = scenario
        .add_spell_to_hand(P0, "Banishment", false)
        .as_enchantment()
        .from_oracle_text(BANISHMENT)
        .with_mana_cost(free())
        .id();
    let disenchant = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Disenchant",
            true,
            "Destroy target artifact or enchantment.",
        )
        .with_mana_cost(free())
        .id();
    let mut runner = scenario.build();

    stage_etb(&mut runner, banishment, Some(target));
    settle(&mut runner);
    for id in [target, twin, other_opponent] {
        assert_eq!(
            runner.state().objects[&id].zone,
            Zone::Exile,
            "{id:?} exiled"
        );
    }
    assert!(
        on_battlefield(runner.state(), mine),
        "the caster's copy stays"
    );

    runner.cast(disenchant).target_object(banishment).resolve();
    assert_eq!(named_on_battlefield(runner.state(), P1, "Soldier"), 2);
    assert_eq!(named_on_battlefield(runner.state(), P2, "Soldier"), 1);
    assert_eq!(named_on_battlefield(runner.state(), P0, "Soldier"), 1);
}

/// CR 610.3b + CR 608.2c: a mass-only bounded move (the root `ChangeZoneAll`
/// itself carries "until this creature leaves") refuses on its own latch when
/// the host left first, and its "if you do" rider sees the move as not
/// performed (no draw). Typed fixture: no printed card carries this exact
/// text; it isolates the mass node with no bounded single-target node above it.
#[test]
fn mass_only_until_exile_refuses_and_its_if_you_do_rider_does_not_fire() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    scenario.add_card_to_library_top(P0, "Island");
    let host = scenario
        .add_creature_to_hand_from_oracle(P0, "Detention Warden", 2, 2, MASS_UNTIL_HOST)
        .with_mana_cost(free())
        .id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P1, "Murder", true, "Destroy target creature.")
        .with_mana_cost(free())
        .id();
    let mut runner = scenario.build();
    assert_mass_only_fixture_shape(&runner, host);

    stage_etb(&mut runner, host, None);
    remove_host_in_response(&mut runner, P1, murder, host);
    // Reach guard: the mass ROOT node latched the departure itself.
    assert!(
        pending_etb_latches(runner.state(), host)[0],
        "mass root latched"
    );
    let hand_before = hand_size(runner.state(), P0);

    settle(&mut runner);
    assert!(on_battlefield(runner.state(), victim), "nothing moves");
    assert_eq!(links_from(runner.state(), host), 0, "no return links");
    assert_eq!(runner.state().last_effect_count, Some(0), "count 0");
    assert_eq!(
        hand_size(runner.state(), P0),
        hand_before,
        "the \"if you do\" draw must not happen"
    );
}

/// Control: with the host alive, the same fixture exiles the opponent's
/// creature and the "if you do" draw happens.
#[test]
fn mass_only_until_exile_with_host_alive_moves_and_draws() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let victim = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    scenario.add_card_to_library_top(P0, "Island");
    let host = scenario
        .add_creature_to_hand_from_oracle(P0, "Detention Warden", 2, 2, MASS_UNTIL_HOST)
        .with_mana_cost(free())
        .id();
    let mut runner = scenario.build();
    assert_mass_only_fixture_shape(&runner, host);

    stage_etb(&mut runner, host, None);
    let hand_before = hand_size(runner.state(), P0);
    settle(&mut runner);
    assert_eq!(runner.state().objects[&victim].zone, Zone::Exile, "moved");
    assert_eq!(links_from(runner.state(), host), 1, "one return link");
    assert_eq!(
        hand_size(runner.state(), P0),
        hand_before + 1,
        "drew a card"
    );
}

/// CR 118.12: with the host alive and no creature to exile, the mass exile was
/// still performed ("started"), so its "if you do" draw happens exactly once.
/// Only a CR 610.3b refusal is non-performance.
#[test]
fn mass_only_until_exile_with_nothing_to_exile_still_draws() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mine = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    scenario.add_card_to_library_top(P0, "Island");
    let host = scenario
        .add_creature_to_hand_from_oracle(P0, "Detention Warden", 2, 2, MASS_UNTIL_HOST)
        .with_mana_cost(free())
        .id();
    let mut runner = scenario.build();
    assert_mass_only_fixture_shape(&runner, host);

    stage_etb(&mut runner, host, None);
    let hand_before = hand_size(runner.state(), P0);
    settle(&mut runner);
    assert!(on_battlefield(runner.state(), host), "host stays");
    assert!(
        on_battlefield(runner.state(), mine),
        "nothing of mine moves"
    );
    assert_eq!(links_from(runner.state(), host), 0, "nothing exiled");
    assert_eq!(
        hand_size(runner.state(), P0),
        hand_before + 1,
        "an empty but performed mass exile still satisfies \"if you do\""
    );
}

/// SHAPE guard for the typed fixture: root `ChangeZoneAll` carrying the
/// until-leaves duration, with a conditioned draw rider.
fn assert_mass_only_fixture_shape(runner: &GameRunner, host: ObjectId) {
    let execute = runner.state().objects[&host]
        .base_trigger_definitions
        .iter()
        .find_map(|trigger| trigger.execute.as_deref())
        .expect("ETB execute");
    assert!(
        matches!(
            &*execute.effect,
            Effect::ChangeZoneAll {
                destination: Zone::Exile,
                ..
            }
        ),
        "fixture root must be the mass exile, got {:?}",
        execute.effect
    );
    assert!(execute.duration.is_some(), "fixture root must be bounded");
    let rider = execute.sub_ability.as_deref().expect("if-you-do rider");
    assert!(
        matches!(&*rider.effect, Effect::Draw { .. }),
        "rider is a draw"
    );
    assert!(rider.condition.is_some(), "rider is conditioned");
}

/// CR 608.2h + CR 201.2a + CR 707.2: the exiled target is a real Clone copying
/// Grizzly Bears. Its copied name reverts once it leaves the battlefield, but
/// "the same name as that permanent" is the name it had there, so P1's Bears
/// token (unkicked Rite of Replication) is exiled too.
#[test]
fn legions_to_ashes_uses_the_copied_name_of_an_exiled_clone() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let model = scenario
        .add_creature(P1, "Grizzly Bears", 2, 2)
        .with_subtypes(vec!["Bear"])
        .id();
    let clone = scenario
        .add_creature_to_hand_from_oracle(P1, "Clone", 0, 0, CLONE)
        .with_subtypes(vec!["Shapeshifter"])
        .with_mana_cost(free())
        .id();
    let my_token = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let rite = scenario
        .add_spell_to_hand(P1, "Rite of Replication", false)
        .from_oracle_text_with_keywords(&["Kicker"], RITE_OF_REPLICATION)
        .with_mana_cost(free())
        .id();
    let legions = scenario
        .add_spell_to_hand_from_oracle(P0, "Legions to Ashes", false, LEGIONS_TO_ASHES)
        .with_mana_cost(free())
        .id();
    let mut runner = scenario.build();
    make_token(&mut runner, my_token);

    // P1's main phase for the Clone and Rite casts.
    runner.state_mut().active_player = P1;
    runner.state_mut().priority_player = P1;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P1 };
    runner
        .cast(clone)
        .replacement_choice(0)
        .copy_target(model)
        .resolve();
    assert_eq!(runner.state().objects[&clone].name, "Grizzly Bears");
    assert_eq!(runner.state().objects[&clone].base_name, "Clone");
    runner
        .cast(rite)
        .decline_optional()
        .target_object(model)
        .resolve();
    let p1_token = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| {
            let o = &runner.state().objects[id];
            o.controller == P1 && o.is_token && o.name == "Grizzly Bears"
        })
        .expect("Rite created one Bears token");

    runner.state_mut().active_player = P0;
    runner.state_mut().priority_player = P0;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: P0 };
    runner.cast(legions).target_object(clone).resolve();

    assert_eq!(
        runner.state().objects[&clone].zone,
        Zone::Exile,
        "target exiled"
    );
    assert!(
        !on_battlefield(runner.state(), p1_token),
        "P1's same-name (copied-name) token must be exiled"
    );
    assert!(
        on_battlefield(runner.state(), model),
        "nontoken Bears stays"
    );
    assert!(
        on_battlefield(runner.state(), my_token),
        "the caster's token stays"
    );
}

/// Control (CR 608.2b): the target is bounced in response, so the spell's only
/// target is illegal and nothing else happens either.
#[test]
fn legions_to_ashes_with_bounced_target_does_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario.add_creature(P1, "Soldier", 1, 1).id();
    let twin_token = scenario.add_creature(P1, "Soldier", 1, 1).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Legions to Ashes", false, LEGIONS_TO_ASHES)
        .with_mana_cost(free())
        .id();
    let unsummon = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Unsummon",
            true,
            "Return target creature to its owner's hand.",
        )
        .with_mana_cost(free())
        .id();
    let mut runner = scenario.build();
    make_token(&mut runner, twin_token);

    let _ = runner.cast(spell).target_object(target).commit();
    let _ = runner.cast(unsummon).target_object(target).commit();
    runner.resolve_top();
    assert_eq!(runner.state().objects[&target].zone, Zone::Hand, "bounced");
    settle(&mut runner);
    assert!(on_battlefield(runner.state(), twin_token), "no mass exile");
    assert_eq!(runner.state().objects[&spell].zone, Zone::Graveyard);
}

/// Control (CR 608.2c + CR 608.2h): the target is owned by P2 but controlled
/// by P1, so "that player" is P1 — only P1's same-name tokens are exiled.
#[test]
fn legions_to_ashes_binds_the_targets_controller_not_its_owner() {
    let mut scenario = GameScenario::new_n_player(3, 71);
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario
        .add_creature(P2, "Soldier", 1, 1)
        .controlled_by(P1)
        .id();
    let p1_token = scenario.add_creature(P1, "Soldier", 1, 1).id();
    let p2_token = scenario.add_creature(P2, "Soldier", 1, 1).id();
    let my_token = scenario.add_creature(P0, "Soldier", 1, 1).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Legions to Ashes", false, LEGIONS_TO_ASHES)
        .with_mana_cost(free())
        .id();
    let mut runner = scenario.build();
    for token in [p1_token, p2_token, my_token] {
        make_token(&mut runner, token);
    }
    assert_eq!(runner.state().objects[&target].controller, P1);

    runner.cast(spell).target_object(target).resolve();
    assert_eq!(runner.state().objects[&target].zone, Zone::Exile);
    assert!(
        !on_battlefield(runner.state(), p1_token),
        "the controller's token"
    );
    assert!(
        on_battlefield(runner.state(), p2_token),
        "not the owner's token"
    );
    assert!(
        on_battlefield(runner.state(), my_token),
        "not the caster's token"
    );
}
