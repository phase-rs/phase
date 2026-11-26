//! CR 508.7: Misleading Signpost and Portal Mage reselect which player or permanent an attacking
//! creature is attacking, through the real pipeline.
//!
//! "When this artifact enters during the declare attackers step, you may reselect which player or
//! permanent target attacking creature is attacking. (It can't attack its controller or their
//! permanents.)" The creature stays attacking and is still considered to have attacked what it was
//! declared attacking (CR 508.7a); the declaration's requirements, restrictions and costs do not
//! apply (CR 508.7b); the new destination is an opponent of the attacking creature's controller,
//! that opponent's planeswalker, or a battle that opponent protects (CR 508.7c). The "defending
//! player" of the creature follows the reselection (CR 508.5 + CR 508.7a), so an ability that
//! targets something the old defending player controls loses its target (rulings). Every card is
//! its verbatim Oracle text.

use std::collections::HashSet;

use engine::ai_support::legal_actions;
use engine::game::combat::{
    apply_resolved_combat_membership, compute_attack_tax, validate_attack_declaration,
    AttackTarget, AttackedPermanentKinds,
};
use engine::game::game_object::ObjectIncarnationRefExt;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{EffectKind, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    AttackTargetChoicePurpose, CastPaymentMode, GameState, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolved_commands::{
    ResolvedCombatMembershipCommand, ResolvedCombatMembershipEdit,
    ResolvedCombatMembershipReplayInvariantError, ResolvedRulesCommand, ResolvedRulesJournal,
};
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

const SIGNPOST: &str = "Flash\nWhen this artifact enters during the declare attackers step, you may reselect which player or permanent target attacking creature is attacking. (It can't attack its controller or their permanents.)\n{T}: Add {U}.";
const PORTAL_MAGE: &str = "Flash\nWhen this creature enters during the declare attackers step, you may reselect which player or permanent target attacking creature is attacking. (It can't attack its controller or their permanents.)";
const CLERIC: &str = "Flying\nWhenever this creature attacks, you gain 1 life.";
const FIEND_BINDER: &str =
    "Whenever this creature attacks, tap target creature defending player controls.";
const JACE: &str = "[+2]: Each player draws a card.\n[\u{2212}1]: Target player draws a card.\n[\u{2212}10]: Target player mills twenty cards.";
const ARCHON: &str = "Flying\nCreatures can't attack you.";
const GHOSTLY_PRISON: &str = "Creatures can't attack you unless their controller pays {2} for each creature they control that's attacking you.";

/// Which card of the class performs the reselection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Card {
    Signpost,
    PortalMage,
}

fn add_card(sc: &mut GameScenario, owner: PlayerId, card: Card) -> ObjectId {
    match card {
        Card::Signpost => sc
            .add_artifact_to_hand_from_oracle(owner, "Misleading Signpost", SIGNPOST)
            .from_oracle_text_with_keywords(&["Flash"], SIGNPOST)
            .id(),
        Card::PortalMage => sc
            .add_creature_to_hand_from_oracle(owner, "Portal Mage", 2, 2, PORTAL_MAGE)
            .from_oracle_text_with_keywords(&["Flash"], PORTAL_MAGE)
            .id(),
    }
}

fn add_cleric(sc: &mut GameScenario) -> ObjectId {
    sc.add_creature_from_oracle(P0, "Moonrise Cleric", 2, 3, CLERIC)
        .from_oracle_text_with_keywords(&["Flying"], CLERIC)
        .id()
}

fn add_jace(sc: &mut GameScenario, controller: PlayerId) -> ObjectId {
    sc.add_planeswalker_from_oracle(controller, "Jace Beleren", "Jace", 3, JACE)
        .as_planeswalker_with_loyalty("Jace", 3)
        .id()
}

/// Drives one game and records what it observed: every engine event and the name of every
/// `WaitingFor` reached after an action.
struct Driver {
    r: GameRunner,
    events: Vec<GameEvent>,
    trace: Vec<&'static str>,
}

impl Driver {
    fn new(r: GameRunner) -> Self {
        let trace = vec![r.state().waiting_for.variant_name()];
        Driver {
            r,
            events: Vec::new(),
            trace,
        }
    }

    fn state(&self) -> &GameState {
        self.r.state()
    }

    fn act(&mut self, action: GameAction) {
        let result = self
            .r
            .act(action.clone())
            .unwrap_or_else(|error| panic!("{action:?} rejected: {error:?}"));
        self.events.extend(result.events);
        self.trace.push(self.r.state().waiting_for.variant_name());
    }

    fn advance_to_combat(&mut self) {
        self.r.advance_to_combat();
        self.trace.push(self.r.state().waiting_for.variant_name());
    }

    /// Declare the attack and answer its trigger prompts; `trigger_target` answers a targeted
    /// attack trigger.
    fn declare(
        &mut self,
        attacker: ObjectId,
        target: AttackTarget,
        trigger_target: Option<ObjectId>,
    ) {
        self.declare_all(&[(attacker, target)], trigger_target);
    }

    /// [`Driver::declare`] for several attackers in one declaration.
    fn declare_all(
        &mut self,
        attacks: &[(ObjectId, AttackTarget)],
        trigger_target: Option<ObjectId>,
    ) {
        let result = self
            .r
            .declare_attackers(attacks)
            .expect("the attack is legal");
        self.events.extend(result.events);
        self.trace.push(self.r.state().waiting_for.variant_name());
        for _ in 0..16 {
            match self.state().waiting_for.clone() {
                WaitingFor::OrderTriggers { triggers, .. } => {
                    let order = (0..triggers.len()).collect();
                    self.act(GameAction::OrderTriggers { order });
                }
                WaitingFor::TriggerTargetSelection { .. } => {
                    let target = trigger_target.expect("the attack trigger has a target");
                    self.act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    });
                }
                _ => return,
            }
        }
        panic!("attack-trigger prompts did not settle");
    }

    /// Resolve everything on the stack (the attack trigger), stopping at the active player's
    /// priority with an empty stack.
    fn resolve_stack(&mut self) {
        for _ in 0..16 {
            match self.state().waiting_for {
                WaitingFor::Priority { .. } if self.state().stack.is_empty() => return,
                WaitingFor::Priority { .. } => self.act(GameAction::PassPriority),
                ref other => panic!("unexpected prompt while resolving: {other:?}"),
            }
        }
        panic!("the stack did not empty");
    }

    /// Pass priority until `player` holds it.
    fn pass_to(&mut self, player: PlayerId) {
        for _ in 0..4 {
            match self.state().waiting_for {
                WaitingFor::Priority { player: holder } if holder == player => return,
                WaitingFor::Priority { .. } => self.act(GameAction::PassPriority),
                ref other => panic!("expected priority: {other:?}"),
            }
        }
        panic!("{player:?} did not receive priority");
    }

    /// Cast `card` (Flash) and pass until its entering trigger asks the optional "you may"
    /// question, answering the trigger's target prompt with `attacker`.
    fn cast_to_optional_choice(&mut self, card: ObjectId, attacker: ObjectId) {
        let phase = self.state().phase;
        self.r.cast(card).commit();
        self.trace.push(self.r.state().waiting_for.variant_name());
        for _ in 0..24 {
            assert_eq!(self.state().phase, phase, "the step did not change");
            match self.state().waiting_for.clone() {
                WaitingFor::OptionalEffectChoice { .. } => return,
                WaitingFor::TriggerTargetSelection { .. } => self.act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Object(attacker)),
                }),
                WaitingFor::Priority { .. } => self.act(GameAction::PassPriority),
                other => panic!("unexpected prompt while resolving the card: {other:?}"),
            }
        }
        panic!("the entering trigger never asked its optional question");
    }

    fn decide(&mut self, accept: bool) {
        self.act(GameAction::DecideOptionalEffect { accept });
    }

    fn choose(&mut self, target: AttackTarget) {
        self.act(GameAction::ChooseEntryAttackTarget { target });
    }

    /// CR 117.3b: the completion boundary — the trigger left the stack and the active player P0
    /// received priority.
    fn assert_boundary(&self) {
        assert!(self.state().stack.is_empty(), "{:?}", self.state().stack);
        assert_eq!(
            self.state().waiting_for,
            WaitingFor::Priority { player: P0 },
            "the active player receives priority after the trigger resolves"
        );
    }

    /// Pass through blockers (no blocks) until combat damage has been dealt.
    fn finish_combat(&mut self) {
        for _ in 0..60 {
            if matches!(self.state().phase, Phase::EndCombat | Phase::PostCombatMain) {
                return;
            }
            match self.state().waiting_for.clone() {
                WaitingFor::Priority { .. } => self.act(GameAction::PassPriority),
                WaitingFor::DeclareBlockers { .. } => self.act(GameAction::DeclareBlockers {
                    assignments: Vec::new(),
                }),
                other => panic!("unexpected prompt in combat: {other:?}"),
            }
        }
        panic!("combat damage was not reached");
    }

    fn attacker(&self, attacker: ObjectId) -> (AttackTarget, PlayerId) {
        let combat = self.state().combat.as_ref().expect("combat");
        let entries: Vec<_> = combat
            .attackers
            .iter()
            .filter(|info| info.object_id == attacker)
            .collect();
        assert_eq!(entries.len(), 1, "exactly one attacker entry: {entries:?}");
        (entries[0].attack_target, entries[0].defending_player)
    }

    fn life(&self, player: PlayerId) -> i32 {
        self.state()
            .players
            .iter()
            .find(|p| p.id == player)
            .unwrap()
            .life
    }

    fn journal_len(&self) -> usize {
        self.state().resolved_rules_journal.entries().len()
    }

    fn reselect_events(&self) -> usize {
        self.events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: EffectKind::ReselectAttackTarget,
                        ..
                    }
                )
            })
            .count()
    }
}

/// Every combat-membership command recorded after `journal_start`, in journal order.
fn membership_commands(
    state: &GameState,
    journal_start: usize,
) -> Vec<ResolvedCombatMembershipCommand> {
    state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter_map(|entry| entry.command.clone())
        .filter_map(|command| match command {
            ResolvedRulesCommand::CombatMembership(command) => Some(command),
            _ => None,
        })
        .collect()
}

/// The reselection prompt's offered destinations, asserting its shape.
fn reselect_prompt(state: &GameState, chooser: PlayerId, attacker: ObjectId) -> Vec<AttackTarget> {
    match &state.waiting_for {
        WaitingFor::EntryAttackTargetChoice {
            player,
            object_id,
            valid_targets,
            purpose,
        } => {
            assert_eq!(*player, chooser, "the ability's controller chooses");
            assert_eq!(*object_id, attacker);
            assert_eq!(*purpose, AttackTargetChoicePurpose::Reselect);
            valid_targets.clone()
        }
        other => panic!("expected the reselection prompt, got {other:?}"),
    }
}

fn as_set(targets: &[AttackTarget]) -> HashSet<AttackTarget> {
    targets.iter().copied().collect()
}

fn assert_optional_choice(state: &GameState, chooser: PlayerId) {
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::OptionalEffectChoice { player, .. } if player == chooser
        ),
        "the trigger's controller decides \"you may\": {:?}",
        state.waiting_for
    );
}

/// The three-player Cleric board: P0's Moonrise Cleric attacks P1; P1 holds `card`.
struct ClericBoard {
    d: Driver,
    cleric: ObjectId,
    card: ObjectId,
}

fn cleric_board(card: Card, p2_setup: impl FnOnce(&mut GameScenario)) -> ClericBoard {
    let mut sc = GameScenario::new_n_player(3, 7);
    sc.at_phase(Phase::PreCombatMain);
    let cleric = add_cleric(&mut sc);
    let card = add_card(&mut sc, P1, card);
    p2_setup(&mut sc);
    ClericBoard {
        d: Driver::new(sc.build()),
        cleric,
        card,
    }
}

/// Attack P1 with the Cleric, resolve its trigger, and have P1 cast the card up to its optional
/// question.
fn cleric_to_optional_choice(b: &mut ClericBoard) {
    b.d.advance_to_combat();
    b.d.declare(b.cleric, AttackTarget::Player(P1), None);
    b.d.resolve_stack();
    b.d.pass_to(P1);
    b.d.cast_to_optional_choice(b.card, b.cleric);
    assert_optional_choice(b.d.state(), P1);
}

/// Row 1: CR 508.7 + CR 508.7a + CR 508.7c — the chooser (P1) is not the attacking creature's
/// controller (P0); the destinations are P0's opponents; choosing P2 rewrites the attacked
/// relation and its defending player in place.
fn reselect_to_third_player(card: Card) {
    let mut b = cleric_board(card, |_| {});
    cleric_to_optional_choice(&mut b);
    let history_before =
        b.d.state()
            .combat
            .as_ref()
            .unwrap()
            .attacking_incarnations_this_combat
            .clone();
    let events_before = b.d.events.len();
    let journal_start = b.d.journal_len();
    b.d.decide(true);

    let offered = reselect_prompt(b.d.state(), P1, b.cleric);
    assert_eq!(
        as_set(&offered),
        HashSet::from([AttackTarget::Player(P1), AttackTarget::Player(P2)]),
        "CR 508.7c: opponents of the attacking creature's controller"
    );
    // AI: one candidate per offered destination.
    let actions: HashSet<String> = legal_actions(b.d.state())
        .iter()
        .map(|action| format!("{action:?}"))
        .collect();
    let expected: HashSet<String> = offered
        .iter()
        .map(|target| {
            format!(
                "{:?}",
                GameAction::ChooseEntryAttackTarget { target: *target }
            )
        })
        .collect();
    assert_eq!(actions, expected);

    b.d.choose(AttackTarget::Player(P2));
    b.d.assert_boundary();
    assert_eq!(b.d.attacker(b.cleric), (AttackTarget::Player(P2), P2));
    let commands = membership_commands(b.d.state(), journal_start);
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert_eq!(
        commands[0].object,
        ObjectIncarnationRef::from_object(&b.d.state().objects[&b.cleric])
    );
    assert_eq!(
        commands[0].edit,
        ResolvedCombatMembershipEdit::Reselect {
            expected_attack_target: AttackTarget::Player(P1),
            expected_attack_target_identity: None,
            resulting_defending_player: P2,
            resulting_attack_target: AttackTarget::Player(P2),
            resulting_attack_target_identity: None,
        }
    );
    assert_eq!(b.d.reselect_events(), 1);

    // Row 4: CR 508.7a + CR 508.6 — the declaration history is unchanged, and no attack trigger
    // fires on the reselection (P0 gained 1 life at declaration only).
    let combat = b.d.state().combat.as_ref().unwrap();
    assert_eq!(
        combat
            .creature_attacked_defenders_this_combat
            .get(&b.cleric),
        Some(&HashSet::from([P1]))
    );
    assert_eq!(
        combat.attacked_defenders_this_combat.get(&P0),
        Some(&HashSet::from([P1]))
    );
    assert_eq!(combat.attacking_incarnations_this_combat, history_before);
    assert_eq!(b.d.life(P0), 21);
    assert!(
        !b.d.events[events_before..]
            .iter()
            .any(|event| matches!(event, GameEvent::AttackersDeclared { .. })),
        "a reselection declares no attack"
    );

    b.d.finish_combat();
    assert_eq!(b.d.life(P2), 18, "the Cleric now deals its damage to P2");
    assert_eq!(b.d.life(P1), 20);
}

#[test]
fn misleading_signpost_reselects_the_attacked_player() {
    reselect_to_third_player(Card::Signpost);
}

#[test]
fn portal_mage_reselects_the_attacked_player() {
    reselect_to_third_player(Card::PortalMage);
}

/// Row 3: CR 603.5 — declining "you may" leaves the attack as declared.
#[test]
fn declining_the_reselection_keeps_the_declared_attack() {
    let mut b = cleric_board(Card::Signpost, |_| {});
    cleric_to_optional_choice(&mut b);
    let journal_start = b.d.journal_len();
    b.d.decide(false);
    assert!(!b.d.trace.contains(&"EntryAttackTargetChoice"));
    b.d.assert_boundary();
    assert_eq!(b.d.attacker(b.cleric), (AttackTarget::Player(P1), P1));
    assert!(membership_commands(b.d.state(), journal_start).is_empty());
    assert_eq!(b.d.reselect_events(), 0);
    b.d.finish_combat();
    assert_eq!(b.d.life(P1), 18);
}

/// Row 10 (R14): choosing the current destination among several is a legal reselection that
/// changes nothing and journals nothing.
#[test]
fn reselecting_the_current_destination_changes_nothing() {
    let mut b = cleric_board(Card::Signpost, |_| {});
    cleric_to_optional_choice(&mut b);
    let journal_start = b.d.journal_len();
    b.d.decide(true);
    let offered = reselect_prompt(b.d.state(), P1, b.cleric);
    assert!(offered.len() >= 2 && offered.contains(&AttackTarget::Player(P1)));
    b.d.choose(AttackTarget::Player(P1));
    b.d.assert_boundary();
    assert_eq!(b.d.reselect_events(), 1);
    assert!(membership_commands(b.d.state(), journal_start).is_empty());
    assert_eq!(b.d.attacker(b.cleric), (AttackTarget::Player(P1), P1));
}

/// A two-player board: P0's Cleric attacks `attack`; P0 holds Signpost; `jaces` lists who
/// controls a Jace Beleren.
struct TwoPlayerBoard {
    d: Driver,
    cleric: ObjectId,
    card: ObjectId,
    jaces: Vec<ObjectId>,
}

fn two_player_board(jace_controllers: &[PlayerId]) -> TwoPlayerBoard {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let cleric = add_cleric(&mut sc);
    let card = add_card(&mut sc, P0, Card::Signpost);
    let jaces = jace_controllers
        .iter()
        .map(|controller| add_jace(&mut sc, *controller))
        .collect();
    TwoPlayerBoard {
        d: Driver::new(sc.build()),
        cleric,
        card,
        jaces,
    }
}

fn two_player_to_optional_choice(b: &mut TwoPlayerBoard, attack: AttackTarget) {
    b.d.advance_to_combat();
    b.d.declare(b.cleric, attack, None);
    b.d.resolve_stack();
    b.d.cast_to_optional_choice(b.card, b.cleric);
    assert_optional_choice(b.d.state(), P0);
}

fn loyalty(state: &GameState, id: ObjectId) -> Option<u32> {
    state.objects[&id].loyalty
}

/// Row 2: CR 508.7c + CR 400.7 — reselecting to the opponent's planeswalker pins it; the
/// attacking creature's controller and its planeswalker are not offered.
#[test]
fn reselecting_to_a_planeswalker_pins_it_and_excludes_the_controllers_own() {
    let mut b = two_player_board(&[P0, P1]);
    let (own_jace, their_jace) = (b.jaces[0], b.jaces[1]);
    two_player_to_optional_choice(&mut b, AttackTarget::Player(P1));
    let journal_start = b.d.journal_len();
    b.d.decide(true);
    let offered = reselect_prompt(b.d.state(), P0, b.cleric);
    assert_eq!(
        as_set(&offered),
        HashSet::from([
            AttackTarget::Player(P1),
            AttackTarget::Planeswalker(their_jace)
        ])
    );
    assert!(!offered.contains(&AttackTarget::Player(P0)));
    assert!(!offered.contains(&AttackTarget::Planeswalker(own_jace)));

    b.d.choose(AttackTarget::Planeswalker(their_jace));
    b.d.assert_boundary();
    assert_eq!(
        b.d.attacker(b.cleric),
        (AttackTarget::Planeswalker(their_jace), P1),
        "CR 508.5: the planeswalker's controller is the defending player"
    );
    let pin = ObjectIncarnationRef::from_object(&b.d.state().objects[&their_jace]);
    let combat = b.d.state().combat.as_ref().unwrap();
    assert_eq!(combat.attacked_permanents.get(&b.cleric), Some(&pin));
    assert_eq!(
        combat.attacked_permanent_kinds.get(&b.cleric),
        Some(&AttackedPermanentKinds::Planeswalker)
    );
    let commands = membership_commands(b.d.state(), journal_start);
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert!(matches!(
        commands[0].edit,
        ResolvedCombatMembershipEdit::Reselect {
            resulting_attack_target_identity: Some(identity),
            ..
        } if identity == pin
    ));

    b.d.finish_combat();
    assert_eq!(loyalty(b.d.state(), their_jace), Some(1));
    assert_eq!(b.d.life(P1), 20);
}

/// Row 9: reselecting from a planeswalker to its controller removes the pin and the kinds
/// record, and combat damage goes to the player.
#[test]
fn reselecting_from_a_planeswalker_to_a_player_removes_the_pin() {
    let mut b = two_player_board(&[P1]);
    let jace = b.jaces[0];
    two_player_to_optional_choice(&mut b, AttackTarget::Planeswalker(jace));
    let pin = ObjectIncarnationRef::from_object(&b.d.state().objects[&jace]);
    let journal_start = b.d.journal_len();
    b.d.decide(true);
    let offered = reselect_prompt(b.d.state(), P0, b.cleric);
    assert_eq!(
        as_set(&offered),
        HashSet::from([AttackTarget::Player(P1), AttackTarget::Planeswalker(jace)])
    );
    b.d.choose(AttackTarget::Player(P1));
    b.d.assert_boundary();
    let combat = b.d.state().combat.as_ref().unwrap();
    assert!(!combat.attacked_permanents.contains_key(&b.cleric));
    assert!(!combat.attacked_permanent_kinds.contains_key(&b.cleric));
    assert_eq!(b.d.attacker(b.cleric), (AttackTarget::Player(P1), P1));
    let commands = membership_commands(b.d.state(), journal_start);
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert_eq!(
        commands[0].edit,
        ResolvedCombatMembershipEdit::Reselect {
            expected_attack_target: AttackTarget::Planeswalker(jace),
            expected_attack_target_identity: Some(pin),
            resulting_attack_target: AttackTarget::Player(P1),
            resulting_attack_target_identity: None,
            resulting_defending_player: P1,
        }
    );

    b.d.finish_combat();
    assert_eq!(b.d.life(P1), 18);
    assert_eq!(loyalty(b.d.state(), jace), Some(3));
}

/// Row 7: with a single legal destination the reselection applies without a prompt; that
/// destination is the current one, so nothing changes and nothing is journaled.
#[test]
fn a_single_legal_destination_applies_without_a_prompt() {
    let mut b = two_player_board(&[]);
    two_player_to_optional_choice(&mut b, AttackTarget::Player(P1));
    let journal_start = b.d.journal_len();
    b.d.decide(true);
    assert!(!b.d.trace.contains(&"EntryAttackTargetChoice"));
    b.d.assert_boundary();
    assert_eq!(b.d.reselect_events(), 1, "the resolver ran");
    assert!(membership_commands(b.d.state(), journal_start).is_empty());
    assert_eq!(b.d.attacker(b.cleric), (AttackTarget::Player(P1), P1));
}

/// Row 6 (R16/R17): entering outside the declare attackers step does not trigger.
fn no_trigger_outside_declare_attackers(card: Card) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let id = add_card(&mut sc, P0, card);
    let mut d = Driver::new(sc.build());
    d.r.cast(id).commit();
    d.trace.push(d.state().waiting_for.variant_name());
    for _ in 0..8 {
        if d.state().stack.is_empty() {
            break;
        }
        d.act(GameAction::PassPriority);
    }
    assert_eq!(d.state().objects[&id].zone, Zone::Battlefield);
    assert!(d.state().stack.is_empty());
    assert!(matches!(d.state().waiting_for, WaitingFor::Priority { .. }));
    assert!(!d.trace.contains(&"OptionalEffectChoice"), "{:?}", d.trace);
    assert_eq!(d.reselect_events(), 0);
}

#[test]
fn misleading_signpost_does_not_trigger_outside_declare_attackers() {
    no_trigger_outside_declare_attackers(Card::Signpost);
}

#[test]
fn portal_mage_does_not_trigger_outside_declare_attackers() {
    no_trigger_outside_declare_attackers(Card::PortalMage);
}

/// The single Reselect command a live answer recorded, the state just before the answer, and
/// the state after it.
fn recorded_reselect(
    d: &mut Driver,
    target: AttackTarget,
) -> (GameState, ResolvedCombatMembershipCommand) {
    let before = d.state().clone();
    let journal_start = d.journal_len();
    d.choose(target);
    let commands = membership_commands(d.state(), journal_start);
    assert_eq!(
        commands.len(),
        1,
        "exactly one Reselect is journaled: {commands:?}"
    );
    (before, commands[0].clone())
}

fn combat_json(state: &GameState) -> serde_json::Value {
    serde_json::to_value(&state.combat).unwrap()
}

fn assert_replays(before: &GameState, command: &ResolvedCombatMembershipCommand, live: &GameState) {
    let mut replay = before.clone();
    apply_resolved_combat_membership(&mut replay, command).expect("the recorded edit replays");
    assert_eq!(combat_json(&replay), combat_json(live));
}

fn assert_precondition_rejected(before: &GameState, command: &ResolvedCombatMembershipCommand) {
    let mut replay = before.clone();
    let untouched = combat_json(&replay);
    let error = apply_resolved_combat_membership(&mut replay, command)
        .expect_err("a mismatched prior relation must not replay");
    assert!(
        matches!(
            error,
            ResolvedCombatMembershipReplayInvariantError::ReselectPreconditionMismatch { .. }
        ),
        "{error:?}"
    );
    assert_eq!(combat_json(&replay), untouched, "a rejection edits nothing");
}

fn with_edit(
    command: &ResolvedCombatMembershipCommand,
    edit: impl FnOnce(&mut ResolvedCombatMembershipEdit),
) -> ResolvedCombatMembershipCommand {
    let mut command = command.clone();
    edit(&mut command.edit);
    command
}

/// Record `command` on a copy of `state`'s journal and decode it, reaching the
/// serialized-authority validator.
fn decode_with(state: &GameState, command: ResolvedCombatMembershipCommand) -> Result<(), String> {
    let mut journal = state.resolved_rules_journal.clone();
    journal.record_combat_membership(command).unwrap();
    serde_json::from_value::<ResolvedRulesJournal>(serde_json::to_value(journal).unwrap())
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Row 5: the journaled reselection replays exactly; the applier refuses a mismatched prior
/// relation; the serialized-authority validator refuses incoherent edits.
#[test]
fn reselect_journal_replays_and_validates() {
    // Player → player (row-1 board).
    let mut b = cleric_board(Card::Signpost, |_| {});
    cleric_to_optional_choice(&mut b);
    b.d.decide(true);
    let (before, command) = recorded_reselect(&mut b.d, AttackTarget::Player(P2));
    assert_replays(&before, &command, b.d.state());
    assert_precondition_rejected(
        &before,
        &with_edit(&command, |edit| {
            if let ResolvedCombatMembershipEdit::Reselect {
                expected_attack_target,
                ..
            } = edit
            {
                *expected_attack_target = AttackTarget::Player(P2);
            }
        }),
    );

    let live = b.d.state();
    assert_eq!(decode_with(live, command.clone()), Ok(()), "reach guard");
    let cleric_pin = ObjectIncarnationRef::from_object(&live.objects[&b.cleric]);
    let cases = [
        (
            with_edit(&command, |edit| {
                if let ResolvedCombatMembershipEdit::Reselect {
                    resulting_attack_target,
                    ..
                } = edit
                {
                    *resulting_attack_target = AttackTarget::Player(P1);
                }
            }),
            "combat-membership reselection changes no attacked relation",
        ),
        (
            with_edit(&command, |edit| {
                if let ResolvedCombatMembershipEdit::Reselect {
                    expected_attack_target_identity,
                    ..
                } = edit
                {
                    *expected_attack_target_identity = Some(cleric_pin);
                }
            }),
            "combat-membership reselection has incoherent prior identity",
        ),
        (
            with_edit(&command, |edit| {
                if let ResolvedCombatMembershipEdit::Reselect {
                    resulting_attack_target,
                    ..
                } = edit
                {
                    *resulting_attack_target = AttackTarget::Planeswalker(b.cleric);
                }
            }),
            "combat-membership reselection has incoherent resulting identity",
        ),
        (
            with_edit(&command, |edit| {
                if let ResolvedCombatMembershipEdit::Reselect {
                    resulting_attack_target,
                    resulting_attack_target_identity,
                    ..
                } = edit
                {
                    *resulting_attack_target = AttackTarget::Planeswalker(b.card);
                    *resulting_attack_target_identity = Some(cleric_pin);
                }
            }),
            "combat-membership reselection has incoherent resulting identity",
        ),
        // V9: CR 508.5 + CR 508.7c — a creature reselected to attack P2 has P2 as its
        // defending player.
        (
            with_edit(&command, |edit| {
                if let ResolvedCombatMembershipEdit::Reselect {
                    resulting_defending_player,
                    ..
                } = edit
                {
                    *resulting_defending_player = P1;
                }
            }),
            "combat-membership reselection has mismatched defending player",
        ),
    ];
    for (bad, message) in cases {
        let error = decode_with(live, bad).expect_err(message);
        assert!(error.contains(message), "{error}");
    }

    // Planeswalker → player (row-9 board).
    let mut b = two_player_board(&[P1]);
    let jace = b.jaces[0];
    two_player_to_optional_choice(&mut b, AttackTarget::Planeswalker(jace));
    b.d.decide(true);
    let (before, command) = recorded_reselect(&mut b.d, AttackTarget::Player(P1));
    assert_replays(&before, &command, b.d.state());
    assert_precondition_rejected(
        &before,
        &with_edit(&command, |edit| {
            if let ResolvedCombatMembershipEdit::Reselect {
                expected_attack_target_identity: Some(pin),
                ..
            } = edit
            {
                pin.incarnation += 1;
            }
        }),
    );

    // CR 506.4c: a retired (unpinned) planeswalker relation reselected onto that pinned
    // planeswalker is coherent.
    let jace_pin = ObjectIncarnationRef::from_object(&b.d.state().objects[&jace]);
    let retired_to_pinned = with_edit(&command, |edit| {
        *edit = ResolvedCombatMembershipEdit::Reselect {
            expected_attack_target: AttackTarget::Planeswalker(jace),
            expected_attack_target_identity: None,
            resulting_defending_player: P1,
            resulting_attack_target: AttackTarget::Planeswalker(jace),
            resulting_attack_target_identity: Some(jace_pin),
        };
    });
    assert_eq!(decode_with(b.d.state(), retired_to_pinned), Ok(()));
}

/// The three-player Fiend Binder board of row 11: P1 controls Grizzly Bears and Jace; P2
/// controls no creature.
struct BinderBoard {
    d: Driver,
    binder: ObjectId,
    bears: ObjectId,
    jace: ObjectId,
}

fn binder_trigger_targets(state: &GameState, binder: ObjectId) -> Option<Vec<TargetRef>> {
    state
        .stack
        .iter()
        .filter(|entry| entry.source_id == binder)
        .find_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility { ability, .. } => Some(ability.targets.clone()),
            _ => None,
        })
}

/// Attack P1 with the Binder (its trigger targets the Bears), have P1 cast Signpost in response
/// and answer its optional question with `accept`.
fn binder_reselect(accept: bool) -> BinderBoard {
    let mut sc = GameScenario::new_n_player(3, 11);
    sc.at_phase(Phase::PreCombatMain);
    let binder = sc
        .add_creature_from_oracle(P0, "Fiend Binder", 3, 2, FIEND_BINDER)
        .id();
    let card = add_card(&mut sc, P1, Card::Signpost);
    let bears = sc.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let jace = add_jace(&mut sc, P1);
    let mut b = BinderBoard {
        d: Driver::new(sc.build()),
        binder,
        bears,
        jace,
    };
    b.d.advance_to_combat();
    b.d.declare(binder, AttackTarget::Player(P1), Some(bears));
    assert_eq!(
        binder_trigger_targets(b.d.state(), binder),
        Some(vec![TargetRef::Object(bears)]),
        "the Binder's trigger is on the stack targeting the Bears"
    );
    b.d.pass_to(P1);
    b.d.cast_to_optional_choice(card, binder);
    assert_optional_choice(b.d.state(), P1);
    assert!(
        binder_trigger_targets(b.d.state(), binder).is_some(),
        "the Binder's trigger is still on the stack under the reselection"
    );
    b.d.decide(accept);
    b
}

/// Resolve the Binder's trigger and return whether it tapped the Bears.
fn settle_binder(b: &mut BinderBoard) -> bool {
    let events_before = b.d.events.len();
    b.d.resolve_stack();
    assert!(b.d.state().stack.is_empty());
    let tapped_event = b.d.events[events_before..].iter().any(|event| {
        matches!(event, GameEvent::PermanentTapped { object_id, .. } if *object_id == b.bears)
    });
    assert_eq!(tapped_event, b.d.state().objects[&b.bears].tapped);
    tapped_event
}

/// Row 11 (R1/R7/R8): reselected to P2, the Binder's defending player is P2, so the Bears (P1's)
/// are no longer a legal target at the CR 608.2b re-check and are not tapped.
#[test]
fn fiend_binder_trigger_loses_its_target_after_reselecting_to_another_player() {
    let mut b = binder_reselect(true);
    let offered = reselect_prompt(b.d.state(), P1, b.binder);
    assert!(offered.contains(&AttackTarget::Player(P2)));
    b.d.choose(AttackTarget::Player(P2));
    assert_eq!(b.d.attacker(b.binder), (AttackTarget::Player(P2), P2));
    assert!(!settle_binder(&mut b), "the Bears stay untapped");
    assert!(matches!(
        b.d.state().waiting_for,
        WaitingFor::Priority { player: P0 }
    ));
}

/// Row 11a: paired positive — declining keeps P1 the defending player and the Bears are tapped.
#[test]
fn fiend_binder_trigger_taps_when_the_reselection_is_declined() {
    let mut b = binder_reselect(false);
    assert_eq!(b.d.attacker(b.binder), (AttackTarget::Player(P1), P1));
    assert!(settle_binder(&mut b));
}

/// Row 11b: CR 508.5 — reselected to P1's own planeswalker, the defending player is still P1
/// (the planeswalker's controller), so the Bears are tapped.
#[test]
fn fiend_binder_trigger_taps_after_reselecting_to_the_same_players_planeswalker() {
    let mut b = binder_reselect(true);
    b.d.choose(AttackTarget::Planeswalker(b.jace));
    assert_eq!(
        b.d.attacker(b.binder),
        (AttackTarget::Planeswalker(b.jace), P1)
    );
    assert!(settle_binder(&mut b));
}

/// Row 15: CR 508.7b — the reselection ignores the restrictions (CR 508.1c) and costs
/// (CR 508.1h) of declaring attackers on the new destination.
fn reselect_ignores_declaration_rules(prison: bool) {
    let mut b = cleric_board(Card::Signpost, |sc| {
        sc.add_basic_land(P0, ManaColor::White);
        sc.add_basic_land(P0, ManaColor::White);
        if prison {
            sc.add_enchantment_from_oracle(P2, "Ghostly Prison", GHOSTLY_PRISON);
        } else {
            sc.add_creature_from_oracle(P2, "Blazing Archon", 5, 6, ARCHON)
                .from_oracle_text_with_keywords(&["Flying"], ARCHON);
        }
    });
    b.d.advance_to_combat();
    // Reach guard: the declaration-time rules really do apply to attacking P2.
    {
        let state = b.d.state();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::DeclareAttackers { .. }
        ));
        let to_p1 = [(b.cleric, AttackTarget::Player(P1))];
        let to_p2 = [(b.cleric, AttackTarget::Player(P2))];
        if prison {
            let (total, _) = compute_attack_tax(state, &to_p2).expect("attacking P2 is taxed");
            assert_eq!(total.mana_value(), 2);
            assert!(
                compute_attack_tax(state, &to_p1).is_none(),
                "attacking P1 is free"
            );
        } else {
            let WaitingFor::DeclareAttackers {
                valid_attack_targets_by_attacker,
                ..
            } = &state.waiting_for
            else {
                unreachable!()
            };
            if let Some(map) = valid_attack_targets_by_attacker {
                let targets = map.get(&b.cleric).expect("the Cleric can attack");
                assert!(targets.contains(&AttackTarget::Player(P1)));
                assert!(!targets.contains(&AttackTarget::Player(P2)));
            }
            assert!(
                validate_attack_declaration(state, &to_p2, &[]).is_err(),
                "CR 508.1c: creatures can't attack P2"
            );
        }
    }
    b.d.declare(b.cleric, AttackTarget::Player(P1), None);
    b.d.resolve_stack();
    b.d.pass_to(P1);
    b.d.cast_to_optional_choice(b.card, b.cleric);
    assert_optional_choice(b.d.state(), P1);
    let p0 = |state: &GameState| {
        let player = state.players.iter().find(|p| p.id == P0).unwrap();
        let untapped_lands = state
            .battlefield
            .iter()
            .filter(|id| {
                let object = &state.objects[id];
                object.controller == P0 && object.name == "Plains" && !object.tapped
            })
            .count();
        (
            serde_json::to_value(&player.mana_pool).unwrap(),
            untapped_lands,
        )
    };
    let p0_before = p0(b.d.state());
    assert_eq!(p0_before.1, 2, "both Plains untapped");
    let journal_start = b.d.journal_len();
    b.d.decide(true);
    let offered = reselect_prompt(b.d.state(), P1, b.cleric);
    assert!(offered.contains(&AttackTarget::Player(P2)), "{offered:?}");
    b.d.choose(AttackTarget::Player(P2));
    b.d.assert_boundary();
    assert!(
        !b.d.trace.contains(&"CombatTaxPayment"),
        "no attack cost is asked: {:?}",
        b.d.trace
    );
    assert_eq!(b.d.attacker(b.cleric), (AttackTarget::Player(P2), P2));
    let commands = membership_commands(b.d.state(), journal_start);
    assert_eq!(commands.len(), 1, "{commands:?}");
    assert!(matches!(
        commands[0].edit,
        ResolvedCombatMembershipEdit::Reselect {
            resulting_attack_target: AttackTarget::Player(P2),
            ..
        }
    ));
    if prison {
        assert_eq!(
            p0(b.d.state()),
            p0_before,
            "no mana is spent on an attack tax"
        );
    }
}

#[test]
fn reselection_ignores_a_cant_attack_restriction() {
    reselect_ignores_declaration_rules(false);
}

#[test]
fn reselection_ignores_an_attack_tax() {
    reselect_ignores_declaration_rules(true);
}

// ---------------------------------------------------------------------------
// CR 508.7a: "that player" after an attack-declaration head reads the player the attack
// DECLARED; the literal "defending player" stays live (CR 508.5). Rulings: "that creature is
// still considered to have attacked the player or permanent as declared, but it is now attacking
// the new player or permanent"; an ability targeting something the "defending player" controls
// loses that target when the defending player changes.
// ---------------------------------------------------------------------------

const MBAKU: &str = "At the beginning of your end step, if there is no monarch, target opponent becomes the monarch.\nWhenever a creature attacks one of your opponents, if that player is the monarch, that creature gets +1/+1 and gains trample until end of turn.";
const DENETHOR: &str = "When Denethor enters, scry 2.\n{3}{R}, {T}, Sacrifice Denethor: Target player becomes the monarch. Denethor deals 3 damage to any target.";
const SEPTIC_RATS: &str = "Infect (This creature deals damage to creatures in the form of -1/-1 counters and to players in the form of poison counters.)\nWhenever this creature attacks, if defending player is poisoned, it gets +1/+1 until end of turn.";
const TREASONOUS_OGRE: &str = "Dethrone (Whenever this creature attacks the player with the most life or tied for most life, put a +1/+1 counter on it.)\nPay 3 life: Add {R}.";
const TEFERIS_PROTECTION: &str = "Until your next turn, your life total can't change and you gain protection from everything. All permanents you control phase out. (While they're phased out, they're treated as though they don't exist. They phase in before you untap during your untap step.)\nExile Teferi's Protection.";

fn power_toughness(state: &GameState, id: ObjectId) -> (Option<i32>, Option<i32>) {
    let object = &state.objects[&id];
    (object.power, object.toughness)
}

fn has_trample(state: &GameState, id: ObjectId) -> bool {
    state.objects[&id].keywords.contains(&Keyword::Trample)
}

fn stack_has_trigger_from(state: &GameState, source: ObjectId) -> bool {
    state.stack.iter().any(|entry| {
        entry.source_id == source && matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
    })
}

/// A three-player board: P0 controls M'Baku and Grizzly Bears and holds Misleading Signpost.
struct MbakuBoard {
    d: Driver,
    mbaku: ObjectId,
    bears: ObjectId,
    signpost: ObjectId,
    denethor: Option<ObjectId>,
}

fn mbaku_board(monarch: PlayerId, with_denethor: bool) -> MbakuBoard {
    let mut sc = GameScenario::new_n_player(3, 17);
    sc.at_phase(Phase::PreCombatMain);
    let mbaku = sc
        .add_creature_from_oracle(P0, "M'Baku, Jabari Chieftain", 4, 3, MBAKU)
        .id();
    let bears = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let signpost = add_card(&mut sc, P0, Card::Signpost);
    let denethor = with_denethor.then(|| {
        sc.add_creature_from_oracle(P0, "Denethor, Stone Seer", 1, 3, DENETHOR)
            .id()
    });
    let mut r = sc.build();
    r.state_mut().monarch = Some(monarch);
    MbakuBoard {
        d: Driver::new(r),
        mbaku,
        bears,
        signpost,
        denethor,
    }
}

/// Attack `defender` with the Bears; M'Baku's trigger goes on the stack.
fn mbaku_attack(b: &mut MbakuBoard, defender: PlayerId) {
    b.d.advance_to_combat();
    b.d.declare(b.bears, AttackTarget::Player(defender), None);
    assert!(
        stack_has_trigger_from(b.d.state(), b.mbaku),
        "reach guard: M'Baku's trigger is on the stack (its fire-time check passed)"
    );
}

/// P0 casts Signpost targeting the Bears; `reselect` answers the optional question (and the
/// destination prompt when it is `Some`). M'Baku's trigger then resolves.
fn mbaku_reselect_and_resolve(b: &mut MbakuBoard, reselect: Option<PlayerId>) {
    b.d.cast_to_optional_choice(b.signpost, b.bears);
    assert_optional_choice(b.d.state(), P0);
    assert!(
        stack_has_trigger_from(b.d.state(), b.mbaku),
        "M'Baku's trigger is still on the stack under the reselection"
    );
    match reselect {
        Some(player) => {
            b.d.decide(true);
            let offered = reselect_prompt(b.d.state(), P0, b.bears);
            assert!(
                offered.contains(&AttackTarget::Player(player)),
                "{offered:?}"
            );
            b.d.choose(AttackTarget::Player(player));
        }
        None => b.d.decide(false),
    }
    b.d.resolve_stack();
    b.d.assert_boundary();
}

/// V1: CR 508.7a + CR 603.4 — M'Baku's "that player is the monarch" is the player the Bears
/// were declared attacking (P1, the monarch), so the reselection to P2 does not stop the pump.
#[test]
fn mbaku_reads_the_declared_player_after_a_reselection() {
    let mut b = mbaku_board(P1, false);
    mbaku_attack(&mut b, P1);
    mbaku_reselect_and_resolve(&mut b, Some(P2));
    assert_eq!(b.d.attacker(b.bears), (AttackTarget::Player(P2), P2));
    assert_eq!(b.d.state().monarch, Some(P1));
    assert_eq!(
        power_toughness(b.d.state(), b.bears),
        (Some(3), Some(3)),
        "the declared player (P1) is the monarch, so the Bears get +1/+1"
    );
    assert!(has_trample(b.d.state(), b.bears));
}

/// V1 paired positive: declining keeps the pump.
#[test]
fn mbaku_pumps_when_the_reselection_is_declined() {
    let mut b = mbaku_board(P1, false);
    mbaku_attack(&mut b, P1);
    mbaku_reselect_and_resolve(&mut b, None);
    assert_eq!(b.d.attacker(b.bears), (AttackTarget::Player(P1), P1));
    assert_eq!(power_toughness(b.d.state(), b.bears), (Some(3), Some(3)));
    assert!(has_trample(b.d.state(), b.bears));
}

/// V1b: CR 508.7a + CR 603.4 — the resolution recheck reads ONLY the declared player. The Bears
/// were declared attacking P2 (the monarch at fire time); Denethor then makes P1 the monarch and
/// the Bears are reselected to P1. The declared player (P2) is no longer the monarch, so the
/// trigger does nothing, even though the player the Bears are now attacking is the monarch.
#[test]
fn mbaku_ignores_the_live_defender_after_a_reselection() {
    let mut b = mbaku_board(P2, true);
    let denethor = b.denethor.unwrap();
    mbaku_attack(&mut b, P2);

    // {3}{R}: funded from the pool in the declare attackers step (CR 500.5 empties the pool
    // between steps, and the activation does not tap sources).
    for _ in 0..4 {
        let _ =
            b.d.r
                .state_mut()
                .add_mana_to_pool(P0, ManaUnit::new(ManaType::Red, ObjectId(0), false, vec![]));
    }
    // Denethor: "Target player becomes the monarch. Denethor deals 3 damage to any target."
    b.d.act(GameAction::ActivateAbility {
        source_id: denethor,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    });
    let mut player_targets = [P1, P2].into_iter();
    for _ in 0..8 {
        match b.d.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                let player = player_targets.next().expect("two target slots");
                b.d.act(GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(player)),
                });
            }
            WaitingFor::ManaPayment { .. } => b.d.act(GameAction::PassPriority),
            WaitingFor::PayCost { .. } => b.d.act(GameAction::SelectCards {
                cards: vec![denethor],
            }),
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt while activating Denethor: {other:?}"),
        }
    }
    assert!(
        player_targets.next().is_none(),
        "both of Denethor's targets were chosen"
    );
    while b.d.state().stack.len() > 1 {
        b.d.act(GameAction::PassPriority);
    }
    assert_eq!(b.d.state().monarch, Some(P1), "Denethor crowned P1");
    assert_eq!(b.d.life(P2), 17);
    b.d.pass_to(P0);

    mbaku_reselect_and_resolve(&mut b, Some(P1));
    assert_eq!(b.d.attacker(b.bears), (AttackTarget::Player(P1), P1));
    assert_eq!(
        b.d.state().monarch,
        Some(P1),
        "the creature's live defender is the monarch"
    );
    assert_eq!(
        power_toughness(b.d.state(), b.bears),
        (Some(2), Some(2)),
        "the declared player (P2) is not the monarch, so the trigger does nothing"
    );
    assert!(!has_trample(b.d.state(), b.bears));
}

/// An `players`-player board: P0 controls the attacker (built by `add`) and holds Signpost.
struct SoloBoard {
    d: Driver,
    attacker: ObjectId,
    signpost: ObjectId,
}

fn solo_board(
    players: u8,
    add: impl FnOnce(&mut GameScenario) -> ObjectId,
    setup: impl FnOnce(&mut GameState),
) -> SoloBoard {
    let mut sc = GameScenario::new_n_player(players, 23);
    sc.at_phase(Phase::PreCombatMain);
    let attacker = add(&mut sc);
    let signpost = add_card(&mut sc, P0, Card::Signpost);
    let mut r = sc.build();
    setup(r.state_mut());
    SoloBoard {
        d: Driver::new(r),
        attacker,
        signpost,
    }
}

/// Attack `defender`, have P0 cast Signpost on the attacker and answer with `reselect`, then
/// resolve the stack.
fn solo_reselect_and_resolve(b: &mut SoloBoard, defender: PlayerId, reselect: Option<PlayerId>) {
    b.d.advance_to_combat();
    b.d.declare(b.attacker, AttackTarget::Player(defender), None);
    assert!(
        stack_has_trigger_from(b.d.state(), b.attacker),
        "reach guard: the attack trigger is on the stack"
    );
    b.d.cast_to_optional_choice(b.signpost, b.attacker);
    match reselect {
        Some(player) => {
            b.d.decide(true);
            b.d.choose(AttackTarget::Player(player));
        }
        None => b.d.decide(false),
    }
    b.d.resolve_stack();
    b.d.assert_boundary();
}

fn septic_rats_board() -> SoloBoard {
    solo_board(
        3,
        |sc| {
            sc.add_creature_from_oracle(P0, "Septic Rats", 2, 2, SEPTIC_RATS)
                .from_oracle_text_with_keywords(&["Infect"], SEPTIC_RATS)
                .id()
        },
        |state| state.players[1].poison_counters = 1,
    )
}

/// V2: CR 508.5 + CR 603.4 — the literal "defending player" stays live: after the Rats are
/// reselected from poisoned P1 to clean P2, the resolution recheck fails.
#[test]
fn septic_rats_defending_player_reads_the_reselected_player() {
    let mut b = septic_rats_board();
    solo_reselect_and_resolve(&mut b, P1, Some(P2));
    assert_eq!(b.d.attacker(b.attacker), (AttackTarget::Player(P2), P2));
    assert_eq!(power_toughness(b.d.state(), b.attacker), (Some(2), Some(2)));
}

/// V2 paired positive: declining keeps P1 the defending player, and the Rats get +1/+1.
#[test]
fn septic_rats_pumps_when_the_reselection_is_declined() {
    let mut b = septic_rats_board();
    solo_reselect_and_resolve(&mut b, P1, None);
    assert_eq!(power_toughness(b.d.state(), b.attacker), (Some(3), Some(3)));
}

fn treasonous_ogre_board() -> SoloBoard {
    solo_board(
        3,
        |sc| {
            sc.add_creature_from_oracle(P0, "Treasonous Ogre", 2, 3, TREASONOUS_OGRE)
                .from_oracle_text_with_keywords(&["Dethrone"], TREASONOUS_OGRE)
                .id()
        },
        |state| state.players[1].life = 25,
    )
}

fn plus_counters(state: &GameState, id: ObjectId) -> u32 {
    state.objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

/// V5a: CR 702.105a + CR 508.7a — Dethrone's "the player with the most life" is the player the
/// Ogre was declared attacking (P1, 25 life). Reselecting to P2 (20 life) does not stop the
/// counter (Dethrone ruling: once it triggers, the counter is put on).
#[test]
fn dethrone_reads_the_declared_player_after_a_reselection() {
    let mut b = treasonous_ogre_board();
    solo_reselect_and_resolve(&mut b, P1, Some(P2));
    assert_eq!(b.d.attacker(b.attacker), (AttackTarget::Player(P2), P2));
    assert_eq!(plus_counters(b.d.state(), b.attacker), 1);
}

/// V5a hostile pair: attacking P2 (not the player with the most life) does not trigger.
#[test]
fn dethrone_does_not_trigger_against_a_player_without_the_most_life() {
    let mut b = treasonous_ogre_board();
    b.d.advance_to_combat();
    b.d.declare(b.attacker, AttackTarget::Player(P2), None);
    assert!(!stack_has_trigger_from(b.d.state(), b.attacker));
    b.d.resolve_stack();
    assert_eq!(plus_counters(b.d.state(), b.attacker), 0);
}

/// V6: CR 702.26b — a phased-out planeswalker is treated as though it does not exist, so it is
/// not a reselection destination. P1 casts Teferi's Protection (Jace phases out), then Signpost:
/// the only destination is P1, which is the current one, so no prompt appears and nothing is
/// journaled. Without Teferi's Protection, Jace is offered.
fn phased_out_planeswalker_reselection(protection: bool) {
    let mut sc = GameScenario::new();
    sc.at_phase(Phase::PreCombatMain);
    let cleric = add_cleric(&mut sc);
    let jace = add_jace(&mut sc, P1);
    let signpost = add_card(&mut sc, P1, Card::Signpost);
    let teferi = sc
        .add_spell_to_hand_from_oracle(P1, "Teferi's Protection", true, TEFERIS_PROTECTION)
        .id();
    let mut d = Driver::new(sc.build());
    d.advance_to_combat();
    d.declare(cleric, AttackTarget::Player(P1), None);
    d.resolve_stack();
    if protection {
        d.pass_to(P1);
        d.r.cast(teferi).commit();
        d.trace.push(d.state().waiting_for.variant_name());
        d.resolve_stack();
        assert!(
            d.state().objects[&jace].is_phased_out(),
            "reach guard: Jace phased out"
        );
    }
    d.pass_to(P1);
    d.cast_to_optional_choice(signpost, cleric);
    assert_optional_choice(d.state(), P1);
    let journal_start = d.journal_len();
    d.decide(true);
    if protection {
        assert!(
            !d.trace.contains(&"EntryAttackTargetChoice"),
            "no prompt: {:?}",
            d.trace
        );
        d.assert_boundary();
        assert_eq!(d.attacker(cleric), (AttackTarget::Player(P1), P1));
        assert!(membership_commands(d.state(), journal_start).is_empty());
        assert!(d.state().objects[&jace].is_phased_out());
    } else {
        assert_eq!(
            as_set(&reselect_prompt(d.state(), P1, cleric)),
            HashSet::from([AttackTarget::Player(P1), AttackTarget::Planeswalker(jace)])
        );
    }
}

#[test]
fn a_phased_out_planeswalker_is_not_a_reselection_destination() {
    phased_out_planeswalker_reselection(true);
}

#[test]
fn a_phased_in_planeswalker_is_a_reselection_destination() {
    phased_out_planeswalker_reselection(false);
}

/// A `players`-player board: P0's Cleric attacks P2; P0 casts Signpost and accepts, reaching
/// the reselection prompt; P2 then concedes (CR 104.3a) while the prompt is pending.
fn conceded_destination_board(players: u8) -> (Driver, ObjectId, usize) {
    let mut b = solo_board(players, add_cleric, |_| {});
    b.d.advance_to_combat();
    b.d.declare(b.attacker, AttackTarget::Player(P2), None);
    b.d.resolve_stack();
    b.d.cast_to_optional_choice(b.signpost, b.attacker);
    let journal_start = b.d.journal_len();
    b.d.decide(true);
    let offered = reselect_prompt(b.d.state(), P0, b.attacker);
    assert!(offered.contains(&AttackTarget::Player(P2)), "{offered:?}");
    b.d.act(GameAction::Concede { player_id: P2 });
    assert!(b.d.state().eliminated_players.contains(&P2));
    // The pending prompt still offers the conceded player.
    assert_eq!(reselect_prompt(b.d.state(), P0, b.attacker), offered);
    let offered_actions: Vec<String> = legal_actions(b.d.state())
        .iter()
        .map(|action| format!("{action:?}"))
        .collect();
    assert!(
        offered_actions.contains(&format!(
            "{:?}",
            GameAction::ChooseEntryAttackTarget {
                target: AttackTarget::Player(P2)
            }
        )),
        "{offered_actions:?}"
    );
    (b.d, b.attacker, journal_start)
}

/// V7: CR 608.2d + CR 508.7c + CR 800.4a — the chosen destination left the game while the
/// choice was pending; the one remaining legal destination (P1) is forced.
fn conceded_destination_single_remaining(choice: PlayerId) {
    let (mut d, cleric, journal_start) = conceded_destination_board(3);
    d.choose(AttackTarget::Player(choice));
    d.assert_boundary();
    assert_eq!(d.attacker(cleric), (AttackTarget::Player(P1), P1));
    let commands = membership_commands(d.state(), journal_start);
    let reselects = commands
        .iter()
        .filter(|command| matches!(command.edit, ResolvedCombatMembershipEdit::Reselect { .. }))
        .count();
    assert_eq!(reselects, 1, "{commands:?}");
}

#[test]
fn a_conceded_destination_resolves_to_the_single_remaining_one() {
    conceded_destination_single_remaining(P2);
}

#[test]
fn choosing_the_remaining_destination_after_a_concession_reselects_it() {
    conceded_destination_single_remaining(P1);
}

/// V8: CR 608.2d + CR 508.7c — with several destinations still legal after the chosen one left
/// the game, the choice is offered again among exactly those.
#[test]
fn a_conceded_destination_with_several_remaining_reprompts() {
    let (mut d, cleric, _) = conceded_destination_board(4);
    let p3 = PlayerId(3);
    d.choose(AttackTarget::Player(P2));
    assert_eq!(
        as_set(&reselect_prompt(d.state(), P0, cleric)),
        HashSet::from([AttackTarget::Player(P1), AttackTarget::Player(p3)])
    );
    // A destination never offered is rejected and changes nothing.
    let waiting_before = d.state().waiting_for.clone();
    let combat_before = combat_json(d.state());
    assert!(d
        .r
        .act(GameAction::ChooseEntryAttackTarget {
            target: AttackTarget::Player(P0)
        })
        .is_err());
    assert_eq!(d.state().waiting_for, waiting_before);
    assert_eq!(combat_json(d.state()), combat_before);

    d.choose(AttackTarget::Player(p3));
    d.assert_boundary();
    assert_eq!(d.attacker(cleric), (AttackTarget::Player(p3), p3));
}

// ---------------------------------------------------------------------------
// The body of an attack-declaration trigger: "that player" is the player the attack was
// declared against (CR 508.7a), while a literal "defending player" stays live (CR 508.5).
// ---------------------------------------------------------------------------

const GORNOG: &str = "Haste\nCowards can't block Warriors.\nWhenever one or more Warriors you control attack a player, target creature that player controls becomes a Coward.\nAttacking Warriors you control get +X/+0, where X is the number of Cowards your opponents control.";
const SOARING_LIGHTBRINGER: &str = "Flying\nOther enchantment creatures you control have flying.\nWhenever you attack a player, create a 1/1 white Glimmer enchantment creature token that's tapped and attacking that player.";
const SIEGE_DRAGON: &str = "Flying\nWhen this creature enters, destroy all Walls your opponents control.\nWhenever this creature attacks, if defending player controls no Walls, it deals 2 damage to each creature without flying that player controls.";
const ARCHNEMESIS: &str = "Enchant opponent\nWhenever you attack enchanted player, that player loses 2 life. You draw a card and gain 2 life.\nWhenever a player attacks you, you may attach this Aura to that player.";
const SERAPHIC_GREATSWORD: &str = "Equipped creature gets +2/+2.\nWhenever equipped creature attacks the player with the most life or tied for most life, create a 4/4 white Angel creature token with flying that's tapped and attacking that player.\nEquip {4}";
const NAMOR: &str = "Flying\nWhenever you cast a noncreature spell, create a 1/1 blue Merfolk creature token.\nWhenever Namor attacks a player who has more life than you, other creatures you control attacking that player get +2/+0 until end of turn.";

/// After the attack trigger is on the stack, P0 casts Misleading Signpost on `attacker` and
/// answers with `reselect`; then the stack resolves to the active player's priority.
fn reselect_under_trigger_and_resolve(
    d: &mut Driver,
    signpost: ObjectId,
    attacker: ObjectId,
    trigger_source: ObjectId,
    reselect: Option<PlayerId>,
) {
    assert!(
        stack_has_trigger_from(d.state(), trigger_source),
        "reach guard: the attack trigger is on the stack"
    );
    d.cast_to_optional_choice(signpost, attacker);
    assert!(
        stack_has_trigger_from(d.state(), trigger_source),
        "the attack trigger is still on the stack under the reselection"
    );
    match reselect {
        Some(player) => {
            d.decide(true);
            d.choose(AttackTarget::Player(player));
        }
        None => d.decide(false),
    }
    d.resolve_stack();
    d.assert_boundary();
}

fn subtypes(state: &GameState, id: ObjectId) -> Vec<String> {
    state.objects[&id].card_types.subtypes.clone()
}

fn tokens_named(state: &GameState, name: &str) -> Vec<ObjectId> {
    let mut ids: Vec<ObjectId> = state
        .objects
        .values()
        .filter(|o| o.zone == Zone::Battlefield && o.is_token && o.name == name)
        .map(|o| o.id)
        .collect();
    ids.sort();
    ids
}

fn on_battlefield(state: &GameState, id: ObjectId) -> bool {
    state.objects[&id].zone == Zone::Battlefield
}

/// The three-player Gornog board: P0 controls Gornog and holds Signpost; P1 and P2 each control
/// Grizzly Bears. Gornog attacks P1 and its trigger targets P1's Bears.
struct GornogBoard {
    d: Driver,
    gornog: ObjectId,
    p1_bears: ObjectId,
    p2_bears: ObjectId,
}

fn gornog_attack_and_resolve(reselect: Option<PlayerId>) -> GornogBoard {
    let mut sc = GameScenario::new_n_player(3, 29);
    sc.at_phase(Phase::PreCombatMain);
    let gornog = sc
        .add_creature_from_oracle(P0, "Gornog, the Red Reaper", 2, 3, GORNOG)
        .with_subtypes(vec!["Minotaur", "Warrior"])
        .from_oracle_text_with_keywords(&["Haste"], GORNOG)
        .id();
    let signpost = add_card(&mut sc, P0, Card::Signpost);
    let p1_bears = sc.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let p2_bears = sc.add_creature(P2, "Grizzly Bears", 2, 2).id();
    let mut d = Driver::new(sc.build());
    d.advance_to_combat();
    d.declare(gornog, AttackTarget::Player(P1), Some(p1_bears));
    reselect_under_trigger_and_resolve(&mut d, signpost, gornog, gornog, reselect);
    GornogBoard {
        d,
        gornog,
        p1_bears,
        p2_bears,
    }
}

/// V10: CR 508.7a + CR 608.2b — Gornog's "target creature that player controls" names the
/// player Gornog was declared attacking (P1). Reselecting Gornog to P2 leaves P1's Bears a legal
/// target, so they become a Coward; reading the live defender (P2) would make the target illegal
/// and the trigger would do nothing.
#[test]
fn gornog_target_stays_legal_after_a_reselection() {
    let b = gornog_attack_and_resolve(Some(P2));
    assert_eq!(b.d.attacker(b.gornog), (AttackTarget::Player(P2), P2));
    assert_eq!(
        subtypes(b.d.state(), b.p1_bears),
        vec!["Coward".to_string()],
        "the declared player's creature becomes a Coward"
    );
    assert_eq!(
        subtypes(b.d.state(), b.p2_bears),
        Vec::<String>::new(),
        "the reselected player's creature is untouched"
    );
}

/// V10 paired positive: declining keeps the Coward.
#[test]
fn gornog_target_becomes_a_coward_when_the_reselection_is_declined() {
    let b = gornog_attack_and_resolve(None);
    assert_eq!(b.d.attacker(b.gornog), (AttackTarget::Player(P1), P1));
    assert_eq!(
        subtypes(b.d.state(), b.p1_bears),
        vec!["Coward".to_string()]
    );
}

/// V11: CR 508.7a + CR 508.4 — Soaring Lightbringer's Glimmer enters attacking "that player",
/// the player the Bears were declared attacking (P1), even after the Bears are reselected to P2.
fn lightbringer_attack_and_resolve(reselect: Option<PlayerId>) -> (Driver, ObjectId) {
    let mut sc = GameScenario::new_n_player(3, 31);
    sc.at_phase(Phase::PreCombatMain);
    let lightbringer = sc
        .add_creature_from_oracle(P0, "Soaring Lightbringer", 4, 5, SOARING_LIGHTBRINGER)
        .from_oracle_text_with_keywords(&["Flying"], SOARING_LIGHTBRINGER)
        .id();
    let bears = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let signpost = add_card(&mut sc, P0, Card::Signpost);
    let mut d = Driver::new(sc.build());
    d.advance_to_combat();
    d.declare(bears, AttackTarget::Player(P1), None);
    reselect_under_trigger_and_resolve(&mut d, signpost, bears, lightbringer, reselect);
    (d, bears)
}

#[test]
fn lightbringer_glimmer_attacks_the_declared_player_after_a_reselection() {
    let (d, bears) = lightbringer_attack_and_resolve(Some(P2));
    assert_eq!(d.attacker(bears), (AttackTarget::Player(P2), P2));
    let glimmers = tokens_named(d.state(), "Glimmer");
    assert_eq!(glimmers.len(), 1, "reach guard: one Glimmer");
    assert_eq!(d.attacker(glimmers[0]), (AttackTarget::Player(P1), P1));
}

/// V11 paired positive: declining, the Glimmer attacks P1.
#[test]
fn lightbringer_glimmer_attacks_the_declared_player_when_the_reselection_is_declined() {
    let (d, bears) = lightbringer_attack_and_resolve(None);
    assert_eq!(d.attacker(bears), (AttackTarget::Player(P1), P1));
    let glimmers = tokens_named(d.state(), "Glimmer");
    assert_eq!(glimmers.len(), 1, "reach guard: one Glimmer");
    assert_eq!(d.attacker(glimmers[0]), (AttackTarget::Player(P1), P1));
}

/// V12: CR 508.5 — Siege Dragon's "that player" anaphors the literal "defending player", which
/// follows the reselection (Signpost ruling), so the damage hits the reselected player's (P2)
/// creatures. An over-broad as-declared fix would damage P1's instead.
fn siege_dragon_attack_and_resolve(reselect: Option<PlayerId>) -> (Driver, ObjectId, ObjectId) {
    let mut sc = GameScenario::new_n_player(3, 37);
    sc.at_phase(Phase::PreCombatMain);
    let dragon = sc
        .add_creature_from_oracle(P0, "Siege Dragon", 5, 5, SIEGE_DRAGON)
        .from_oracle_text_with_keywords(&["Flying"], SIEGE_DRAGON)
        .id();
    let signpost = add_card(&mut sc, P0, Card::Signpost);
    let p1_bears = sc.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let p2_bears = sc.add_creature(P2, "Grizzly Bears", 2, 2).id();
    let mut d = Driver::new(sc.build());
    d.advance_to_combat();
    d.declare(dragon, AttackTarget::Player(P1), None);
    reselect_under_trigger_and_resolve(&mut d, signpost, dragon, dragon, reselect);
    (d, p1_bears, p2_bears)
}

#[test]
fn siege_dragon_damages_the_reselected_defending_players_creatures() {
    let (d, p1_bears, p2_bears) = siege_dragon_attack_and_resolve(Some(P2));
    assert!(!on_battlefield(d.state(), p2_bears), "P2's Bears die");
    assert!(on_battlefield(d.state(), p1_bears), "P1's Bears live");
}

/// V12 paired positive: declining, P1's Bears die.
#[test]
fn siege_dragon_damages_the_declared_defenders_creatures_when_the_reselection_is_declined() {
    let (d, p1_bears, p2_bears) = siege_dragon_attack_and_resolve(None);
    assert!(!on_battlefield(d.state(), p1_bears), "P1's Bears die");
    assert!(on_battlefield(d.state(), p2_bears), "P2's Bears live");
}

/// The life totals of P0, P1 and P2.
fn lives(d: &Driver) -> [i32; 3] {
    [d.life(P0), d.life(P1), d.life(P2)]
}

/// V13: CR 303.4b + CR 508.7a — Archnemesis's "that player" is the enchanted player the attack
/// was declared against (P1). Returns the driver, the life totals before combat, and P0's hand
/// size just before the attack trigger resolved (after Signpost left it).
fn archnemesis_attack_and_resolve(reselect: Option<PlayerId>) -> (Driver, [i32; 3], usize) {
    use engine::game::effects::attach::attach_to_player;
    use engine::game::layers::evaluate_layers;
    use engine::game::trigger_index::reindex_object_triggers;

    let mut sc = GameScenario::new_n_player(3, 41);
    sc.at_phase(Phase::PreCombatMain);
    let archnemesis = {
        let mut builder = sc.add_creature(P0, "Archnemesis", 0, 0);
        builder.as_enchantment();
        builder.with_subtypes(vec!["Aura"]);
        builder.from_oracle_text(ARCHNEMESIS);
        builder.id()
    };
    let bears = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let signpost = add_card(&mut sc, P0, Card::Signpost);
    for _ in 0..4 {
        sc.add_card_to_library_top(P0, "Plains");
    }
    let mut r = sc.build();
    attach_to_player(r.state_mut(), archnemesis, P1);
    evaluate_layers(r.state_mut());
    reindex_object_triggers(r.state_mut(), archnemesis);
    let mut d = Driver::new(r);
    let before = lives(&d);
    let hand_after_cast = d.state().players[0].hand.len() - 1;
    d.advance_to_combat();
    d.declare(bears, AttackTarget::Player(P1), None);
    reselect_under_trigger_and_resolve(&mut d, signpost, bears, archnemesis, reselect);
    assert_eq!(d.state().phase, Phase::DeclareAttackers, "before blockers");
    (d, before, hand_after_cast)
}

#[test]
fn archnemesis_drains_the_declared_enchanted_player_after_a_reselection() {
    let (d, [p0, p1, p2], hand_after_cast) = archnemesis_attack_and_resolve(Some(P2));
    assert_eq!(lives(&d), [p0 + 2, p1 - 2, p2]);
    assert_eq!(
        d.state().players[0].hand.len(),
        hand_after_cast + 1,
        "the controller draws a card"
    );
}

/// V13 paired positive: declining, P1 loses 2.
#[test]
fn archnemesis_drains_the_enchanted_player_when_the_reselection_is_declined() {
    let (d, [p0, p1, p2], hand_after_cast) = archnemesis_attack_and_resolve(None);
    assert_eq!(lives(&d), [p0 + 2, p1 - 2, p2]);
    assert_eq!(d.state().players[0].hand.len(), hand_after_cast + 1);
}

/// V14: CR 508.7a + CR 508.4 — Seraphic Greatsword's Angel enters attacking "that player", the
/// player with the most life the equipped creature was declared attacking (P1, 25 life), even
/// after the equipped creature is reselected to P2.
fn greatsword_attack_and_resolve(reselect: Option<PlayerId>) -> (Driver, ObjectId) {
    use engine::game::game_object::AttachTarget;

    let mut sc = GameScenario::new_n_player(3, 43);
    sc.at_phase(Phase::PreCombatMain);
    sc.with_life(P1, 25);
    let bears = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let greatsword = sc
        .add_artifact_from_oracle(P0, "Seraphic Greatsword", SERAPHIC_GREATSWORD)
        .with_subtypes(vec!["Equipment"])
        .id();
    let signpost = add_card(&mut sc, P0, Card::Signpost);
    let mut r = sc.build();
    {
        let state = r.state_mut();
        state.objects.get_mut(&greatsword).unwrap().attached_to = Some(AttachTarget::Object(bears));
        state
            .objects
            .get_mut(&bears)
            .unwrap()
            .attachments
            .push(greatsword);
    }
    let mut d = Driver::new(r);
    d.advance_to_combat();
    d.declare(bears, AttackTarget::Player(P1), None);
    reselect_under_trigger_and_resolve(&mut d, signpost, bears, greatsword, reselect);
    (d, bears)
}

#[test]
fn greatsword_angel_attacks_the_declared_player_after_a_reselection() {
    let (d, bears) = greatsword_attack_and_resolve(Some(P2));
    assert_eq!(d.attacker(bears), (AttackTarget::Player(P2), P2));
    let angels = tokens_named(d.state(), "Angel");
    assert_eq!(angels.len(), 1, "reach guard: one Angel");
    assert_eq!(d.attacker(angels[0]), (AttackTarget::Player(P1), P1));
}

/// V14 paired positive: declining, the Angel attacks P1.
#[test]
fn greatsword_angel_attacks_the_declared_player_when_the_reselection_is_declined() {
    let (d, bears) = greatsword_attack_and_resolve(None);
    assert_eq!(d.attacker(bears), (AttackTarget::Player(P1), P1));
    let angels = tokens_named(d.state(), "Angel");
    assert_eq!(angels.len(), 1, "reach guard: one Angel");
    assert_eq!(d.attacker(angels[0]), (AttackTarget::Player(P1), P1));
}

/// V15: CR 508.7a + CR 611.2c — Namor's "other creatures you control attacking that player" is
/// scoped to the player Namor was declared attacking (P1, 25 life). Namor is reselected to P2;
/// the Bears still attack P1, so they get +2/+0. Reading Namor's live defender (P2) would skip
/// them. Casting Signpost also triggers Namor's noncreature-spell ability (a Merfolk token),
/// which resolves before the attack trigger.
fn namor_attack_and_resolve(reselect: Option<PlayerId>) -> (Driver, ObjectId, ObjectId) {
    let mut sc = GameScenario::new_n_player(3, 47);
    sc.at_phase(Phase::PreCombatMain);
    sc.with_life(P1, 25);
    let namor = sc
        .add_creature_from_oracle(P0, "Namor, Atlantean King", 2, 2, NAMOR)
        .from_oracle_text_with_keywords(&["Flying"], NAMOR)
        .id();
    let bears = sc.add_creature(P0, "Grizzly Bears", 2, 2).id();
    let signpost = add_card(&mut sc, P0, Card::Signpost);
    let mut d = Driver::new(sc.build());
    d.advance_to_combat();
    d.declare_all(
        &[
            (namor, AttackTarget::Player(P1)),
            (bears, AttackTarget::Player(P1)),
        ],
        None,
    );
    reselect_under_trigger_and_resolve(&mut d, signpost, namor, namor, reselect);
    assert_eq!(
        tokens_named(d.state(), "Merfolk").len(),
        1,
        "reach guard: the noncreature-spell trigger resolved"
    );
    (d, namor, bears)
}

#[test]
fn namor_pumps_attackers_of_the_declared_player_after_a_reselection() {
    let (d, namor, bears) = namor_attack_and_resolve(Some(P2));
    assert_eq!(d.attacker(namor), (AttackTarget::Player(P2), P2));
    assert_eq!(d.attacker(bears), (AttackTarget::Player(P1), P1));
    assert_eq!(
        power_toughness(d.state(), bears),
        (Some(4), Some(2)),
        "the Bears attack the declared player (P1), so they get +2/+0"
    );
}

/// V15 paired positive: declining, the Bears get +2/+0.
#[test]
fn namor_pumps_attackers_of_the_declared_player_when_the_reselection_is_declined() {
    let (d, namor, bears) = namor_attack_and_resolve(None);
    assert_eq!(d.attacker(namor), (AttackTarget::Player(P1), P1));
    assert_eq!(power_toughness(d.state(), bears), (Some(4), Some(2)));
}
