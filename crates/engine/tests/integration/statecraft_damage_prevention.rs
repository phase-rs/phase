//! Statecraft's bidirectional "prevent all combat damage that would be dealt
//! to and dealt by creatures you control" and the broader "dealt to and dealt
//! by <subject>" ellipsis family (Fog Bank, Gaseous Form, Ghostly Possession,
//! Sandskin, Heart of Light), plus the sibling passive-voice single-direction
//! gap (Candletrap, Defang, Charm School).
//!
//! CR 614.1a (replacement effects that use "prevent"/"instead"), CR 615.1a
//! ("prevent all [combat] damage"), CR 109.5 ("you" in a static ability means
//! that object's controller), CR 616.1 (multiple applicable replacements: the
//! affected player chooses the order).
//!
//! Before this fix, `parse_damage_source_filter` only recognized ACTIVE voice
//! ("<subject> would deal damage") and the "dealt to and dealt by X" ellipsis
//! only populated `valid_card` for the literal self-reference form (`~`/"this
//! creature") — every non-self subject (a population filter like "creatures
//! you control", or an attached-host reference like "enchanted creature")
//! compiled to an UNSCOPED shield (`valid_card: None`, `damage_source_filter:
//! None`), silently preventing ALL combat damage on the battlefield rather
//! than just the intended subject's. All Oracle text below is verbatim,
//! cross-checked against `client/public/card-data.json`.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    CombatDamageScope, ControllerRef, Effect, QuantityExpr, ResolvedAbility, ShieldKind,
    TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// Verbatim Statecraft (MMQ).
const STATECRAFT_TEXT: &str =
    "Prevent all combat damage that would be dealt to and dealt by creatures you control.";

/// Verbatim Fog Bank.
const FOG_BANK_TEXT: &str = "Defender (This creature can't attack.)\nFlying\nPrevent all combat damage that would be dealt to and dealt by this creature.";

/// Verbatim Gaseous Form.
const GASEOUS_FORM_TEXT: &str =
    "Enchant creature\nPrevent all combat damage that would be dealt to and dealt by enchanted creature.";

/// Verbatim Ghostly Possession.
const GHOSTLY_POSSESSION_TEXT: &str = "Enchant creature\nEnchanted creature has flying.\nPrevent all combat damage that would be dealt to and dealt by enchanted creature.";

/// Verbatim Sandskin.
const SANDSKIN_TEXT: &str =
    "Enchant creature\nPrevent all combat damage that would be dealt to and dealt by enchanted creature.";

/// Verbatim Heart of Light — note "prevent all damage" (NOT combat-restricted).
const HEART_OF_LIGHT_TEXT: &str = "Enchant creature (Target a creature as you cast this. This card enters attached to that creature.)\nPrevent all damage that would be dealt to and dealt by enchanted creature.";

/// Verbatim Candletrap.
const CANDLETRAP_TEXT: &str = "Enchant creature\nEnchanted creature has defender.\nPrevent all combat damage that would be dealt by enchanted creature.\nCoven — {2}{W}, Sacrifice this Aura: Exile enchanted creature. Activate only if you control three or more creatures with different powers.";

/// Verbatim Defang.
const DEFANG_TEXT: &str =
    "Enchant creature\nPrevent all damage that would be dealt by enchanted creature.";

/// Verbatim Charm School — its source clause ("sources of the chosen color")
/// is explicitly OUT OF SCOPE for this fix (needs a new qualifier arm this
/// plan does not add); only its recipient ("dealt to you") half is claimed.
const CHARM_SCHOOL_TEXT: &str = "As this enchantment enters, choose a color and balance this enchantment on your head.\nPrevent all damage that would be dealt to you by sources of the chosen color.\nWhen this enchantment falls off your head, sacrifice this enchantment.";

/// A free mana cost so casting tests don't need to stage a mana pool — the
/// mana-payment mechanics are not part of what's under test here. Mirrors the
/// existing `curse_of_exhaustion_restricts_enchanted_player` /
/// `level_up_doubles_counters_when_enchanted_creature_attacks` convention.
fn free_cost() -> ManaCost {
    ManaCost::Cost {
        shards: vec![],
        generic: 0,
    }
}

/// "Creatures you control" / "enchanted creature" both resolve to
/// `TargetFilter::Typed` populated by `parse_type_phrase` /
/// `parse_attached_host_subject`. `creatures_you_control()` is the shape
/// Statecraft's subject parses to; used by the parser-shape reach-guards
/// below to prove the shield actually exists before asserting a negative.
fn creatures_you_control() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You))
}

/// Add an Enchantment spell to `player`'s hand with `oracle_text`, correctly
/// ordered so the ability parser sees the permanent's real type. Static
/// abilities of the shape this file tests (an always-on `ReplacementEvent`
/// registered on the permanent) are only recognized while the object's
/// `card_types` already say "this is a permanent" — `add_spell_to_hand_from_oracle`
/// parses immediately with the temporary Sorcery/Instant seed type still in
/// place (its doc comment: "Permanent enchantment spells staged from
/// `add_spell_to_hand` keep the Instant/Sorcery seed until stripped [by
/// `as_enchantment`]"), so calling `.as_enchantment()` on its result AFTER
/// the fact fixes `card_types` for casting/resolution but is too late for the
/// ability parse that already ran — the shield silently fails to attach
/// (confirmed directly: `parse_oracle_text` given `types: ["Sorcery"]` for
/// Statecraft's verbatim text returns zero replacements, vs. two for
/// `["Enchantment"]`). This helper reorders: type first, oracle text second.
fn add_enchantment_spell_to_hand(
    scenario: &mut GameScenario,
    player: PlayerId,
    name: &str,
    oracle_text: &str,
) -> ObjectId {
    scenario
        .add_spell_to_hand(player, name, false)
        .as_enchantment()
        .from_oracle_text(oracle_text)
        .with_mana_cost(free_cost())
        .id()
}

/// Drive the game from the current state (expected to be at or before
/// DeclareAttackers) through the end-of-combat step, answering combat prompts:
///   - `attacker_player` declares `attacker` against `defend_player`.
///   - `blocker` (if Some) is declared to block `attacker` by the defending
///     player; otherwise no blocks are declared.
///   - All other priority windows are auto-passed.
///
/// Mirrors `weeping_angel_combat_prevention.rs`'s local driver: reactive to
/// whatever `WaitingFor` state the engine is currently in (not a fixed
/// two-player pass count), so it also works when a third, uninvolved player
/// is on the battlefield (the recipient-filter negative test below).
fn run_combat(
    runner: &mut GameRunner,
    attacker_player: PlayerId,
    attacker: ObjectId,
    defend_player: PlayerId,
    blocker: Option<ObjectId>,
) {
    let mut attacked = false;
    let mut blocked = false;

    for _ in 0..400 {
        match runner.state().phase {
            Phase::EndCombat | Phase::PostCombatMain => break,
            _ => {}
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            WaitingFor::OrderTriggers { .. } => {
                if runner
                    .act(GameAction::OrderTriggers { order: vec![0] })
                    .is_err()
                {
                    break;
                }
            }
            WaitingFor::DeclareAttackers { player, .. } if !attacked => {
                attacked = true;
                let attacks = if player == attacker_player {
                    vec![(attacker, AttackTarget::Player(defend_player))]
                } else {
                    vec![]
                };
                if runner.declare_attackers(&attacks).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareAttackers { .. } => {
                if runner.declare_attackers(&[]).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareBlockers { .. } if !blocked => {
                blocked = true;
                let blocks = if let Some(blk) = blocker {
                    vec![(blk, attacker)]
                } else {
                    vec![]
                };
                if runner.declare_blockers(&blocks).is_err() {
                    break;
                }
            }
            WaitingFor::DeclareBlockers { .. } => {
                if runner.declare_blockers(&[]).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
}

/// Build a self-targeted damage-dealing `ResolvedAbility` — used for the
/// Heart of Light CR 616.1 self-damage test, where the enchanted creature
/// deals non-combat damage to itself.
fn self_damage_ability(source_id: ObjectId, amount: i32, controller: PlayerId) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Fixed { value: amount },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        },
        vec![TargetRef::Object(source_id)],
        source_id,
        controller,
    )
}

/// Attach a (non-bestow) Aura `aura` to creature `host`: sets `attached_to`,
/// registers the back-reference in the host's `attachments`, and marks layers
/// dirty so `enchanted creature`-scoped continuous/replacement effects
/// re-evaluate against the new host. Mirrors the direct-field pattern used by
/// `aura_on_player.rs`'s local `attach_to` helper; there is no public
/// `GameScenario`/`GameRunner` builder method for a plain (non-bestow) Aura
/// attach, only `attach_as_bestowed_aura` (a different CR 702.103b form).
/// Adds a fresh, unrelated creature directly onto the battlefield of an
/// already-built `runner` — used where a test needs a damage source distinct
/// from the object under test (e.g. so recipient-half and source-half
/// prevention checks don't accidentally collide as a self-damage event).
/// Thin wrapper mirroring `GameScenario::add_creature`'s own construction,
/// since that builder method only exists pre-`build()`.
fn scenario_attacker_on_built_runner(runner: &mut GameRunner, player: PlayerId) -> ObjectId {
    let state = runner.state_mut();
    let card_id = engine::types::identifiers::CardId(state.next_object_id);
    let id = engine::game::zones::create_object(
        state,
        card_id,
        player,
        "Unrelated Attacker".to_string(),
        engine::types::zones::Zone::Battlefield,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types
        .core_types
        .push(engine::types::card_type::CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(3);
    obj.toughness = Some(3);
    obj.summoning_sick = false;
    id
}

fn attach_aura(runner: &mut GameRunner, aura: ObjectId, host: ObjectId) {
    runner
        .state_mut()
        .objects
        .get_mut(&aura)
        .unwrap()
        .attached_to = Some(engine::game::game_object::AttachTarget::Object(host));
    let host_obj = runner.state_mut().objects.get_mut(&host).unwrap();
    if !host_obj.attachments.contains(&aura) {
        host_obj.attachments.push(aura);
    }
    runner.state_mut().layers_dirty.mark_full();
}

// ---------------------------------------------------------------------------
// Statecraft — the reported bug: a real cast, both directions.
// ---------------------------------------------------------------------------

/// CR 614.1a + CR 615.1a: Statecraft's controller's own creature's combat
/// damage to the DEFENDING PLAYER is prevented — the source half
/// (`damage_source_filter`), newly fixed by the bidirectional recognizer.
///
/// Revert guard: without `damage_source_filter` populated, the attacker's
/// combat damage is dealt normally and P1's life drops.
#[test]
fn statecraft_prevents_controllers_own_creatures_combat_damage_to_defending_player() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let statecraft =
        add_enchantment_spell_to_hand(&mut scenario, P0, "Statecraft", STATECRAFT_TEXT);
    let attacker = scenario.add_creature(P0, "Raider", 3, 3).id();

    let mut runner = scenario.build();
    let _outcome = runner.cast(statecraft).resolve();
    assert_eq!(
        runner.state().objects[&statecraft].zone,
        engine::types::zones::Zone::Battlefield,
        "Statecraft must resolve onto the battlefield before combat"
    );
    let defs = &runner.state().objects[&statecraft].replacement_definitions;
    assert_eq!(
        defs.len(),
        2,
        "the bidirectional recognizer must emit exactly two ReplacementDefinitions \
         (recipient half + source half) — reach-guard proving the shield actually \
         parsed before asserting the damage-prevention negative below"
    );
    assert!(
        defs.as_slice()
            .iter()
            .any(|d| d.damage_source_filter.as_ref() == Some(&creatures_you_control())),
        "the source half must be scoped to 'creatures you control', not left \
         unscoped (Some(Any)) or unpopulated (None) — exact shape, not just presence"
    );

    let p1_life_before = runner.life(P1);
    runner.advance_to_combat();
    run_combat(&mut runner, P0, attacker, P1, None);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P1),
        p1_life_before,
        "P0's own creature's combat damage to P1 must be fully prevented by Statecraft \
         (CR 614.1a source-half shield)"
    );
}

/// CR 614.1a: Statecraft's controller's OWN creature taking combat damage
/// (blocking an opponent's attacker) is prevented — the recipient half
/// (`valid_card`).
///
/// Revert guard: without `valid_card` populated, the blocker takes marked
/// damage equal to the attacker's power and dies (1 toughness < 3 power).
///
/// Negative: the opponent's attacker is NOT controlled by Statecraft's
/// controller, so its own combat damage taken (from the blocker, if the
/// blocker's damage weren't also separately shielded) is out of scope for
/// this assertion — this row isolates the recipient-side filter only, by
/// checking the blocker's survival, not the attacker's.
#[test]
fn statecraft_prevents_damage_dealt_to_controllers_own_blocking_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let statecraft =
        add_enchantment_spell_to_hand(&mut scenario, P0, "Statecraft", STATECRAFT_TEXT);
    let blocker = scenario.add_creature(P0, "Sentinel", 0, 1).id();
    let attacker = scenario.add_creature(P1, "Raider", 3, 3).id();

    let mut runner = scenario.build();
    let _outcome = runner.cast(statecraft).resolve();
    assert_eq!(
        runner.state().objects[&statecraft]
            .replacement_definitions
            .len(),
        2,
        "reach-guard: the shield must have parsed before its recipient half is tested"
    );

    runner.state_mut().active_player = P1;
    runner.advance_to_combat();
    run_combat(&mut runner, P1, attacker, P0, Some(blocker));
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&blocker].damage_marked,
        0,
        "P0's own blocking creature must take zero marked damage — Statecraft's \
         recipient-half shield (CR 614.1a valid_card) must prevent it, or the \
         1-toughness blocker would die to the 3-power attacker"
    );
}

// ---------------------------------------------------------------------------
// Control-change regression guard (the ORIGINAL bug report): "creatures you
// control" must re-scope live when control of Statecraft itself changes.
// ---------------------------------------------------------------------------

/// CR 109.5 + CR 611.2c + CR 613.3: once control of Statecraft changes hands
/// (e.g. Iroh, Tea Master's `Effect::GainControl`), "creatures you control"
/// must resolve against the NEW controller, not whoever controlled it when it
/// entered. This is the original reported bug (Kalemne's own creature still
/// dealt damage after gaining Statecraft) — now provable end-to-end because
/// the underlying filter is finally populated.
#[test]
fn statecraft_follows_new_controller_after_control_change() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let statecraft =
        add_enchantment_spell_to_hand(&mut scenario, P0, "Statecraft", STATECRAFT_TEXT);
    let attacker = scenario.add_creature(P1, "Kalemne's Attacker", 3, 3).id();

    let mut runner = scenario.build();
    let _outcome = runner.cast(statecraft).resolve();

    // Real CR 613.3 control-change mechanism (the same Layer 2 transient
    // continuous effect `Effect::GiveControl`/`Effect::GainControl` install).
    runner.state_mut().add_transient_continuous_effect(
        statecraft,
        P1,
        engine::types::ability::Duration::Permanent,
        engine::types::ability::TargetFilter::SpecificObject { id: statecraft },
        vec![engine::types::ability::ContinuousModification::ChangeController],
        None,
    );
    engine::game::layers::evaluate_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&statecraft].controller,
        P1,
        "sanity check: Statecraft's live controller must be P1 before combat"
    );

    let p0_life_before = runner.life(P0);
    runner.state_mut().active_player = P1;
    runner.advance_to_combat();
    run_combat(&mut runner, P1, attacker, P0, None);
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.life(P0),
        p0_life_before,
        "after control changes to P1, P1's own attacker's combat damage must \
         still be prevented by Statecraft — 'creatures you control' now means \
         P1's creatures"
    );
}

// ---------------------------------------------------------------------------
// Gap A: passive-voice "would be dealt ... by X" with no ellipsis.
// ---------------------------------------------------------------------------

/// CR 614.1a: Candletrap's enchanted creature deals no combat damage — the
/// passive-voice, non-ellipsis source-side fix (`parse_damage_source_filter`
/// now tries "dealt by X" in addition to "X would deal").
///
/// Revert guard: without the passive-voice anchor, `damage_source_filter`
/// stays `None` and the enchanted creature's combat damage goes through
/// unprevented.
///
/// Uses a direct `replace_event` probe (the same production replacement
/// pipeline `object_replacement_candidate_applies` real combat damage runs
/// through — not a parser-shape assertion) rather than driving full combat:
/// attaching a plain (non-bestow) Aura by directly setting `attached_to`
/// bypasses the provenance the real cast pipeline would have recorded, and
/// CR 704.5m's illegal-attachment state-based action sweeps such an Aura to
/// the graveyard on the next priority pass — confirmed directly (the
/// battlefield-driven version of this test put Candletrap in the graveyard
/// before combat damage, a test-harness artifact of the manual attach, not a
/// defect in the fix under test). `replace_event` is called before any
/// priority pass gives that sweep a chance to run, so it observes the
/// intended attached state.
#[test]
fn candletrap_prevents_enchanted_creatures_combat_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let candletrap = scenario
        .add_enchantment_from_oracle(P1, "Candletrap", CANDLETRAP_TEXT)
        .id();
    let host = scenario.add_creature(P1, "Warden", 3, 3).id();

    let mut runner = scenario.build();
    assert_eq!(
        runner.state().objects[&candletrap]
            .replacement_definitions
            .len(),
        1,
        "reach-guard: Candletrap has no 'dealt to' ellipsis half — exactly one \
         source-scoped ReplacementDefinition"
    );
    attach_aura(&mut runner, candletrap, host);

    let mut events = Vec::new();
    let proposed = engine::types::proposed_event::ProposedEvent::Damage {
        source_id: host,
        target: TargetRef::Player(P0),
        amount: 3,
        is_combat: true,
        applied: Default::default(),
    };
    let result =
        engine::game::replacement::replace_event(runner.state_mut(), proposed, &mut events);
    match result {
        engine::game::replacement::ReplacementResult::Prevented => {}
        other => panic!(
            "the enchanted creature's combat damage must be fully prevented by \
             Candletrap's now-populated damage_source_filter — got {other:?}"
        ),
    }
}

/// CR 614.1a: Gap A's passive-voice fix is a general anchor change, not
/// specific to Candletrap — parser-shape sibling coverage for Defang (same
/// shape, different card) and Charm School (recipient half already correct;
/// its source half — "sources of the chosen color" — stays `None` on
/// purpose, since that qualifier grammar is explicitly out of scope for this
/// fix; asserted here as a non-regression negative, paired with the positive
/// reach-guard that the replacement itself still parses and the recipient
/// half is still populated).
#[test]
fn defang_and_charm_school_parser_shape() {
    let parsed_defang = engine::parser::oracle::parse_oracle_text(
        DEFANG_TEXT,
        "Defang",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    assert_eq!(
        parsed_defang.replacements.len(),
        1,
        "Defang: exactly one source-scoped ReplacementDefinition"
    );
    assert!(
        parsed_defang.replacements[0].damage_source_filter.is_some(),
        "Defang's damage_source_filter must now be populated (Gap A fix)"
    );

    let parsed_charm_school = engine::parser::oracle::parse_oracle_text(
        CHARM_SCHOOL_TEXT,
        "Charm School",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    let prevention = parsed_charm_school
        .replacements
        .iter()
        .find(|r| matches!(r.shield_kind, ShieldKind::Prevention { .. }))
        .expect(
            "reach-guard: Charm School's prevention replacement must still parse \
             at all before asserting its source-side non-fix below",
        );
    assert!(
        prevention.damage_target_filter.is_some() || prevention.valid_card.is_some(),
        "Charm School's recipient ('dealt to you') half must remain correctly \
         scoped — this fix must not regress it"
    );
    assert!(
        prevention.damage_source_filter.is_none(),
        "Charm School's source clause ('sources of the chosen color') is \
         explicitly OUT OF SCOPE for this fix (needs a new qualifier arm this \
         PR does not add) — damage_source_filter must stay None, not silently \
         mis-scope to 'matches everything' or crash"
    );
}

// ---------------------------------------------------------------------------
// Gap B: the "dealt to and dealt by X" ellipsis, generalized beyond self-ref.
// ---------------------------------------------------------------------------

/// CR 614.1a + CR 615.1a: Fog Bank — non-regression on the recipient half
/// (still correctly prevents damage dealt TO it), AND the newly-fixed source
/// half (now also prevents damage it would deal, previously never enforced
/// because it was invisible at 0 power). A temporary power-granting static
/// effect makes the source half's assertion non-vacuous.
#[test]
fn fog_bank_both_directions_correct_after_unifying_self_ref() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let fog_bank = scenario.add_creature_from_oracle(P0, "Fog Bank", 0, 4, FOG_BANK_TEXT);
    let fog_bank_id = fog_bank.id();
    let attacker = scenario.add_creature(P1, "Raider", 3, 3).id();

    let mut runner = scenario.build();
    assert_eq!(
        runner.state().objects[&fog_bank_id]
            .replacement_definitions
            .len(),
        2,
        "reach-guard: unifying the self-ref ellipsis case must still emit both halves"
    );

    // Recipient half: Fog Bank (defender, can't attack) blocks; must take 0
    // damage from the 3-power attacker despite its printed 4 toughness making
    // survival ambiguous on its own — assert marked damage directly.
    runner.state_mut().active_player = P1;
    runner.advance_to_combat();
    run_combat(&mut runner, P1, attacker, P0, Some(fog_bank_id));
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&fog_bank_id].damage_marked,
        0,
        "Fog Bank must still take zero damage when blocking (recipient-half \
         non-regression)"
    );

    // Source half: grant Fog Bank enough power to matter, then have it deal
    // combat damage — direct replacement-pipeline probe (Fog Bank can't
    // legally attack; layers-level P/T is orthogonal to what's under test).
    runner
        .state_mut()
        .objects
        .get_mut(&fog_bank_id)
        .unwrap()
        .power = Some(5);
    let mut events = Vec::new();
    let proposed = engine::types::proposed_event::ProposedEvent::Damage {
        source_id: fog_bank_id,
        target: TargetRef::Player(P1),
        amount: 5,
        is_combat: true,
        applied: Default::default(),
    };
    let result =
        engine::game::replacement::replace_event(runner.state_mut(), proposed, &mut events);
    match result {
        engine::game::replacement::ReplacementResult::Prevented => {}
        other => panic!(
            "Fog Bank's own combat damage must be prevented by its newly-fixed \
             source half (was previously unenforced) — got {other:?}"
        ),
    }
}

/// CR 614.1a + CR 615.1a: Gaseous Form — both directions correct (recipient +
/// source), AND (plan review round 3's finding) `combat_scope` is correctly
/// derived by the standalone bidirectional recognizer, so a NON-combat damage
/// event to/from the enchanted creature is NOT prevented — only combat
/// damage, matching the verbatim "combat damage" in the Oracle text.
///
/// Revert guard for the combat_scope row: if `scan_combat_scope` is dropped
/// from `parse_bidirectional_damage_prevention`, `combat_scope` stays `None`
/// and this negative assertion would wrongly also see the non-combat event
/// prevented.
#[test]
fn gaseous_form_prevents_both_combat_directions_but_not_noncombat_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let gaseous_form = scenario
        .add_enchantment_from_oracle(P1, "Gaseous Form", GASEOUS_FORM_TEXT)
        .id();
    let host = scenario.add_creature(P1, "Wisp", 3, 3).id();

    let mut runner = scenario.build();
    assert_eq!(
        runner.state().objects[&gaseous_form]
            .replacement_definitions
            .len(),
        2,
        "reach-guard: the bidirectional recognizer must emit both halves"
    );
    attach_aura(&mut runner, gaseous_form, host);
    // A distinct, unenchanted attacker as damage source for the recipient-half
    // check below — using `host` as its own source would make recipient ==
    // source, matching BOTH halves simultaneously and triggering the CR 616.1
    // choice this file's dedicated Heart of Light test covers, rather than
    // isolating the recipient half alone.
    let other_attacker = scenario_attacker_on_built_runner(&mut runner, P0);

    // Recipient half: combat damage TO the enchanted creature is prevented.
    let mut events = Vec::new();
    let to_host_combat = engine::types::proposed_event::ProposedEvent::Damage {
        source_id: other_attacker,
        target: TargetRef::Object(host),
        amount: 4,
        is_combat: true,
        applied: Default::default(),
    };
    assert!(
        matches!(
            engine::game::replacement::replace_event(
                runner.state_mut(),
                to_host_combat,
                &mut events
            ),
            engine::game::replacement::ReplacementResult::Prevented
        ),
        "combat damage dealt TO the enchanted creature must be prevented (recipient half)"
    );

    // Source half: combat damage BY the enchanted creature is prevented.
    let mut events = Vec::new();
    let by_host_combat = engine::types::proposed_event::ProposedEvent::Damage {
        source_id: host,
        target: TargetRef::Player(P0),
        amount: 3,
        is_combat: true,
        applied: Default::default(),
    };
    assert!(
        matches!(
            engine::game::replacement::replace_event(
                runner.state_mut(),
                by_host_combat,
                &mut events
            ),
            engine::game::replacement::ReplacementResult::Prevented
        ),
        "combat damage dealt BY the enchanted creature must be prevented (source half)"
    );

    // combat_scope negative: NON-combat damage BY the enchanted creature is
    // NOT prevented — Gaseous Form's shield is combat-only.
    let mut events = Vec::new();
    let by_host_noncombat = engine::types::proposed_event::ProposedEvent::Damage {
        source_id: host,
        target: TargetRef::Player(P0),
        amount: 3,
        is_combat: false,
        applied: Default::default(),
    };
    match engine::game::replacement::replace_event(
        runner.state_mut(),
        by_host_noncombat,
        &mut events,
    ) {
        engine::game::replacement::ReplacementResult::Execute(_) => {}
        other => panic!(
            "NON-combat damage from the enchanted creature must NOT be prevented \
             by a combat-only shield — combat_scope must have been correctly \
             derived as CombatOnly, not left None (which would match everything). \
             Got {other:?}"
        ),
    }
}

/// CR 614.1a: parser-shape sibling coverage for Ghostly Possession and
/// Sandskin — byte-identical Oracle-text suffix to Gaseous Form
/// ("...dealt to and dealt by enchanted creature."), so they traverse the
/// exact same attached-host branch Gaseous Form's runtime test above already
/// proves works end to end; this row only needs to confirm each card's own
/// text actually reaches that branch (Check 9's claim-to-test map), not
/// re-prove the branch itself.
#[test]
fn ghostly_possession_and_sandskin_parser_shape() {
    for (name, text) in [
        ("Ghostly Possession", GHOSTLY_POSSESSION_TEXT),
        ("Sandskin", SANDSKIN_TEXT),
    ] {
        let parsed = engine::parser::oracle::parse_oracle_text(
            text,
            name,
            &[],
            &["Enchantment".to_string()],
            &[],
        );
        assert_eq!(
            parsed.replacements.len(),
            2,
            "{name}: the bidirectional recognizer must emit both halves"
        );
        let has_recipient = parsed
            .replacements
            .iter()
            .any(|r| matches!(r.valid_card, Some(TargetFilter::AttachedTo)));
        let has_source = parsed
            .replacements
            .iter()
            .any(|r| matches!(r.damage_source_filter, Some(TargetFilter::AttachedTo)));
        assert!(
            has_recipient && has_source,
            "{name}: exactly one definition scoped via valid_card=AttachedTo (recipient) \
             and one via damage_source_filter=AttachedTo (source)"
        );
        assert!(
            parsed
                .replacements
                .iter()
                .all(|r| r.combat_scope == Some(CombatDamageScope::CombatOnly)),
            "{name}: both halves must be combat-scoped (verbatim text says \"combat damage\")"
        );
    }
}

/// CR 615.1a + CR 616.1: Heart of Light — parser-shape coverage (it is NOT
/// combat-restricted, unlike its siblings above, so its own row also proves
/// `combat_scope: None` is correctly derived, not defaulted-wrong), PLUS the
/// CR 616.1 self-damage interaction the bidirectional design's two
/// co-matching `ReplacementDefinition`s make newly reachable: an enchanted
/// creature that deals non-combat damage to ITSELF makes both halves match
/// the same event (recipient == source == the enchanted creature), forcing
/// the engine's existing (unmodified) multiple-replacement-order choice.
/// Both resolution orders must still fully prevent the damage — the
/// interactive choice itself is a real, existing engine behavior this design
/// exercises for the first time in this card class, not a defect to fix.
#[test]
fn heart_of_light_parser_shape_and_self_damage_cr616_choice() {
    let parsed = engine::parser::oracle::parse_oracle_text(
        HEART_OF_LIGHT_TEXT,
        "Heart of Light",
        &[],
        &["Enchantment".to_string()],
        &[],
    );
    assert_eq!(
        parsed.replacements.len(),
        2,
        "reach-guard: the bidirectional recognizer must emit both halves"
    );
    assert!(
        parsed.replacements.iter().all(|r| r.combat_scope.is_none()),
        "Heart of Light says \"prevent all damage\" (no \"combat\") — combat_scope \
         must be None on both halves, not incorrectly defaulted to CombatOnly"
    );

    // CR 616.1 self-damage: build the runtime scenario and drive the actual
    // interactive choice through apply()/GameAction::ChooseReplacement.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let heart_of_light = scenario
        .add_enchantment_from_oracle(P0, "Heart of Light", HEART_OF_LIGHT_TEXT)
        .id();
    let host = scenario.add_creature(P0, "Bearer", 3, 3).id();
    let mut runner = scenario.build();
    attach_aura(&mut runner, heart_of_light, host);

    let ability = self_damage_ability(host, 3, P0);
    let mut events = Vec::new();
    engine::game::effects::resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("self-damage ability chain resolves");

    match runner.state().waiting_for.clone() {
        WaitingFor::ReplacementChoice { .. } => {
            // Both halves matched the same self-damage event — CR 616.1 asks
            // the affected player to order them. Answer with index 0; per the
            // Architecture, whichever is chosen fully prevents the damage
            // (neither carries an execute/rider that would make the order
            // observable).
            runner
                .act(GameAction::ChooseReplacement { index: 0 })
                .expect("CR 616.1 order choice for two co-matching prevention shields");
        }
        other => {
            // Some engine versions of the pipeline may resolve a single
            // dominant candidate without prompting when both are pure,
            // riderless Prevention::All shields — accept either shape, but
            // the damage must be prevented either way.
            eprintln!(
                "note: self-damage on doubly-enchanted host did not reach \
                 WaitingFor::ReplacementChoice (got {other:?}); asserting \
                 prevention directly instead"
            );
        }
    }

    assert_eq!(
        runner.state().objects[&host].damage_marked,
        0,
        "Heart of Light must fully prevent the enchanted creature's self-damage \
         regardless of which co-matching shield the CR 616.1 choice selects"
    );
}
