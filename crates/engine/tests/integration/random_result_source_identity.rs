//! Random-result source identity: printed-card regressions and explicitly synthetic boundaries.
//! Oracle strings checked against data/mtgjson/AtomicCards.json.
//! CR 113.7a: an activated/triggered ability survives its source's departure.
//! CR 400.7: leaving and returning creates a different object.

use engine::game::effects::flip_coin;
use engine::game::keywords::object_has_effective_keyword_kind;
use engine::game::printed_cards::snapshot_object_face;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::trigger_source_context_for_latch;
use engine::game::zones;
use engine::types::ability::{
    AbilityCondition, AbilityDefinition, AbilityKind, CoinFlipRun, ControllerRef, Effect,
    EffectOutcomeSignal, MultiTargetSpec, ObjectScope, PtValue, QuantityExpr, QuantityRef,
    ReplacementDefinition, ResolvedAbility, SubAbilityLink, TargetChoiceTiming, TargetFilter,
    TargetRef, TriggerDefinition, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card::LayoutKind;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, GameRng, GameState, RandomDraw, StackEntry, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::KeywordKind;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::triggers::TriggerMode;
use engine::types::zones::{EtbTapState, Zone};
use rand::Rng;

use super::rules::AttackTarget;

const SANDSWIMMER: &str = "{R}: Flip a coin. If you win the flip, return this creature to its owner's hand. If you lose the flip, sacrifice this creature.";
const EFREET: &str = "Flying\n{0}: Flip a coin. If you win the flip, this creature phases out. If you lose the flip, sacrifice this creature. (While it's phased out, it's treated as though it doesn't exist. It phases in before you untap during your next untap step.)";
const RIG: &str = "Trample\nThis creature attacks each combat if able.\nWhenever this creature is dealt damage, flip a coin. If you lose the flip, sacrifice this creature.\nWhen this creature dies, flip a coin. If you lose the flip, it deals 4 damage to each creature and each player.";
const RAL: &str = "Instant and sorcery spells you cast cost {1} less to cast.\nWhenever you cast an instant or sorcery spell during your turn, flip a coin. If you lose the flip, Ral deals 1 damage to you. If you win the flip, you may exile Ral. If you do, return him to the battlefield transformed under his owner's control.";
const RAL_BACK: &str = "Ral enters with an additional loyalty counter on him for each instant and sorcery spell you've cast this turn.\n[+1]: Until your next turn, instant and sorcery spells you cast cost {1} less to cast.\n[−2]: Ral deals 2 damage divided as you choose among one or two targets. Draw a card if you control a blue permanent other than Ral.\n[−8]: Exile the top eight cards of your library. You may cast instant and sorcery spells from among them this turn without paying their mana costs.";
const ROGUE: &str = "Sneak Attack — Whenever this creature attacks, roll a d20.\n1—9 | This creature gains deathtouch until end of turn.\n10—19 | It gets +1/+0 and gains deathtouch until end of turn.\n20 | It gets +3/+0 and gains first strike and deathtouch until end of turn.";
const NOTHIC: &str = "Weird Insight — When this creature dies, roll a d20.\n1—9 | You draw a card and you lose 1 life.\n10—19 | You draw two cards and you lose 2 life.\n20 | You draw seven cards and you lose 7 life.";
const THUMB: &str = "If you would flip a coin, instead flip two coins and ignore one.";
const CLOUDSHIFT: &str = "Exile target creature you control, then return that card to the battlefield under your control.";
const FLICKER_OF_FATE: &str = "Exile target creature or enchantment, then return it to the battlefield under its owner's control.";
const GROWTH: &str = "Target creature gets +3/+3 until end of turn.";
const CREMATE: &str = "Exile target card from a graveyard.\nDraw a card.";

fn cost(generic: u32, shards: &[ManaCostShard]) -> ManaCost {
    ManaCost::Cost {
        shards: shards.to_vec(),
        generic,
    }
}

fn fund(scenario: &mut GameScenario) {
    for player in [P0, P1] {
        let mana = [
            ManaType::White,
            ManaType::Black,
            ManaType::Red,
            ManaType::Green,
        ]
        .into_iter()
        .flat_map(|color| (0..8).map(move |_| ManaUnit::new(color, ObjectId(0), false, vec![])))
        .collect();
        scenario.with_mana_pool(player, mana);
        scenario.with_life(player, 40);
        scenario.with_library_top(player, &["A", "B", "C", "D", "E", "F", "G", "H"]);
    }
}

fn instant(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    oracle: &str,
    mana: ManaCost,
) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(player, name, true, oracle)
        .with_mana_cost(mana)
        .id()
}

fn cloudshift(scenario: &mut GameScenario) -> ObjectId {
    instant(
        scenario,
        P0,
        "Cloudshift",
        CLOUDSHIFT,
        cost(0, &[ManaCostShard::White]),
    )
}

fn murder(scenario: &mut GameScenario) -> ObjectId {
    instant(
        scenario,
        P0,
        "Murder",
        "Destroy target creature.",
        cost(1, &[ManaCostShard::Black, ManaCostShard::Black]),
    )
}

fn coin_seed(outcomes: &[bool]) -> u64 {
    (0..100_000)
        .find(|&seed| {
            let mut rng = GameRng::seed_from_u64(seed);
            outcomes
                .iter()
                .all(|&won| rng.draw(RandomDraw::Outcome).random_bool(0.5) == won)
        })
        .expect("a seed realizes the requested coin outcomes")
}

fn die_seed(result: u8) -> u64 {
    (0..100_000)
        .find(|&seed| {
            let mut rng = GameRng::seed_from_u64(seed);
            rng.draw(RandomDraw::Outcome).random_range(1..=20_u8) == result
        })
        .expect("a seed realizes the requested d20 result")
}

fn pin(runner: &mut GameRunner, seed: u64) {
    let state = runner.state_mut();
    state.rng_seed = seed;
    state.rng_word_pos = 0;
    state.rng = GameRng::seed_from_u64(seed);
}

// Deliberately only passes priority/orders triggers. Choices under test must be
// observed and answered by their callers, never silently defaulted by a driver.
struct Run {
    runner: GameRunner,
    events: Vec<GameEvent>,
}

impl Run {
    fn new(scenario: GameScenario) -> Self {
        let mut runner = scenario.build();
        engine::game::layers::flush_layers(runner.state_mut());
        Self {
            runner,
            events: Vec::new(),
        }
    }

    fn act(&mut self, action: GameAction) {
        self.events.extend(
            self.runner
                .act(action)
                .expect("public game action must succeed")
                .events,
        );
    }

    fn step(&mut self) {
        match self.runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                assert!(!self.runner.state().stack.is_empty(), "work must remain");
                self.act(GameAction::PassPriority);
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                self.act(GameAction::OrderTriggers {
                    order: (0..triggers.len()).collect(),
                });
            }
            other => panic!("unexpected resolution window: {other:?}"),
        }
    }

    fn until(&mut self, reached: impl Fn(&GameState) -> bool) {
        for _ in 0..64 {
            if reached(self.runner.state()) {
                return;
            }
            self.step();
        }
        panic!("required resolution boundary was not reached");
    }

    fn finish(&mut self) {
        self.until(|state| {
            state.stack.is_empty() && matches!(state.waiting_for, WaitingFor::Priority { .. })
        });
        assert!(self.runner.state().resolution_stack.is_empty());
    }

    fn activate(&mut self, source: ObjectId) {
        self.act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        });
        if matches!(
            self.runner.state().waiting_for,
            WaitingFor::ManaPayment { .. }
        ) {
            self.act(GameAction::PassPriority);
        }
        assert!(matches!(
            self.runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(self.runner.state().stack.iter().any(|entry| {
            entry.source_id == source
                && matches!(entry.kind, StackEntryKind::ActivatedAbility { .. })
        }));
        assert!(self.events.iter().any(|event| matches!(
            event,
            GameEvent::AbilityActivated { source_id, player_id, .. }
                if *source_id == source && *player_id == P0
        )));
    }

    fn commit_at(&mut self, spell: ObjectId, target: ObjectId) {
        drop(self.runner.cast(spell).target_object(target).commit());
        assert!(self
            .runner
            .state()
            .stack
            .iter()
            .any(|entry| entry.source_id == spell));
    }

    fn spell_only(&mut self, spell: ObjectId) {
        self.until(|state| state.objects[&spell].zone == Zone::Graveyard);
        while matches!(
            self.runner.state().waiting_for,
            WaitingFor::OrderTriggers { .. }
        ) {
            self.step();
        }
    }

    fn blink(&mut self, spell: ObjectId, source: ObjectId) {
        let old = self.runner.state().objects[&source].incarnation;
        let start = self.events.len();
        self.commit_at(spell, source);
        self.spell_only(spell);
        assert_eq!(self.runner.state().objects[&source].zone, Zone::Battlefield);
        // CR 400.7: the response really left and returned, rather than merely
        // changing a test stamp or manufacturing a trigger.
        assert_ne!(self.runner.state().objects[&source].incarnation, old);
        assert_eq!(
            moves(&self.events[start..], source),
            vec![
                (Some(Zone::Battlefield), Zone::Exile),
                (Some(Zone::Exile), Zone::Battlefield),
            ]
        );
    }

    fn keep(&mut self, player: PlayerId, won: bool) {
        let WaitingFor::CoinFlipKeepChoice {
            player: actor,
            results,
            keep_count,
        } = &self.runner.state().waiting_for
        else {
            panic!(
                "required Thumb keep prompt: {:?}",
                self.runner.state().waiting_for
            );
        };
        assert_eq!(*actor, player);
        assert_eq!(*keep_count, 1);
        assert_eq!(results.len(), 2);
        assert_ne!(
            results[0], results[1],
            "mixed choices discriminate the kept outcome"
        );
        assert!(self.runner.state().active_coin_flip_frame().is_some());
        let index = results.iter().position(|&result| result == won).unwrap();
        self.act(GameAction::SelectCoinFlips {
            keep_indices: vec![index],
        });
    }

    fn assert_flips(&self, expected: &[(PlayerId, bool)]) {
        // CR 705.2: only the flipping player wins/loses; ignored flips emit no
        // logical result. This guards every unchanged-source assertion below.
        let actual: Vec<_> = self
            .events
            .iter()
            .filter_map(|event| match event {
                GameEvent::CoinFlipped { player_id, won } => Some((*player_id, *won)),
                _ => None,
            })
            .collect();
        assert_eq!(actual, expected);
    }

    fn assert_die(&self, player: PlayerId, result: u8) {
        let actual: Vec<_> = self
            .events
            .iter()
            .filter_map(|event| match event {
                GameEvent::DieRolled {
                    player_id,
                    sides,
                    result,
                } => Some((*player_id, *sides, *result)),
                _ => None,
            })
            .collect();
        assert_eq!(actual, vec![(player, 20, Some(result))]);
    }
}

/// CR 601.2c + CR 603.3d: both spell and trigger target announcements are
/// answered through the same `ChooseTarget` action.
fn target_prompt_open(state: &GameState) -> bool {
    matches!(
        state.waiting_for,
        WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. }
    )
}

fn moves(events: &[GameEvent], source: ObjectId) -> Vec<(Option<Zone>, Zone)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::ZoneChanged {
                object_id,
                from,
                to,
                ..
            } if *object_id == source => Some((*from, *to)),
            _ => None,
        })
        .collect()
}

fn assert_trigger(run: &Run, source: ObjectId, controller: PlayerId) {
    assert!(
        run.runner.state().stack.iter().any(|entry| {
            entry.source_id == source
                && entry.controller == controller
                && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
        }),
        "a real source event must place the expected controller's trigger on the stack"
    );
}

fn activated_flip(efreet: bool, blink: bool, thumb: bool, restore: bool, won: bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    fund(&mut scenario);
    let (name, power, toughness, oracle) = if efreet {
        ("Frenetic Efreet", 2, 1, EFREET)
    } else {
        ("Viashino Sandswimmer", 3, 2, SANDSWIMMER)
    };
    let source = scenario
        .add_creature_from_oracle(P0, name, power, toughness, oracle)
        .id();
    let other = scenario
        .add_creature_from_oracle(P0, name, power, toughness, oracle)
        .id();
    let response = cloudshift(&mut scenario);
    if thumb {
        scenario
            .add_artifact_from_oracle(P0, "Krark's Thumb", THUMB)
            .as_legendary();
    }
    let mut run = Run::new(scenario);
    let mana_before = run.runner.state().players[0].mana_pool.total();
    run.activate(source);
    assert_eq!(
        run.runner.state().players[0].mana_pool.total(),
        mana_before - usize::from(!efreet),
        "the printed activation cost was paid"
    );
    if blink {
        run.blink(response, source);
    }
    let outcomes = if thumb { vec![true, false] } else { vec![won] };
    pin(&mut run.runner, coin_seed(&outcomes));
    if thumb {
        run.until(|state| matches!(state.waiting_for, WaitingFor::CoinFlipKeepChoice { .. }));
        let entry = run
            .runner
            .state()
            .resolving_stack_entry
            .as_ref()
            .expect("the keep pause retains the activation carrier");
        assert_eq!(entry.source_id, source);
        assert!(matches!(
            &entry.kind,
            StackEntryKind::ActivatedAbility { source_id, ability }
                if *source_id == source && ability.source_id == source
        ));
        if restore {
            // Full-state persistence at a legitimately produced pause; no fixture,
            // version pin, or replacement of the persisted source with live data.
            let bytes = serde_json::to_vec(run.runner.state()).expect("serialize parked game");
            let state: GameState = serde_json::from_slice(&bytes).expect("restore parked game");
            run.runner = GameRunner::from_state(state);
        }
        run.keep(P0, won);
    }
    run.finish();
    run.assert_flips(&[(P0, won)]);
    let object = &run.runner.state().objects[&source];
    // CR 400.7 + CR 113.7a: the old activation still flips but cannot move or
    // phase out a different object. Paired unblinked rows prove both branches.
    let expected = if blink || (efreet && won) {
        Zone::Battlefield
    } else if won {
        Zone::Hand
    } else {
        Zone::Graveyard
    };
    assert_eq!(object.zone, expected);
    // CR 702.26b: phasing is a status change, not a zone change.
    assert_eq!(object.is_phased_out(), efreet && won && !blink);
    assert_eq!(run.runner.state().objects[&other].zone, Zone::Battlefield);
    assert!(!run.runner.state().objects[&other].is_phased_out());
    if efreet && won && !blink {
        assert!(run.events.iter().any(|event| matches!(event,
            GameEvent::PermanentPhasedOut { object_id, .. } if *object_id == source)));
    }
}

#[test]
fn sandswimmer_old_win_does_not_bounce_returned_source() {
    activated_flip(false, false, false, false, true);
    activated_flip(false, true, false, false, true);
}

#[test]
fn sandswimmer_old_loss_does_not_sacrifice_returned_source() {
    activated_flip(false, false, false, false, false);
    activated_flip(false, true, false, false, false);
}

#[test]
fn efreet_inline_old_win_keeps_returned_source_phased_in() {
    activated_flip(true, false, false, false, true);
    activated_flip(true, true, false, false, true);
}

#[test]
fn efreet_inline_old_loss_does_not_sacrifice_returned_source() {
    activated_flip(true, false, false, false, false);
    activated_flip(true, true, false, false, false);
}

#[test]
fn efreet_resumed_old_win_keeps_returned_source_phased_in() {
    activated_flip(true, false, true, false, true);
    activated_flip(true, true, true, false, true);
}

#[test]
fn efreet_resumed_old_loss_does_not_sacrifice_returned_source() {
    activated_flip(true, false, true, false, false);
    activated_flip(true, true, true, false, false);
}

#[test]
fn restored_thumb_win_does_not_bounce_returned_sandswimmer() {
    activated_flip(false, false, true, true, true);
    activated_flip(false, true, true, true, true);
}

#[test]
fn restored_thumb_loss_does_not_sacrifice_returned_sandswimmer() {
    activated_flip(false, false, true, true, false);
    activated_flip(false, true, true, true, false);
}

fn rig_damage_flip(blink: bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    fund(&mut scenario);
    let rig = scenario
        .add_creature_from_oracle(P0, "Volatile Rig", 4, 4, RIG)
        .as_artifact_creature()
        .id();
    let shock = instant(
        &mut scenario,
        P0,
        "Shock",
        "Shock deals 2 damage to any target.",
        cost(0, &[ManaCostShard::Red]),
    );
    let response = cloudshift(&mut scenario);
    let mut run = Run::new(scenario);
    run.commit_at(shock, rig);
    run.spell_only(shock);
    assert_eq!(run.runner.state().objects[&rig].damage_marked, 2);
    assert_trigger(&run, rig, P0);
    assert!(run.events.iter().any(|event| matches!(
        event,
        GameEvent::DamageDealt { target: TargetRef::Object(object), amount: 2, .. }
            if *object == rig
    )));
    if blink {
        run.blink(response, rig);
        assert_eq!(run.runner.state().objects[&rig].damage_marked, 0);
    }
    // The damage trigger loses; a resulting dies trigger wins, so it cannot
    // deal four and accidentally kill the returned-source witness.
    pin(&mut run.runner, coin_seed(&[false, true]));
    run.finish();
    run.assert_flips(if blink {
        &[(P0, false)]
    } else {
        &[(P0, false), (P0, true)]
    });
    assert_eq!(
        run.runner.state().objects[&rig].zone,
        if blink {
            Zone::Battlefield
        } else {
            Zone::Graveyard
        }
    );
    assert_eq!(run.runner.state().players[0].life, 40);
    assert_eq!(run.runner.state().players[1].life, 40);
}

#[test]
fn rig_damage_loss_sacrifices_the_current_source() {
    rig_damage_flip(false);
}

#[test]
fn rig_old_damage_loss_does_not_sacrifice_returned_source() {
    rig_damage_flip(true);
}

fn rig_dies_flip(won: bool, token: bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    fund(&mut scenario);
    let rig = scenario
        .add_creature_from_oracle(P0, "Volatile Rig", 4, 4, RIG)
        .as_artifact_creature()
        .id();
    let own = scenario.add_creature(P0, "Own damage witness", 0, 10).id();
    let opposing = scenario
        .add_creature(P1, "Other damage witness", 0, 10)
        .id();
    let kill = murder(&mut scenario);
    let cremate = instant(
        &mut scenario,
        P0,
        "Cremate",
        CREMATE,
        cost(0, &[ManaCostShard::Black]),
    );
    let mut run = Run::new(scenario);
    if token {
        // Fixture a token copy with the printed abilities, as in other token
        // integration scenarios; Murder still produces the actual death.
        run.runner
            .state_mut()
            .objects
            .get_mut(&rig)
            .expect("the Rig token exists before Murder")
            .is_token = true;
        assert!(run.runner.state().objects[&rig].is_token);
    }
    run.commit_at(kill, rig);
    run.spell_only(kill);
    assert_eq!(
        moves(&run.events, rig),
        vec![(Some(Zone::Battlefield), Zone::Graveyard)],
        "Murder must actually kill the Rig before its dies trigger resolves"
    );
    // CR 603.10a: the dies trigger looks back at the departed permanent.
    assert_trigger(&run, rig, P0);
    if token {
        // CR 111.7: the token ceases to exist outside the battlefield, but its
        // death has already triggered the ability now waiting on the stack.
        assert!(
            !run.runner.state().objects.contains_key(&rig),
            "the trigger must resolve without any source object in state.objects"
        );
    } else {
        assert_eq!(run.runner.state().objects[&rig].zone, Zone::Graveyard);
        run.commit_at(cremate, rig);
        run.spell_only(cremate);
        assert_eq!(run.runner.state().objects[&rig].zone, Zone::Exile);
    }
    pin(&mut run.runner, coin_seed(&[won]));
    run.finish();
    run.assert_flips(&[(P0, won)]);
    // CR 113.7a: this branch needs no live permanent to deal its printed damage.
    let damage = if won { 0 } else { 4 };
    for player in &run.runner.state().players {
        assert_eq!(player.life, 40 - damage);
    }
    for object in [own, opposing] {
        assert_eq!(
            run.runner.state().objects[&object].damage_marked,
            damage as u32
        );
        assert_eq!(run.runner.state().objects[&object].zone, Zone::Battlefield);
    }
    let targets: Vec<_> = run
        .events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DamageDealt {
                source_id,
                target,
                amount,
                ..
            } if *source_id == rig => Some((target.clone(), *amount)),
            _ => None,
        })
        .collect();
    if won {
        assert!(targets.is_empty());
    } else {
        assert_eq!(targets.len(), 4);
        for target in [
            TargetRef::Player(P0),
            TargetRef::Player(P1),
            TargetRef::Object(own),
            TargetRef::Object(opposing),
        ] {
            assert!(targets.contains(&(target, 4)));
        }
    }
}

#[test]
fn departed_rig_dies_loss_damages_each_creature_and_player() {
    rig_dies_flip(false, false);
}

#[test]
fn departed_rig_dies_win_deals_no_damage() {
    rig_dies_flip(true, false);
}

#[test]
fn departed_token_rig_dies_loss_damages_each_creature_and_player() {
    rig_dies_flip(false, true);
}

#[test]
fn departed_token_rig_dies_win_deals_no_damage() {
    rig_dies_flip(true, true);
}

fn ral_flip(won: bool, accept: bool, blink: bool, targeted: bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    fund(&mut scenario);
    let ral = scenario
        .add_creature_from_oracle(P0, "Ral, Monsoon Mage", 1, 3, RAL)
        .with_subtypes(vec!["Human", "Wizard"])
        .with_mana_cost(cost(1, &[ManaCostShard::Red]))
        .as_legendary()
        .id();
    // Both printed faces are built from their verbatim Oracle text. The donor
    // stays in hand and supplies only the back-face data, not a second Ral.
    let back = scenario
        .add_spell_to_hand_from_oracle(P0, "Ral, Leyline Prodigy", false, RAL_BACK)
        .as_planeswalker_with_loyalty("Ral", 2)
        .as_legendary()
        .with_mana_cost(ManaCost::NoCost)
        .with_color(vec![ManaColor::Blue, ManaColor::Red])
        .id();
    let witness = scenario.add_creature(P0, "Unchanged witness", 0, 10).id();
    let draw = instant(
        &mut scenario,
        P0,
        "Targetless draw",
        "Draw a card.",
        cost(1, &[]),
    );
    // H1: a real targeted instant spends its target on the witness, so the
    // accepted exile's own result must still be the only referent of "return
    // him" — the cast-event target is a competing authority.
    let growth = instant(
        &mut scenario,
        P0,
        "Giant Growth",
        GROWTH,
        cost(0, &[ManaCostShard::Green]),
    );
    // Opponent's instant avoids creating a second Ral cast trigger while
    // blinking him in response to the first trigger.
    let response = instant(
        &mut scenario,
        P1,
        "Flicker of Fate",
        FLICKER_OF_FATE,
        cost(1, &[ManaCostShard::White]),
    );
    let mut run = Run::new(scenario);
    let mut face = snapshot_object_face(&run.runner.state().objects[&back]);
    face.is_swap_snapshot = false;
    face.layout_kind = Some(LayoutKind::Transform);
    run.runner
        .state_mut()
        .objects
        .get_mut(&ral)
        .unwrap()
        .back_face = Some(face);
    if targeted {
        drop(run.runner.cast(growth).target_object(witness).commit());
    } else {
        drop(run.runner.cast(draw).commit());
    }
    let hand_before_draw = run.runner.state().players[0].hand.len();
    assert_trigger(&run, ral, P0);
    if blink {
        assert!(matches!(
            run.runner.state().waiting_for,
            WaitingFor::Priority { player: P0 }
        ));
        run.act(GameAction::PassPriority);
        assert!(matches!(
            run.runner.state().waiting_for,
            WaitingFor::Priority { player: P1 }
        ));
        run.blink(response, ral);
    }
    let start = run.events.len();
    let incarnation = run.runner.state().objects[&ral].incarnation;
    pin(&mut run.runner, coin_seed(&[won]));
    run.until(|state| {
        matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. })
            || (state.stack.is_empty() && matches!(state.waiting_for, WaitingFor::Priority { .. }))
    });
    // Deliberately exercise the existing prompt-policy limitation: a winning
    // flip offers the optional instruction even after the source left and
    // returned. CR 608.2d forbids impossible choices; it does not require this
    // stale-source offer or establish that its cost can be paid.
    if won {
        assert!(
            matches!(
                run.runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { player: P0, source_id, .. } if source_id == ral
            ),
            "winning flip must reach Ral's optional choice, got {:?}",
            run.runner.state().waiting_for
        );
        run.act(GameAction::DecideOptionalEffect { accept });
    }
    run.finish();
    run.assert_flips(&[(P0, won)]);
    let object = &run.runner.state().objects[&ral];
    let transformed = won && accept && !blink;
    let core_types: Vec<CoreType> = object.card_types.core_types.clone();
    println!(
        "T1 row won={won} accept={accept} blink={blink}: transformed={} name={:?} \
         core_types={core_types:?} subtypes={:?} incarnation={} (pre={incarnation})",
        object.transformed, object.name, object.card_types.subtypes, object.incarnation,
    );
    assert_eq!(object.zone, Zone::Battlefield);
    assert_eq!(object.controller, P0);
    // CR 712.14a + CR 400.7j: the accepted exile's own return finds the moved
    // card and puts its printed planeswalker face onto the battlefield. A stale
    // source's completed exile moved nothing, so its dependent return has no
    // referent and the returned permanent keeps its front face.
    assert_eq!(object.transformed, transformed);
    assert_eq!(
        object.name,
        if transformed {
            "Ral, Leyline Prodigy"
        } else {
            "Ral, Monsoon Mage"
        }
    );
    assert_eq!(core_types.contains(&CoreType::Planeswalker), transformed);
    assert_eq!(core_types.contains(&CoreType::Creature), !transformed);
    if !transformed {
        assert!(object.card_types.subtypes.iter().any(|s| s == "Human"));
        assert!(object.card_types.subtypes.iter().any(|s| s == "Wizard"));
    }
    if transformed {
        assert_ne!(object.incarnation, incarnation);
        assert_eq!(
            moves(&run.events[start..], ral),
            vec![
                (Some(Zone::Battlefield), Zone::Exile),
                (Some(Zone::Exile), Zone::Battlefield),
            ]
        );
    } else {
        assert_eq!(object.incarnation, incarnation);
        assert!(moves(&run.events[start..], ral).is_empty());
    }
    assert_eq!(
        run.runner.state().players[0].life,
        if won { 40 } else { 39 }
    );
    assert_eq!(run.runner.state().players[1].life, 40);
    let witness_object = &run.runner.state().objects[&witness];
    assert_eq!(witness_object.zone, Zone::Battlefield);
    assert!(!witness_object.transformed);
    assert_eq!(
        witness_object.power,
        Some(if targeted { 3 } else { 0 }),
        "the triggering spell's own effect, and nothing else, affects its target"
    );
    assert_eq!(
        witness_object.toughness,
        Some(if targeted { 13 } else { 10 })
    );
    assert_eq!(
        run.runner.state().players[0].hand.len(),
        hand_before_draw + usize::from(!targeted)
    );
}

#[test]
fn ral_winning_accept_exiles_and_returns_transformed() {
    ral_flip(true, true, false, false);
}

#[test]
fn ral_winning_decline_preserves_the_front_face() {
    ral_flip(true, false, false, false);
}

#[test]
fn ral_losing_flip_damages_only_the_trigger_controller() {
    ral_flip(false, false, false, false);
}

#[test]
fn ral_targeted_cast_return_uses_exile_result() {
    ral_flip(true, true, false, true);
    ral_flip(true, true, true, true);
}

#[test]
fn ral_old_winning_flip_does_not_exile_returned_source() {
    ral_flip(true, true, true, false);
}

/// F3: a control change after the trigger is on the stack keeps the trigger's
/// controller (CR 603.3a) and the printed owner's control on the return
/// (CR 110.2a). No Krark's Thumb and no direct controller assignment.
fn ral_control_change(won: bool, accept: bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    fund(&mut scenario);
    let ral = scenario
        .add_creature_from_oracle(P0, "Ral, Monsoon Mage", 1, 3, RAL)
        .with_subtypes(vec!["Human", "Wizard"])
        .with_mana_cost(cost(1, &[ManaCostShard::Red]))
        .as_legendary()
        .id();
    let back = scenario
        .add_spell_to_hand_from_oracle(P0, "Ral, Leyline Prodigy", false, RAL_BACK)
        .as_planeswalker_with_loyalty("Ral", 2)
        .as_legendary()
        .with_mana_cost(ManaCost::NoCost)
        .with_color(vec![ManaColor::Blue, ManaColor::Red])
        .id();
    let draw = instant(
        &mut scenario,
        P0,
        "Targetless draw",
        "Draw a card.",
        cost(1, &[]),
    );
    let steal = instant(
        &mut scenario,
        P1,
        "Act of Aggression",
        "Gain control of target creature until end of turn. Untap that creature. \
         It gains haste until end of turn.",
        cost(3, &[ManaCostShard::Red, ManaCostShard::Red]),
    );
    let mut run = Run::new(scenario);
    let mut face = snapshot_object_face(&run.runner.state().objects[&back]);
    face.is_swap_snapshot = false;
    face.layout_kind = Some(LayoutKind::Transform);
    run.runner
        .state_mut()
        .objects
        .get_mut(&ral)
        .unwrap()
        .back_face = Some(face);
    let seed = coin_seed(&[won]);
    pin(&mut run.runner, seed);
    drop(run.runner.cast(draw).commit());
    assert_trigger(&run, ral, P0);
    let incarnation = run.runner.state().objects[&ral].incarnation;
    assert!(matches!(
        run.runner.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
    run.act(GameAction::PassPriority);
    assert!(matches!(
        run.runner.state().waiting_for,
        WaitingFor::Priority { player: P1 }
    ));
    drop(run.runner.cast(steal).target_object(ral).commit());
    run.until(|state| {
        state.stack.len() == 2 && matches!(state.waiting_for, WaitingFor::Priority { .. })
    });
    {
        let object = &run.runner.state().objects[&ral];
        assert_eq!(object.owner, P0);
        assert_eq!(object.controller, P1);
        assert_eq!(object.incarnation, incarnation);
    }
    run.until(|state| {
        matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. })
            || (state.stack.is_empty() && matches!(state.waiting_for, WaitingFor::Priority { .. }))
    });
    if won {
        // CR 603.3a + CR 705.2: the old trigger keeps P0 as its controller.
        assert!(
            matches!(
                run.runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { player: P0, source_id, .. } if source_id == ral
            ),
            "the old trigger's optional choice belongs to P0, got {:?}",
            run.runner.state().waiting_for
        );
        run.act(GameAction::DecideOptionalEffect { accept });
    }
    run.finish();
    run.assert_flips(&[(P0, won)]);
    let object = &run.runner.state().objects[&ral];
    let transformed = won && accept;
    assert_eq!(object.zone, Zone::Battlefield);
    assert_eq!(object.owner, P0);
    assert_eq!(object.transformed, transformed);
    assert_eq!(
        object.name,
        if transformed {
            "Ral, Leyline Prodigy"
        } else {
            "Ral, Monsoon Mage"
        }
    );
    // CR 110.2a: the return is under the owner's control; a declined option
    // leaves the stolen creature under P1 until end of turn.
    assert_eq!(object.controller, if transformed { P0 } else { P1 });
    assert_eq!(
        run.runner.state().players[0].life,
        if won { 40 } else { 39 }
    );
    assert_eq!(run.runner.state().players[1].life, 40);
}

#[test]
fn ral_trigger_controller_returns_transformed_under_owner_after_control_change() {
    ral_control_change(true, true);
}

#[test]
fn ral_declined_return_leaves_stolen_front_face_under_new_controller() {
    ral_control_change(true, false);
}

#[test]
fn ral_loss_after_control_change_damages_the_trigger_controller() {
    ral_control_change(false, false);
}

/// Synthetic coverage for the accepted-optional-move result handoff. The
/// resolving ability is an attack trigger that declares the witness in
/// its root target slot, then flips a coin whose winning branch is the optional
/// `SelfRef` move. Its stashed dependent is the `ParentTarget` return, followed
/// by a separately printed tail. No printed-card claim is made for the
/// synthetic witness.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ParkedMoveTail {
    /// H2: an independent, object-free second instruction (+2 life).
    GainTwoLife,
    /// H4: an independent declared-slot instruction and its anaphoric tail,
    /// both naming the witness the root slot announced.
    SlotPumpAndDamage,
}

fn move_definition(
    destination: Zone,
    target: TargetFilter,
    transformed: bool,
) -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::ChangeZone {
            origin: None,
            destination,
            target,
            owner_library: false,
            enter_transformed: transformed,
            enters_under: None,
            enter_tapped: EtbTapState::Unspecified,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            face_down_profile: None,
            enters_modified_if: None,
        },
    )
}

/// R-F1: CR 608.2d + CR 400.7j: an untargeted subject is chosen while the
/// instruction resolves; the later return must not mistake the pre-choice
/// empty target list for a completed-empty move.
#[test]
fn synthetic_optional_resolution_time_move_returns_sole_eligible_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    fund(&mut scenario);
    let mut producer = move_definition(
        Zone::Exile,
        TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
        false,
    );
    producer.optional = true;
    producer.target_choice_timing = TargetChoiceTiming::Resolution;
    let mut dependent = move_definition(Zone::Battlefield, TargetFilter::ParentTarget, false);
    dependent.sub_link = SubAbilityLink::SequentialSibling;
    dependent.condition = Some(AbilityCondition::EffectOutcome {
        signal: EffectOutcomeSignal::OptionalEffectPerformed,
    });
    let mut flip = AbilityDefinition::new(AbilityKind::Spell, flip_to(producer));
    flip.sub_ability = Some(Box::new(dependent));
    let trigger = TriggerDefinition::new(TriggerMode::Attacks)
        .valid_card(TargetFilter::SelfRef)
        .execute(flip);
    let source = scenario
        .add_creature(P0, "Sole eligible resolution-time creature", 1, 3)
        .with_trigger_definition(trigger)
        .id();
    let mut run = Run::new(scenario);
    run.runner.advance_to_combat();
    run.act(GameAction::DeclareAttackers {
        attacks: vec![(source, AttackTarget::Player(P1))],
        bands: vec![],
    });
    pin(&mut run.runner, coin_seed(&[true]));
    run.until(|state| matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }));
    let incarnation = run.runner.state().objects[&source].incarnation;
    let start = run.events.len();
    run.act(GameAction::DecideOptionalEffect { accept: true });
    run.finish();
    run.assert_flips(&[(P0, true)]);
    assert_eq!(
        moves(&run.events[start..], source),
        vec![
            (Some(Zone::Battlefield), Zone::Exile),
            (Some(Zone::Exile), Zone::Battlefield),
        ],
        "the resolution-selected creature must be exiled AND returned"
    );
    assert_eq!(run.runner.state().objects[&source].zone, Zone::Battlefield);
    assert_ne!(run.runner.state().objects[&source].incarnation, incarnation);
}

/// R-F2: CR 115.1d + CR 608.2c: the immediate gated move names an announced
/// root slot, not the optional producer's result. The live-source row also
/// follows that witness with a later anaphoric move. The zero-card choice is
/// legal (CR 608.2d); neither row needs an impossible stale-source option.
fn optional_move_before_declared_slot_consumer(zero_cards: bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    fund(&mut scenario);
    let witness = scenario
        .add_creature(P0, "Declared move witness", 0, 10)
        .id();
    let mut producer = if zero_cards {
        let mut producer = move_definition(
            Zone::Exile,
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            false,
        );
        producer.target_choice_timing = TargetChoiceTiming::Resolution;
        producer.multi_target = Some(MultiTargetSpec::fixed(0, 0));
        producer
    } else {
        move_definition(Zone::Exile, TargetFilter::SelfRef, false)
    };
    producer.optional = true;
    let mut dependent = move_definition(
        Zone::Graveyard,
        TargetFilter::ParentTargetSlot { index: 0 },
        false,
    );
    dependent.sub_link = SubAbilityLink::SequentialSibling;
    dependent.condition = Some(AbilityCondition::EffectOutcome {
        signal: EffectOutcomeSignal::OptionalEffectPerformed,
    });
    if !zero_cards {
        let mut anaphoric = move_definition(Zone::Hand, TargetFilter::ParentTarget, false);
        anaphoric.sub_link = SubAbilityLink::SequentialSibling;
        dependent.sub_ability = Some(Box::new(anaphoric));
    }
    let mut flip = AbilityDefinition::new(AbilityKind::Spell, flip_to(producer));
    flip.sub_ability = Some(Box::new(dependent));
    let mut root = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Pump {
            power: PtValue::Fixed(0),
            toughness: PtValue::Fixed(0),
            target: TargetFilter::Typed(TypedFilter::creature()),
        },
    );
    root.sub_ability = Some(Box::new(if zero_cards {
        // A separately announced player slot keeps the optional branch free
        // of inherited OBJECT targets without altering the witness in slot 0.
        let mut player_step = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 0 },
                target: TargetFilter::Player,
            },
        );
        player_step.sub_ability = Some(Box::new(flip));
        player_step
    } else {
        flip
    }));
    let trigger = TriggerDefinition::new(TriggerMode::Attacks)
        .valid_card(TargetFilter::SelfRef)
        .execute(root);
    let source = scenario
        .add_creature(P0, "Optional producer before declared move", 1, 3)
        .with_trigger_definition(trigger)
        .id();
    let mut run = Run::new(scenario);
    run.runner.advance_to_combat();
    run.act(GameAction::DeclareAttackers {
        attacks: vec![(source, AttackTarget::Player(P1))],
        bands: vec![],
    });
    run.until(target_prompt_open);
    run.act(GameAction::ChooseTarget {
        target: Some(TargetRef::Object(witness)),
    });
    if zero_cards {
        assert!(target_prompt_open(run.runner.state()));
        run.act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P0)),
        });
    }
    run.until(|state| {
        matches!(state.waiting_for, WaitingFor::Priority { .. })
            && state.stack.iter().any(|entry| entry.source_id == source)
    });
    let declared = run
        .runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == source)
        .and_then(|entry| entry.ability())
        .expect("the trigger is on the stack");
    assert_eq!(declared.targets, vec![TargetRef::Object(witness)]);
    pin(&mut run.runner, coin_seed(&[true]));
    run.until(|state| matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }));
    let start = run.events.len();
    run.act(GameAction::DecideOptionalEffect { accept: true });
    run.finish();
    run.assert_flips(&[(P0, true)]);
    let expected_witness_moves = if zero_cards {
        vec![(Some(Zone::Battlefield), Zone::Graveyard)]
    } else {
        vec![
            (Some(Zone::Battlefield), Zone::Graveyard),
            (Some(Zone::Graveyard), Zone::Hand),
        ]
    };
    assert_eq!(
        moves(&run.events[start..], witness),
        expected_witness_moves,
        "the gated declared-slot move and any later anaphor must use the witness"
    );
    assert_eq!(
        run.runner.state().objects[&witness].zone,
        if zero_cards {
            Zone::Graveyard
        } else {
            Zone::Hand
        }
    );
    let expected_source_moves = if zero_cards {
        vec![]
    } else {
        vec![(Some(Zone::Battlefield), Zone::Exile)]
    };
    assert_eq!(moves(&run.events[start..], source), expected_source_moves);
    assert_eq!(
        run.runner.state().objects[&source].zone,
        if zero_cards {
            Zone::Battlefield
        } else {
            Zone::Exile
        }
    );
}

#[test]
fn synthetic_optional_zero_card_move_preserves_gated_declared_slot() {
    optional_move_before_declared_slot_consumer(true);
}

#[test]
fn synthetic_optional_self_move_preserves_gated_declared_slot_and_anaphor() {
    optional_move_before_declared_slot_consumer(false);
}

/// H2/H4 synthetic: the resolving attack trigger declares the witness in its
/// root target slot, then flips one coin. Its winning branch is the optional
/// `SelfRef` move; the stashed dependent is the `ParentTarget` return followed
/// by a separately printed tail. The dependent node therefore inherits the
/// witness as a stale referent — the accepted move's own result must supersede
/// it. No printed-card claim is made for the synthetic witness.
fn parked_optional_move(tail: ParkedMoveTail, live: bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    fund(&mut scenario);
    let witness = scenario
        .add_creature(P0, "Declared slot witness", 0, 10)
        .id();
    let mut win = move_definition(Zone::Exile, TargetFilter::SelfRef, false);
    win.optional = true;
    let tail_definition = match tail {
        ParkedMoveTail::GainTwoLife => {
            let mut tail = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 2 },
                    player: TargetFilter::Controller,
                },
            );
            tail.sub_link = SubAbilityLink::SequentialSibling;
            tail
        }
        ParkedMoveTail::SlotPumpAndDamage => {
            let mut anaphoric = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::ParentTargetSlot { index: 0 },
                    damage_source: None,
                    excess: None,
                },
            );
            anaphoric.sub_link = SubAbilityLink::SequentialSibling;
            let mut pump = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Pump {
                    power: PtValue::Fixed(1),
                    toughness: PtValue::Fixed(0),
                    target: TargetFilter::ParentTargetSlot { index: 0 },
                },
            );
            pump.sub_link = SubAbilityLink::SequentialSibling;
            pump.sub_ability = Some(Box::new(anaphoric));
            pump
        }
    };
    let mut dependent = move_definition(Zone::Battlefield, TargetFilter::ParentTarget, false);
    dependent.sub_link = SubAbilityLink::SequentialSibling;
    dependent.condition = Some(AbilityCondition::EffectOutcome {
        signal: EffectOutcomeSignal::OptionalEffectPerformed,
    });
    dependent.sub_ability = Some(Box::new(tail_definition));
    let mut flip = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::FlipCoins {
            flips: CoinFlipRun::Exactly {
                count: QuantityExpr::Fixed { value: 1 },
            },
            win_effect: Some(Box::new(win)),
            lose_effect: None,
            flipper: TargetFilter::Controller,
        },
    );
    flip.sub_ability = Some(Box::new(dependent));
    let mut root = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Pump {
            power: PtValue::Fixed(0),
            toughness: PtValue::Fixed(0),
            target: TargetFilter::Typed(TypedFilter::creature()),
        },
    );
    root.sub_ability = Some(Box::new(flip));
    let trigger = TriggerDefinition::new(TriggerMode::Attacks)
        .valid_card(TargetFilter::SelfRef)
        .execute(root);
    let source = scenario
        .add_creature(P0, "Synthetic optional move source", 1, 3)
        .with_trigger_definition(trigger)
        .id();
    let response = cloudshift(&mut scenario);
    let plains = scenario.add_basic_land(P0, ManaColor::White);

    let mut run = Run::new(scenario);
    let incarnation = run.runner.state().objects[&source].incarnation;
    run.runner.advance_to_combat();
    assert!(matches!(
        run.runner.state().waiting_for,
        WaitingFor::DeclareAttackers { player: P0, .. }
    ));
    run.act(GameAction::DeclareAttackers {
        attacks: vec![(source, AttackTarget::Player(P1))],
        bands: vec![],
    });
    // The trigger's declared root slot is announced publicly before it reaches
    // the stack; answer that prompt in whichever window the engine raises it.
    let mut announced = false;
    run.until(|state| {
        if target_prompt_open(state) {
            return true;
        }
        matches!(state.waiting_for, WaitingFor::Priority { .. })
            && state.stack.iter().any(|entry| entry.source_id == source)
    });
    if target_prompt_open(run.runner.state()) {
        run.act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(witness)),
        });
        announced = true;
        run.until(|state| {
            matches!(state.waiting_for, WaitingFor::Priority { .. })
                && state.stack.iter().any(|entry| entry.source_id == source)
        });
    }
    let announced_targets: Vec<_> = run
        .runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == source)
        .and_then(|entry| entry.ability())
        .map(|ability| ability.targets.clone())
        .unwrap_or_default();
    assert!(
        announced_targets.contains(&TargetRef::Object(witness)),
        "the trigger must name the witness in its declared root slot (announced={announced}), got {announced_targets:?}"
    );
    // Advancing phases emptied the setup pool; pay for the response with a real
    // basic-land mana activation in this priority window.
    let before = run.runner.state().players[0].mana_pool.total();
    run.act(GameAction::ActivateAbility {
        source_id: plains,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    });
    assert_eq!(run.runner.state().players[0].mana_pool.total(), before + 1);
    run.until(|state| {
        matches!(state.waiting_for, WaitingFor::Priority { .. })
            && state.stack.iter().any(|entry| entry.source_id == source)
    });
    assert_trigger(&run, source, P0);
    if !live {
        run.blink(response, source);
    }
    // The accepted move must not move the returned permanent; its baseline is
    // the incarnation after any test-setup blink.
    let baseline_incarnation = run.runner.state().objects[&source].incarnation;
    let start = run.events.len();
    pin(&mut run.runner, coin_seed(&[true]));
    run.until(|state| matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }));
    assert!(matches!(
        run.runner.state().waiting_for,
        WaitingFor::OptionalEffectChoice { player: P0, source_id, .. } if source_id == source
    ));
    run.act(GameAction::DecideOptionalEffect { accept: true });
    run.finish();
    run.assert_flips(&[(P0, true)]);

    let object = &run.runner.state().objects[&source];
    if live {
        // CR 400.7j: the accepted exile's own return finds the card it moved.
        assert_eq!(object.zone, Zone::Battlefield);
        assert_ne!(object.incarnation, incarnation);
        assert_eq!(
            moves(&run.events[start..], source),
            vec![
                (Some(Zone::Battlefield), Zone::Exile),
                (Some(Zone::Exile), Zone::Battlefield),
            ]
        );
    } else {
        assert_eq!(object.zone, Zone::Battlefield);
        assert_eq!(object.incarnation, baseline_incarnation);
        assert!(moves(&run.events[start..], source).is_empty());
    }
    let untouched = &run.runner.state().objects[&witness];
    match tail {
        ParkedMoveTail::GainTwoLife => {
            assert_eq!(run.runner.state().players[0].life, 42);
            assert_eq!(untouched.power, Some(0));
            assert_eq!(untouched.damage_marked, 0);
        }
        ParkedMoveTail::SlotPumpAndDamage => {
            // Exactly once: the dependent return consumed the move result and
            // the declared-slot instruction kept its own authority.
            assert_eq!(untouched.power, Some(1));
            assert_eq!(untouched.damage_marked, 1);
            assert_eq!(run.runner.state().players[0].life, 40);
        }
    }
}

#[test]
fn synthetic_accepted_move_tail_resolves_for_live_source() {
    parked_optional_move(ParkedMoveTail::GainTwoLife, true);
}

#[test]
fn synthetic_accepted_empty_move_keeps_independent_tail() {
    parked_optional_move(ParkedMoveTail::GainTwoLife, false);
}

#[test]
fn synthetic_declared_slot_tail_survives_live_move_result() {
    parked_optional_move(ParkedMoveTail::SlotPumpAndDamage, true);
}

#[test]
fn synthetic_declared_slot_tail_survives_empty_move_result() {
    parked_optional_move(ParkedMoveTail::SlotPumpAndDamage, false);
}

/// CR 614.1a + CR 614.6: an `instead` replacement substitutes exile for the
/// original graveyard-entry event, installed on an otherwise inert battlefield
/// permanent. It makes the accepted move's *delivery* ineligible for the
/// normal-path binder, which is the branch this fixture feeds.
fn graveyard_to_exile_redirect() -> ReplacementDefinition {
    ReplacementDefinition::new(ReplacementEvent::Moved)
        .destination_zone(Zone::Graveyard)
        .execute(AbilityDefinition::new(
            AbilityKind::Spell,
            *move_definition(Zone::Exile, TargetFilter::SelfRef, false).effect,
        ))
        .description("If a card would be put into a graveyard, exile it instead.".to_string())
}

/// M7 (test strength): an accepted optional public-zone move whose delivery is
/// REPLACED is not this producer's own completed result, so the parked
/// dependent return must keep its pre-existing route rather than be bound to
/// the requested destination or silently replaced by a `NoOp`.
///
/// CR 614.6 + CR 616.1: the move still happens, but to the redirected
/// destination. That is precisely why `publish_accepted_optional_move_result`
/// rejects a `ReplacementApplied` delivery: the binder's own-original-delivery
/// rule cannot claim it.
fn replaced_optional_move_delivery(redirect: bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    fund(&mut scenario);
    let host = redirect.then(|| scenario.add_creature(P0, "Redirect host", 0, 1).id());
    let mut win = move_definition(Zone::Graveyard, TargetFilter::SelfRef, false);
    win.optional = true;
    let mut dependent = move_definition(Zone::Battlefield, TargetFilter::ParentTarget, false);
    dependent.sub_link = SubAbilityLink::SequentialSibling;
    dependent.condition = Some(AbilityCondition::EffectOutcome {
        signal: EffectOutcomeSignal::OptionalEffectPerformed,
    });
    let mut flip = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::FlipCoins {
            flips: CoinFlipRun::Exactly {
                count: QuantityExpr::Fixed { value: 1 },
            },
            win_effect: Some(Box::new(win)),
            lose_effect: None,
            flipper: TargetFilter::Controller,
        },
    );
    flip.sub_ability = Some(Box::new(dependent));
    let trigger = TriggerDefinition::new(TriggerMode::Attacks)
        .valid_card(TargetFilter::SelfRef)
        .execute(flip);
    let source = scenario
        .add_creature(P0, "Synthetic replaced-move source", 1, 3)
        .with_trigger_definition(trigger)
        .id();

    let mut run = Run::new(scenario);
    if let Some(host) = host {
        let object = run
            .runner
            .state_mut()
            .objects
            .get_mut(&host)
            .expect("redirect host is on the battlefield");
        let redirect = graveyard_to_exile_redirect();
        // The layer pass reseeds the live list from the printed base, so both
        // must carry the redirect.
        object.replacement_definitions.push(redirect.clone());
        std::sync::Arc::make_mut(&mut object.base_replacement_definitions).push(redirect);
    }
    run.runner.advance_to_combat();
    run.act(GameAction::DeclareAttackers {
        attacks: vec![(source, AttackTarget::Player(P1))],
        bands: vec![],
    });
    run.until(|state| {
        matches!(state.waiting_for, WaitingFor::Priority { .. })
            && state.stack.iter().any(|entry| entry.source_id == source)
    });
    assert_trigger(&run, source, P0);
    let start = run.events.len();
    pin(&mut run.runner, coin_seed(&[true]));
    run.until(|state| matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }));
    run.act(GameAction::DecideOptionalEffect { accept: true });
    run.finish();
    run.assert_flips(&[(P0, true)]);

    let replaced = run.events[start..]
        .iter()
        .any(|event| matches!(event, GameEvent::ReplacementApplied { .. }));
    assert_eq!(
        replaced, redirect,
        "the redirect fixture must actually replace the delivery"
    );
    // Both routes leave the card on the battlefield, but only the unredirected
    // row is the binder's own-result path; the redirected row is the
    // pre-existing continuation route, which must still run (a wrongly-enabled
    // binder treats the redirected delivery as empty and `NoOp`s
    // the dependent return, stranding the card in exile).
    let object = &run.runner.state().objects[&source];
    assert_eq!(
        object.zone,
        Zone::Battlefield,
        "the dependent return must still run for a replaced delivery"
    );
    let expected = if redirect {
        vec![
            (Some(Zone::Battlefield), Zone::Exile),
            (Some(Zone::Exile), Zone::Battlefield),
        ]
    } else {
        vec![
            (Some(Zone::Battlefield), Zone::Graveyard),
            (Some(Zone::Graveyard), Zone::Battlefield),
        ]
    };
    assert_eq!(moves(&run.events[start..], source), expected);
}

#[test]
fn synthetic_replaced_optional_move_keeps_the_pre_existing_route() {
    replaced_optional_move_delivery(true);
}

#[test]
fn synthetic_completed_optional_move_binds_its_own_delivery() {
    replaced_optional_move_delivery(false);
}

/// M6 (test strength): the accepted optional move's destination decides whether
/// its own result may bind the parked dependent return. `Exile` is in scope;
/// `Hand` is not.
///
/// The dependent clause inherits the declared-slots WITNESS as its stale
/// referent, while the producer moves the ability SOURCE. The two routes then
/// reach different objects: an in-scope producer replaces the stale referent
/// with the object it moved, an out-of-scope producer leaves the pre-existing
/// route alone. Both objects' move logs distinguish the outcomes.
fn optional_move_destination_scope(producer_destination: Zone) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    fund(&mut scenario);
    let witness = scenario
        .add_creature(P0, "Declared slot witness", 0, 10)
        .id();
    let mut win = move_definition(producer_destination, TargetFilter::SelfRef, false);
    win.optional = true;
    // "If you do, put it into its owner's graveyard" — the anaphoric follower.
    let mut dependent = move_definition(Zone::Graveyard, TargetFilter::ParentTarget, false);
    dependent.sub_link = SubAbilityLink::SequentialSibling;
    dependent.condition = Some(AbilityCondition::EffectOutcome {
        signal: EffectOutcomeSignal::OptionalEffectPerformed,
    });
    let mut flip = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::FlipCoins {
            flips: CoinFlipRun::Exactly {
                count: QuantityExpr::Fixed { value: 1 },
            },
            win_effect: Some(Box::new(win)),
            lose_effect: None,
            flipper: TargetFilter::Controller,
        },
    );
    flip.sub_ability = Some(Box::new(dependent));
    let mut root = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Pump {
            power: PtValue::Fixed(0),
            toughness: PtValue::Fixed(0),
            target: TargetFilter::Typed(TypedFilter::creature()),
        },
    );
    root.sub_ability = Some(Box::new(flip));
    let trigger = TriggerDefinition::new(TriggerMode::Attacks)
        .valid_card(TargetFilter::SelfRef)
        .execute(root);
    let source = scenario
        .add_creature(P0, "Synthetic scoped-move source", 1, 3)
        .with_trigger_definition(trigger)
        .id();

    let mut run = Run::new(scenario);
    run.runner.advance_to_combat();
    run.act(GameAction::DeclareAttackers {
        attacks: vec![(source, AttackTarget::Player(P1))],
        bands: vec![],
    });
    // The trigger's declared root slot is announced publicly before it reaches
    // the stack; answer that prompt in whichever window the engine raises it.
    let mut announced = false;
    run.until(|state| {
        if target_prompt_open(state) {
            return true;
        }
        matches!(state.waiting_for, WaitingFor::Priority { .. })
            && state.stack.iter().any(|entry| entry.source_id == source)
    });
    if target_prompt_open(run.runner.state()) {
        run.act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(witness)),
        });
        announced = true;
        run.until(|state| {
            matches!(state.waiting_for, WaitingFor::Priority { .. })
                && state.stack.iter().any(|entry| entry.source_id == source)
        });
    }
    assert!(
        announced,
        "the witness must be announced into the root slot before the move"
    );
    let start = run.events.len();
    pin(&mut run.runner, coin_seed(&[true]));
    run.until(|state| matches!(state.waiting_for, WaitingFor::OptionalEffectChoice { .. }));
    run.act(GameAction::DecideOptionalEffect { accept: true });
    run.finish();
    run.assert_flips(&[(P0, true)]);

    let witness_object = &run.runner.state().objects[&witness];
    let source_object = &run.runner.state().objects[&source];
    if producer_destination == Zone::Exile {
        // Positive control: the in-scope producer binds ITS OWN moved source,
        // superseding the stale inherited witness.
        assert_eq!(
            moves(&run.events[start..], source),
            vec![
                (Some(Zone::Battlefield), Zone::Exile),
                (Some(Zone::Exile), Zone::Graveyard),
            ]
        );
        assert_eq!(source_object.zone, Zone::Graveyard);
        assert_eq!(witness_object.zone, Zone::Battlefield);
        assert!(moves(&run.events[start..], witness).is_empty());
    } else {
        // Negative: `Hand` is outside the binder's scope, so the accepted move
        // never binds.
        assert_eq!(
            moves(&run.events[start..], source),
            vec![(Some(Zone::Battlefield), Zone::Hand)]
        );
        assert_eq!(source_object.zone, Zone::Hand);
    }
}

#[test]
fn synthetic_out_of_scope_optional_move_keeps_the_pre_existing_route() {
    optional_move_destination_scope(Zone::Hand);
}

#[test]
fn synthetic_exile_scoped_optional_move_binds_its_result() {
    optional_move_destination_scope(Zone::Exile);
}

fn declare_attack(run: &mut Run, source: ObjectId, response_land: ObjectId) {
    run.runner.advance_to_combat();
    assert!(matches!(
        run.runner.state().waiting_for,
        WaitingFor::DeclareAttackers { player: P0, .. }
    ));
    run.act(GameAction::DeclareAttackers {
        attacks: vec![(source, AttackTarget::Player(P1))],
        bands: vec![],
    });
    run.until(|state| {
        matches!(state.waiting_for, WaitingFor::Priority { .. })
            && state.stack.iter().any(|entry| entry.source_id == source)
    });
    assert_trigger(run, source, P0);
    // Advancing phases emptied the setup pool. Pay for the response using
    // a real basic-land mana activation in this priority window.
    let before = run.runner.state().players[0].mana_pool.total();
    run.act(GameAction::ActivateAbility {
        source_id: response_land,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    });
    assert_eq!(run.runner.state().players[0].mana_pool.total(), before + 1);
    assert_trigger(run, source, P0);
}

fn rogue_roll(blink: bool, result: u8) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    fund(&mut scenario);
    let rogue = scenario
        .add_creature_from_oracle(P0, "Lightfoot Rogue", 2, 1, ROGUE)
        .id();
    let response = cloudshift(&mut scenario);
    let plains = scenario.add_basic_land(P0, ManaColor::White);
    let mut run = Run::new(scenario);
    declare_attack(&mut run, rogue, plains);
    if blink {
        run.blink(response, rogue);
    }
    pin(&mut run.runner, die_seed(result));
    run.finish();
    run.assert_die(P0, result);
    // CR 706.3b + CR 400.7: the printed table is part of the old attack
    // ability, not a grant to the unrelated returned creature.
    let bonus = if blink || result < 10 {
        0
    } else if result < 20 {
        1
    } else {
        3
    };
    assert_eq!(run.runner.state().objects[&rogue].power, Some(2 + bonus));
    assert_eq!(run.runner.state().objects[&rogue].toughness, Some(1));
    assert_eq!(
        object_has_effective_keyword_kind(run.runner.state(), rogue, KeywordKind::Deathtouch),
        !blink
    );
    assert_eq!(
        object_has_effective_keyword_kind(run.runner.state(), rogue, KeywordKind::FirstStrike),
        !blink && result == 20
    );
    assert_eq!(run.runner.state().objects[&rogue].zone, Zone::Battlefield);
}

#[test]
fn lightfoot_rogue_current_rows_grant_the_printed_stats_and_keywords() {
    for result in [1, 10, 20] {
        rogue_roll(false, result);
    }
}

#[test]
fn lightfoot_rogue_old_rows_do_not_buff_returned_source() {
    for result in [1, 10, 20] {
        rogue_roll(true, result);
    }
}

#[test]
fn nothic_departed_source_draws_and_loses_life_for_its_trigger_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    fund(&mut scenario);
    let nothic = scenario
        .add_creature_from_oracle(P0, "Nothic", 4, 3, NOTHIC)
        .id();
    let kill = murder(&mut scenario);
    let cremate = instant(
        &mut scenario,
        P0,
        "Cremate",
        CREMATE,
        cost(0, &[ManaCostShard::Black]),
    );
    let mut run = Run::new(scenario);
    // Initial fixture: P0 controls P1's Nothic. Death puts it in its owner's
    // graveyard but does not transfer control of the triggered ability.
    run.runner
        .state_mut()
        .objects
        .get_mut(&nothic)
        .unwrap()
        .owner = P1;
    run.commit_at(kill, nothic);
    run.spell_only(kill);
    assert_eq!(run.runner.state().objects[&nothic].zone, Zone::Graveyard);
    assert!(run.runner.state().players[1].graveyard.contains(&nothic));
    assert_eq!(run.runner.state().objects[&nothic].controller, P1);
    assert_trigger(&run, nothic, P0);
    run.commit_at(cremate, nothic);
    run.spell_only(cremate);
    assert_eq!(run.runner.state().objects[&nothic].zone, Zone::Exile);
    let hand = [
        run.runner.state().players[0].hand.len(),
        run.runner.state().players[1].hand.len(),
    ];
    pin(&mut run.runner, die_seed(10));
    run.finish();
    run.assert_die(P0, 10);
    // CR 603.3a + CR 603.10a: the former controller, not the source's owner
    // or its graveyard object, draws two and loses two for this printed row.
    assert_eq!(run.runner.state().players[0].hand.len(), hand[0] + 2);
    assert_eq!(run.runner.state().players[1].hand.len(), hand[1]);
    assert_eq!(run.runner.state().players[0].life, 38);
    assert_eq!(run.runner.state().players[1].life, 40);
}

fn self_move(origin: Zone, destination: Zone) -> Effect {
    Effect::ChangeZone {
        origin: Some(origin),
        destination,
        target: TargetFilter::SelfRef,
        owner_library: false,
        enter_transformed: false,
        enters_under: None,
        enter_tapped: EtbTapState::Unspecified,
        enters_attacking: false,
        up_to: false,
        enter_with_counters: vec![],
        conditional_enter_with_counters: vec![],
        face_down_profile: None,
        enters_modified_if: None,
    }
}

fn flip_to(branch: AbilityDefinition) -> Effect {
    Effect::FlipCoins {
        flips: CoinFlipRun::Exactly {
            count: QuantityExpr::Fixed { value: 1 },
        },
        win_effect: Some(Box::new(branch)),
        lose_effect: None,
        flipper: TargetFilter::Controller,
    }
}

#[test]
fn synthetic_inline_dies_branch_finds_successor_and_its_own_second_move() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    fund(&mut scenario);
    let mut branch = AbilityDefinition::new(
        AbilityKind::Spell,
        self_move(Zone::Graveyard, Zone::Battlefield),
    );
    branch.sub_ability = Some(Box::new(AbilityDefinition::new(
        AbilityKind::Spell,
        self_move(Zone::Battlefield, Zone::Hand),
    )));
    let trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
        .valid_card(TargetFilter::SelfRef)
        .origin(Zone::Battlefield)
        .destination(Zone::Graveyard)
        .trigger_zones(vec![Zone::Battlefield])
        .execute(AbilityDefinition::new(AbilityKind::Spell, flip_to(branch)));
    let source = scenario
        .add_creature(P0, "Synthetic successor witness", 2, 2)
        .with_trigger_definition(trigger)
        .id();
    let kill = murder(&mut scenario);
    let mut run = Run::new(scenario);
    let a = run.runner.state().objects[&source].incarnation;
    run.commit_at(kill, source);
    run.spell_only(kill);
    assert_eq!(run.runner.state().objects[&source].zone, Zone::Graveyard);
    assert_ne!(run.runner.state().objects[&source].incarnation, a);
    assert_trigger(&run, source, P0);
    let StackEntryKind::TriggeredAbility { ability, .. } = &run
        .runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == source)
        .unwrap()
        .kind
    else {
        unreachable!("the source has a triggered carrier");
    };
    // Reach guards for the normal producer, not a synthetic hostile stamp.
    assert_eq!(ability.source_incarnation, Some(a));
    assert_eq!(ability.trigger_source_incarnation(), Some(a));
    let start = run.events.len();
    pin(&mut run.runner, coin_seed(&[true]));
    run.finish();
    run.assert_flips(&[(P0, true)]);
    // CR 400.7e: the dies trigger finds its immediate public-zone successor.
    // CR 400.7j: the following instruction finds the object it just moved.
    assert_eq!(
        moves(&run.events[start..], source),
        vec![
            (Some(Zone::Graveyard), Zone::Battlefield),
            (Some(Zone::Battlefield), Zone::Hand),
        ]
    );
    assert_eq!(run.runner.state().objects[&source].zone, Zone::Hand);
}

#[test]
fn synthetic_scoped_actor_survives_identity_transfer_without_borrowing_foreign_history() {
    // Synthetic seam coverage only: no supported-card claim is made for a
    // resolver deliberately invoked without its own stack carrier.
    for carrier_kind in ["absent", "foreign", "matching"] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let source = scenario
            .add_creature(P0, "Synthetic branch source", 2, 2)
            .id();
        let foreign = scenario.add_creature(P0, "Foreign carrier", 3, 3).id();
        let mut run = Run::new(scenario);
        let source_context = trigger_source_context_for_latch(
            run.runner.state(),
            &run.runner.state().objects[&source],
        );
        let foreign_context = trigger_source_context_for_latch(
            run.runner.state(),
            &run.runner.state().objects[&foreign],
        );
        zones::move_to_zone(run.runner.state_mut(), source, Zone::Exile, &mut run.events);
        zones::move_to_zone(
            run.runner.state_mut(),
            source,
            Zone::Battlefield,
            &mut run.events,
        );
        assert_ne!(
            run.runner.state().objects[&source].incarnation,
            source_context.identity.reference.incarnation
        );
        if carrier_kind != "absent" {
            let context = if carrier_kind == "foreign" {
                foreign_context
            } else {
                source_context
            };
            let carrier_source = context.identity.reference.object_id;
            let mut carrier = ResolvedAbility::new(Effect::NoOp, vec![], carrier_source, P0);
            carrier.set_trigger_source_recursive(context);
            run.runner.state_mut().resolving_stack_entry = Some(StackEntry {
                id: ObjectId(900),
                source_id: carrier_source,
                controller: P0,
                kind: StackEntryKind::TriggeredAbility {
                    source_id: carrier_source,
                    ability: Box::new(carrier),
                    condition: None,
                    trigger_event: None,
                    description: None,
                    source_name: String::new(),
                    subject_match_count: None,
                    die_result: None,
                    provenance: None,
                },
            });
        }
        let mut branch = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: TargetFilter::Controller,
            },
        );
        branch.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            self_move(Zone::Battlefield, Zone::Hand),
        )));
        // Represents a player-scope iteration: its actor is P1 even though
        // the triggering source's captured controller remains P0.
        let mut flip = ResolvedAbility::new(flip_to(branch), vec![], source, P1);
        flip.set_scoped_player_recursive(P1);
        pin(&mut run.runner, coin_seed(&[true]));
        flip_coin::resolve(run.runner.state_mut(), &flip, &mut run.events).unwrap();
        run.assert_flips(&[(P1, true)]);
        assert_eq!(run.runner.state().players[0].life, 20);
        assert_eq!(run.runner.state().players[1].life, 23);
        assert_eq!(
            run.runner.state().objects[&source].zone,
            if carrier_kind == "matching" {
                Zone::Battlefield
            } else {
                Zone::Hand
            },
            "only the matching carrier supplies stale-source authority"
        );
        run.runner.state_mut().resolving_stack_entry = None;
        run.finish();
    }
}

fn characteristic_branch(blink: bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    fund(&mut scenario);
    let branch = AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::GainLife {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Source,
                },
            },
            player: TargetFilter::Controller,
        },
    );
    let trigger = TriggerDefinition::new(TriggerMode::Attacks)
        .valid_card(TargetFilter::SelfRef)
        .execute(AbilityDefinition::new(AbilityKind::Spell, flip_to(branch)));
    let source = scenario
        .add_creature(P0, "Synthetic power reader", 2, 8)
        .with_plus_counters(3)
        .with_trigger_definition(trigger)
        .id();
    let response = cloudshift(&mut scenario);
    let land = scenario.add_basic_land(
        P0,
        if blink {
            ManaColor::White
        } else {
            ManaColor::Green
        },
    );
    let growth = instant(
        &mut scenario,
        P0,
        "Giant Growth",
        GROWTH,
        cost(0, &[ManaCostShard::Green]),
    );
    let mut run = Run::new(scenario);
    assert_eq!(run.runner.state().objects[&source].power, Some(5));
    declare_attack(&mut run, source, land);
    if blink {
        run.blink(response, source);
        assert_eq!(run.runner.state().objects[&source].power, Some(2));
    } else {
        run.commit_at(growth, source);
        run.spell_only(growth);
        assert_eq!(run.runner.state().objects[&source].power, Some(8));
    }
    pin(&mut run.runner, coin_seed(&[true]));
    run.finish();
    run.assert_flips(&[(P0, true)]);
    // CR 608.2h: exact-live reads use current power (8); after departure,
    // captured power (5) wins over the distinguishable same-id return (2).
    assert_eq!(
        run.runner.state().players[0].life,
        if blink { 45 } else { 48 }
    );
    assert_eq!(run.runner.state().players[1].life, 40);
}

#[test]
fn synthetic_exact_live_result_branch_reads_changed_source_power() {
    characteristic_branch(false);
}

#[test]
fn synthetic_departed_result_branch_reads_captured_source_power() {
    characteristic_branch(true);
}
