//! Phase 7: a declined "if you do" gate skips only the instructions it governs.
//!
//! CR 608.2c: the controller of a resolving spell or ability follows its
//! instructions in the order written. CR 118.12: an "if you do" clause checks
//! whether the player chose to take the optional action. When that action is
//! declined, the gate governs its own resolution steps and every later
//! instruction that names something only the gated action produced. A later
//! instruction whose referents exist whether or not the action happened still
//! happens.
//!
//! Every card below is built from its verbatim Oracle text (reminder text
//! included). Every target is chosen with `GameAction::ChooseTarget`, and each
//! row asserts that the engine accepted the choice. The synthetic boards
//! ("SynJ3", "SynGap1", …) carry text no printed card has; they drive shapes the
//! corpus does not reach.

use engine::game::combat::{build_declare_attackers_waiting_for, AttackTarget};
use engine::game::game_object::BackFaceData;
use engine::game::keywords::has_keyword;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::specialize::SpecializeFaceMap;
use engine::game::static_abilities::{check_static_ability, StaticCheckContext};
use engine::game::triggers::drain_order_triggers_with_identity;
use engine::types::ability::{ContinuousModification, EffectKind, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const NEYITH: &str = "Whenever one or more creatures you control fight or become blocked, draw a card.\nAt the beginning of combat on your turn, you may pay {2}{R/G}. If you do, double target creature's power until end of turn. That creature must be blocked this combat if able. ({R/G} can be paid with either {R} or {G}.)";
const LORTHOS: &str = "Whenever Lorthos attacks, you may pay {8}. If you do, tap up to eight target permanents. Those permanents don't untap during their controllers' next untap steps.";
const LOCALIZED_DESTRUCTION: &str = "You get {E} (an energy counter), then you may pay one or more {E}. If you do, each creature you control with power equal to the amount of {E} paid this way gains indestructible until end of turn.\nDestroy all creatures.";
const WITCHS_MARK: &str = "You may discard a card. If you do, draw two cards.\nCreate a Wicked Role token attached to up to one target creature you control. (If you control another Role on it, put that one into the graveyard. Enchanted creature gets +1/+1. When this token is put into a graveyard, each opponent loses 1 life.)";
const WYLL_OF_THE_BLADE_PACT: &str = "When this creature specializes, you may sacrifice another creature or an artifact. If you do, untap Wyll of the Blade Pact. After this main phase, there is an additional combat phase followed by an additional main phase.";
const CHOICE_OF_FORTUNES: &str = "Seek two cards. You may shuffle them into your library. If you do, seek two cards.\nYou have no maximum hand size for the rest of the game.";
const KRENKO_BARON_OF_TIN_STREET: &str = "Haste\n{T}, Sacrifice an artifact: Put a +1/+1 counter on each Goblin you control.\nWhenever an artifact is put into a graveyard from the battlefield, you may pay {R}. If you do, create a 1/1 red Goblin creature token. It gains haste until end of turn.";
const KALASTRIA_HIGHBORN: &str = "Whenever this creature or another Vampire you control dies, you may pay {B}. If you do, target player loses 2 life and you gain 2 life.";
const HELLKITE_CHARGER: &str = "Flying, haste\nWhenever this creature attacks, you may pay {5}{R}{R}. If you do, untap all attacking creatures and after this phase, there is an additional combat phase.";
const HOLLOW_SPECTER: &str = "Flying\nWhenever this creature deals combat damage to a player, you may pay {X}. If you do, that player reveals X cards from their hand and you choose one of them. That player discards that card.";
const ARDENT_DUSTSPEAKER: &str = "Whenever this creature attacks, you may put an instant or sorcery card from your graveyard on the bottom of your library. If you do, exile the top two cards of your library. You may play those cards this turn.";
const KHERU_LICH_LORD: &str = "At the beginning of your upkeep, you may pay {2}{B}. If you do, return a creature card at random from your graveyard to the battlefield. It gains flying, trample, and haste. Exile that card at the beginning of your next end step. If it would leave the battlefield, exile it instead of putting it anywhere else.";
const LOYAL_UNICORN: &str = "Vigilance\nLieutenant — At the beginning of combat on your turn, if you control your commander, prevent all combat damage that would be dealt to creatures you control this turn. Other creatures you control gain vigilance until end of turn.";
const IROH_TEA_MASTER: &str = "When Iroh enters, create a Food token.\nAt the beginning of combat on your turn, you may have target opponent gain control of target permanent you control. When you do, create a 1/1 white Ally creature token. Put a +1/+1 counter on that token for each permanent you own that your opponents control.";

/// (i) completes the gate's sentence, (ii) names the declared target, (iv) names
/// only a player.
const SYN_J3: &str = "At the beginning of combat on your turn, you may pay {2}. If you do, double target creature's power until end of turn. That creature must be blocked this combat if able. You gain 2 life.";
/// (i) the token is the gate's own step, (iv) "You gain 1 life" names only a
/// player. (iii) "that token" names what only the gated action creates, but the
/// synthetic parse binds it to `ParentTarget` (measured), so on the declined
/// path (iii) is refused as an unaudited `PutCounter` shape.
const SYN_GAP1: &str = "At the beginning of combat on your turn, you may pay {2}. If you do, create a 1/1 white Ally creature token. You gain 1 life. Put a +1/+1 counter on that token.";
/// The Krenko shape: "It" is the gated token.
const SYN_TOKEN_IT: &str = "At the beginning of combat on your turn, you may pay {2}. If you do, create a 1/1 red Goblin creature token. It gains haste until end of turn.";
/// A gate that moves its declared target (CR 400.7).
const SYN_REANIMATE_THAT: &str = "At the beginning of combat on your turn, you may pay {2}. If you do, return target creature card from your graveyard to the battlefield. That creature must be blocked this combat if able.";
const SYN_EXILE_THAT: &str = "At the beginning of combat on your turn, you may pay {2}. If you do, exile target creature. That creature must be blocked this combat if able.";
/// A kept producer and its rider: "It" names the Goblin the ungated sentence
/// creates.
const SYN_RIDER: &str = "At the beginning of combat on your turn, you may pay {2}. If you do, draw a card. Create a 1/1 red Goblin creature token. It gains haste until end of turn.";

fn mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

fn grant_priority(runner: &mut GameRunner, player: PlayerId) {
    let state = runner.state_mut();
    state.priority_player = player;
    state.waiting_for = WaitingFor::Priority { player };
}

/// Drives the game until the stack is empty. Targets are chosen in `wants`
/// order (then none), the optional action is answered `accept`, and `pay` is
/// added to the deciding player's pool when the action is accepted. Every
/// submitted action must be accepted.
fn drive(
    runner: &mut GameRunner,
    wants: &[TargetRef],
    accept: bool,
    pay: Vec<ManaUnit>,
) -> Vec<GameEvent> {
    let mut events = Vec::new();
    let mut wants = wants.iter().cloned();
    let mut pay = Some(pay);
    let mut offered = false;
    for _ in 0..200 {
        let result = match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
                continue;
            }
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                runner.act(GameAction::ChooseTarget {
                    target: wants.next(),
                })
            }
            WaitingFor::OptionalEffectChoice { player, .. } => {
                offered = true;
                if accept {
                    for unit in pay.take().unwrap_or_default() {
                        runner
                            .state_mut()
                            .add_mana_to_pool(player, unit)
                            .expect("mana is added");
                    }
                }
                runner.act(GameAction::DecideOptionalEffect { accept })
            }
            WaitingFor::PayAmountChoice { min, max, .. } => {
                offered = true;
                runner.act(GameAction::SubmitPayAmount {
                    amount: if accept { max } else { min },
                })
            }
            WaitingFor::DeclareBlockers { .. } => runner.act(GameAction::DeclareBlockers {
                assignments: vec![],
            }),
            WaitingFor::DiscardChoice { count, cards, .. } => runner.act(GameAction::SelectCards {
                cards: cards.into_iter().take(count).collect(),
            }),
            // Until the optional action is offered, an empty stack only means
            // the trigger has not happened yet (combat damage, a death).
            WaitingFor::Priority { .. } if offered && runner.state().stack.is_empty() => {
                return events;
            }
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
            other => panic!("unexpected wait {other:?}"),
        };
        events.extend(result.expect("the action is accepted").events);
    }
    panic!("the game did not settle (offered: {offered})");
}

fn resolved(events: &[GameEvent]) -> Vec<EffectKind> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::EffectResolved { kind, .. } => Some(*kind),
            _ => None,
        })
        .collect()
}

/// The objects each transient continuous effect granting `mode` applies to.
fn static_mode_recipients(runner: &GameRunner, mode: &StaticMode) -> Vec<ObjectId> {
    runner
        .state()
        .transient_continuous_effects
        .iter()
        .filter(|tce| {
            tce.modifications.iter().any(|modification| {
                matches!(modification, ContinuousModification::AddStaticMode { mode: m } if m == mode)
            })
        })
        .map(|tce| match tce.affected {
            TargetFilter::SpecificObject { id } => id,
            ref other => panic!("unexpected affected filter {other:?}"),
        })
        .collect()
}

fn power(runner: &GameRunner, id: ObjectId) -> Option<i32> {
    runner.state().objects[&id].power
}

fn has_kw(runner: &mut GameRunner, id: ObjectId, keyword: &Keyword) -> bool {
    runner.state_mut().layers_dirty.mark_full();
    evaluate_layers(runner.state_mut());
    has_keyword(&runner.state().objects[&id], keyword)
}

fn tokens_named(runner: &GameRunner, name: &str) -> Vec<ObjectId> {
    runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let obj = &runner.state().objects[id];
            obj.is_token && obj.name == name
        })
        .collect()
}

fn attack_with(runner: &mut GameRunner, attacker: ObjectId) {
    runner.state_mut().waiting_for = build_declare_attackers_waiting_for(runner.state());
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("the attack is accepted");
}

/// Two creatures for P0 (`c1`, `c2`), the synthetic trigger's source, and a
/// two-card library; the trigger fires at the beginning of combat and targets
/// `c2` when it has a target.
struct CombatBoard {
    runner: GameRunner,
    c1: ObjectId,
    c2: ObjectId,
    events: Vec<GameEvent>,
}

fn combat_board(name: &str, text: &str, accept: bool) -> CombatBoard {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, name, 3, 3, text);
    let c1 = scenario.add_creature(P0, "c1", 2, 2).id();
    let c2 = scenario.add_creature(P0, "c2", 2, 2).id();
    scenario.with_library_top(P0, &["L1", "L2"]);
    let mut runner = scenario.build();
    runner.advance_to_phase(Phase::BeginCombat);
    let events = drive(
        &mut runner,
        &[TargetRef::Object(c2)],
        accept,
        mana(3, ManaType::Green),
    );
    CombatBoard {
        runner,
        c1,
        c2,
        events,
    }
}

fn neyith(accept: bool) -> CombatBoard {
    combat_board("Neyith of the Dire Hunt", NEYITH, accept)
}

/// J-2a: declined, the declared creature still must be blocked (CR 509.1c),
/// its power is not doubled, and no other creature is affected.
#[test]
fn neyith_declined_keeps_must_be_blocked_on_the_declared_creature() {
    let board = neyith(false);
    let must_be_blocked = StaticMode::MustBeBlocked { by: None };
    assert_eq!(
        static_mode_recipients(&board.runner, &must_be_blocked),
        vec![board.c2],
        "only the declared creature must be blocked"
    );
    assert_eq!(
        power(&board.runner, board.c2),
        Some(2),
        "power is not doubled"
    );
    assert_eq!(power(&board.runner, board.c1), Some(2));
    assert_eq!(resolved(&board.events), vec![EffectKind::GenericEffect]);
}

/// J-2a′: paid, the declared creature's power doubles and it must be blocked.
/// GREEN AT BASE: reach guard of J-2a; the paid path never reaches the
/// declined walk, so no in-scope mutation reddens it (preservation only).
#[test]
fn neyith_paid_doubles_power_and_keeps_must_be_blocked() {
    let board = neyith(true);
    let must_be_blocked = StaticMode::MustBeBlocked { by: None };
    assert_eq!(
        static_mode_recipients(&board.runner, &must_be_blocked),
        vec![board.c2]
    );
    assert_eq!(power(&board.runner, board.c2), Some(4));
    assert_eq!(power(&board.runner, board.c1), Some(2));
}

/// J-2b: declined, nothing is tapped by the ability, and the declared (already
/// tapped) permanent does not untap during its controller's next untap step,
/// while an undeclared tapped permanent does.
#[test]
fn lorthos_declined_declared_permanent_does_not_untap() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let lorthos = scenario
        .add_creature_from_oracle(P0, "Lorthos, the Tidemaker", 8, 8, LORTHOS)
        .id();
    let o1 = scenario.add_creature(P1, "o1", 2, 2).id();
    let o2 = scenario.add_creature(P1, "o2", 2, 2).id();
    let mut runner = scenario.build();
    for id in [o1, o2] {
        runner.state_mut().objects.get_mut(&id).unwrap().tapped = true;
    }
    attack_with(&mut runner, lorthos);
    let events = drive(&mut runner, &[TargetRef::Object(o1)], false, vec![]);
    assert!(
        !resolved(&events).contains(&EffectKind::Tap),
        "nothing is tapped by the declined gate"
    );

    let turn_before = runner.state().turn_number;
    for _ in 0..400 {
        if runner.state().turn_number > turn_before && runner.state().phase != Phase::Untap {
            break;
        }
        let result = match runner.state().waiting_for.clone() {
            WaitingFor::DeclareBlockers { .. } => runner.act(GameAction::DeclareBlockers {
                assignments: vec![],
            }),
            WaitingFor::DeclareAttackers { .. } => runner.act(GameAction::DeclareAttackers {
                attacks: vec![],
                bands: vec![],
            }),
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
                continue;
            }
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
            other => panic!("unexpected wait {other:?}"),
        };
        result.expect("the action is accepted");
    }
    assert_eq!(runner.state().active_player, P1, "reached P1's turn");
    assert!(
        runner.state().objects[&o1].tapped,
        "the declared permanent did not untap"
    );
    assert!(
        !runner.state().objects[&o2].tapped,
        "the undeclared permanent untapped"
    );
}

fn localized_destruction(accept: bool) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let c1 = scenario.add_creature(P0, "c1", 1, 1).id();
    let o1 = scenario.add_creature(P1, "o1", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Localized Destruction", false, LOCALIZED_DESTRUCTION)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![],
        })
        .id();
    scenario.with_mana_pool(P0, mana(1, ManaType::Colorless));
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    runner.cast(spell).commit();
    drive(&mut runner, &[], accept, vec![]);
    (runner, c1, o1)
}

/// J-2c: declined, "You get {E}" stands and "Destroy all creatures." still
/// happens.
#[test]
fn localized_destruction_declined_destroys_every_creature() {
    let (runner, c1, o1) = localized_destruction(false);
    assert_eq!(runner.state().players[0].energy, 1, "the energy is kept");
    assert_eq!(runner.state().objects[&c1].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&o1].zone, Zone::Graveyard);
}

/// J-2e: paid, the creature whose power equals the energy paid survives.
/// GREEN AT BASE: reach guard of J-2c; preservation only.
#[test]
fn localized_destruction_paid_spares_the_matching_creature() {
    let (runner, c1, o1) = localized_destruction(true);
    assert_eq!(runner.state().objects[&c1].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&o1].zone, Zone::Graveyard);
}

fn witchs_mark(accept: bool) -> (GameRunner, ObjectId, Vec<GameEvent>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let c1 = scenario.add_creature(P0, "c1", 2, 2).id();
    scenario.add_card_to_hand(P0, "Filler");
    scenario.with_library_top(P0, &["L1", "L2", "L3"]);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Witch's Mark", false, WITCHS_MARK)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![],
        })
        .id();
    scenario.with_mana_pool(P0, mana(1, ManaType::Colorless));
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    runner.cast(spell).commit();
    let events = drive(&mut runner, &[TargetRef::Object(c1)], accept, vec![]);
    (runner, c1, events)
}

/// J-2d: declined, no card is drawn and the Wicked Role is still created on the
/// declared creature.
#[test]
fn witchs_mark_declined_still_creates_the_role_on_the_declared_creature() {
    let (runner, c1, events) = witchs_mark(false);
    let roles = tokens_named(&runner, "Wicked Role");
    assert_eq!(roles.len(), 1, "the Role is created");
    assert_eq!(
        runner.state().objects[&roles[0]].attached_to,
        Some(engine::game::game_object::AttachTarget::Object(c1))
    );
    assert_eq!(power(&runner, c1), Some(3));
    assert!(!resolved(&events).contains(&EffectKind::Draw), "no draw");
    assert_eq!(runner.state().players[0].library.len(), 3);
    assert_eq!(runner.state().players[0].hand.len(), 1, "no card discarded");
}

/// J-2e: paid, the discard, the two draws and the Role all happen.
/// GREEN AT BASE: reach guard of J-2d; preservation only.
#[test]
fn witchs_mark_paid_draws_and_creates_the_role() {
    let (runner, c1, events) = witchs_mark(true);
    assert_eq!(tokens_named(&runner, "Wicked Role").len(), 1);
    assert_eq!(power(&runner, c1), Some(3));
    assert!(resolved(&events).contains(&EffectKind::Draw));
    assert_eq!(runner.state().players[0].library.len(), 1);
}

fn wyll(accept: bool) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let wyll = {
        let mut builder = scenario.add_creature(P0, "Wyll of the Blade Pact", 3, 3);
        builder.from_oracle_text_with_keywords(&["specialize"], "Specialize {0}");
        builder.id()
    };
    // The specialized face carries the trigger: parse it on a scratch object and
    // install its trigger definitions on the white face.
    let harvest = {
        let mut builder = scenario.add_creature_to_hand(P0, "Wyll of the Blade Pact", 3, 3);
        builder.from_oracle_text(WYLL_OF_THE_BLADE_PACT);
        builder.id()
    };
    let fodder = scenario.add_creature(P0, "c1", 2, 2).id();
    let discard = scenario
        .add_creature_to_hand(P0, "White Discard", 1, 1)
        .id();
    let mut runner = scenario.build();
    {
        let triggers = runner.state().objects[&harvest]
            .base_trigger_definitions
            .clone();
        let back = BackFaceData {
            is_swap_snapshot: false,
            trigger_printed_origins: Vec::new(),
            name: "Wyll of the Blade Pact".into(),
            power: Some(3),
            toughness: Some(3),
            loyalty: None,
            printed_loyalty: None,
            defense: None,
            card_types: CardType {
                core_types: vec![CoreType::Creature],
                ..Default::default()
            },
            mana_cost: ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::White],
            },
            keywords: vec![],
            abilities: vec![],
            trigger_definitions: triggers.into(),
            replacement_definitions: Default::default(),
            static_definitions: Default::default(),
            color: vec![ManaColor::White],
            printed_ref: None,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: vec![],
            casting_options: vec![],
            layout_kind: None,
            parse_warnings: vec![],
        };
        let mut faces = SpecializeFaceMap::new();
        faces.insert(ManaColor::White, back);
        let state = runner.state_mut();
        let obj = state.objects.get_mut(&wyll).unwrap();
        obj.specialize_faces = Some(faces);
        obj.tapped = true;
        state.objects.get_mut(&discard).unwrap().color = vec![ManaColor::White];
        state.players[0].hand.retain(|id| *id != harvest);
    }
    runner
        .act(GameAction::ActivateAbility {
            source_id: wyll,
            ability_index: 0,
        })
        .expect("specialize is activated");
    runner
        .act(GameAction::SelectCards {
            cards: vec![discard],
        })
        .expect("the discard is accepted");
    for _ in 0..8 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::SpecializeColor { .. }
        ) {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }
    if matches!(
        runner.state().waiting_for,
        WaitingFor::SpecializeColor { .. }
    ) {
        runner
            .act(GameAction::ChooseSpecializeColor {
                color: ManaColor::White,
            })
            .expect("the color is accepted");
    }
    let mut offered = false;
    for _ in 0..40 {
        let result = match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                offered = true;
                runner.act(GameAction::DecideOptionalEffect { accept })
            }
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
                continue;
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
            _ => runner.act(GameAction::SelectCards {
                cards: vec![fodder],
            }),
        };
        result.expect("the action is accepted");
    }
    assert!(offered, "the specialize trigger's choice was offered");
    (runner, wyll, fodder)
}

/// J-2f: declined, Wyll is not untapped, and the additional combat and main
/// phases still happen.
#[test]
fn wyll_declined_still_adds_the_additional_phases() {
    let (runner, wyll, fodder) = wyll(false);
    assert!(
        !runner.state().extra_phases.is_empty(),
        "the additional phases are added"
    );
    assert!(runner.state().objects[&wyll].tapped, "Wyll is not untapped");
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Battlefield);
}

/// J-2f paid: the sacrifice, the untap and the additional phases all happen.
/// GREEN AT BASE: reach guard of J-2f; preservation only.
#[test]
fn wyll_paid_untaps_and_adds_the_additional_phases() {
    let (runner, wyll, fodder) = wyll(true);
    assert!(!runner.state().extra_phases.is_empty());
    assert!(!runner.state().objects[&wyll].tapped);
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
}

fn choice_of_fortunes(accept: bool) -> (GameRunner, Vec<GameEvent>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["L1", "L2", "L3", "L4", "L5", "L6"]);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Choice of Fortunes", false, CHOICE_OF_FORTUNES)
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![],
        })
        .id();
    scenario.with_mana_pool(P0, mana(1, ManaType::Colorless));
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    runner.cast(spell).commit();
    let events = drive(&mut runner, &[], accept, vec![]);
    (runner, events)
}

fn assert_no_maximum_hand_size_emblem(runner: &GameRunner) {
    let emblems: Vec<_> = runner
        .state()
        .objects
        .values()
        .filter(|obj| obj.is_emblem)
        .collect();
    assert_eq!(emblems.len(), 1, "one emblem");
    assert_eq!(emblems[0].controller, P0, "CR 114.2: the emblem is P0's");
    assert_eq!(emblems[0].zone, Zone::Command);
    assert!(
        check_static_ability(
            runner.state(),
            StaticMode::NoMaximumHandSize,
            &StaticCheckContext {
                player_id: Some(P0),
                ..Default::default()
            },
        ),
        "CR 402.2: P0 has no maximum hand size"
    );
}

/// J-2g: declined, there is no shuffle and no second seek, and the player still
/// gets the "no maximum hand size" emblem.
#[test]
fn choice_of_fortunes_declined_still_creates_the_emblem() {
    let (runner, events) = choice_of_fortunes(false);
    assert_eq!(
        resolved(&events),
        vec![EffectKind::Seek, EffectKind::CreateEmblem]
    );
    assert_no_maximum_hand_size_emblem(&runner);
}

/// J-2g′: accepted, every instruction happens. GREEN AT BASE: reach guard of
/// J-2g; preservation only.
#[test]
fn choice_of_fortunes_accepted_seeks_twice_and_creates_the_emblem() {
    let (runner, events) = choice_of_fortunes(true);
    assert_eq!(
        resolved(&events),
        vec![
            EffectKind::Seek,
            EffectKind::Shuffle,
            EffectKind::Seek,
            EffectKind::CreateEmblem
        ]
    );
    assert_no_maximum_hand_size_emblem(&runner);
}

/// J-3 (a): declined, the gate's doubling is skipped, and both later
/// instructions happen: "That creature" is the declared creature, and the life
/// gain names only a player.
#[test]
fn syn_j3_declined_keeps_the_declared_target_rider_and_the_life_gain() {
    let board = combat_board("SynJ3", SYN_J3, false);
    let must_be_blocked = StaticMode::MustBeBlocked { by: None };
    assert_eq!(
        static_mode_recipients(&board.runner, &must_be_blocked),
        vec![board.c2]
    );
    assert_eq!(power(&board.runner, board.c2), Some(2));
    assert_eq!(board.runner.life(P0), 22);
}

/// J-3 (b): paid, every instruction happens. GREEN AT BASE: reach guard of
/// J-3 (a); preservation only.
#[test]
fn syn_j3_paid_resolves_every_instruction() {
    let board = combat_board("SynJ3", SYN_J3, true);
    let must_be_blocked = StaticMode::MustBeBlocked { by: None };
    assert_eq!(
        static_mode_recipients(&board.runner, &must_be_blocked),
        vec![board.c2]
    );
    assert_eq!(power(&board.runner, board.c2), Some(4));
    assert_eq!(board.runner.life(P0), 22);
}

/// J-3 (c), the charter's J-3 in one chain: declined, the gated token is not
/// created, the counter sentence is skipped (an unaudited `PutCounter`; see
/// [`SYN_GAP1`]), and "You gain 1 life" still happens. Red at base; red under
/// M-3.
#[test]
fn syn_gap1_declined_gains_life_without_the_token_or_its_counter() {
    let board = combat_board("SynGap1", SYN_GAP1, false);
    assert_eq!(resolved(&board.events), vec![EffectKind::GainLife]);
    assert_eq!(board.runner.life(P0), 21);
    assert!(tokens_named(&board.runner, "Ally").is_empty(), "no token");
}

/// J-3 (b): paid, the token, life gain and counter instructions each resolve.
/// GREEN AT BASE: reach guard of J-3 (c); preservation only. KNOWN-BAD: the
/// parser binds "that token" to `ParentTarget`, so the counter lands on no
/// object (measured: the Ally, c1 and c2 carry no counters, at base and on the
/// candidate); plan J-3 passed-half (iii) is unmet for that reason. This row
/// pins only the resolved kinds, the Ally and the life total.
#[test]
fn syn_gap1_paid_resolves_each_instruction_kind() {
    let board = combat_board("SynGap1", SYN_GAP1, true);
    assert_eq!(
        resolved(&board.events),
        vec![
            EffectKind::Token,
            EffectKind::GainLife,
            EffectKind::PutCounter
        ]
    );
    assert_eq!(tokens_named(&board.runner, "Ally").len(), 1);
    assert_eq!(board.runner.life(P0), 21);
}

/// J-3 (e): the Krenko shape. "It" names the token only the gated action
/// creates, so nothing happens. GREEN AT BASE, NON-DISCRIMINATING: the later
/// node is a `ContinuationStep` (the gate's own step), skipped before any
/// referent is read; unchanged under M-4 (measured).
#[test]
fn syn_token_it_declined_creates_nothing() {
    let board = combat_board("SynTokIt", SYN_TOKEN_IT, false);
    assert!(resolved(&board.events).is_empty());
    assert!(tokens_named(&board.runner, "Goblin").is_empty());
}

/// J-3 (g): a gate that would return its declared target makes "That
/// creature" a new object only the gated action produces (CR 400.7), so the
/// later instruction is governed. GREEN AT BASE; red under M-5 (measured).
#[test]
fn syn_reanimate_that_declined_resolves_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "SynReThat", 3, 3, SYN_REANIMATE_THAT);
    let bear = scenario.add_creature_to_graveyard(P0, "Gy Bear", 2, 2).id();
    let mut runner = scenario.build();
    runner.advance_to_phase(Phase::BeginCombat);
    let events = drive(&mut runner, &[TargetRef::Object(bear)], false, vec![]);
    assert!(resolved(&events).is_empty());
    assert!(runner.state().transient_continuous_effects.is_empty());
    assert_eq!(runner.state().objects[&bear].zone, Zone::Graveyard);
}

/// J-3 (h): a gate that would exile its declared target governs "That
/// creature". GREEN AT BASE; red under M-5 (measured).
#[test]
fn syn_exile_that_declined_resolves_nothing() {
    let board = combat_board("SynExThat", SYN_EXILE_THAT, false);
    assert!(resolved(&board.events).is_empty());
    assert!(board.runner.state().transient_continuous_effects.is_empty());
    assert_eq!(
        board.runner.state().objects[&board.c2].zone,
        Zone::Battlefield
    );
}

/// A kept instruction and the later rider that names its result are kept or
/// skipped together (CR 608.2c: later text may modify earlier text). The rider
/// "It gains haste" names the Goblin, and it is not proven independent of the
/// declined gate, so the Goblin sentence is skipped with it: declined, nothing
/// happens (the base reading), never a Goblin without haste. GREEN AT BASE;
/// red under M-11b (the coupling disabled: a Goblin without haste; measured).
#[test]
fn syn_rider_declined_creates_no_goblin_without_its_rider() {
    let board = combat_board("SynRider", SYN_RIDER, false);
    assert!(
        tokens_named(&board.runner, "Goblin").is_empty(),
        "no Goblin is created without its haste rider"
    );
    assert!(resolved(&board.events).is_empty());
    assert_eq!(board.runner.state().players[0].library.len(), 2, "no draw");
}

/// Paid, the draw, the Goblin and its haste all happen. GREEN AT BASE:
/// reach guard of the declined row; preservation only.
#[test]
fn syn_rider_paid_creates_a_goblin_with_haste() {
    let mut board = combat_board("SynRider", SYN_RIDER, true);
    let goblins = tokens_named(&board.runner, "Goblin");
    assert_eq!(goblins.len(), 1);
    assert!(has_kw(&mut board.runner, goblins[0], &Keyword::Haste));
    assert_eq!(board.runner.state().players[0].library.len(), 1);
}

/// J-6 (a): Krenko, Baron of Tin Street. "It gains haste" names the gated
/// token; declined, no token is created. GREEN AT BASE, NON-DISCRIMINATING:
/// "It" is a `ContinuationStep`, a step of the gate itself, so no single
/// in-scope mutation reaches a referent check (M-4 measured unchanged on the
/// same shape, J-3 (e)).
#[test]
fn krenko_declined_creates_no_goblin() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(
        P0,
        "Krenko, Baron of Tin Street",
        1,
        2,
        KRENKO_BARON_OF_TIN_STREET,
    );
    let artifact = scenario.add_artifact_from_oracle(P0, "Bauble", "").id();
    let shatter = scenario
        .add_spell_to_hand_from_oracle(P0, "Shatter", true, "Destroy target artifact.")
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![],
        })
        .id();
    scenario.with_mana_pool(P0, mana(1, ManaType::Colorless));
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    runner.cast(shatter).target_object(artifact).commit();
    let events = drive(&mut runner, &[], false, vec![]);
    assert_eq!(runner.state().objects[&artifact].zone, Zone::Graveyard);
    assert!(!resolved(&events).contains(&EffectKind::Token));
    assert!(tokens_named(&runner, "Goblin").is_empty());
}

/// J-6 (a′): Kalastria Highborn. "and you gain 2 life" completes the gated
/// sentence; declined, no life changes (ruling 2010-03-01). Green at base: a
/// reach guard.
#[test]
fn kalastria_highborn_declined_changes_no_life() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_creature_from_oracle(P0, "Kalastria Highborn", 2, 2, KALASTRIA_HIGHBORN)
        .with_subtypes(vec!["Vampire", "Shaman"]);
    let vampire = scenario
        .add_creature(P0, "Vamp", 2, 2)
        .with_subtypes(vec!["Vampire"])
        .id();
    let kill = scenario
        .add_spell_to_hand_from_oracle(P0, "Kill", true, "Destroy target creature.")
        .with_mana_cost(ManaCost::Cost {
            generic: 1,
            shards: vec![],
        })
        .id();
    scenario.with_mana_pool(P0, mana(1, ManaType::Colorless));
    let mut runner = scenario.build();
    grant_priority(&mut runner, P0);
    runner.cast(kill).target_object(vampire).commit();
    let events = drive(&mut runner, &[TargetRef::Player(P1)], false, vec![]);
    assert_eq!(runner.state().objects[&vampire].zone, Zone::Graveyard);
    assert_eq!(resolved(&events), vec![EffectKind::Destroy]);
    assert_eq!(runner.life(P0), 20);
    assert_eq!(runner.life(P1), 20);
}

/// J-6 (a′): Hellkite Charger. "and after this phase, there is an additional
/// combat phase" completes the gated sentence; declined, no additional combat.
/// Green at base: a reach guard.
#[test]
fn hellkite_charger_declined_adds_no_combat_phase() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let hellkite = scenario
        .add_creature_from_oracle(P0, "Hellkite Charger", 5, 5, HELLKITE_CHARGER)
        .id();
    let mut runner = scenario.build();
    attack_with(&mut runner, hellkite);
    let events = drive(&mut runner, &[], false, vec![]);
    assert!(resolved(&events).is_empty());
    assert!(runner.state().extra_phases.is_empty());
    assert!(runner.state().objects[&hellkite].tapped);
}

fn hollow_specter(accept: bool) -> (GameRunner, Vec<GameEvent>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let specter = scenario
        .add_creature_from_oracle(P0, "Hollow Specter", 2, 2, HOLLOW_SPECTER)
        .id();
    scenario.add_card_to_hand(P1, "H1");
    scenario.add_card_to_hand(P1, "H2");
    let mut runner = scenario.build();
    attack_with(&mut runner, specter);
    let events = drive(&mut runner, &[], accept, mana(1, ManaType::Colorless));
    (runner, events)
}

/// J-6 (b2): Hollow Specter declined: nothing is revealed or discarded.
/// GREEN AT BASE; red under M-4 (measured).
#[test]
fn hollow_specter_declined_reveals_and_discards_nothing() {
    let (runner, events) = hollow_specter(false);
    assert_eq!(runner.life(P1), 18, "combat damage was dealt");
    assert!(resolved(&events).is_empty());
    assert_eq!(runner.state().players[1].hand.len(), 2);
}

/// J-6 (b2) reach guard at X = 1: paid, the gated reveal and discard effects
/// resolve. GREEN AT BASE; preservation only. KNOWN-BAD (residue): at X = 1
/// the whole hand is revealed and nothing is discarded, at base and on the
/// candidate (measured: 2 cards revealed, P1's hand 2, graveyard 0). This row
/// pins only the resolved kinds.
#[test]
fn hollow_specter_paid_x1_resolves_the_reveal_and_discard_kinds() {
    let (_, events) = hollow_specter(true);
    assert_eq!(
        resolved(&events),
        vec![EffectKind::Reveal, EffectKind::DiscardCard]
    );
}

/// J-6 (b3): Ardent Dustspeaker declined: nothing is exiled and no play
/// permission is granted. GREEN AT BASE; red under M-4 (measured).
#[test]
fn ardent_dustspeaker_declined_exiles_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::DeclareAttackers);
    let dustspeaker = scenario
        .add_creature_from_oracle(P0, "Ardent Dustspeaker", 3, 4, ARDENT_DUSTSPEAKER)
        .id();
    scenario.add_spell_to_graveyard(P0, "Gy Shock", true);
    scenario.with_library_top(P0, &["T1", "T2", "T3"]);
    let mut runner = scenario.build();
    attack_with(&mut runner, dustspeaker);
    let events = drive(&mut runner, &[], false, vec![]);
    assert!(resolved(&events).is_empty());
    assert_eq!(runner.state().players[0].library.len(), 3);
    assert_eq!(runner.state().players[0].graveyard.len(), 1);
    assert!(runner.state().exile.is_empty());
}

/// J-6 (b4): Kheru Lich Lord declined: nothing returns and no delayed exile is
/// created. GREEN AT BASE; red under M-4 (measured).
#[test]
fn kheru_lich_lord_declined_returns_nothing() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario.add_creature_from_oracle(P0, "Kheru Lich Lord", 4, 4, KHERU_LICH_LORD);
    let bear = scenario.add_creature_to_graveyard(P0, "Gy Bear", 2, 2).id();
    let mut runner = scenario.build();
    runner.advance_to_phase(Phase::Upkeep);
    let events = drive(&mut runner, &[], false, vec![]);
    assert!(resolved(&events).is_empty());
    assert_eq!(runner.state().objects[&bear].zone, Zone::Graveyard);
    assert!(runner.state().delayed_triggers.is_empty());
}

/// Loyal Unicorn's controller attacks with two 2/2 creatures, each blocked by
/// a 3/3. Returns whether each attacker survived combat, whether the second
/// one gained vigilance, and what the Lieutenant trigger resolved.
fn loyal_unicorn(with_commander: bool) -> (bool, bool, bool, Vec<EffectKind>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Loyal Unicorn", 3, 4, LOYAL_UNICORN);
    let c1 = scenario.add_creature(P0, "c1", 2, 2).id();
    let c3 = scenario.add_creature(P0, "c3", 2, 2).id();
    let commander = scenario.add_creature(P0, "Test Commander", 3, 3).id();
    let b1 = scenario.add_creature(P1, "b1", 3, 3).id();
    let b3 = scenario.add_creature(P1, "b3", 3, 3).id();
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&commander)
        .unwrap()
        .is_commander = with_commander;
    let mut events = Vec::new();
    runner.advance_to_phase(Phase::BeginCombat);
    for _ in 0..20 {
        let result = match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                drain_order_triggers_with_identity(runner.state_mut());
                continue;
            }
            // The engine asks for a creature for the prevention instruction.
            WaitingFor::TriggerTargetSelection { .. } => runner.act(GameAction::ChooseTarget {
                target: Some(TargetRef::Object(c1)),
            }),
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
            other => panic!("unexpected wait {other:?}"),
        };
        events.extend(result.expect("the action is accepted").events);
    }
    let vigilance = has_kw(&mut runner, c3, &Keyword::Vigilance);
    for _ in 0..10 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority is passed");
    }
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![
                (c1, AttackTarget::Player(P1)),
                (c3, AttackTarget::Player(P1)),
            ],
            bands: vec![],
        })
        .expect("the attack is accepted");
    for _ in 0..10 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareBlockers { .. }
        ) {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("priority is passed");
    }
    runner
        .act(GameAction::DeclareBlockers {
            assignments: vec![(b1, c1), (b3, c3)],
        })
        .expect("the blocks are accepted");
    runner.combat_damage();
    let survived = |id: ObjectId| runner.state().objects[&id].zone == Zone::Battlefield;
    (survived(c1), survived(c3), vigilance, resolved(&events))
}

/// J-6 (c): Loyal Unicorn's intervening "if" (CR 603.4) is not an "if you do"
/// gate. With a commander, combat damage to every creature its controller
/// controls is prevented and the other creatures gain vigilance.
/// GREEN AT BASE, NON-DISCRIMINATING: there is no `OptionalEffectPerformed`
/// gate, so the declined walk is unreachable.
#[test]
fn loyal_unicorn_with_commander_prevents_combat_damage_and_grants_vigilance() {
    let (c1_survived, c3_survived, vigilance, kinds) = loyal_unicorn(true);
    assert_eq!(
        kinds,
        vec![EffectKind::PreventDamage, EffectKind::GenericEffect]
    );
    assert!(c1_survived && c3_survived, "combat damage is prevented");
    assert!(vigilance);
}

/// J-6 (c): without a commander, nothing happens. GREEN AT BASE,
/// NON-DISCRIMINATING, as above.
#[test]
fn loyal_unicorn_without_commander_does_nothing() {
    let (c1_survived, c3_survived, vigilance, kinds) = loyal_unicorn(false);
    assert!(kinds.is_empty());
    assert!(!c1_survived && !c3_survived, "combat damage is dealt");
    assert!(!vigilance);
}

/// J-6 (d): Iroh, Tea Master's reflexive "when you do" (CR 603.12): declined,
/// no control changes and no Ally token is created. GREEN AT BASE,
/// NON-DISCRIMINATING: the condition is `WhenYouDo`, which the gate-shape
/// check refuses.
#[test]
fn iroh_declined_creates_no_ally() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Iroh, Tea Master", 2, 2, IROH_TEA_MASTER);
    let c1 = scenario.add_creature(P0, "c1", 2, 2).id();
    let mut runner = scenario.build();
    runner.advance_to_phase(Phase::BeginCombat);
    let events = drive(
        &mut runner,
        &[TargetRef::Object(c1), TargetRef::Player(P1)],
        false,
        vec![],
    );
    assert!(!resolved(&events).contains(&EffectKind::Token));
    assert!(tokens_named(&runner, "Ally").is_empty());
    assert_eq!(runner.state().objects[&c1].controller, P0);
}
