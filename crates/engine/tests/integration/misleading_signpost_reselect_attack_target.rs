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
use engine::types::events::GameEvent;
use engine::types::game_state::{AttackTargetChoicePurpose, GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::{ObjectId, ObjectIncarnationRef};
use engine::types::mana::ManaColor;
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
        let result = self
            .r
            .declare_attackers(&[(attacker, target)])
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
