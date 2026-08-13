//! Issue #5904 — CR 303.4f + CR 303.4g for a token that is a copy of an Aura.
//!
//! A token copy of an Aura (Yenna, Redtooth Regent; Court of Vantress copying a
//! Curse) never passed through the entry-time `attach_to` slot, so it entered
//! unattached and died to the CR 704.5m unattached-Aura state-based action. It
//! must instead choose a host as it enters (CR 303.4f) — or, when there is no
//! legal host at all, not be created at all (CR 303.4g).

use engine::game::game_object::AttachTarget;
use engine::types::ability::{
    ContinuousModification, ControllerRef, Effect, PtValue, QuantityExpr, TargetFilter, TargetRef,
    TypeFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::keywords::{Keyword, ProtectionTarget};
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;
use engine::types::ObjectId;

use engine::game::engine::{apply, EngineError};
use engine::game::scenario::{GameRunner, GameScenario};

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);

// Oracle text taken verbatim from the shipped card data (`client/public/card-data.json`).
const YENNA: &str =
    "{2}, {T}: Choose target enchantment you control that doesn't have the same name \
as another permanent you control. Create a token that's a copy of it, except it isn't legendary. \
If the token is an Aura, untap Yenna, then scry 2. Activate only as a sorcery.";

const COOPED_UP: &str = "Enchant creature\n\
Enchanted creature can't attack or block.\n\
{2}{W}: Exile enchanted creature.";

const CURSE_OF_BLOODLETTING: &str = "Enchant player\n\
If a source would deal damage to enchanted player, it deals double that damage to that player \
instead.";

const SIGIL_OF_THE_EMPTY_THRONE: &str =
    "Whenever you cast an enchantment spell, create a 4/4 white Angel creature token with flying.";

/// Every battlefield permanent with `name`, id-sorted.
fn battlefield_named(runner: &GameRunner, name: &str) -> Vec<ObjectId> {
    let mut ids: Vec<ObjectId> = runner
        .state()
        .objects
        .iter()
        .filter(|(_, o)| o.zone == Zone::Battlefield && o.name == name)
        .map(|(id, _)| *id)
        .collect();
    ids.sort();
    ids
}

/// What a driven activation observed. Recorded rather than asserted inline so
/// each test names the specific claim it is making.
#[derive(Default)]
struct Observed {
    host_prompts: Vec<(PlayerId, Vec<TargetRef>)>,
    saw_scry: bool,
    events: Vec<GameEvent>,
}

impl Observed {
    fn token_created_ids(&self) -> Vec<ObjectId> {
        self.events
            .iter()
            .filter_map(|event| match event {
                GameEvent::TokenCreated { object_id, .. } => Some(*object_id),
                _ => None,
            })
            .collect()
    }

    fn entered_battlefield_ids(&self) -> Vec<ObjectId> {
        self.events
            .iter()
            .filter_map(|event| match event {
                GameEvent::ZoneChanged {
                    object_id,
                    to: Zone::Battlefield,
                    ..
                } => Some(*object_id),
                _ => None,
            })
            .collect()
    }
}

/// What [`drive`] does when a CR 303.4f host prompt opens.
enum HostAnswers<'a> {
    /// Answer each prompt from this sequence, in prompt order.
    Answer(&'a [TargetRef]),
    /// Return with the prompt still open, for tests that inspect or submit to
    /// the live window themselves.
    HaltAtPrompt,
}

/// Drive an activation to its next priority window.
///
/// `targets` is a POOL, not a sequence: each target-selection window is answered
/// with the first pool entry the engine accepts, which keeps a test from
/// depending on the order the engine happens to declare its slots in. `hosts` IS
/// a sequence — CR 303.4f prompt order is part of what several tests assert.
fn drive(runner: &mut GameRunner, targets: &[TargetRef], hosts: HostAnswers<'_>) -> Observed {
    let mut observed = Observed::default();
    let mut pool: Vec<TargetRef> = targets.to_vec();
    let mut next_host = 0usize;
    for _ in 0..60 {
        match &runner.state().waiting_for {
            WaitingFor::ManaPayment { .. } => {
                observed
                    .events
                    .extend(runner.act(GameAction::PassPriority).expect("mana").events);
            }
            WaitingFor::TargetSelection { .. } => {
                let accepted = pool.iter().position(|candidate| {
                    match runner.act(GameAction::ChooseTarget {
                        target: Some(candidate.clone()),
                    }) {
                        Ok(result) => {
                            observed.events.extend(result.events);
                            true
                        }
                        Err(_) => false,
                    }
                });
                let Some(index) = accepted else {
                    panic!(
                        "no supplied target was legal for {:?}",
                        runner.state().waiting_for
                    );
                };
                pool.remove(index);
            }
            WaitingFor::ReturnAsAuraTarget {
                player,
                legal_targets,
                ..
            } => {
                observed.host_prompts.push((*player, legal_targets.clone()));
                let HostAnswers::Answer(hosts) = hosts else {
                    return observed;
                };
                let host = hosts
                    .get(next_host)
                    .unwrap_or_else(|| panic!("unexpected host prompt #{next_host}"))
                    .clone();
                next_host += 1;
                observed.events.extend(
                    runner
                        .act(GameAction::ChooseTarget { target: Some(host) })
                        .expect("choose the token Aura's host")
                        .events,
                );
            }
            WaitingFor::ScryChoice { cards, .. } => {
                observed.saw_scry = true;
                let cards = cards.clone();
                observed.events.extend(
                    runner
                        .act(GameAction::SelectCards { cards })
                        .expect("scry keep")
                        .events,
                );
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    break;
                }
                observed.events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("resolve")
                        .events,
                );
            }
            other => panic!("unexpected window: {other:?}"),
        }
    }
    observed
}

/// Yenna plus two untapped colorless mana, ready to activate.
fn yenna_scenario() -> (GameScenario, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(
        P0,
        vec![
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
            ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]),
        ],
    );
    // Scry needs cards to look at; without them Yenna's rider resolves silently
    // and `saw_scry` could never discriminate.
    scenario.with_library_top(P0, &["Scry A", "Scry B", "Scry C"]);
    let yenna = scenario
        .add_creature_from_oracle(P0, "Yenna, Redtooth Regent", 4, 4, YENNA)
        .id();
    (scenario, yenna)
}

fn enchant_creature() -> Keyword {
    Keyword::Enchant(TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature)))
}

fn activate(runner: &mut GameRunner, source: ObjectId) {
    runner
        .act(GameAction::ActivateAbility {
            source_id: source,
            ability_index: 0,
        })
        .expect("activate");
}

/// CR 303.4f: with more than one legal host the token Aura's controller chooses
/// which one it enchants, and the rest of the creating ability still runs.
#[test]
fn multi_host_copy_prompts_and_attaches() {
    let (mut scenario, yenna) = yenna_scenario();
    let host_a = scenario.add_creature(P0, "Host A", 2, 2).id();
    let host_b = scenario.add_creature(P0, "Host B", 2, 2).id();
    let aura = scenario
        .add_enchantment_from_oracle(P0, "Cooped Up", COOPED_UP)
        .with_subtypes(vec!["Aura"])
        .with_keyword(enchant_creature())
        .id();

    let mut runner = scenario.build();
    {
        let s = runner.state_mut();
        s.objects.get_mut(&yenna).unwrap().tapped = false;
        // CR 704.5m: the copy source must itself be legally attached.
        s.objects.get_mut(&aura).unwrap().attached_to = Some(AttachTarget::Object(host_a));
    }
    assert_eq!(battlefield_named(&runner, "Cooped Up").len(), 1);

    activate(&mut runner, yenna);
    let observed = drive(
        &mut runner,
        &[TargetRef::Object(aura)],
        HostAnswers::Answer(&[TargetRef::Object(host_b)]),
    );

    assert_eq!(
        observed.host_prompts.len(),
        1,
        "CR 303.4f: with two legal hosts the controller must be asked exactly once"
    );
    assert_eq!(
        observed.host_prompts[0].0, P0,
        "the token's controller asks"
    );
    let on_battlefield = battlefield_named(&runner, "Cooped Up");
    assert_eq!(
        on_battlefield.len(),
        2,
        "CR 303.4f + CR 704.5m: the token copy must survive on the battlefield \
         (got {on_battlefield:?})"
    );
    let token = on_battlefield
        .iter()
        .copied()
        .find(|id| *id != aura)
        .expect("token copy exists");
    assert_eq!(
        runner.state().objects[&token].attached_to,
        Some(AttachTarget::Object(host_b)),
        "CR 303.4f: attached to the CHOSEN host, not merely to some host"
    );
    // CR 608.2c: the host choice must not swallow the rest of Yenna's ability.
    // These two are the assertions that flip if the deferral of the
    // `LastCreated`-gated rider is reverted.
    assert!(
        !runner.state().objects[&yenna].tapped,
        "the Aura-gated untap rider must still resolve after the host choice"
    );
    assert!(
        observed.saw_scry,
        "the Aura-gated scry rider must still resolve after the host choice"
    );
}

/// CR 303.4f: exactly one legal host means no choice to make — auto-attach, no
/// prompt, and (the reach-guard for the negative) the rider still runs.
#[test]
fn single_host_auto_attaches_without_prompt() {
    let (mut scenario, yenna) = yenna_scenario();
    // Yenna is the ONLY creature, so she is the only legal host.
    let aura = scenario
        .add_enchantment_from_oracle(P0, "Cooped Up", COOPED_UP)
        .with_subtypes(vec!["Aura"])
        .with_keyword(enchant_creature())
        .id();

    let mut runner = scenario.build();
    {
        let s = runner.state_mut();
        s.objects.get_mut(&yenna).unwrap().tapped = false;
        s.objects.get_mut(&aura).unwrap().attached_to = Some(AttachTarget::Object(yenna));
    }

    activate(&mut runner, yenna);
    let observed = drive(
        &mut runner,
        &[TargetRef::Object(aura)],
        HostAnswers::Answer(&[]),
    );

    assert!(
        observed.host_prompts.is_empty(),
        "CR 303.4f: one legal host is not a choice"
    );
    let on_battlefield = battlefield_named(&runner, "Cooped Up");
    assert_eq!(
        on_battlefield.len(),
        2,
        "CR 303.4f: the token copy must auto-attach to the only legal host \
         (got {on_battlefield:?})"
    );
    let token = on_battlefield
        .iter()
        .copied()
        .find(|id| *id != aura)
        .expect("token copy exists");
    assert_eq!(
        runner.state().objects[&token].attached_to,
        Some(AttachTarget::Object(yenna)),
        "CR 303.4f: attached to the sole legal host"
    );
    assert!(observed.saw_scry, "the Aura-gated rider still runs");
}

/// A copy driver whose token is controlled by a DIFFERENT player than the copy
/// source's controller, so a controller-scoped enchant ability can be legal for
/// the source and illegal for the token.
///
/// `p0_creature` is the reach-guard dial: `Some` gives the token exactly one
/// legal host (the positive control), `None` gives it none (CR 303.4g).
fn cross_controller_copy_scenario(p0_creature: Option<&str>) -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let engine_id = scenario
        .add_enchantment_from_oracle(P0, "Copy Engine", "")
        .with_ability(Effect::CopyTokenOf {
            target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Enchantment)),
            owner: TargetFilter::Controller,
            source_filter: None,
            enters_attacking: false,
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            extra_keywords: Vec::new(),
            additional_modifications: Vec::new(),
        })
        .id();

    let p0_host = p0_creature.map(|name| scenario.add_creature(P0, name, 2, 2).id());
    let p1_host = scenario.add_creature(P1, "Their Ally", 2, 2).id();
    // "Enchant creature you control" — legal for P1's Aura on P1's creature, and
    // (with no P0 creature) illegal for every object once the copy is P0's.
    let aura = scenario
        .add_enchantment_from_oracle(P1, "Loyal Leash", "Enchant creature you control")
        .with_subtypes(vec!["Aura"])
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You),
        )))
        .id();

    let mut runner = scenario.build();
    {
        let s = runner.state_mut();
        s.objects.get_mut(&aura).unwrap().attached_to = Some(AttachTarget::Object(p1_host));
    }
    (runner, engine_id, p0_host.unwrap_or(aura))
}

/// CR 303.4g: "If an Aura is entering the battlefield and there is no legal
/// object or player for it to enchant … If the Aura is a token, it isn't
/// created." Not "created and then swept" — the graveyard assertion is what
/// separates the two.
#[test]
fn no_legal_host_means_the_token_is_not_created() {
    let (mut runner, engine_id, _) = cross_controller_copy_scenario(None);
    let aura = battlefield_named(&runner, "Loyal Leash")[0];
    let battlefield_before = runner.state().objects.len();
    let next_id_before = runner.state().next_object_id;
    // Seed the CR 111.1 anaphora slot with an unrelated earlier effect's token so
    // "no token was created" cannot pass merely because the slot was never
    // touched.
    let stale = ObjectId(9_999);
    runner.state_mut().last_created_token_ids = vec![stale];

    activate(&mut runner, engine_id);
    let observed = drive(
        &mut runner,
        &[TargetRef::Object(aura)],
        HostAnswers::Answer(&[]),
    );

    assert!(
        observed.host_prompts.is_empty(),
        "CR 303.4g: no legal host is not a choice"
    );
    assert_eq!(
        battlefield_named(&runner, "Loyal Leash"),
        vec![aura],
        "CR 303.4g: no token copy on the battlefield"
    );
    assert_eq!(
        runner.state().objects.len(),
        battlefield_before,
        "CR 303.4g: the token must not remain in `state.objects` at all"
    );
    assert!(
        observed.token_created_ids().is_empty(),
        "CR 303.4g: a token that isn't created emits no `TokenCreated` \
         (got {:?})",
        observed.token_created_ids()
    );
    let uncreated = ObjectId(next_id_before);
    assert!(
        !observed.entered_battlefield_ids().contains(&uncreated),
        "CR 303.4g: a token that isn't created emits no battlefield `ZoneChanged`"
    );
    assert!(
        !runner.state().last_created_token_ids.contains(&uncreated),
        "CR 303.4g: a token that isn't created is not \"the token created this way\""
    );
    assert!(
        !runner.state().last_created_token_ids.contains(&stale),
        "CR 603.7: the uncreated entry still republishes THIS batch's (empty) list, \
         so a prior effect's token cannot leak into \"the token created this way\" \
         (got {:?})",
        runner.state().last_created_token_ids
    );
    // THE DISCRIMINATOR. A naive "create it and let CR 704.5m sweep it"
    // implementation passes every assertion above and fails this one.
    assert!(
        runner.state().players[0].graveyard.is_empty(),
        "CR 303.4g: the token is not created, so nothing reaches a graveyard \
         (got {:?})",
        runner.state().players[0].graveyard
    );
}

/// Reach-guard for [`no_legal_host_means_the_token_is_not_created`]: the SAME
/// driver, one legal host added, must create and attach the token. Without this
/// the negative above would also pass if the copy effect never ran at all.
#[test]
fn cross_controller_copy_with_a_legal_host_is_created() {
    let (mut runner, engine_id, p0_host) = cross_controller_copy_scenario(Some("Our Ally"));
    let aura = battlefield_named(&runner, "Loyal Leash")[0];

    activate(&mut runner, engine_id);
    drive(
        &mut runner,
        &[TargetRef::Object(aura)],
        HostAnswers::Answer(&[]),
    );

    let on_battlefield = battlefield_named(&runner, "Loyal Leash");
    assert_eq!(
        on_battlefield.len(),
        2,
        "reach-guard: the copy effect really does produce an Aura token here"
    );
    let token = on_battlefield
        .iter()
        .copied()
        .find(|id| *id != aura)
        .expect("token copy exists");
    assert_eq!(
        runner.state().objects[&token].attached_to,
        Some(AttachTarget::Object(p0_host)),
        "CR 303.4f: the copy's controller-scoped enchant ability binds to ITS controller's creature"
    );
}

/// CR 303.4f: "the effect … doesn't specify the object **or player** the Aura
/// will enchant". A Curse (enchant player) must offer player hosts.
///
/// Driven with Yenna rather than Court of Vantress — Court's clause is an upkeep
/// trigger gated on the monarch, and none of that is the behaviour under test.
#[test]
fn player_hosts_are_offered_and_attach() {
    let (mut scenario, yenna) = yenna_scenario();
    let curse = scenario
        .add_enchantment_from_oracle(P0, "Curse of Bloodletting", CURSE_OF_BLOODLETTING)
        .with_subtypes(vec!["Aura", "Curse"])
        .with_keyword(Keyword::Enchant(TargetFilter::Player))
        .id();

    let mut runner = scenario.build();
    {
        let s = runner.state_mut();
        s.objects.get_mut(&yenna).unwrap().tapped = false;
        s.objects.get_mut(&curse).unwrap().attached_to = Some(AttachTarget::Player(P1));
    }

    activate(&mut runner, yenna);
    let observed = drive(
        &mut runner,
        &[TargetRef::Object(curse)],
        HostAnswers::Answer(&[TargetRef::Player(P1)]),
    );

    assert_eq!(
        observed.host_prompts.len(),
        1,
        "CR 303.4f: two legal players is a choice"
    );
    let (chooser, legal) = &observed.host_prompts[0];
    assert_eq!(*chooser, P0);
    assert!(
        legal.iter().any(|t| matches!(t, TargetRef::Player(_))),
        "CR 303.4f: a Curse's legal hosts are PLAYERS (got {legal:?})"
    );
    let token = battlefield_named(&runner, "Curse of Bloodletting")
        .into_iter()
        .find(|id| *id != curse)
        .expect("token copy exists");
    assert_eq!(
        runner.state().objects[&token].attached_to,
        Some(AttachTarget::Player(P1)),
        "CR 303.4f: attached to the chosen PLAYER"
    );
}

/// A copy of a non-Aura enchantment is untouched by CR 303.4f: no prompt, no
/// attachment, and it survives a full state-based-action pass.
#[test]
fn non_aura_enchantment_copy_is_unchanged() {
    let (mut scenario, yenna) = yenna_scenario();
    let sigil = scenario
        .add_enchantment_from_oracle(P0, "Sigil of the Empty Throne", SIGIL_OF_THE_EMPTY_THRONE)
        .id();

    let mut runner = scenario.build();
    runner.state_mut().objects.get_mut(&yenna).unwrap().tapped = false;

    activate(&mut runner, yenna);
    let observed = drive(
        &mut runner,
        &[TargetRef::Object(sigil)],
        HostAnswers::Answer(&[]),
    );

    assert!(
        observed.host_prompts.is_empty(),
        "a non-Aura enchantment copy raises no CR 303.4f prompt"
    );
    let on_battlefield = battlefield_named(&runner, "Sigil of the Empty Throne");
    assert_eq!(on_battlefield.len(), 2, "the copy token exists");
    let token = on_battlefield
        .into_iter()
        .find(|id| *id != sigil)
        .expect("token copy exists");
    assert!(
        runner.state().objects[&token].attached_to.is_none(),
        "a non-Aura is attached to nothing"
    );

    // A full SBA pass must not touch it (CR 704.5m applies to Auras only).
    let mut events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);
    assert_eq!(
        runner.state().objects[&token].zone,
        Zone::Battlefield,
        "CR 704.5m does not sweep a non-Aura"
    );
    assert!(
        !observed.saw_scry,
        "reach-guard: Yenna's Aura-gated rider correctly did NOT run for a non-Aura copy"
    );
}

/// CR 303.4f applies only when "the effect putting it onto the battlefield
/// doesn't specify the object … the Aura will enchant". A Role token names its
/// host, so it must not be prompted for one.
///
/// Scope, stated precisely so this is not read as more than it is: this drives
/// `apply_create_token`'s `spec.attach_to` route, NOT the liminal
/// `entry.attach_to.is_none()` gate this change added. That gate cannot be
/// reached today — both production `LiminalEntry` constructors (`meld.rs`,
/// `token_copy.rs`) pass `attach_to: None` — so it is kept as CR 303.4f's own
/// precondition rather than as covered code. What this test does guarantee is
/// the regression that matters: the new consult must not start prompting for
/// tokens whose host the effect already names.
#[test]
fn effect_specified_host_raises_no_prompt() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Two creatures, so a CR 303.4f prompt WOULD have something to ask about.
    let host_a = scenario.add_creature(P0, "Host A", 2, 2).id();
    let _host_b = scenario.add_creature(P0, "Host B", 2, 2).id();
    // Lord Skitter's Blessing's shipped `Effect::Token`, as an activated ability.
    let maker = scenario
        .add_enchantment_from_oracle(P0, "Role Maker", "")
        .with_ability(Effect::Token {
            name: "Wicked Role".to_string(),
            power: PtValue::Fixed(0),
            toughness: PtValue::Fixed(0),
            types: vec![
                "Enchantment".to_string(),
                "Aura".to_string(),
                "Role".to_string(),
            ],
            colors: Vec::new(),
            keywords: Vec::new(),
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            owner: TargetFilter::Controller,
            attach_to: Some(TargetFilter::Typed(
                TypedFilter::creature().controller(ControllerRef::You),
            )),
            enters_attacking: false,
            supertypes: Vec::new(),
            static_abilities: Vec::new(),
            enter_with_counters: Vec::new(),
        })
        .id();

    let mut runner = scenario.build();
    activate(&mut runner, maker);
    let observed = drive(
        &mut runner,
        &[TargetRef::Object(host_a)],
        HostAnswers::Answer(&[]),
    );

    assert!(
        observed.host_prompts.is_empty(),
        "CR 303.4f does not apply when the effect specifies the host"
    );
    let token = battlefield_named(&runner, "Wicked Role");
    assert_eq!(token.len(), 1, "reach-guard: the Role token was created");
    assert!(
        runner.state().objects[&token[0]].attached_to.is_some(),
        "the effect-specified host is bound"
    );
}

/// CR 303.4d: an Aura that's also a creature can't enchant anything — that is a
/// state-based action (CR 704.5m via CR 303.4d), NOT CR 303.4g's "isn't
/// created". The token IS created, `TokenCreated` fires, and it then dies.
///
/// The *source* stays a plain, legally attached Aura (an Aura-creature on the
/// battlefield would be swept by that same SBA before the copy could resolve);
/// the CR 707.9 "except" exception is what makes the TOKEN the Aura-creature.
#[test]
fn aura_creature_copy_is_created_then_swept() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let host = scenario.add_creature(P0, "Host", 2, 2).id();
    let maker = scenario
        .add_enchantment_from_oracle(P0, "Animating Copier", "")
        .with_ability(Effect::CopyTokenOf {
            target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Enchantment)),
            owner: TargetFilter::Controller,
            source_filter: None,
            enters_attacking: false,
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            extra_keywords: Vec::new(),
            additional_modifications: vec![ContinuousModification::AddType {
                core_type: CoreType::Creature,
            }],
        })
        .id();
    let aura = scenario
        .add_enchantment_from_oracle(P0, "Cooped Up", COOPED_UP)
        .with_subtypes(vec!["Aura"])
        .with_keyword(enchant_creature())
        .id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&aura)
        .unwrap()
        .attached_to = Some(AttachTarget::Object(host));

    activate(&mut runner, maker);
    let observed = drive(
        &mut runner,
        &[TargetRef::Object(aura)],
        HostAnswers::Answer(&[]),
    );

    assert!(
        observed.host_prompts.is_empty(),
        "CR 303.4d: an Aura that's also a creature enchants nothing, so no host is chosen"
    );
    let created = observed.token_created_ids();
    assert_eq!(
        created.len(),
        1,
        "CR 303.4d is not CR 303.4g: the token IS created and `TokenCreated` fires \
         (got {created:?})"
    );
    let token = created[0];
    assert_eq!(
        battlefield_named(&runner, "Cooped Up"),
        vec![aura],
        "CR 704.5m via CR 303.4d: the created Aura-creature is then swept off the battlefield"
    );
    assert!(
        runner.state().objects.get(&token).is_none(),
        "CR 111.7: a token that leaves the battlefield ceases to exist"
    );
}

/// CR 303.4f binds the host choice to ONE player — the one the Aura is entering
/// under the control of. No other player may answer it.
///
/// That the bound player is the TOKEN's controller (rather than the active or
/// activating player) is asserted directly against the decision seam by
/// `zone_pipeline::tests::entering_aura_hosts_reports_the_objects_own_controller`;
/// this test covers the submission gate the prompt then enforces.
#[test]
fn only_the_bound_chooser_may_answer_the_host_prompt() {
    let (mut scenario, yenna) = yenna_scenario();
    let host_a = scenario.add_creature(P0, "Host A", 2, 2).id();
    let _host_b = scenario.add_creature(P0, "Host B", 2, 2).id();
    let aura = scenario
        .add_enchantment_from_oracle(P0, "Cooped Up", COOPED_UP)
        .with_subtypes(vec!["Aura"])
        .with_keyword(enchant_creature())
        .id();

    let mut runner = scenario.build();
    {
        let s = runner.state_mut();
        s.objects.get_mut(&yenna).unwrap().tapped = false;
        s.objects.get_mut(&aura).unwrap().attached_to = Some(AttachTarget::Object(host_a));
    }

    activate(&mut runner, yenna);
    let observed = drive(
        &mut runner,
        &[TargetRef::Object(aura)],
        HostAnswers::HaltAtPrompt,
    );
    assert_eq!(observed.host_prompts.len(), 1, "the host prompt opened");
    assert_eq!(
        observed.host_prompts[0].0, P0,
        "CR 303.4f: bound to the token's controller"
    );

    let rejected = apply(
        runner.state_mut(),
        P1,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(host_a)),
        },
    );
    assert!(
        matches!(rejected, Err(EngineError::WrongPlayer)),
        "a player who is not the bound chooser must not be able to answer \
         (got {rejected:?})"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReturnAsAuraTarget { .. }
        ),
        "reach-guard: the rejected submission left the window open rather than \
         being swallowed"
    );

    apply(
        runner.state_mut(),
        P0,
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(host_a)),
        },
    )
    .expect("the bound chooser may answer");
    let token = battlefield_named(&runner, "Cooped Up")
        .into_iter()
        .find(|id| *id != aura)
        .expect("token copy exists");
    assert_eq!(
        runner.state().objects[&token].attached_to,
        Some(AttachTarget::Object(host_a))
    );
}

/// CR 303.4f: an effect that creates two Aura tokens asks once per ENTERING
/// token — the rule is written per-Aura, not per-effect — and each answer binds
/// its OWN token. Also the proof that `remaining_count` survives the pause and
/// that the second entry is not overwritten by the first.
#[test]
fn two_copies_each_choose_their_own_host() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let engine_id = scenario
        .add_enchantment_from_oracle(P0, "Copy Engine", "")
        .with_ability(Effect::CopyTokenOf {
            target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Enchantment)),
            owner: TargetFilter::Controller,
            source_filter: None,
            enters_attacking: false,
            tapped: false,
            count: QuantityExpr::Fixed { value: 2 },
            extra_keywords: Vec::new(),
            additional_modifications: Vec::new(),
        })
        .id();
    let host_a = scenario.add_creature(P0, "Host A", 2, 2).id();
    let host_b = scenario.add_creature(P0, "Host B", 2, 2).id();
    let aura = scenario
        .add_enchantment_from_oracle(P0, "Cooped Up", COOPED_UP)
        .with_subtypes(vec!["Aura"])
        .with_keyword(enchant_creature())
        .id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&aura)
        .unwrap()
        .attached_to = Some(AttachTarget::Object(host_a));

    activate(&mut runner, engine_id);
    let observed = drive(
        &mut runner,
        &[TargetRef::Object(aura)],
        HostAnswers::Answer(&[TargetRef::Object(host_a), TargetRef::Object(host_b)]),
    );

    assert_eq!(
        observed.host_prompts.len(),
        2,
        "CR 303.4f is asked once per entering Aura token"
    );
    let tokens: Vec<ObjectId> = battlefield_named(&runner, "Cooped Up")
        .into_iter()
        .filter(|id| *id != aura)
        .collect();
    assert_eq!(tokens.len(), 2, "both tokens were created (got {tokens:?})");
    let hosts: Vec<Option<AttachTarget>> = tokens
        .iter()
        .map(|id| runner.state().objects[id].attached_to)
        .collect();
    assert_eq!(
        hosts,
        vec![
            Some(AttachTarget::Object(host_a)),
            Some(AttachTarget::Object(host_b))
        ],
        "each token attaches to the host chosen for IT"
    );
    assert_eq!(
        runner.state().last_created_token_ids,
        tokens,
        "CR 111.1: both tokens are \"the tokens created this way\""
    );
}

/// CR 704.4: state-based actions pay no attention to what happens during the
/// resolution of a spell or ability. An open CR 303.4f host prompt is
/// mid-resolution, so the token must not be swept while it is open — and an
/// unrelated pending SBA (a player at 0 life) must still process once it closes.
#[test]
fn an_open_host_prompt_does_not_expose_the_token_to_sbas() {
    let (mut scenario, yenna) = yenna_scenario();
    let host_a = scenario.add_creature(P0, "Host A", 2, 2).id();
    let _host_b = scenario.add_creature(P0, "Host B", 2, 2).id();
    let aura = scenario
        .add_enchantment_from_oracle(P0, "Cooped Up", COOPED_UP)
        .with_subtypes(vec!["Aura"])
        .with_keyword(enchant_creature())
        .id();

    let mut runner = scenario.build();
    {
        let s = runner.state_mut();
        s.objects.get_mut(&yenna).unwrap().tapped = false;
        s.objects.get_mut(&aura).unwrap().attached_to = Some(AttachTarget::Object(host_a));
    }

    activate(&mut runner, yenna);
    let observed = drive(
        &mut runner,
        &[TargetRef::Object(aura)],
        HostAnswers::HaltAtPrompt,
    );
    assert_eq!(observed.host_prompts.len(), 1, "the host prompt opened");
    let WaitingFor::ReturnAsAuraTarget { returned_id, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected the CR 303.4f host prompt to still be open, got {:?}",
            runner.state().waiting_for
        );
    };

    // NON-VACUITY: while the prompt is open the token is on the battlefield and
    // unattached — i.e. it is right now a live CR 704.5m candidate, which is what
    // makes the survival assertion below a claim rather than a tautology.
    assert_eq!(runner.state().objects[&returned_id].zone, Zone::Battlefield);
    assert!(runner.state().objects[&returned_id].attached_to.is_none());

    let mut events = Vec::new();
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);
    assert_eq!(
        runner.state().objects[&returned_id].zone,
        Zone::Battlefield,
        "CR 704.4 + CR 704.5m: an unattached Aura that is mid-entry is not swept"
    );

    // An unrelated pending loss must still process once resolution finishes.
    // Set here rather than at scenario build time: a player at 0 life on the
    // first priority check ends the game before the ability ever resolves.
    runner.state_mut().players[1].life = 0;
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(host_a)),
        })
        .expect("answer the host prompt");
    assert_eq!(
        runner.state().objects[&returned_id].attached_to,
        Some(AttachTarget::Object(host_a)),
        "the token was still on the battlefield to be attached when the prompt closed"
    );
    assert!(
        runner.state().players[1].is_eliminated,
        "CR 704.5a: the 0-life loss still processes once resolution finishes"
    );
}

// ── The NON-LIMINAL copy path ────────────────────────────────────────────────
//
// `token_copy.rs` takes a second, separate route whenever the copy has entry
// counters to seed (CR 306.5b copied loyalty, or a CR 614.1c "enters with N
// counters" self-replacement on the copied card) or a modification that cannot
// be folded before entry. Fylgja is an Aura with exactly such a
// self-replacement, so copying it drives that route rather than the liminal one.

// Oracle text taken verbatim from the shipped card data.
const FYLGJA: &str = "Enchant creature\n\
This Aura enters with four healing counters on it.\n\
Remove a healing counter from this Aura: Prevent the next 1 damage that would be dealt to \
enchanted creature this turn.\n\
{2}{W}: Put a healing counter on this Aura.";

/// P0 copies P1's Fylgja. The source's `enchant creature you control` binds to
/// P1's own creature (so the source stays legally attached and alive), while the
/// COPY — P0's — sees only P0's `hosts` creatures. `hosts == 0` is therefore a
/// genuine CR 303.4g case with an untouched P0 graveyard.
fn fylgja_copy_scenario(hosts: usize) -> (GameRunner, ObjectId, ObjectId, Vec<ObjectId>) {
    fylgja_copy_scenario_with(hosts, 1, Vec::new())
}

/// [`fylgja_copy_scenario`] with an explicit copy `count` and CR 707.9 "except"
/// body, for the two seam properties the 1×/no-exception shape cannot reach: a
/// multi-token non-liminal batch, and an exception that changes what the entrant
/// IS before the CR 303.4f/g consult reads it.
fn fylgja_copy_scenario_with(
    hosts: usize,
    count: i32,
    additional_modifications: Vec<ContinuousModification>,
) -> (GameRunner, ObjectId, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let maker = scenario
        .add_enchantment_from_oracle(P0, "Copy Engine", "")
        .with_ability(Effect::CopyTokenOf {
            target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Enchantment)),
            owner: TargetFilter::Controller,
            source_filter: None,
            enters_attacking: false,
            tapped: false,
            count: QuantityExpr::Fixed { value: count },
            extra_keywords: Vec::new(),
            additional_modifications,
        })
        .id();
    let host_ids: Vec<ObjectId> = (0..hosts)
        .map(|i| scenario.add_creature(P0, &format!("Host {i}"), 2, 2).id())
        .collect();
    let their_host = scenario.add_creature(P1, "Their Ally", 2, 2).id();
    let aura = scenario
        .add_enchantment_from_oracle(P1, "Fylgja", FYLGJA)
        .with_subtypes(vec!["Aura"])
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You),
        )))
        .id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&aura)
        .unwrap()
        .attached_to = Some(AttachTarget::Object(their_host));
    (runner, maker, aura, host_ids)
}

/// CR 303.4f + CR 614.1c on the non-liminal copy path: the host choice pauses the
/// entry BEFORE its entry counters are seeded, so the resume must still seed
/// them.
///
/// The counter assertion is the discriminating one: it is exactly what fails if
/// this pause is resumed through `ApplyCopyTokenModificationsAndFinalize`
/// (whose handler skips `etb_counters`) instead of through
/// `ContinueCopyTokenEntryAfterAuraHost`.
#[test]
fn non_liminal_copy_prompts_and_still_seeds_entry_counters() {
    let (mut runner, maker, aura, hosts) = fylgja_copy_scenario(2);
    // Reach-guard for the whole fixture: the copy SOURCE really does carry the
    // CR 614.1c self-replacement that forces the non-liminal route.
    assert_eq!(
        runner.state().objects[&aura].counters.values().sum::<u32>(),
        0,
        "fixture: the pre-existing source was placed directly and never entered, \
         so only the TOKEN's entry can produce counters"
    );

    activate(&mut runner, maker);
    let observed = drive(
        &mut runner,
        &[TargetRef::Object(aura)],
        HostAnswers::Answer(&[TargetRef::Object(hosts[1])]),
    );

    assert_eq!(
        observed.host_prompts.len(),
        1,
        "CR 303.4f applies on the non-liminal copy path too"
    );
    let token = battlefield_named(&runner, "Fylgja")
        .into_iter()
        .find(|id| *id != aura)
        .expect("token copy exists");
    assert_eq!(
        runner.state().objects[&token].attached_to,
        Some(AttachTarget::Object(hosts[1])),
        "CR 303.4f: attached to the chosen host"
    );
    assert_eq!(
        runner.state().objects[&token]
            .counters
            .values()
            .sum::<u32>(),
        4,
        "CR 614.1c: the entry counters parked behind the host choice must still be \
         seeded on resume (got {:?})",
        runner.state().objects[&token].counters
    );
    assert!(
        !observed.token_created_ids().is_empty(),
        "CR 111.1: the token's entry events fire after the pause"
    );
}

/// CR 303.4g on the non-liminal copy path: same verdict, same discriminator.
#[test]
fn non_liminal_copy_with_no_legal_host_is_not_created() {
    let (mut runner, maker, aura, _) = fylgja_copy_scenario(0);
    let objects_before = runner.state().objects.len();

    activate(&mut runner, maker);
    let observed = drive(
        &mut runner,
        &[TargetRef::Object(aura)],
        HostAnswers::Answer(&[]),
    );

    assert!(observed.host_prompts.is_empty());
    assert!(
        observed.token_created_ids().is_empty(),
        "CR 303.4g: no `TokenCreated` for a token that isn't created"
    );
    assert_eq!(
        runner.state().objects.len(),
        objects_before,
        "CR 303.4g: the token must not remain in `state.objects`"
    );
    assert!(
        runner.state().players[0].graveyard.is_empty(),
        "CR 303.4g: not created, so nothing reaches a graveyard (got {:?})",
        runner.state().players[0].graveyard
    );
    assert_eq!(
        battlefield_named(&runner, "Fylgja"),
        vec![aura],
        "reach-guard: the copy SOURCE is still legally attached, so the copy effect \
         really had something to copy"
    );
}

/// CR 707.9b + CR 303.4d on the NON-LIMINAL copy path: the CR 303.4f/g consult
/// must read the entrant as it will exist after the copy exceptions.
///
/// The two copy seams apply CR 707.9 exceptions at different points — the liminal
/// one folds them into the copiable values before entry, the non-liminal one
/// defers them to `apply_token_modifications` after the birth — so only a
/// projection makes them agree about what the entrant is.
///
/// Fixture reachability, stated because the sibling `aura_creature_copy_is_
/// created_then_swept` looks identical and is NOT this test: that one copies
/// Cooped Up, whose only modification (`AddType`) is liminal-immediate and which
/// has no entry counters, so it takes the LIMINAL seam. Fylgja carries a
/// CR 614.1c "enters with four healing counters" self-replacement, which forces
/// the non-liminal route regardless of the modification.
///
/// The revert-failing assertion is `created.len() == 1`. Reading the stored
/// object instead of the projection sees a plain Aura with `enchant creature you
/// control`, finds zero P0 creatures, and takes CR 303.4g — the token is silently
/// never created.
#[test]
fn non_liminal_copy_reads_the_entrant_after_its_copy_exceptions() {
    let (mut runner, maker, aura, _) = fylgja_copy_scenario_with(
        0,
        1,
        vec![ContinuousModification::AddType {
            core_type: CoreType::Creature,
        }],
    );
    // Reach-guard: the copy SOURCE really carries the CR 614.1c self-replacement
    // that forces the non-liminal route (it was placed directly, so it has no
    // counters of its own — only an ENTRY can produce them).
    assert_eq!(
        runner.state().objects[&aura].counters.values().sum::<u32>(),
        0,
        "fixture: only the token's entry can seed counters"
    );

    activate(&mut runner, maker);
    let observed = drive(
        &mut runner,
        &[TargetRef::Object(aura)],
        HostAnswers::Answer(&[]),
    );

    assert!(
        observed.host_prompts.is_empty(),
        "CR 303.4d: an Aura that's also a creature enchants nothing, so no host is chosen"
    );
    let created = observed.token_created_ids();
    assert_eq!(
        created.len(),
        1,
        "CR 303.4d is not CR 303.4g: with the `AddType` exception read, the token IS \
         created even though no legal host exists for the unmodified body (got {created:?})"
    );
    assert_eq!(
        battlefield_named(&runner, "Fylgja"),
        vec![aura],
        "CR 704.5m via CR 303.4d: the created Aura-creature is then swept, exactly as \
         on the liminal seam"
    );
}

/// CR 111.1 + CR 608.2c on the NON-LIMINAL copy path: a multi-token batch whose
/// first token pauses on a CR 303.4f host prompt must still publish BOTH tokens
/// as "the tokens created this way".
///
/// `two_copies_each_choose_their_own_host` pins the same property on the liminal
/// seam. This is its non-liminal twin: the nested pause assigns
/// `state.last_created_token_ids = []` and the first token survives only inside
/// `pending.created_ids`, one frame-lifetime assumption away from being dropped
/// from the anaphor.
///
/// The revert-failing assertion is the two-element `created` list together with
/// both tokens being attached to distinct hosts.
#[test]
fn two_non_liminal_copies_each_choose_their_own_host() {
    let (mut runner, maker, aura, hosts) = fylgja_copy_scenario_with(2, 2, Vec::new());

    activate(&mut runner, maker);
    let observed = drive(
        &mut runner,
        &[TargetRef::Object(aura)],
        HostAnswers::Answer(&[TargetRef::Object(hosts[0]), TargetRef::Object(hosts[1])]),
    );

    assert_eq!(
        observed.host_prompts.len(),
        2,
        "CR 303.4f: each token in the batch chooses its own host"
    );
    let created = observed.token_created_ids();
    assert_eq!(
        created.len(),
        2,
        "CR 111.1: both tokens of the batch are created across the nested pause \
         (got {created:?})"
    );
    let tokens: Vec<ObjectId> = battlefield_named(&runner, "Fylgja")
        .into_iter()
        .filter(|id| *id != aura)
        .collect();
    assert_eq!(tokens.len(), 2, "both token copies are on the battlefield");
    let mut attached: Vec<Option<AttachTarget>> = tokens
        .iter()
        .map(|id| runner.state().objects[id].attached_to)
        .collect();
    attached.sort_by_key(|target| match target {
        Some(AttachTarget::Object(id)) => id.0,
        _ => u64::MAX,
    });
    assert_eq!(
        attached,
        vec![
            Some(AttachTarget::Object(hosts[0])),
            Some(AttachTarget::Object(hosts[1])),
        ],
        "CR 303.4f: each token is attached to the host its own prompt chose"
    );
    for token in &tokens {
        assert_eq!(
            runner.state().objects[token].counters.values().sum::<u32>(),
            4,
            "CR 614.1c: every token in the batch still seeds its entry counters"
        );
    }
}

// ── The entering-Aura ATTACHMENT AUTHORITY ───────────────────────────────────
//
// The decide half (`entering_aura_hosts_projected`) and the act half
// (`apply_entering_aura_hosts` / the `ReturnAsAuraTarget` resume) must judge the
// SAME object. On the non-liminal copy path they were derived separately: the
// decide half read the CR 707.9 projection, the act half re-derived CR 701.3a
// legality from `state.objects[token]`, which on that path still holds the
// PRE-exception body until `apply_token_modifications` runs.
//
// A colour exception is the sharpest instrument for that split. The fixtures
// below copy a WHITE Aura with an "except it's blue" exception, over hosts whose
// CR 702.16c protection is keyed to the colour on ONE side of the exception:
//
//   * protection from WHITE  → illegal for the source, LEGAL for the entrant
//   * protection from BLUE   → legal for the source, ILLEGAL for the entrant
//
// so the fixture discriminates in both directions at once. It is not enough for
// the act half to skip its legality check — a "trust the offered list" fix would
// pass the first arm and still be wrong; it has to check against the entrant.

/// CR 707.9b: "except it's blue" — the exception under test.
fn recolored_blue() -> ContinuousModification {
    ContinuousModification::SetColor {
        colors: vec![ManaColor::Blue],
    }
}

/// [`fylgja_copy_scenario_with`]'s colour-exception sibling: P0 copies P1's
/// **white** Fylgja with an "except it's blue" exception, and each of P0's
/// candidate hosts carries protection from the colour named in
/// `host_protections`.
///
/// Fylgja is retained deliberately — its verbatim CR 614.1c "enters with four
/// healing counters" self-replacement is what forces the NON-LIMINAL copy seam,
/// which is the seam whose two halves disagreed. The `Enchant` keyword is
/// overridden the same way [`fylgja_copy_scenario_with`] overrides it, so the
/// copy's hosts are P0's creatures rather than P1's.
fn recolored_fylgja_scenario(
    host_protections: &[ManaColor],
) -> (GameRunner, ObjectId, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let maker = scenario
        .add_enchantment_from_oracle(P0, "Copy Engine", "")
        .with_ability(Effect::CopyTokenOf {
            target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Enchantment)),
            owner: TargetFilter::Controller,
            source_filter: None,
            enters_attacking: false,
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            extra_keywords: Vec::new(),
            additional_modifications: vec![recolored_blue()],
        })
        .id();
    let host_ids: Vec<ObjectId> = host_protections
        .iter()
        .enumerate()
        .map(|(i, color)| {
            scenario
                .add_creature(P0, &format!("Host {i}"), 2, 2)
                .with_keyword(Keyword::Protection(ProtectionTarget::Color(*color)))
                .id()
        })
        .collect();
    let their_host = scenario.add_creature(P1, "Their Ally", 2, 2).id();
    let aura = scenario
        .add_enchantment_from_oracle(P1, "Fylgja", FYLGJA)
        .with_subtypes(vec!["Aura"])
        .with_keyword(Keyword::Enchant(TargetFilter::Typed(
            TypedFilter::creature().controller(ControllerRef::You),
        )))
        .id();

    let mut runner = scenario.build();
    {
        let source = runner.state_mut().objects.get_mut(&aura).unwrap();
        source.attached_to = Some(AttachTarget::Object(their_host));
        // CR 707.2 + CR 105.2a: `intrinsic_copiable_values` reads `base_color`,
        // so this is the colour the copy is made from. `color` is set alongside
        // it because that is what the CR 702.16c gate reads on the STORED token
        // body — i.e. the value the act half wrongly judged against.
        source.base_color = vec![ManaColor::White];
        source.color = vec![ManaColor::White];
    }
    (runner, maker, aura, host_ids)
}

/// The token copy of Fylgja on the battlefield, and its host.
fn fylgja_token(runner: &GameRunner, source: ObjectId) -> Option<(ObjectId, Option<AttachTarget>)> {
    battlefield_named(runner, "Fylgja")
        .into_iter()
        .find(|id| *id != source)
        .map(|id| (id, runner.state().objects[&id].attached_to))
}

/// Reach-guard shared by the colour-exception tests: the fixture really is a
/// WHITE source, and the entrant really did come out BLUE. Without both, a green
/// assertion below would prove nothing about which body the act half read.
fn assert_color_exception_landed(runner: &GameRunner, source: ObjectId, token: ObjectId) {
    assert_eq!(
        runner.state().objects[&source].color,
        vec![ManaColor::White],
        "fixture: the copy SOURCE is white, so protection-from-white is what the \
         pre-exception body would trip"
    );
    assert_eq!(
        runner.state().objects[&token].color,
        vec![ManaColor::Blue],
        "CR 707.9b: the copy exception really recoloured the entrant (got {:?})",
        runner.state().objects[&token].color
    );
}

/// CR 303.4f + CR 701.3b + CR 702.16c on the AUTO-ATTACH half: with exactly one
/// host legal for the entrant, the token must end up attached to it.
///
/// `Host 0` has protection from BLUE (legal for the white source, illegal for the
/// blue entrant); `Host 1` has protection from WHITE (illegal for the source,
/// legal for the entrant). CR 303.4f's "legal … according to the Aura" makes
/// `Host 1` the sole legal host.
///
/// The revert-failing assertion is `attached == Some(Object(hosts[1]))`. With the
/// act half re-deriving legality from the stored PRE-exception (white) body,
/// `attach_to` judges `Host 1` protected and CR 701.3b makes the attach a silent
/// no-op — the token enters unattached and CR 704.5m sweeps it, so the assertion
/// fails on `None` (or on the token being absent entirely).
#[test]
fn auto_attached_copy_uses_the_entrant_after_its_color_exception() {
    let (mut runner, maker, aura, hosts) =
        recolored_fylgja_scenario(&[ManaColor::Blue, ManaColor::White]);

    activate(&mut runner, maker);
    let observed = drive(
        &mut runner,
        &[TargetRef::Object(aura)],
        HostAnswers::Answer(&[]),
    );

    assert!(
        observed.host_prompts.is_empty(),
        "CR 303.4f: exactly one host is legal for the entrant, so there is nothing \
         to ask — this test is pinned to the AUTO-attach half"
    );
    let (token, attached) = fylgja_token(&runner, aura).unwrap_or_else(|| {
        panic!(
            "CR 303.4f + CR 704.5m: the token copy must survive on the battlefield, \
             attached to the host chosen for the post-exception entrant"
        )
    });
    assert_color_exception_landed(&runner, aura, token);
    assert_eq!(
        attached,
        Some(AttachTarget::Object(hosts[1])),
        "CR 701.3b: the act half must judge the ENTRANT (blue), so the \
         protection-from-white host is legal and the protection-from-blue host is not"
    );
}

/// The PLAYER-host mirror, driven through the same production pipeline: a Curse
/// -class copy whose colour exception lands, chosen host answered through
/// `GameAction::ChooseTarget`, and the token attached to the chosen player.
///
/// HONEST SCOPE — this one is a routing / non-regression test, not a
/// revert-discriminating one, and the reason is a property of the card pool
/// rather than of the fix. `attach_to_player`'s CR 303.4i gate reads exactly one
/// projection-sensitive input: `player_protection_from_object`. At the PLAYER
/// level that resolver implements `Everything`, `FromPlayer` and `ChosenCardType`
/// and is deliberately inert for `Color` — no card in the pool grants a player
/// protection from a colour (the seven `PlayerProtection` cards are Absolute
/// Virtue, Noble Heritage, Perch Protection, Runed Halo, Teferi's Protection,
/// The One Ring, The Stasis Coffin), so a colour exception cannot flip a player
/// host's legality. The type-keyed quality that IS live can only get MORE
/// restrictive under the copy exceptions the parser produces. The player half of
/// the act-half gate is therefore pinned at the seam instead, by
/// `zone_pipeline::entering_aura_attachment_tests::
/// player_host_attach_uses_the_supplied_entrant`.
#[test]
fn chosen_player_host_resume_survives_the_color_exception() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let maker = scenario
        .add_enchantment_from_oracle(P0, "Copy Engine", "")
        .with_ability(Effect::CopyTokenOf {
            target: TargetFilter::Typed(TypedFilter::new(TypeFilter::Enchantment)),
            owner: TargetFilter::Controller,
            source_filter: None,
            enters_attacking: false,
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            extra_keywords: Vec::new(),
            additional_modifications: vec![recolored_blue()],
        })
        .id();
    // Fylgja's verbatim CR 614.1c "enters with four healing counters"
    // self-replacement is what selects the NON-LIMINAL copy seam — the seam under
    // test. Only the `Enchant` keyword is overridden, to the player-scoped filter
    // a Curse carries, exactly as `fylgja_copy_scenario_with` overrides it to a
    // controller-scoped creature filter.
    let aura = scenario
        .add_enchantment_from_oracle(P1, "Fylgja", FYLGJA)
        .with_subtypes(vec!["Aura", "Curse"])
        .with_keyword(Keyword::Enchant(TargetFilter::Player))
        .id();

    let mut runner = scenario.build();
    {
        let source = runner.state_mut().objects.get_mut(&aura).unwrap();
        source.attached_to = Some(AttachTarget::Player(P0));
        source.base_color = vec![ManaColor::White];
        source.color = vec![ManaColor::White];
    }

    activate(&mut runner, maker);
    let observed = drive(
        &mut runner,
        &[TargetRef::Object(aura)],
        HostAnswers::Answer(&[TargetRef::Player(P1)]),
    );

    assert_eq!(
        observed.host_prompts.len(),
        1,
        "CR 303.4f: two legal players is a choice"
    );
    assert!(
        observed.host_prompts[0]
            .1
            .iter()
            .all(|target| matches!(target, TargetRef::Player(_))),
        "CR 303.4f: a Curse's legal hosts are PLAYERS (got {:?})",
        observed.host_prompts[0].1
    );
    let (token, attached) = fylgja_token(&runner, aura).unwrap_or_else(|| {
        panic!("CR 303.4f + CR 704.5m: the Curse token copy must survive its host choice")
    });
    assert_color_exception_landed(&runner, aura, token);
    assert_eq!(
        attached,
        Some(AttachTarget::Player(P1)),
        "CR 303.4f: the player-host resume attaches to the CHOSEN player"
    );
    assert!(
        runner.state().entering_aura_authority.is_none(),
        "the parked entering-Aura authority is spent by the resume, never left behind"
    );
}

/// CR 303.4f on the CHOICE half: the `WaitingFor::ReturnAsAuraTarget` resume path
/// attaches through the same authority.
///
/// Both hosts carry protection from WHITE, so both are legal for the blue entrant
/// and neither is legal for the white source — the controller is asked, and the
/// answer must actually attach.
///
/// This is the arm the auto-attach tests cannot reach: the choice returns to the
/// event loop, so the entrant has to survive a pause. The revert-failing
/// assertion is `attached == Some(Object(hosts[1]))`; with the resume arm calling
/// the unprojected `attach_to`, the stored white body is protected against by
/// BOTH offered hosts, the attach no-ops (CR 701.3b) and CR 704.5m sweeps the
/// token the player just chose a host for.
#[test]
fn chosen_host_resume_uses_the_entrant_after_its_color_exception() {
    let (mut runner, maker, aura, hosts) =
        recolored_fylgja_scenario(&[ManaColor::White, ManaColor::White]);

    activate(&mut runner, maker);
    let observed = drive(
        &mut runner,
        &[TargetRef::Object(aura)],
        HostAnswers::Answer(&[TargetRef::Object(hosts[1])]),
    );

    assert_eq!(
        observed.host_prompts.len(),
        1,
        "CR 303.4f: two hosts are legal for the entrant, so this test is pinned to \
         the CHOICE half"
    );
    assert_eq!(
        observed.host_prompts[0].1.len(),
        2,
        "CR 702.16c: both protection-from-white hosts are legal for the BLUE entrant \
         (got {:?})",
        observed.host_prompts[0].1
    );
    let (token, attached) = fylgja_token(&runner, aura).unwrap_or_else(|| {
        panic!(
            "CR 303.4f + CR 704.5m: the token must survive the host choice it was \
             legally offered"
        )
    });
    assert_color_exception_landed(&runner, aura, token);
    assert_eq!(
        attached,
        Some(AttachTarget::Object(hosts[1])),
        "CR 303.4f: the resume must attach to the CHOSEN host, judged against the \
         entrant the choice was offered for"
    );
    assert_eq!(
        runner.state().objects[&token]
            .counters
            .values()
            .sum::<u32>(),
        4,
        "CR 614.1c: the rest of the parked entry tail still runs after the resume"
    );
    assert!(
        runner.state().entering_aura_authority.is_none(),
        "the parked entering-Aura authority is spent by the resume, never left behind"
    );
}
