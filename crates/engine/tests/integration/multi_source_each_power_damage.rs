//! Runtime regression: "multiple source creatures each deal damage equal to
//! their power to a single target" (`DamageSource::EachTarget`).
//!
//! Three Standard-legal cards are genuinely supported by this clause — Allies at
//! Last, Coordinated Clobbering, Terrific Team-Up. A fourth, Graceful Takedown,
//! has a HETEROGENEOUS compound source set — "<group A> and up to one other
//! target <group B>" — now supported via `EachDealsDamageEqualToPower`'s optional
//! `extra_source` group (see the `graceful_takedown_*` tests below). The
//! single-source form ("target creature you control deals damage equal to its
//! power to target creature") was already supported; this exercises the
//! MULTI-source generalization where EACH chosen source deals its OWN power to
//! the shared recipient.
//!
//! CR 120.1: the object that deals damage is the source of that damage.
//! CR 601.2c: a variable number of targets is announced once; each chosen object
//!            becomes a target.
//! CR 208.1 + CR 608.2: a creature's power is a modifiable characteristic, read
//!            at resolution (current value).
//!
//! The recipients are sized so the assertions DISCRIMINATE the multi-source
//! semantics: a 1/5 recipient survives a single power-3 source (3 < 5) but dies
//! to the SUM of two power-3 sources (6 >= 5). Reverting the parser change
//! (clause → `Effect::Unimplemented`, no damage) or the runtime change (only one
//! source resolves, 3 damage) leaves the recipient alive and fails the test.

use engine::game::effects::deal_damage;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::types::ability::{
    DamageSource, Effect, ObjectScope, PreventionAmount, QuantityExpr, QuantityRef,
    ReplacementDefinition, ReplacementMode, ResolvedAbility, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::replacements::ReplacementEvent;
use engine::types::zones::Zone;

/// Verbatim Oracle text (Scryfall) — Graceful Takedown, {1}{G} Sorcery.
const GRACEFUL_TAKEDOWN: &str = "Any number of target enchanted creatures you control and up to \
     one other target creature you control each deal damage equal to their power to target \
     creature you don't control.";

/// Verbatim Oracle text (Scryfall) — Friendly Rivalry, {R}{G} Instant. Same
/// compound class, singular group A ("Target creature you control", count 1),
/// but a LEGENDARY-restricted group B.
const FRIENDLY_RIVALRY: &str = "Target creature you control and up to one other target legendary \
     creature you control each deal damage equal to their power to target creature you don't \
     control.";

/// Genuinely attach a fresh Aura permanent to `host` so `host` matches the
/// `EnchantedBy` group-A filter (CR 303.4). Returns the Aura's id.
fn attach_aura(state: &mut GameState, host: ObjectId, owner: PlayerId, name: &str) -> ObjectId {
    let card_id = CardId(state.next_object_id);
    let aura = create_object(state, card_id, owner, name.to_string(), Zone::Battlefield);
    {
        let obj = state.objects.get_mut(&aura).unwrap();
        obj.card_types.core_types = vec![CoreType::Enchantment];
        obj.card_types.subtypes = vec!["Aura".to_string()];
        obj.base_card_types = obj.card_types.clone();
        obj.attached_to = Some(host.into());
    }
    state.objects.get_mut(&host).unwrap().attachments.push(aura);
    aura
}

const COORDINATED_CLOBBERING: &str = "Tap one or two target untapped creatures you control. \
     They each deal damage equal to their power to target creature an opponent controls.";

const ALLIES_AT_LAST: &str = "Up to two target creatures you control each deal damage equal \
     to their power to target creature an opponent controls.";

const TERRIFIC_TEAM_UP: &str = "One or two target creatures you control each get +1/+0 until \
     end of turn. They each deal damage equal to their power to target creature an opponent \
     controls.";

/// Coordinated Clobbering — back-reference form ("They each deal …" after the
/// tap sentence). Two power-3 creatures each deal 3 to a 1/5 opponent creature:
/// 6 total, lethal. Asserts the recipient is dealt the SUM of both sources'
/// powers (it dies and leaves the battlefield), and both sources are tapped.
#[test]
fn coordinated_clobbering_two_sources_each_deal_own_power() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Two power-3 sources the controller will tap and have deal damage.
    let source_a = scenario.add_vanilla(P0, 3, 3);
    let source_b = scenario.add_vanilla(P0, 3, 3);
    // A 1/5 recipient: survives 3 damage (one source) but dies to 6 (both).
    let recipient = scenario.add_vanilla(P1, 1, 5);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Coordinated Clobbering", false, COORDINATED_CLOBBERING)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();

    // Sources first (the two tapped creatures), then the shared recipient.
    let outcome = runner
        .cast(spell)
        .target_objects(&[source_a, source_b, recipient])
        .resolve();

    let state = outcome.state();
    // CR 208.1 + CR 608.2 + CR 120.1: 3 (source_a) + 3 (source_b) = 6 damage; the 1/5 dies.
    assert_eq!(
        outcome.zone_of(recipient),
        Zone::Graveyard,
        "recipient must take 6 total damage (both sources) and die; \
         single-source 3 would leave it alive — got recipient in {:?}",
        outcome.zone_of(recipient)
    );
    // The leading "Tap one or two target … creatures" sentence taps both sources.
    assert!(
        state.objects[&source_a].tapped,
        "source_a must be tapped by the tap clause"
    );
    assert!(
        state.objects[&source_b].tapped,
        "source_b must be tapped by the tap clause"
    );
}

/// Coordinated Clobbering — single chosen source (the "one or two" lower bound).
/// One power-3 source deals exactly 3 to a 1/5 recipient: it SURVIVES (3 < 5).
/// This negative case proves the recipient's death in the two-source test comes
/// from the SUM of both sources, not from a single source over-dealing.
#[test]
fn coordinated_clobbering_single_source_deals_only_its_own_power() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let source_a = scenario.add_vanilla(P0, 3, 3);
    let recipient = scenario.add_vanilla(P1, 1, 5);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Coordinated Clobbering", false, COORDINATED_CLOBBERING)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();

    let outcome = runner
        .cast(spell)
        .target_objects(&[source_a, recipient])
        .resolve();

    let state = outcome.state();
    // CR 208.1 + CR 608.2: only source_a's power (3) is dealt; the 1/5 survives.
    assert_eq!(
        outcome.zone_of(recipient),
        Zone::Battlefield,
        "single source deals only its own power (3 < 5); recipient must survive"
    );
    assert_eq!(
        state.objects[&recipient].damage_marked, 3,
        "recipient must be marked exactly 3 (source_a's power), not more"
    );
    assert!(state.objects[&source_a].tapped, "source_a must be tapped");
}

/// Allies at Last — direct subject form ("Up to two target creatures you control
/// each deal damage equal to their power …"). Two power-4 sources each deal 4 to
/// a 2/7 recipient: 8 total, lethal (8 >= 7). Exercises the `TargetOnly` source
/// picker + `EachTarget` sub-ability path (no preceding tap/pump sentence).
#[test]
fn allies_at_last_direct_subject_two_sources_each_deal_own_power() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let source_a = scenario.add_vanilla(P0, 4, 4);
    let source_b = scenario.add_vanilla(P0, 4, 4);
    // 2/7 recipient: survives one power-4 source (4 < 7), dies to both (8 >= 7).
    let recipient = scenario.add_vanilla(P1, 2, 7);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Allies at Last", false, ALLIES_AT_LAST)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();

    let outcome = runner
        .cast(spell)
        .target_objects(&[source_a, source_b, recipient])
        .resolve();

    // CR 120.1 + CR 208.1 + CR 608.2: 4 + 4 = 8 damage from the two sources; the 2/7 dies.
    assert_eq!(
        outcome.zone_of(recipient),
        Zone::Graveyard,
        "recipient must take 8 total (both sources) and die"
    );
}

/// Terrific Team-Up — the "get +1/+0 then they each deal damage" form. The
/// SAME-resolution +1/+0 pump must be applied BEFORE each source's power is read
/// for damage (CR 608.2c: instructions are followed in order; CR 208.1: power is
/// modifiable). Two 3/3 sources become 4/3, so 4 + 4 = 8 damage kills a 2/7
/// recipient. The buff is LOAD-BEARING for lethality: without it the sources deal
/// only 3 + 3 = 6 (< 7) and the recipient survives. Reverting the parser change
/// (clause → `Unimplemented`, no damage) or dropping the pump-then-power ordering
/// leaves the recipient alive and fails this assertion.
#[test]
fn terrific_team_up_same_resolution_pump_is_read_before_damage() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Two 3/3 sources: base power 3 each (6 total) is NON-lethal vs toughness 7;
    // only the +1/+0 buff (effective power 4 each, 8 total) is lethal.
    let source_a = scenario.add_vanilla(P0, 3, 3);
    let source_b = scenario.add_vanilla(P0, 3, 3);
    let recipient = scenario.add_vanilla(P1, 2, 7);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Terrific Team-Up", false, TERRIFIC_TEAM_UP)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();

    let outcome = runner
        .cast(spell)
        .target_objects(&[source_a, source_b, recipient])
        .resolve();

    // CR 608.2c + CR 208.1: the +1/+0 is applied first, so each source's power is
    // read as 4 at damage resolution: 4 + 4 = 8 >= 7, the 2/7 dies. Base power 6
    // would leave it alive — the pump being read after itself is the discriminator.
    assert_eq!(
        outcome.zone_of(recipient),
        Zone::Graveyard,
        "recipient must die to the BUFFED power (8 total); base power 6 would not be lethal"
    );
}

// ---------------------------------------------------------------------------
// PR #3898 review (matthewevans, HIGH): `DamageSource::EachTarget` is a PUBLIC
// damage primitive that targets ARBITRARY creatures, so a chosen source can have
// granted deathtouch/lifelink/wither/infect and the recipient can have combined-
// damage interactions. The variant must deal all per-source damage as a true
// SIMULTANEOUS batch (each event carrying its OWN source id) and preserve each
// source's identity through the replacement pause→resume path. The three tests
// below drive the real `EachTarget` resolver and each FAILS if the batch is
// reverted to the old sequential / single-source-flattened behavior.
// ---------------------------------------------------------------------------

/// Build a `DealDamage { EachTarget }` ability over `[sources.., recipient]` with
/// the per-source `Power{Target}` amount the parser produces. The damage source
/// id is a sentinel — `EachTarget` reads each member's OWN context, never the
/// ability source's, so the sentinel never deals damage itself.
fn each_target_power_damage(sources_then_recipient: Vec<TargetRef>) -> ResolvedAbility {
    ResolvedAbility::new(
        Effect::DealDamage {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Target,
                },
            },
            target: TargetFilter::Any,
            damage_source: Some(DamageSource::EachTarget),
            excess: None,
        },
        sources_then_recipient,
        engine::types::identifiers::ObjectId(9_999),
        P0,
    )
}

/// CR 120.4a + CR 702.2b/702.2c: a chosen source with granted DEATHTOUCH makes
/// the batch lethal even though the SUM of marked damage is below the recipient's
/// toughness. Two power-1 sources (one deathtouch, one not) deal 1 + 1 = 2 to a
/// 0/10 recipient. 2 < 10 is NOT lethal by marked damage, but the deathtoucher's
/// 1 is lethal (CR 702.2b), so the 0/10 dies. The non-deathtouch source proves
/// the keyword is read PER SOURCE: if `EachTarget` flattened to one shared
/// context, either both would have deathtouch (then this is trivially lethal and
/// doesn't discriminate) — instead each carries its own, and the recipient also
/// records `dealt_deathtouch_damage` from the deathtoucher alone.
#[test]
fn each_target_per_source_deathtouch_kills_recipient_below_marked_lethal() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let deathtoucher = scenario.add_creature(P0, "DT 1/1", 1, 1).deathtouch().id();
    let plain = scenario.add_vanilla(P0, 1, 1);
    // 0/10: survives 2 marked damage on its own; only deathtouch makes it lethal.
    let recipient = scenario.add_vanilla(P1, 0, 10);

    let mut runner = scenario.build();
    let ability = each_target_power_damage(vec![
        TargetRef::Object(deathtoucher),
        TargetRef::Object(plain),
        TargetRef::Object(recipient),
    ]);
    let mut events = Vec::new();
    deal_damage::resolve(runner.state_mut(), &ability, &mut events)
        .expect("EachTarget deathtouch batch resolves");
    // CR 704: SBAs run after the effect and destroy the deathtouched creature.
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);

    let state = runner.state();
    assert_eq!(
        state.objects[&recipient].zone,
        Zone::Graveyard,
        "the deathtoucher's 1 damage is lethal (CR 702.2b); recipient must die even though \
         2 total marked is below toughness 10 — flattening keywords across sources or \
         dropping per-source deathtouch leaves it alive"
    );
}

/// CR 702.15b + CR 120.3f: per-source LIFELINK gains life PER source. A lifelink
/// source and a non-lifelink source each deal 3 to the recipient; the controller
/// gains exactly 3 (the lifelink source's damage), not 0 (no lifelink read) and
/// not 6 (lifelink wrongly applied to BOTH sources from a shared context). This
/// pins the per-source context: only the lifelinker's 3 damage gains life.
#[test]
fn each_target_per_source_lifelink_gains_life_for_lifelink_source_only() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let lifelinker = scenario.add_creature(P0, "LL 3/3", 3, 3).lifelink().id();
    let plain = scenario.add_vanilla(P0, 3, 3);
    // A big recipient so it survives 6 — the assertion is about life gain, not death.
    let recipient = scenario.add_vanilla(P1, 0, 20);

    let mut runner = scenario.build();
    let life_before = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .life;

    let ability = each_target_power_damage(vec![
        TargetRef::Object(lifelinker),
        TargetRef::Object(plain),
        TargetRef::Object(recipient),
    ]);
    let mut events = Vec::new();
    deal_damage::resolve(runner.state_mut(), &ability, &mut events)
        .expect("EachTarget lifelink batch resolves");

    let life_after = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .life;
    assert_eq!(
        life_after - life_before,
        3,
        "only the lifelink source's 3 damage gains life: 0 would mean lifelink not read, \
         6 would mean lifelink wrongly applied to BOTH sources from a flattened context"
    );
    assert_eq!(
        runner.state().objects[&recipient].damage_marked,
        6,
        "both sources still deal their 3 (combined 6 marked); lifelink does not change damage"
    );
}

/// CR 120.4b + CR 616.1e: THE reviewer-cited bug. A damage-replacement choice on
/// the recipient pauses the batch mid-flight; the REMAINING sources must resume
/// with their OWN identity, not flattened to the paused source's id.
///
/// `source_a` (power 3, NO deathtouch) is processed first and pauses on the
/// recipient's optional prevention shield. `source_b` (power 1, DEATHTOUCH) is in
/// the resumed tail. The recipient is a 0/10: 3 + 1 = 4 marked is below toughness,
/// so it dies ONLY if `source_b`'s deathtouch survives the pause (CR 702.2b).
///
/// Old behavior (flatten the remaining chain to the paused source's id): the
/// resumed `source_b` node carried `source_a`'s id (no deathtouch) → no lethal
/// deathtouch → recipient SURVIVES with 4 marked. The fix stashes per-source ids,
/// so `source_b` keeps its deathtouch and the recipient dies. This assertion flips
/// when the per-source stash is reverted to a single `damage_source_id`.
#[test]
fn each_target_replacement_pause_preserves_other_sources_identity_on_resume() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let source_a = scenario.add_vanilla(P0, 3, 3);
    let source_b = scenario.add_creature(P0, "DT 1/1", 1, 1).deathtouch().id();

    // 0/10 recipient hosting an OPTIONAL "may prevent the next 1 damage" shield so
    // the FIRST source's damage surfaces a ReplacementChoice (CR 615 + CR 616.1e).
    // SelfRef scopes it to damage dealt to the recipient only. The test always
    // DECLINES, so the prevention branch never runs — the shield exists purely to
    // force the per-source replacement pause that exercises the resume path.
    let prevention = ReplacementDefinition::new(ReplacementEvent::DamageDone)
        .valid_card(TargetFilter::SelfRef)
        .prevention_shield(PreventionAmount::Next(1))
        .mode(ReplacementMode::Optional { decline: None })
        .description("You may prevent the next 1 damage to this creature.".to_string());
    let recipient = scenario
        .add_creature(P1, "Shielded 0/10", 0, 10)
        .with_replacement_definition(prevention)
        .id();

    let mut runner = scenario.build();
    let ability = each_target_power_damage(vec![
        TargetRef::Object(source_a),
        TargetRef::Object(source_b),
        TargetRef::Object(recipient),
    ]);
    let mut events = Vec::new();
    deal_damage::resolve(runner.state_mut(), &ability, &mut events)
        .expect("EachTarget batch resolves into a replacement pause");

    // The batch paused on the recipient's optional prevention shield.
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "the optional prevention shield must pause the batch with a ReplacementChoice, got {:?}",
        runner.state().waiting_for
    );

    // Decline every prompt (the deathtoucher's pass may re-prompt) so all damage
    // is dealt. Index 1 is the decline slot of an Optional with no decline branch.
    for _ in 0..4 {
        if !matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ) {
            break;
        }
        runner
            .act(GameAction::ChooseReplacement { index: 1 })
            .expect("declining the prevention shield resolves the choice");
    }
    engine::game::sba::check_state_based_actions(runner.state_mut(), &mut events);

    let state = runner.state();
    assert!(
        !matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. }),
        "all prevention prompts must be resolved"
    );
    // CR 702.2b: source_b's deathtouch (1 damage) is lethal. The recipient dies
    // ONLY if source_b's identity survived the pause — under the old flatten-to-
    // one-source-id bug it would inherit source_a's (no deathtouch) and survive.
    assert_eq!(
        state.objects[&recipient].zone,
        Zone::Graveyard,
        "after the mid-batch pause, source_b's deathtouch must persist on resume and kill the \
         0/10 (4 marked < toughness, so only deathtouch is lethal); flattening the resumed \
         chain to source_a's id loses the deathtouch and leaves the recipient alive"
    );
}

// ---------------------------------------------------------------------------
// Graceful Takedown ({1}{G} Sorcery) — the HETEROGENEOUS compound source set:
// group A ("any number of target enchanted creatures you control") + optional
// group B ("up to one other target creature you control") each deal their OWN
// power to a "target creature you don't control". Threaded through the shared
// `EachDealsDamageEqualToPower` resolver via the new `extra_source` group.
// CR 115.4 (other target) + CR 601.2c (announce) + CR 120.1 (own-source damage)
// + CR 303.4 (Aura-attached ⇒ EnchantedBy) + CR 109.5 (you = controller).
// ---------------------------------------------------------------------------

/// Σ-of-powers discriminator. Two ENCHANTED sources (power 3 + 2) and one other
/// un-enchanted source (power 4) each deal their own power to a 0/12 recipient:
/// 3 + 2 + 4 = 9 marked. Reverting the group-B slot drops the un-enchanted
/// source → 5 marked; reverting the parser leaves `Unimplemented` → 0 marked.
/// The exact `== 9` assertion flips under either revert.
#[test]
fn graceful_takedown_group_b_source_adds_its_own_power() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let ench_a = scenario.add_vanilla(P0, 3, 3);
    let ench_b = scenario.add_vanilla(P0, 2, 2);
    let un_ench = scenario.add_vanilla(P0, 4, 4);
    // 0/12 survives 9 so the exact marked total is observable (not reset by death).
    let recipient = scenario.add_vanilla(P1, 0, 12);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Graceful Takedown", false, GRACEFUL_TAKEDOWN)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    attach_aura(runner.state_mut(), ench_a, P0, "Aura A");
    attach_aura(runner.state_mut(), ench_b, P0, "Aura B");

    // Slot order: [group-A sources.., group-B source, recipient]. The driver
    // consumes objects in declaration order per slot.
    let outcome = runner
        .cast(spell)
        .target_objects(&[ench_a, ench_b, un_ench, recipient])
        .resolve();

    let state = outcome.state();
    // CR 120.1 + CR 208.1 + CR 608.2: 3 + 2 (group A) + 4 (group B) = 9.
    assert_eq!(
        state.objects[&recipient].damage_marked, 9,
        "recipient must take Σ of all three sources' own powers (3+2+4=9); dropping the \
         group-B slot would leave 5, reverting the parser would leave 0"
    );
}

/// Group A is `unlimited(0)` — its lower bound is 0, so a board with ZERO
/// enchanted creatures still resolves: the sole group-B source deals its power.
/// One un-enchanted source (power 5) → 5 marked on the 0/12 recipient. Proves
/// the group-B path is not gated on a non-empty group A.
#[test]
fn graceful_takedown_group_b_only_when_no_enchanted_sources() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let un_ench = scenario.add_vanilla(P0, 5, 5);
    let recipient = scenario.add_vanilla(P1, 0, 12);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Graceful Takedown", false, GRACEFUL_TAKEDOWN)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    // No auras attached — zero enchanted creatures, so group A contributes nothing.
    let outcome = runner
        .cast(spell)
        .target_objects(&[un_ench, recipient])
        .resolve();

    assert_eq!(
        outcome.state().objects[&recipient].damage_marked,
        5,
        "with no enchanted sources the group-B source alone deals its power (5)"
    );
}

/// Slot legality + group-B distinctness (CR 115.3 / CR 115.4), driven through
/// the real incremental target-selection pipeline (`WaitingFor::TargetSelection`
/// → `GameAction::ChooseTarget`), inspecting the engine-computed offered set
/// (`selection.current_legal_targets`) at each slot. Every negative assertion is
/// paired with a positive reach-guard so none is vacuous.
#[test]
fn graceful_takedown_slot_legality_and_group_b_distinctness() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let ench_a = scenario.add_vanilla(P0, 3, 3);
    let ench_b = scenario.add_vanilla(P0, 2, 2);
    let un_ench = scenario.add_vanilla(P0, 4, 4);
    let recipient = scenario.add_vanilla(P1, 0, 12);

    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Graceful Takedown", false, GRACEFUL_TAKEDOWN)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    attach_aura(runner.state_mut(), ench_a, P0, "Aura A");
    attach_aura(runner.state_mut(), ench_b, P0, "Aura B");

    let card_id = runner.state().objects[&spell].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting Graceful Takedown (zero cost) must be accepted");

    let mut chose_group_a: Vec<ObjectId> = Vec::new();
    let mut saw_group_b = false;
    let mut saw_recipient = false;

    for _ in 0..16 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { selection, .. } => {
                let legal = &selection.current_legal_targets;
                let obj = |id: ObjectId| TargetRef::Object(id);
                let choice = if legal.contains(&obj(recipient)) {
                    // Recipient slot: opponent creature only; NONE of my creatures.
                    saw_recipient = true;
                    assert!(
                        !legal.contains(&obj(ench_a))
                            && !legal.contains(&obj(ench_b))
                            && !legal.contains(&obj(un_ench)),
                        "recipient (you don't control) must NOT offer your own creatures, \
                         legal = {legal:?}"
                    );
                    obj(recipient)
                } else if legal.contains(&obj(un_ench)) {
                    // Group B: group A never offers the un-enchanted creature, so
                    // its presence identifies the group-B slot. The two already-
                    // chosen group-A sources must be excluded (CR 115.4 "other").
                    saw_group_b = true;
                    assert!(
                        !legal.contains(&obj(ench_a)) && !legal.contains(&obj(ench_b)),
                        "group B must exclude the already-chosen group-A sources \
                         (CR 115.4 distinctness), legal = {legal:?}"
                    );
                    obj(un_ench)
                } else {
                    // Group A: enchanted creatures only. Reach-guard below asserts
                    // BOTH enchanted creatures are offered (via chose_group_a==2).
                    assert!(
                        !legal.contains(&obj(un_ench)),
                        "group A (enchanted creatures) must NOT offer the un-enchanted \
                         creature, legal = {legal:?}"
                    );
                    let pick = [ench_a, ench_b]
                        .into_iter()
                        .find(|c| legal.contains(&obj(*c)) && !chose_group_a.contains(c))
                        .expect("an enchanted creature must be offered in a group-A slot");
                    chose_group_a.push(pick);
                    obj(pick)
                };
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(choice),
                    })
                    .expect("declaring the slot target must succeed");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected waiting state during Graceful targeting: {other:?}"),
        }
    }

    assert_eq!(
        chose_group_a.len(),
        2,
        "reach-guard: BOTH enchanted creatures must be offered+chosen across the group-A slots"
    );
    assert!(saw_group_b, "reach-guard: the group-B slot must surface");
    assert!(
        saw_recipient,
        "reach-guard: the recipient slot must surface"
    );
}

/// Friendly Rivalry — the SAME compound class with a SINGULAR (count-1) group A.
/// The deleted honest-deferral gate also covered this card; without the count-1
/// arm it regresses to a wrong-but-green single-group parse that drops group B.
/// A group-A creature (power 3) and the group-B legendary creature (power 2) each
/// deal their own power to a 0/12 recipient: 3 + 2 = 5. Dropping group B (or
/// reverting the count-1 arm) leaves 3; both flip the exact `== 5` assertion.
#[test]
fn friendly_rivalry_singular_group_a_plus_legendary_group_b() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let group_a = scenario.add_vanilla(P0, 3, 3);
    // Group B must be a LEGENDARY creature you control (the filter restriction).
    let legend_b = scenario
        .add_creature(P0, "Legend 2/2", 2, 2)
        .as_legendary()
        .id();
    let recipient = scenario.add_vanilla(P1, 0, 12);

    let spell = scenario
        // CR 601.2: Friendly Rivalry is an {R}{G} Instant (is_instant = true).
        .add_spell_to_hand_from_oracle(P0, "Friendly Rivalry", true, FRIENDLY_RIVALRY)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    let outcome = runner
        .cast(spell)
        .target_objects(&[group_a, legend_b, recipient])
        .resolve();

    assert_eq!(
        outcome.state().objects[&recipient].damage_marked,
        5,
        "singular group A (3) + legendary group B (2) = 5; dropping group B (or reverting the \
         count-1 arm) would leave 3"
    );
}

/// Friendly Rivalry group-B LEGENDARY filter, discriminated at runtime through
/// the real cast pipeline. Group B is restricted to LEGENDARY creatures you
/// control (CR 205.4a `HasSupertype`), so a non-legendary creature you control
/// is NOT a legal group-B source. The board offers, as the "other" source, BOTH
/// a non-legendary creature (power 4, listed FIRST) and a legendary one (power
/// 2). The greedy target driver would grab the power-4 non-legendary for group B
/// if the supertype filter were absent (Σ = 1 + 4 = 5); because the filter holds
/// it is rejected and group B takes the legendary (Σ = 1 + 2 = 3). The exact
/// `== 3` assertion flips to 5 if the `HasSupertype{Legendary}` filter is dropped.
/// The legendary one being consumed as a source (its 2 lands) is the positive
/// reach-guard that the negative (non-legendary rejected) is not vacuous.
#[test]
fn friendly_rivalry_group_b_requires_legendary() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);

    let mand_a = scenario.add_vanilla(P0, 1, 1);
    let non_legend_other = scenario.add_vanilla(P0, 4, 4);
    let legend_other = scenario
        .add_creature(P0, "Legend 2/2", 2, 2)
        .as_legendary()
        .id();
    let recipient = scenario.add_vanilla(P1, 0, 12);

    let spell = scenario
        // CR 601.2: Friendly Rivalry is an {R}{G} Instant (is_instant = true).
        .add_spell_to_hand_from_oracle(P0, "Friendly Rivalry", true, FRIENDLY_RIVALRY)
        .with_mana_cost(ManaCost::zero())
        .id();

    let mut runner = scenario.build();
    // Group A = mand_a (1). Group B: the non-legendary power-4 creature is offered
    // FIRST but must be rejected (not legendary); group B takes the legendary (2).
    let outcome = runner
        .cast(spell)
        .target_objects(&[mand_a, non_legend_other, legend_other, recipient])
        .resolve();

    assert_eq!(
        outcome.state().objects[&recipient].damage_marked,
        3,
        "group B rejects the non-legendary power-4 creature and takes the legendary power-2 one: \
         Σ = 1 + 2 = 3. A dropped HasSupertype{{Legendary}} filter would take the power-4 → 5"
    );
}
