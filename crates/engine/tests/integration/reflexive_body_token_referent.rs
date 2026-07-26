//! BB-FU9 — a clause whose subject is the token published by a clause under an
//! AFFIRMATIVE reflexive gate ("When you do, create a token. Put a +1/+1 counter
//! on that token.") binds that token, and is re-linked onto the gated
//! instruction's continuation path so the resolver's condition-false descent
//! cannot resolve it against a STALE entry of `state.last_created_token_ids` —
//! a game-lifetime ledger that is never cleared at a resolution boundary
//! (CR 603.12 + CR 608.2c + CR 609.3).
//!
//! Every row drives the REAL pipeline — `parse_oracle_text` /
//! `parse_effect_chain`, then `build_resolved_from_def_with_targets` +
//! `resolve_ability_chain` + `runner.act(DecideOptionalEffect)` — and asserts
//! RESOLVED board state. Four rows (T14, T15, T18, T19) are explicitly
//! AST-SHAPE rows: what they pin is that a `sub_link` does not LIE about which
//! instruction a clause belongs to, which is a parse-time property with no
//! behavioural twin (the condition-false descent resolves the clause either
//! way, from a different parent).
//!
//! **`engine::ai_support::legal_actions()` is never called here.** It simulates
//! every candidate action on a CLONE and silently swallows the mutation being
//! measured; this lane read a false 0 that way before.
//!
//! Synthetic Oracle text (T8, T12–T15, T17, T19, T20) is disclosed as such and
//! is templated verbatim from shipped patterns: "you may pay {N}. If you do,
//! create …" (Akoum Stonewaker, Krenko, Cadric) and "If you didn't create a
//! token this way, create …" (Springheart Nantuko). The two-publisher and
//! intervening-instruction shapes have no shipped witness, which is why they
//! are synthetic. Every shipped-card claim is asserted on the shipped card
//! (T1, T2, T3, T5, T6, T7, T9, T10, T11, T16, T18, T21), with Oracle text
//! byte-verbatim from MTGJSON `AtomicCards.json`.

use engine::game::ability_utils::build_resolved_from_def_with_targets;
use engine::game::effects::resolve_ability_chain;
use engine::game::game_object::AttachTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::parser::oracle_effect::parse_effect_chain;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, SubAbilityLink, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::KeywordKind;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

// ─────────────────────────────────────────────────────────────────────────────
// Shipped-card Oracle text — byte-verbatim from `data/mtgjson/AtomicCards.json`.
// A paraphrase can take a different parser branch, so a paraphrased fixture can
// go green while the real card stays broken.
// ─────────────────────────────────────────────────────────────────────────────

const IROH: &str = "When Iroh enters, create a Food token.\nAt the beginning of combat on your turn, you may have target opponent gain control of target permanent you control. When you do, create a 1/1 white Ally creature token. Put a +1/+1 counter on that token for each permanent you own that your opponents control.";
/// The Iroh reflexive BODY with the gate stripped — the ungated positive
/// control for T4.
const IROH_BODY: &str = "Create a 1/1 white Ally creature token. Put a +1/+1 counter on that token for each permanent you own that your opponents control.";
const SUMMONERS_SENDING: &str = "At the beginning of your end step, you may exile target creature card from a graveyard. If you do, create a 1/1 white Spirit creature token with flying. Put a +1/+1 counter on it if the exiled card's mana value is 4 or greater.";
const NORTH_POLE: &str = "At the beginning of your upkeep, target opponent draws a card and creates a Treasure token.\nWhenever chaos ensues, create a 2/2 white Alien creature token. When you do, tap target nontoken creature an opponent controls. Put a stun counter on it. (If a permanent with a stun counter would become untapped, remove one from it instead.)";
const RATONHNHAKETON: &str = "As long as Ratonhnhak\u{e9}\u{a789}ton hasn't dealt damage yet, it has hexproof and can't be blocked.\nWhenever Ratonhnhak\u{e9}\u{a789}ton deals combat damage to a player, create a 1/1 black Assassin creature token with menace. When you do, return target Equipment card from your graveyard to the battlefield, then attach it to that token.";
const AKOUM: &str = "Landfall \u{2014} Whenever a land you control enters, you may pay {2}{R}. If you do, create a 3/1 red Elemental creature token with trample and haste. Exile that token at the beginning of the next end step.";
const SAHEELI: &str = "Whenever you cast an Artificer or artifact spell, you get {E} (an energy counter).\nAt the beginning of combat on your turn, you may pay {E}{E}{E}. When you do, create a token that's a copy of target permanent you control, except it's a 5/5 artifact creature in addition to its other types and has haste. Sacrifice it at the beginning of the next end step.";
const CADRIC: &str = "The \"legend rule\" doesn't apply to tokens you control.\nWhenever another nontoken legendary permanent you control enters, you may pay {1}. If you do, create a token that's a copy of it. That token gains haste. Sacrifice it at the beginning of the next end step.";
const REBELLION: &str = "Whenever you clash, you may pay {1}. If you do, create a 3/1 red Elemental Shaman creature token. If you won, that token gains haste until end of turn. (This ability triggers after the clash ends.)";

// ─────────────────────────────────────────────────────────────────────────────
// Synthetic building-block fixtures (disclosed above).
// ─────────────────────────────────────────────────────────────────────────────

const BASE_IFYOUDO: &str = "When this creature enters, you may pay {2}. If you do, create a 1/1 white Ally creature token. Put a +1/+1 counter on that token.";
const BASE_WHENYOUDO: &str = "When this creature enters, you may pay {2}. When you do, create a 1/1 white Ally creature token. Put a +1/+1 counter on that token.";
const COMPLEMENT: &str = "When this creature enters, you may pay {2}. If you do, create a 1/1 white Ally creature token. If you didn't create a token this way, create a 1/1 green Insect creature token.";
const INTERVENING_UNGATED: &str = "When this creature enters, create a 1/1 white Ally creature token. You may pay {2}. If you do, draw a card. Put a +1/+1 counter on that token.";
const TWOPUB_GATED_LAST: &str = "When this creature enters, create a 1/1 white Ally creature token. You may pay {2}. If you do, create a 1/1 green Insect creature token. Put a +1/+1 counter on that token.";
const TWOPUB_UNGATED_LAST: &str = "When this creature enters, you may pay {2}. If you do, create a 1/1 green Insect creature token. Create a 1/1 white Ally creature token. Put a +1/+1 counter on that token.";
const GAP1_IFYOUDO: &str = "When this creature enters, you may pay {2}. If you do, create a 1/1 white Ally creature token. You gain 1 life. Put a +1/+1 counter on that token.";
const GAP1_WHENYOUDO: &str = "When this creature enters, you may pay {2}. When you do, create a 1/1 white Ally creature token. You gain 1 life. Put a +1/+1 counter on that token.";
const GAP1_DELAYED: &str = "When this creature enters, you may pay {2}. If you do, create a 1/1 white Ally creature token. You gain 1 life. Exile that token at the beginning of the next end step.";

// ─────────────────────────────────────────────────────────────────────────────
// Shared helpers — each used by more than one test.
// ─────────────────────────────────────────────────────────────────────────────

fn add_object(runner: &mut GameRunner, player: PlayerId, name: &str, zone: Zone) -> ObjectId {
    let state = runner.state_mut();
    let card_id = CardId(state.next_object_id);
    create_object(state, card_id, player, name.to_string(), zone)
}

fn add_creature(runner: &mut GameRunner, player: PlayerId, name: &str) -> ObjectId {
    let id = add_object(runner, player, name, Zone::Battlefield);
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types.push(CoreType::Creature);
    obj.base_card_types = obj.card_types.clone();
    obj.power = Some(2);
    obj.toughness = Some(2);
    id
}

fn counter(runner: &GameRunner, id: ObjectId, kind: &CounterType) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(kind)
        .copied()
        .unwrap_or(0)
}

fn p1p1(runner: &GameRunner, id: ObjectId) -> u32 {
    counter(runner, id, &CounterType::Plus1Plus1)
}

fn kw_counter(runner: &GameRunner, id: ObjectId, k: KeywordKind) -> u32 {
    counter(runner, id, &CounterType::Keyword(k))
}

/// The multi-authority second `LastCreated` candidate: a token from an EARLIER
/// resolution, sitting in the game-lifetime `last_created_token_ids` ledger. A
/// stale bind lands HERE, so every negative assertion names this object rather
/// than saying "not the source".
fn pre_seed_decoy(runner: &mut GameRunner) -> ObjectId {
    let decoy = add_creature(runner, P0, "STALE DECOY TOKEN");
    runner.state_mut().objects.get_mut(&decoy).unwrap().is_token = true;
    runner.state_mut().last_created_token_ids = vec![decoy];
    decoy
}

/// In-test revert-probe for the §7.3 re-link pass: restore the BASE
/// `SequentialSibling` link on every node the pass re-tagged.
fn unrelink(def: &mut AbilityDefinition) {
    if let Some(sub) = def.sub_ability.as_deref_mut() {
        if reads_last_created(sub) && sub.sub_link == SubAbilityLink::ContinuationStep {
            sub.sub_link = SubAbilityLink::SequentialSibling;
        }
        unrelink(sub);
    }
    if let Some(e) = def.else_ability.as_deref_mut() {
        unrelink(e);
    }
}

/// Grant mana only when `funded`. `drive_ship` seeds energy separately, so a
/// false fixture can still drive Saheeli's `{E}{E}{E}` gate false in T16.
fn fund(scenario: &mut GameScenario, funded: bool) {
    if funded {
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Red, ObjectId(9_999), false, vec![]); 6],
        );
    }
}

/// Descend a definition's spine by a path of `s` (sub_ability) / `e`
/// (else_ability) steps: `""` is the definition itself, `"ss"` its
/// grandchild.
fn node_at<'a>(def: &'a AbilityDefinition, path: &str) -> &'a AbilityDefinition {
    let mut cursor = def;
    for (i, step) in path.chars().enumerate() {
        cursor = match step {
            's' => cursor.sub_ability.as_deref(),
            'e' => cursor.else_ability.as_deref(),
            other => panic!("bad path step {other:?}"),
        }
        .unwrap_or_else(|| panic!("path {path:?} has no node at step {i}"));
    }
    cursor
}

fn sub_link_at(def: &AbilityDefinition, path: &str) -> SubAbilityLink {
    node_at(def, path).sub_link
}

/// The `TargetFilter` a named node's effect carries — the seeder-side property
/// (`PutCounter.target`), which `sub_link` alone cannot see.
fn target_at(def: &AbilityDefinition, path: &str) -> TargetFilter {
    node_at(def, path)
        .effect
        .target_filter()
        .cloned()
        .unwrap_or_else(|| panic!("node at {path:?} carries no target filter"))
}

/// Does any position inside this ONE node's effect read `LastCreated`? Mirrors
/// `lower::ability_reads_last_created`'s coverage (`GenericEffect` grant
/// recipients and a `CreateDelayedTrigger`'s inner definition included) by
/// asking the serialized effect, so the answer does not depend on an
/// enumeration of which `Effect` variants can carry the referent.
fn effect_reads_last_created(def: &AbilityDefinition) -> bool {
    serde_json::to_string(&*def.effect)
        .expect("effect serializes")
        .contains("\"LastCreated\"")
}

/// Same question over the whole within-clause chain (node + sub + else).
fn reads_last_created(def: &AbilityDefinition) -> bool {
    effect_reads_last_created(def)
        || def.sub_ability.as_deref().is_some_and(reads_last_created)
        || def.else_ability.as_deref().is_some_and(reads_last_created)
}

/// Visit this node and every node reachable through `sub_ability` /
/// `else_ability`.
fn walk(def: &AbilityDefinition, visit: &mut impl FnMut(&AbilityDefinition)) {
    visit(def);
    if let Some(s) = def.sub_ability.as_deref() {
        walk(s, visit);
    }
    if let Some(e) = def.else_ability.as_deref() {
        walk(e, visit);
    }
}

/// Every `AbilityDefinition` root a parsed card carries.
fn card_definitions(name: &str, text: &str, types: &[&str]) -> Vec<AbilityDefinition> {
    let types: Vec<String> = types.iter().map(|s| (*s).to_string()).collect();
    let parsed = parse_oracle_text(text, name, &[], &types, &[]);
    parsed
        .abilities
        .into_iter()
        .chain(
            parsed
                .triggers
                .into_iter()
                .filter_map(|t| t.execute)
                .map(|b| *b),
        )
        .chain(
            parsed
                .replacements
                .into_iter()
                .filter_map(|r| r.execute)
                .map(|b| *b),
        )
        .collect()
}

fn parse_trigger(text: &str, name: &str, types: &[&str], index: usize) -> AbilityDefinition {
    let types: Vec<String> = types.iter().map(|s| (*s).to_string()).collect();
    let parsed = parse_oracle_text(text, name, &[], &types, &[]);
    *parsed
        .triggers
        .get(index)
        .unwrap_or_else(|| panic!("{name} has a trigger[{index}]"))
        .execute
        .clone()
        .unwrap_or_else(|| panic!("{name} trigger[{index}] has an execute"))
}

fn resolve_def(
    runner: &mut GameRunner,
    def: &AbilityDefinition,
    source: ObjectId,
    targets: Vec<TargetRef>,
) {
    let resolved = build_resolved_from_def_with_targets(def, source, P0, targets);
    let mut events = Vec::new();
    let _ = resolve_ability_chain(runner.state_mut(), &resolved, &mut events, 0);
}

/// Answer the optional-payment prompt through the real action pipeline.
fn decide(runner: &mut GameRunner, accept: bool) -> bool {
    runner
        .act(GameAction::DecideOptionalEffect { accept })
        .is_ok()
}

/// Answer a sub-clause's `TriggerTargetSelection` prompt and let the resulting
/// stack object resolve. Used where the targeted clause is NOT the chain's head
/// (North Pole's `SetTapState`, Ratonhnhaké꞉ton's `ChangeZone`), so its target
/// cannot be pre-wired into `build_resolved_from_def_with_targets`'s slot list
/// and must go through the real `apply()` action pipeline.
fn select_targets_and_resolve(runner: &mut GameRunner, targets: Vec<TargetRef>) {
    runner
        .act(GameAction::SelectTargets { targets })
        .expect("the reflexive body's target slot accepts the declared target");
    runner.advance_until_stack_empty();
}

/// Tokens created during THIS resolution (the decoy is excluded by id).
fn live_tokens(runner: &GameRunner, decoy: ObjectId) -> Vec<ObjectId> {
    let mut v: Vec<ObjectId> = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| runner.state().objects[id].is_token && *id != decoy)
        .collect();
    v.sort();
    v
}

fn token_named(runner: &GameRunner, decoy: ObjectId, name: &str) -> ObjectId {
    live_tokens(runner, decoy)
        .into_iter()
        .find(|id| runner.state().objects[id].name == name)
        .unwrap_or_else(|| panic!("a token named {name:?} was created"))
}

/// Does any pending delayed trigger target `id`?
fn delayed_targets(runner: &GameRunner, id: ObjectId) -> bool {
    runner
        .state()
        .delayed_triggers
        .iter()
        .any(|d| d.ability.targets.contains(&TargetRef::Object(id)))
}

// ═════════════════════════════════════════════════════════════════════════════
// T1–T7 — shipped cards, gate-TRUE and the declines.
// ═════════════════════════════════════════════════════════════════════════════

/// T1 — Iroh's reflexive body ("When you do, create a 1/1 white Ally creature
/// token. Put a +1/+1 counter on that token for each permanent you own that
/// your opponents control.") binds the ALLY TOKEN, not the permanent given away
/// by the antecedent.
///
/// FLIPS: reverting the §7.2 gated-publisher carve-out re-binds the counter to
/// the given-away Bear — `bear +1/+1 == 0` reds (it becomes 1) and
/// `ally +1/+1 == 1` reds (it becomes 0).
#[test]
fn iroh_reflexive_body_counter_binds_created_token() {
    let mut runner = GameScenario::new().build();
    let source = add_creature(&mut runner, P0, "Iroh, Tea Master");
    let bear = add_creature(&mut runner, P0, "Bear");
    let decoy = pre_seed_decoy(&mut runner);

    let def = parse_trigger(IROH, "Iroh, Tea Master", &["Creature"], 1);
    resolve_def(
        &mut runner,
        &def,
        source,
        vec![TargetRef::Object(bear), TargetRef::Player(P1)],
    );
    assert!(decide(&mut runner, true), "the reflexive gate was accepted");

    let ally = token_named(&runner, decoy, "Ally");
    assert_eq!(
        p1p1(&runner, ally),
        1,
        "'that token' binds the Ally created by the reflexive body"
    );
    assert_eq!(
        p1p1(&runner, bear),
        0,
        "the given-away permanent (the antecedent's target) receives NO counter"
    );
    assert_eq!(
        p1p1(&runner, decoy),
        0,
        "the stale decoy receives no counter"
    );
}

/// T2 — Summoner's Sending: the counter lands on the SPIRIT token, not on the
/// creature card the antecedent exiled.
///
/// FLIPS: reverting the §7.2 carve-out moves the counter from the token
/// (1 → 0) onto the exiled card (0 → 1).
#[test]
fn summoners_sending_counter_binds_created_token() {
    let (runner, decoy, card) = drive_sending(4);
    let spirit = token_named(&runner, decoy, "Spirit");

    assert_eq!(
        p1p1(&runner, spirit),
        1,
        "'it' binds the created Spirit token (exiled card MV 4 satisfies the gate)"
    );
    assert_eq!(
        p1p1(&runner, card),
        0,
        "the exiled creature card receives NO counter"
    );
}

/// T3 — adjacent-value hostile: the same card with an MV-1 exiled card places
/// NO counter, and the negative is reach-guarded (the Spirit token really was
/// created and the card really is in exile, so the assertion cannot be
/// satisfied by an upstream short-circuit).
#[test]
fn summoners_sending_low_mv_places_no_counter() {
    let (runner, decoy, card) = drive_sending(1);
    let spirit = token_named(&runner, decoy, "Spirit");

    // Reach-guards.
    assert!(
        runner.state().objects[&spirit].is_token,
        "reach-guard: the Spirit token WAS created, so the consumer had a live referent"
    );
    assert_eq!(
        runner.state().objects[&card].zone,
        Zone::Exile,
        "reach-guard: the antecedent exile really happened (the mana-value input exists)"
    );

    assert_eq!(
        p1p1(&runner, spirit),
        0,
        "mana value 1 fails the 'is 4 or greater' condition — no counter"
    );
    assert_eq!(p1p1(&runner, card), 0, "and none on the exiled card either");
}

/// Shared Summoner's Sending drive: a creature card of mana value `mv` in a
/// graveyard, a pre-seeded decoy, the reflexive exile ACCEPTED.
fn drive_sending(mv: u32) -> (GameRunner, ObjectId, ObjectId) {
    let mut runner = GameScenario::new().build();
    let source = add_creature(&mut runner, P0, "Summoner's Sending");
    let card = add_object(&mut runner, P0, "Fat Creature", Zone::Graveyard);
    {
        let obj = runner.state_mut().objects.get_mut(&card).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.mana_cost = ManaCost::Cost {
            generic: mv,
            shards: vec![],
        };
    }
    let decoy = pre_seed_decoy(&mut runner);

    let def = parse_trigger(SUMMONERS_SENDING, "Summoner's Sending", &["Enchantment"], 0);
    resolve_def(&mut runner, &def, source, vec![TargetRef::Object(card)]);
    assert!(
        decide(&mut runner, true),
        "the reflexive exile was accepted"
    );
    (runner, decoy, card)
}

/// T4 — positive control. The Iroh BODY with no reflexive connector at all
/// still binds the created token, proving the harness, the per-permanent count
/// math and `LastCreated` resolution work independently of the gate.
#[test]
fn ungated_chain_still_binds_created_token() {
    let mut runner = GameScenario::new().build();
    let source = add_creature(&mut runner, P0, "Ungated Body");
    // One permanent P0 OWNS that an opponent CONTROLS — the "for each" count's
    // only member, so the expected value is 1 rather than a vacuous 0.
    // `base_controller` is load-bearing: the layer pass rewrites
    // `obj.controller` from `base_controller.unwrap_or(owner)` on every
    // recompute, so setting `controller` alone is silently undone.
    let loaned = add_creature(&mut runner, P0, "Loaned Bear");
    {
        let obj = runner.state_mut().objects.get_mut(&loaned).unwrap();
        obj.controller = P1;
        obj.base_controller = Some(P1);
    }
    let decoy = pre_seed_decoy(&mut runner);

    let def = parse_effect_chain(IROH_BODY, AbilityKind::Spell);
    resolve_def(&mut runner, &def, source, vec![]);

    let ally = token_named(&runner, decoy, "Ally");
    // Reach-guard: the dynamic count really has a member, so `== 1` below is a
    // measured count rather than an accidental zero.
    assert_eq!(
        runner.state().objects[&loaned].controller,
        P1,
        "reach-guard: the counted permanent really is owned by P0 and controlled by P1"
    );
    assert_eq!(
        p1p1(&runner, ally),
        1,
        "ungated 'that token' binds the created Ally (one permanent owned by P0 under P1's control)"
    );
    assert_eq!(
        p1p1(&runner, decoy),
        0,
        "the stale decoy receives no counter"
    );
}

/// T5 — negative control. North Pole Research Base's reflexive body is
/// "When you do, tap target nontoken creature an opponent controls. Put a stun
/// counter on it." — its gated clause is a `SetTapState`, NOT a token
/// publisher, so "it" must keep binding the TAPPED CREATURE even though an
/// Alien token was created earlier in the same chain.
#[test]
fn north_pole_stun_counter_stays_on_tapped_creature() {
    let mut runner = GameScenario::new().build();
    let source = add_creature(&mut runner, P0, "North Pole Research Base");
    let victim = add_creature(&mut runner, P1, "Opponent Bear");
    let decoy = pre_seed_decoy(&mut runner);

    let def = parse_trigger(NORTH_POLE, "North Pole Research Base", &["Land"], 1);
    resolve_def(&mut runner, &def, source, vec![]);
    select_targets_and_resolve(&mut runner, vec![TargetRef::Object(victim)]);

    let alien = token_named(&runner, decoy, "Alien");
    // Reach-guards: the token publisher DID run and the reflexive body DID tap.
    assert!(
        runner.state().objects[&alien].is_token,
        "reach-guard: the Alien token exists, so a LastCreated mis-bind had a live target"
    );
    assert!(
        runner.state().objects[&victim].tapped,
        "reach-guard: the reflexive body really resolved (the target is tapped)"
    );

    let stun = CounterType::Stun;
    assert_eq!(
        counter(&runner, victim, &stun),
        1,
        "'it' after a SetTapState binds the tapped creature (CR 608.2c)"
    );
    assert_eq!(
        counter(&runner, alien, &stun),
        0,
        "the Alien token created earlier in the chain receives NO stun counter"
    );
}

/// T6 — no-regression control. Ratonhnhaké꞉ton's reflexive body returns an
/// Equipment card and attaches it to "that token": the NON-counter consumer of
/// a gated publisher's referent must still bind the new Assassin token.
#[test]
fn ratonhnhaketon_attach_still_binds_created_token() {
    let mut runner = GameScenario::new().build();
    let source = add_creature(&mut runner, P0, "Ratonhnhak\u{e9}\u{a789}ton");
    let equipment = add_object(&mut runner, P0, "Bone Saw", Zone::Graveyard);
    {
        let obj = runner.state_mut().objects.get_mut(&equipment).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.card_types.subtypes.push("Equipment".to_string());
        obj.base_card_types = obj.card_types.clone();
    }
    let decoy = pre_seed_decoy(&mut runner);

    let def = parse_trigger(
        RATONHNHAKETON,
        "Ratonhnhak\u{e9}\u{a789}ton",
        &["Creature"],
        0,
    );
    resolve_def(&mut runner, &def, source, vec![]);
    select_targets_and_resolve(&mut runner, vec![TargetRef::Object(equipment)]);

    let assassin = token_named(&runner, decoy, "Assassin");
    // Reach-guard: the antecedent return really happened, so the attach clause
    // had something to attach.
    assert_eq!(
        runner.state().objects[&equipment].zone,
        Zone::Battlefield,
        "reach-guard: the Equipment really returned from the graveyard"
    );
    assert_eq!(
        runner.state().objects[&equipment].attached_to,
        Some(AttachTarget::Object(assassin)),
        "the returned Equipment attaches to the NEWLY created Assassin token"
    );
    assert_ne!(
        runner.state().objects[&equipment].attached_to,
        Some(AttachTarget::Object(decoy)),
        "and never to the stale decoy"
    );
}

/// T7 — hostile: DECLINE. With the reflexive gate declined no token is created,
/// so the consumer must place nothing at all — in particular not on the decoy,
/// which is still the only entry in `last_created_token_ids`.
#[test]
fn declined_reflexive_body_places_no_counter_on_stale_token() {
    let mut runner = GameScenario::new().build();
    let source = add_creature(&mut runner, P0, "Iroh, Tea Master");
    let bear = add_creature(&mut runner, P0, "Bear");
    let decoy = pre_seed_decoy(&mut runner);

    let def = parse_trigger(IROH, "Iroh, Tea Master", &["Creature"], 1);
    resolve_def(
        &mut runner,
        &def,
        source,
        vec![TargetRef::Object(bear), TargetRef::Player(P1)],
    );
    assert!(
        decide(&mut runner, false),
        "the reflexive gate was DECLINED"
    );

    assert_eq!(
        live_tokens(&runner, decoy),
        Vec::<ObjectId>::new(),
        "reach-guard: declining really created no token"
    );
    assert_eq!(
        runner.state().last_created_token_ids,
        vec![decoy],
        "reach-guard: the decoy is still the only LastCreated candidate"
    );
    assert_eq!(
        p1p1(&runner, decoy),
        0,
        "the stale decoy receives NO counter on the declined path"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// T8–T17 — the gate-FALSE hazard, the shipped consumers, and the anti-over-skip
// controls.
// ═════════════════════════════════════════════════════════════════════════════

/// Drive one synthetic trigger fixture with a pre-seeded decoy, optionally
/// funded, optionally with the re-link reverted in-test.
struct Driven {
    runner: GameRunner,
    decoy: ObjectId,
    dlife: i32,
}

fn drive_synth(text: &str, funded: bool, revert: bool) -> Driven {
    let mut scenario = GameScenario::new();
    fund(&mut scenario, funded);
    let mut runner = scenario.build();
    let source = add_creature(&mut runner, P0, "Synth");
    let decoy = pre_seed_decoy(&mut runner);
    // Draw fuel, so a "Draw a card" reach-guard is measurable.
    add_object(&mut runner, P0, "Library Filler", Zone::Library);
    let life0 = runner.state().players[0].life;

    let mut def = parse_trigger(text, "Synth", &["Creature"], 0);
    if revert {
        unrelink(&mut def);
    }
    resolve_def(&mut runner, &def, source, vec![]);
    decide(&mut runner, true);

    let dlife = runner.state().players[0].life - life0;
    Driven {
        runner,
        decoy,
        dlife,
    }
}

/// T8 — THE HAZARD. Accept "you may pay {2}" with zero mana: `pay.rs` sets
/// `cost_payment_failed_flag`, the reflexive gate evaluates FALSE, and the
/// resolver's condition-false descent would resolve the counter clause with no
/// token created. Both connectors are measured — `If you do` and `When you do`
/// are NOT interchangeable at the resolver.
///
/// FLIPS: dropping the whole §7.3 re-link pass (or the in-test `unrelink`)
/// makes the decoy's `+1/+1` go 0 → 1 on BOTH gates.
#[test]
fn reflexive_gate_false_on_accept_places_no_counter_on_stale_token() {
    for (label, text) in [("if you do", BASE_IFYOUDO), ("when you do", BASE_WHENYOUDO)] {
        let d = drive_synth(text, false, false);
        assert!(
            d.runner.state().cost_payment_failed_flag,
            "[{label}] reach-guard: the payment really failed, so the gate really went FALSE"
        );
        assert_eq!(
            live_tokens(&d.runner, d.decoy),
            Vec::<ObjectId>::new(),
            "[{label}] reach-guard: no token was created on the false gate"
        );
        assert_eq!(
            p1p1(&d.runner, d.decoy),
            0,
            "[{label}] the stale decoy receives NO counter"
        );
    }
}

/// T9 — shipped `IfYouDo` consumer: Akoum Stonewaker's "Exile that token at the
/// beginning of the next end step." On a failed payment no delayed trigger may
/// be created at all, and none may name the decoy.
///
/// FLIPS: the in-test `unrelink` revert-probe makes a delayed `ChangeZone`
/// appear TARGETING THE DECOY.
#[test]
fn akoum_stonewaker_gate_false_creates_no_delayed_trigger_on_stale_token() {
    let d = drive_ship(AKOUM, 0, false, false, false);
    assert!(
        d.runner.state().cost_payment_failed_flag,
        "reach-guard: {{2}}{{R}} could not be paid, so the IfYouDo gate went FALSE"
    );
    assert!(
        !delayed_targets(&d.runner, d.decoy),
        "no delayed trigger targets the stale decoy"
    );
    assert!(
        d.runner.state().delayed_triggers.is_empty(),
        "and no delayed trigger was created at all"
    );
}

/// T10 — shipped cascade consumer: Cadric, Soul Kindler's "That token gains
/// haste. Sacrifice it at the beginning of the next end step." Both legs of the
/// cascade must be skipped on a false gate.
///
/// FLIPS: the in-test `unrelink` makes a delayed `Sacrifice` target the decoy.
#[test]
fn cadric_gate_false_skips_the_whole_cascade() {
    let d = drive_ship(CADRIC, 0, false, false, false);
    assert!(
        d.runner.state().cost_payment_failed_flag,
        "reach-guard: {{1}} could not be paid, so the IfYouDo gate went FALSE"
    );
    assert_eq!(
        live_tokens(&d.runner, d.decoy),
        Vec::<ObjectId>::new(),
        "reach-guard: no copy token was created"
    );
    assert_eq!(
        d.runner.state().objects[&d.decoy].keywords.len(),
        0,
        "leg 1: the decoy gained no haste"
    );
    assert!(
        !delayed_targets(&d.runner, d.decoy),
        "leg 2: no delayed Sacrifice targets the decoy"
    );
}

/// T11 — true-path parity. With the payment funded, all three shipped
/// consumers behave IDENTICALLY with and without the re-link: the token is
/// created and every consumer binds the NEW token.
#[test]
fn akoum_and_cadric_and_rebellion_gate_true_are_unchanged() {
    for revert in [false, true] {
        // Akoum: the delayed ChangeZone names the new Elemental.
        let d = drive_ship(AKOUM, 0, true, revert, false);
        let elemental = token_named(&d.runner, d.decoy, "Elemental");
        assert!(
            delayed_targets(&d.runner, elemental),
            "[revert={revert}] Akoum's delayed exile names the NEW token"
        );
        assert!(
            !delayed_targets(&d.runner, d.decoy),
            "[revert={revert}] and never the decoy"
        );

        // Cadric: the copy token exists WITH haste, and the delayed Sacrifice
        // names it.
        let d = drive_ship(CADRIC, 0, true, revert, false);
        let copy = *live_tokens(&d.runner, d.decoy)
            .first()
            .expect("Cadric's copy token exists");
        assert_eq!(
            d.runner.state().objects[&copy].keywords.len(),
            1,
            "[revert={revert}] Cadric's copy token gained haste"
        );
        assert!(
            delayed_targets(&d.runner, copy),
            "[revert={revert}] Cadric's delayed Sacrifice names the NEW token"
        );

        // Rebellion: the Elemental Shaman is created (its own EventOutcomeWon
        // tail is still evaluated).
        let d = drive_ship(REBELLION, 0, true, revert, false);
        token_named(&d.runner, d.decoy, "Elemental Shaman");
    }
}

/// T12 — anti-over-skip #1. Springheart Nantuko's templating: the tail carries
/// its OWN `Not{OptionalEffectPerformed}` complement condition, so an accept
/// with zero mana must STILL create the Insect. A blanket "skip the whole
/// reflexive descent" remedy fails here — this is the test that would have
/// caught the rejected round-2 design.
#[test]
fn reflexive_complement_branch_still_resolves_on_failed_payment() {
    let d = drive_synth(COMPLEMENT, false, false);
    assert!(
        d.runner.state().cost_payment_failed_flag,
        "reach-guard: the payment failed, so the complement branch is the live one"
    );
    let insect = token_named(&d.runner, d.decoy, "Insect");
    assert!(
        d.runner.state().objects[&insect].is_token,
        "the 'If you didn't create a token this way' branch still resolves"
    );
}

/// T13 — anti-over-skip #2. The token is created UNGATED and the false gate
/// sits on a NON-publisher clause ("If you do, draw a card"), so the live Ally
/// must still get its counter.
///
/// FLIPS: dropping the §7.3 publisher lookup (re-linking after ANY prior gated
/// clause) re-tags the tail `SequentialSibling → ContinuationStep` and the
/// live Ally's `+1/+1` goes 1 → 0.
#[test]
fn ungated_token_keeps_live_referent_across_a_false_gate() {
    let d = drive_synth(INTERVENING_UNGATED, false, false);
    let ally = token_named(&d.runner, d.decoy, "Ally");
    assert!(
        d.runner.state().cost_payment_failed_flag,
        "reach-guard: the intervening {{2}} really failed, so a false gate is on the path"
    );
    assert_eq!(
        p1p1(&d.runner, ally),
        1,
        "the UNGATED publisher's token still receives its counter"
    );
    assert_eq!(
        p1p1(&d.runner, d.decoy),
        0,
        "and the stale decoy receives none"
    );
}

/// T14 — two-publisher hostile pair. The NEAREST preceding publisher decides,
/// not "any gated publisher anywhere in the chain".
///
/// Behavioural: gated-last places nothing (the Insect was never created);
/// ungated-last still places on the live Ally.
///
/// AST-SHAPE (load-bearing): on ungated-last the consumer's `sub_link` STAYS
/// `SequentialSibling`. FLIPS: dropping the publisher-gate conjunct re-tags it
/// `ContinuationStep`. The drive rows are unchanged by that drop — the descent
/// resolves the ungated publisher's own chain either way — so the AST row is
/// what discriminates the conjunct, and is labelled as such.
#[test]
fn nearest_publisher_decides_the_relink() {
    let gated = drive_synth(TWOPUB_GATED_LAST, false, false);
    let ally = token_named(&gated.runner, gated.decoy, "Ally");
    assert!(
        gated.runner.state().cost_payment_failed_flag,
        "reach-guard: the {{2}} failed, so the nearest (Insect) publisher is gate-FALSE"
    );
    assert_eq!(
        p1p1(&gated.runner, ally),
        0,
        "gated-last: the consumer belongs to the ungated Ally's SUCCESSOR gate and places nothing"
    );
    assert_eq!(
        p1p1(&gated.runner, gated.decoy),
        0,
        "gated-last: and nothing on the stale decoy"
    );

    let ungated = drive_synth(TWOPUB_UNGATED_LAST, false, false);
    let ally = token_named(&ungated.runner, ungated.decoy, "Ally");
    assert_eq!(
        p1p1(&ungated.runner, ally),
        1,
        "ungated-last: the nearest publisher is UNGATED, so the live Ally gets its counter"
    );
    assert_eq!(
        p1p1(&ungated.runner, ungated.decoy),
        0,
        "ungated-last: and nothing on the stale decoy"
    );

    // AST-SHAPE row.
    let def = parse_trigger(TWOPUB_UNGATED_LAST, "Synth", &["Creature"], 0);
    let consumer = "sss";
    assert!(
        matches!(target_at(&def, consumer), TargetFilter::LastCreated),
        "reach-guard: the consumer really is the LastCreated reader"
    );
    assert_eq!(
        sub_link_at(&def, consumer),
        SubAbilityLink::SequentialSibling,
        "an UNGATED nearest publisher must NOT be re-linked (CR 608.2c: still the next \
         independent instruction)"
    );
}

/// T15 — an independent instruction between the gated publisher and a
/// DELAYED-TRIGGER consumer blocks the re-link. AST-SHAPE row: the consumer's
/// `sub_link` stays `SequentialSibling`, because re-tagging it would make it a
/// continuation step of the intervening sibling — the descent selects that
/// sibling and resolves the consumer anyway, so the conjunct's job is to stop
/// `sub_link` from LYING, not to change behaviour.
///
/// FLIPS: dropping `gated_instruction_reaches` re-tags the
/// `CreateDelayedTrigger` consumer to `ContinuationStep`.
#[test]
fn intervening_instruction_blocks_the_relink() {
    let d = drive_synth(GAP1_DELAYED, false, false);
    assert!(
        d.runner.state().cost_payment_failed_flag,
        "reach-guard: the payment failed, so the gate went FALSE"
    );
    assert_eq!(
        d.dlife, 1,
        "reach-guard: the intervening 'You gain 1 life.' really resolved"
    );

    let def = parse_trigger(GAP1_DELAYED, "Synth", &["Creature"], 0);
    let mut delayed_links = Vec::new();
    walk(&def, &mut |node| {
        if matches!(&*node.effect, Effect::CreateDelayedTrigger { .. }) {
            delayed_links.push((node.sub_link, effect_reads_last_created(node)));
        }
    });
    assert_eq!(
        delayed_links.len(),
        1,
        "reach-guard: exactly one CreateDelayedTrigger consumer was parsed"
    );
    assert!(
        delayed_links[0].1,
        "reach-guard: that consumer really reads LastCreated"
    );
    assert_eq!(
        delayed_links[0].0,
        SubAbilityLink::SequentialSibling,
        "an intervening independent instruction blocks the re-link (CR 608.2c)"
    );
}

/// T16 — shipped `WhenYouDo` consumer on the FALSE path: Saheeli, Radiant
/// Creator's "Sacrifice it at the beginning of the next end step." with zero
/// energy. `fund` grants energy only when funded, which is what lets this gate
/// go false at all.
///
/// FLIPS: the in-test `unrelink` makes a delayed `Sacrifice` target the decoy.
#[test]
fn saheeli_when_you_do_gate_false_creates_no_delayed_trigger_on_stale_token() {
    let d = drive_ship(SAHEELI, 1, false, false, true);
    assert!(
        d.runner.state().cost_payment_failed_flag,
        "reach-guard: {{E}}{{E}}{{E}} could not be paid with 0 energy, so WhenYouDo went FALSE"
    );
    assert!(
        !delayed_targets(&d.runner, d.decoy),
        "no delayed Sacrifice targets the stale decoy"
    );
    assert!(
        d.runner.state().delayed_triggers.is_empty(),
        "and no delayed trigger was created at all"
    );
}

/// T17 — round-5's seeder-side conjunct: an independent instruction between the
/// gated publisher and a COUNTER consumer blocks the SEED, so the consumer never
/// acquires a `LastCreated` target in the first place.
///
/// FLIPS: dropping the seeder's reach conjunct puts the decoy's `+1/+1` at
/// 0 → 1 on BOTH connectors.
#[test]
fn intervening_instruction_blocks_the_seed() {
    for (label, text) in [("if you do", GAP1_IFYOUDO), ("when you do", GAP1_WHENYOUDO)] {
        let d = drive_synth(text, false, false);
        assert!(
            d.runner.state().cost_payment_failed_flag,
            "[{label}] reach-guard: the payment failed, so the gate went FALSE"
        );
        assert_eq!(
            d.dlife, 1,
            "[{label}] reach-guard: the intervening 'You gain 1 life.' really resolved"
        );
        assert_eq!(
            p1p1(&d.runner, d.decoy),
            0,
            "[{label}] the stale decoy receives NO counter"
        );

        // The seeder-side property `sub_link` alone cannot see: the PutCounter
        // never became a `LastCreated` reader.
        let def = parse_trigger(text, "Synth", &["Creature"], 0);
        let mut put_targets = Vec::new();
        walk(&def, &mut |node| {
            if matches!(&*node.effect, Effect::PutCounter { .. }) {
                put_targets.push(node.effect.target_filter().cloned());
            }
        });
        assert_eq!(
            put_targets.len(),
            1,
            "[{label}] reach-guard: exactly one PutCounter consumer was parsed"
        );
        assert!(
            !matches!(put_targets[0], Some(TargetFilter::LastCreated)),
            "[{label}] the blocked seed leaves the PutCounter off LastCreated (got {:?})",
            put_targets[0]
        );
    }
}

/// Shared shipped-card drive. `trig` selects the trigger, `targeted` supplies a
/// real object target (Saheeli's `CopyTokenOf`), `revert` applies the in-test
/// `unrelink` revert-probe.
fn drive_ship(text: &str, trig: usize, funded: bool, revert: bool, targeted: bool) -> Driven {
    let mut scenario = GameScenario::new();
    fund(&mut scenario, funded);
    let mut runner = scenario.build();
    let source = add_creature(&mut runner, P0, "Src");
    let decoy = pre_seed_decoy(&mut runner);
    let victim = add_creature(&mut runner, P0, "Victim");
    if funded {
        runner.state_mut().players[0].energy = 3;
    }
    // Give `CopyTokenOf { TriggeringSource }` a real antecedent object.
    runner.state_mut().current_trigger_event =
        Some(engine::types::events::GameEvent::PermanentUntapped { object_id: victim });
    let life0 = runner.state().players[0].life;

    let mut def = parse_trigger(text, "Src", &["Creature"], trig);
    if revert {
        unrelink(&mut def);
    }
    let targets = if targeted {
        vec![TargetRef::Object(victim)]
    } else {
        vec![]
    };
    resolve_def(&mut runner, &def, source, targets);
    decide(&mut runner, true);

    let dlife = runner.state().players[0].life - life0;
    Driven {
        runner,
        decoy,
        dlife,
    }
}

// ═════════════════════════════════════════════════════════════════════════════
// T18 — the corpus-complete stale-bind invariant over the 20 cards whose AST
// this change moved.
// ═════════════════════════════════════════════════════════════════════════════

/// The 20 cards of the BASE↔POST AST diff over all 34348 corpus cards, embedded
/// as inline consts — name, Oracle text and `types` byte-verbatim from
/// `AtomicCards.json`. Hermetic on purpose: `data/mtgjson/AtomicCards.json` and
/// `client/public/card-data.json` are both gitignored and CI's `rust-test` job
/// is a bare checkout + `cargo nextest run`, so a corpus-file form of this test
/// could only be permanently red or permanently skipped.
const CHANGED: &[(&str, &str, &[&str])] = &[
    (
        "Akoum Stonewaker",
        "Landfall — Whenever a land you control enters, you may pay {2}{R}. If you do, create a 3/1 red Elemental creature token with trample and haste. Exile that token at the beginning of the next end step.",
        &["Creature"],
    ),
    (
        "Blight Herder",
        "When you cast this spell, you may put two cards your opponents own from exile into their owners' graveyards. If you do, create three 1/1 colorless Eldrazi Scion creature tokens. They have \"Sacrifice this token: Add {C}.\"",
        &["Creature"],
    ),
    (
        "Boulder Jockey",
        "({D} is a land drop. You may give up one potential land drop this turn to pay for {D}.)\nWhenever Boulder Jockey attacks, you may pay {D}. If you do, create a 3/3 colorless Construct artifact creature token named Boulder that's tapped and attacking. Sacrifice that token at the beginning of the next end step.",
        &["Creature"],
    ),
    (
        "Cadric, Soul Kindler",
        "The \"legend rule\" doesn't apply to tokens you control.\nWhenever another nontoken legendary permanent you control enters, you may pay {1}. If you do, create a token that's a copy of it. That token gains haste. Sacrifice it at the beginning of the next end step.",
        &["Creature"],
    ),
    (
        "Dalek Intensive Care",
        "When you planeswalk to Dalek Intensive Care and at the beginning of your upkeep, exile a non-Dalek creature you control. If you do, create a 3/3 black Dalek artifact creature token with menace. It gains haste until end of turn.\nWhenever chaos ensues, target Dalek you control deals damage equal to its power to target creature you don't control.",
        &["Plane"],
    ),
    (
        "Felhide Spiritbinder",
        "Inspired — Whenever this creature becomes untapped, you may pay {1}{R}. If you do, create a token that's a copy of another target creature, except it's an enchantment in addition to its other types. It gains haste. Exile it at the beginning of the next end step.",
        &["Creature"],
    ),
    (
        "Flameshadow Conjuring",
        "Whenever a nontoken creature you control enters, you may pay {R}. If you do, create a token that's a copy of that creature. That token gains haste. Exile it at the beginning of the next end step.",
        &["Enchantment"],
    ),
    (
        "God-Pharaoh's Gift",
        "At the beginning of combat on your turn, you may exile a creature card from your graveyard. If you do, create a token that's a copy of that card, except it's a 4/4 black Zombie. It gains haste until end of turn.",
        &["Artifact"],
    ),
    (
        "Gyrus, Waker of Corpses",
        "Gyrus enters with a number of +1/+1 counters on it equal to the amount of mana spent to cast it.\nWhenever Gyrus attacks, you may exile target creature card with lesser power from your graveyard. If you do, create a token that's a copy of that card and that's tapped and attacking. Exile the token at end of combat.",
        &["Creature"],
    ),
    (
        "Inalla, Archmage Ritualist",
        "Eminence — Whenever another nontoken Wizard you control enters, if Inalla is in the command zone or on the battlefield, you may pay {1}. If you do, create a token that's a copy of that Wizard. The token gains haste. Exile it at the beginning of the next end step.\nTap five untapped Wizards you control: Target player loses 7 life.",
        &["Creature"],
    ),
    (
        "Iroh, Tea Master",
        "When Iroh enters, create a Food token.\nAt the beginning of combat on your turn, you may have target opponent gain control of target permanent you control. When you do, create a 1/1 white Ally creature token. Put a +1/+1 counter on that token for each permanent you own that your opponents control.",
        &["Creature"],
    ),
    (
        "Kavaron Harrier",
        "Whenever this creature attacks, you may pay {2}. If you do, create a 2/2 colorless Robot artifact creature token that's tapped and attacking. Sacrifice that token at end of combat.",
        &["Artifact", "Creature"],
    ),
    (
        "Krenko, Baron of Tin Street",
        "Haste\n{T}, Sacrifice an artifact: Put a +1/+1 counter on each Goblin you control.\nWhenever an artifact is put into a graveyard from the battlefield, you may pay {R}. If you do, create a 1/1 red Goblin creature token. It gains haste until end of turn.",
        &["Creature"],
    ),
    (
        "Rebellion of the Flamekin",
        "Whenever you clash, you may pay {1}. If you do, create a 3/1 red Elemental Shaman creature token. If you won, that token gains haste until end of turn. (This ability triggers after the clash ends.)",
        &["Kindred", "Enchantment"],
    ),
    (
        "Saheeli, Radiant Creator",
        "Whenever you cast an Artificer or artifact spell, you get {E} (an energy counter).\nAt the beginning of combat on your turn, you may pay {E}{E}{E}. When you do, create a token that's a copy of target permanent you control, except it's a 5/5 artifact creature in addition to its other types and has haste. Sacrifice it at the beginning of the next end step.",
        &["Creature"],
    ),
    (
        "Séance",
        "At the beginning of each upkeep, you may exile target creature card from your graveyard. If you do, create a token that's a copy of that card, except it's a Spirit in addition to its other types. Exile it at the beginning of the next end step.",
        &["Enchantment"],
    ),
    (
        "Summoner's Sending",
        "At the beginning of your end step, you may exile target creature card from a graveyard. If you do, create a 1/1 white Spirit creature token with flying. Put a +1/+1 counter on it if the exiled card's mana value is 4 or greater.",
        &["Enchantment"],
    ),
    (
        "Timothar, Baron of Bats",
        "Ward—Discard a card.\nWhenever another nontoken Vampire you control dies, you may pay {1} and exile it. If you do, create a 1/1 black Bat creature token with flying. It gains \"When this token deals combat damage to a player, sacrifice it and return the exiled card to the battlefield tapped.\"",
        &["Creature"],
    ),
    (
        "Ultron, Artificial Malevolence",
        "Whenever another nontoken artifact you control enters, you may pay {2}. If you do, create a token that's a copy of it. If the token isn't a creature, it becomes a 2/2 Robot Villain creature in addition to its other types.",
        &["Artifact", "Creature"],
    ),
    (
        "Vile Redeemer",
        "Devoid (This card has no color.)\nFlash\nWhen you cast this spell, you may pay {C}. If you do, create a 1/1 colorless Eldrazi Scion creature token for each nontoken creature that died under your control this turn. Those tokens have \"Sacrifice this token: Add {C}.\"",
        &["Creature"],
    ),
];

/// T18 — no node tagged `SequentialSibling` reads `TargetFilter::LastCreated`,
/// over the 20 cards whose AST this change moved. Such a node is resolved by the
/// condition-false descent against `state.last_created_token_ids`, a
/// game-lifetime ledger, so it would bind a token from an EARLIER resolution.
///
/// SCOPE: a FROZEN list. Because a card acquires a new `LastCreated` bind from
/// this change only if its parsed AST changed, and the BASE↔POST diff over all
/// 34348 cards is exactly these 20, checking the invariant here checks it over
/// the corpus AS OF THIS CHANGE — it cannot see a card that acquires the shape
/// later.
///
/// ONE exemption, and it is the same predicate the shipping guard uses:
/// `AbilityDefinition::is_self_gated_reflexive` (`else_ability.is_none()` and
/// condition `EffectOutcome{OptionalEffectPerformed}`). A self-gated node is
/// dropped by the descent's own false-condition skip wherever it sits, so it
/// cannot stale-bind. The test CALLS that method rather than re-deriving it, so
/// the guard and the invariant cannot drift. It is deliberately NOT widened to
/// every `is_affirmative_reflexive_gate` condition: a `WhenYouDo` node reading
/// `LastCreated` DOES stale-bind (its evaluator keys on the effect it rides, so
/// on a `PutCounter` it is a constant true and gates nothing), and T20's
/// `COMMA_GAP_WHENYOUDO` row carries that measurement.
///
/// NON-VACUITY (asserted in the same test): the number of `ContinuationStep`
/// nodes that DO read `LastCreated` is > 0. On the shipped tree it is 25, and
/// `SequentialSibling` readers are 0; with the re-link reverted the two counts
/// swap sides.
#[test]
fn no_sequential_sibling_reads_last_created() {
    let mut stale: Vec<String> = Vec::new();
    let mut exempt = 0usize;
    let mut continuation_readers = 0usize;

    for (name, text, types) in CHANGED {
        for def in card_definitions(name, text, types) {
            walk(&def, &mut |node| {
                if !effect_reads_last_created(node) {
                    return;
                }
                if node.sub_link != SubAbilityLink::SequentialSibling {
                    continuation_readers += 1;
                } else if node.is_self_gated_reflexive() {
                    // Benign: the descent's false-condition skip drops it.
                    exempt += 1;
                } else {
                    stale.push(format!("{name}: {:?}", node.effect));
                }
            });
        }
    }

    assert_eq!(
        CHANGED.len(),
        20,
        "the frozen list is the 20-card BASE↔POST AST diff"
    );
    // The invariant is asserted BEFORE the non-vacuity control on purpose:
    // reverting the re-link moves every reader back to `SequentialSibling`, so
    // both assertions would fire and the failure must name the stale nodes
    // rather than report an empty continuation count.
    assert!(
        stale.is_empty(),
        "stale LastCreated bind(s) on {} SequentialSibling node(s): {stale:#?}",
        stale.len()
    );
    assert!(
        continuation_readers > 0,
        "non-vacuity: no LastCreated reader was found at all, so the invariant checks nothing \
         (exempt={exempt})"
    );
}

// ═════════════════════════════════════════════════════════════════════════════
// T19–T21 — the "Repeat this process for …" replication grammar.
// ═════════════════════════════════════════════════════════════════════════════

const REPEAT_HEAD: &str =
    "When this creature enters, you may pay {2}. If you do, create a 1/1 white Ally creature token";

/// The complete reachable join space of `try_parse_repeat_process_for_keywords`
/// (anchored on exactly two tags: "repeat this process for " / "do the same
/// for "), crossed with both directive spellings and with the antecedent placed
/// before and after the gated publisher.
const REPEAT_TAILS: &[(&str, &str)] = &[
    ("repeat_sentence", ". Put a flying counter on this creature. Repeat this process for first strike. Put a +1/+1 counter on that token."),
    ("repeat_comma", ", put a flying counter on this creature, repeat this process for first strike. Put a +1/+1 counter on that token."),
    ("all_comma", ", put a flying counter on this creature, repeat this process for first strike, put a +1/+1 counter on that token."),
    ("repeat_then", ", put a flying counter on this creature, then repeat this process for first strike. Put a +1/+1 counter on that token."),
    ("repeat_and", ", put a flying counter on this creature and repeat this process for first strike. Put a +1/+1 counter on that token."),
    ("dosame_sentence", ". Put a flying counter on this creature. Do the same for first strike. Put a +1/+1 counter on that token."),
    ("dosame_comma", ", put a flying counter on this creature, do the same for first strike. Put a +1/+1 counter on that token."),
    ("dosame_then", ", put a flying counter on this creature, then do the same for first strike. Put a +1/+1 counter on that token."),
    ("dosame_and", ", put a flying counter on this creature and do the same for first strike. Put a +1/+1 counter on that token."),
];

/// The DIVERGENCE fixtures: the keyword-counter antecedent sits BEFORE the
/// gated publisher, so the directive clause lands directly after the publisher
/// joined by a comma / "then" / "and" / sentence — the shapes where a clone
/// could land between a gated publisher and a `LastCreated` consumer.
const REPEAT_HOSTILE: &[(&str, &str)] = &[
    ("hostile_comma", "When this creature enters, put a flying counter on this creature. You may pay {2}. If you do, create a 1/1 white Ally creature token, repeat this process for first strike. Put a +1/+1 counter on that token."),
    ("hostile_then", "When this creature enters, put a flying counter on this creature. You may pay {2}. If you do, create a 1/1 white Ally creature token, then repeat this process for first strike. Put a +1/+1 counter on that token."),
    ("hostile_and", "When this creature enters, put a flying counter on this creature. You may pay {2}. If you do, create a 1/1 white Ally creature token and repeat this process for first strike. Put a +1/+1 counter on that token."),
    ("hostile_dosame_comma", "When this creature enters, put a flying counter on this creature. You may pay {2}. If you do, create a 1/1 white Ally creature token, do the same for first strike. Put a +1/+1 counter on that token."),
    ("hostile_sentence", "When this creature enters, put a flying counter on this creature. You may pay {2}. If you do, create a 1/1 white Ally creature token. Repeat this process for first strike. Put a +1/+1 counter on that token."),
    ("and_min", "When this creature enters, put a flying counter on this creature and repeat this process for first strike."),
    ("comma_min", "When this creature enters, put a flying counter on this creature, repeat this process for first strike."),
    ("sentence_min", "When this creature enters, put a flying counter on this creature. Repeat this process for first strike."),
    ("and_gated", "When this creature enters, you may pay {2}. If you do, create a 1/1 white Ally creature token and put a flying counter on this creature and repeat this process for first strike. Put a +1/+1 counter on that token."),
];

/// T20's fixture set, reused by T19 (its templates DO read `LastCreated`, which
/// the `this creature` fixtures above structurally cannot).
///
/// (a) GATED-HAZARD + controls, (b) round-8 NO-HAZARD, (c) round-11 acceptance
/// and its paired negative.
const T20_CASES: &[(&str, &str, &[&str])] = &[
    // ── (a) the transplant hazard: template is its OWN sentence, so the clone's
    // landing slot is off the gated instruction's continuation path.
    ("hazard_seq", "When this creature enters, you may pay {2}. If you do, create a 1/1 white Ally creature token. Put a flying counter on that token. You gain 1 life. Repeat this process for first strike.", &["Creature"]),
    ("hazard_seq_whenyoudo", "When this creature enters, you may pay {2}. When you do, create a 1/1 white Ally creature token. Put a flying counter on that token. You gain 1 life. Repeat this process for first strike.", &["Creature"]),
    // Hazard via a LATER token creator: the clone's own nearest publisher is the
    // Soldier, so the flying template's referent would be transplanted.
    ("hazard_2ndpub", "When this creature enters, you may pay {2}. If you do, create a 1/1 white Ally creature token. Put a flying counter on that token, then create a 1/1 white Soldier creature token. Repeat this process for first strike.", &["Creature"]),
    // (a) controls.
    ("adj", "When this creature enters, you may pay {2}. If you do, create a 1/1 white Ally creature token. Put a flying counter on that token. Repeat this process for first strike.", &["Creature"]),
    ("comma_selfref", "When this creature enters, you may pay {2}. If you do, create a 1/1 white Ally creature token, put a flying counter on this creature. Repeat this process for first strike.", &["Creature"]),
    ("ungated_gap", "When this creature enters, create a 1/1 white Ally creature token. Put a flying counter on that token. You gain 1 life. Repeat this process for first strike.", &["Creature"]),
    // ── (b) NO-HAZARD: no token, no LastCreated, no gated publisher anywhere.
    ("r8_gap_selfref", "When this creature enters, put a flying counter on this creature. You gain 1 life. Repeat this process for first strike.", &["Creature"]),
    ("r8_gap2_selfref", "When this creature enters, put a flying counter on this creature. You gain 1 life. Draw a card. Repeat this process for first strike.", &["Creature"]),
    ("r8_gap_selfref_dosame", "When this creature enters, put a flying counter on this creature. You gain 1 life. Do the same for first strike.", &["Creature"]),
    ("r8_kathril_gap", "When Kathril enters, put a flying counter on any creature you control if a creature card in your graveyard has flying. You gain 1 life. Repeat this process for first strike, deathtouch.", &["Creature"]),
    ("r8_adj_selfref", "When this creature enters, put a flying counter on this creature. Repeat this process for first strike.", &["Creature"]),
    // ── (c) round-11 acceptance: the landing slot IS reached.
    ("cont_gap", "When this creature enters, you may pay {2}. If you do, create a 1/1 white Ally creature token. Put a flying counter on that token, then you gain 1 life. Repeat this process for first strike.", &["Creature"]),
    ("cont_gap_2kw", "When this creature enters, you may pay {2}. If you do, create a 1/1 white Ally creature token. Put a flying counter on that token, then you gain 1 life. Repeat this process for first strike and deathtouch.", &["Creature"]),
    // ── (c) self-gating leg, and its PAIRED NEGATIVE one connector word apart.
    ("comma_gap_ifyoudo", "When this creature enters, you may pay {2}. If you do, create a 1/1 white Ally creature token, put a flying counter on that token. You gain 1 life. Repeat this process for first strike.", &["Creature"]),
    ("comma_gap_dosame", "When this creature enters, you may pay {2}. If you do, create a 1/1 white Ally creature token, put a flying counter on that token. You gain 1 life. Do the same for first strike.", &["Creature"]),
    ("comma_gap_whenyoudo", "When this creature enters, you may pay {2}. When you do, create a 1/1 white Ally creature token, put a flying counter on that token. You gain 1 life. Repeat this process for first strike.", &["Creature"]),
];

/// A clone node: a `PutCounter` placing one of the REPLICATED keyword counters
/// (the templates in these fixtures all place `flying`). Typed, never a
/// `blob.contains("first strike")` substring flag — that flag false-positives
/// on the bare-`and` rows, where the phrase survives inside an `Unimplemented`
/// description.
fn is_clone_node(def: &AbilityDefinition) -> bool {
    matches!(
        &*def.effect,
        Effect::PutCounter {
            counter_type: CounterType::Keyword(KeywordKind::FirstStrike | KeywordKind::Deathtouch),
            ..
        }
    )
}

fn counter_types_of(def: &AbilityDefinition) -> Vec<CounterType> {
    let mut out = Vec::new();
    walk(def, &mut |node| {
        if let Effect::PutCounter { counter_type, .. } = &*node.effect {
            out.push(counter_type.clone());
        }
    });
    out
}

/// T19 — L1 tripwire over the replication grammar's OWN complete join space.
///
/// The invariant: no replicated clone node reads `LastCreated` from a landing
/// slot the resolver can reach without that token — i.e. no clone is both a
/// `SequentialSibling` `LastCreated` reader and not self-gated. Measured 0 on
/// the shipped tree.
///
/// Round 6's original XOR form (`clone_present XOR consumer_binds_LastCreated`)
/// was VACUOUS with respect to its own purpose: all 18 `this creature` fixtures
/// are structurally incapable of carrying a `LastCreated` template, so both
/// conjuncts were true on the counterexample. T20's fixture set — whose
/// templates DO read `LastCreated` — is therefore carried here too, and the XOR
/// is replaced by the direct invariant.
///
/// SCOPE: the invariant is conditioned on "its nearest GATED publisher is
/// unreachable", so a fixture that carries no affirmative reflexive gate at all
/// (`ungated_gap`) is excluded — its publisher creates its token
/// unconditionally, so a `SequentialSibling` `LastCreated` clone there is BASE
/// behaviour and cannot stale-bind. The exclusion is per FIXTURE, not per
/// publisher, which is the conservative direction: a fixture with a gate
/// somewhere but an ungated nearest publisher is still checked, so this can
/// over-report but never under-report.
///
/// NON-VACUITY (asserted): at least one fixture materialises a clone at all,
/// and at least one clone node reads `LastCreated`, so both arms are witnessed.
#[test]
fn repeat_process_directive_never_joins_a_continuation_path() {
    let joined: Vec<(String, String, Vec<&str>)> = REPEAT_TAILS
        .iter()
        .map(|(l, t)| {
            (
                (*l).to_string(),
                format!("{REPEAT_HEAD}{t}"),
                vec!["Creature"],
            )
        })
        .chain(
            REPEAT_HOSTILE
                .iter()
                .map(|(l, t)| ((*l).to_string(), (*t).to_string(), vec!["Creature"])),
        )
        .chain(
            T20_CASES
                .iter()
                .map(|(l, t, ty)| ((*l).to_string(), (*t).to_string(), ty.to_vec())),
        )
        .collect();

    let mut clones_seen = 0usize;
    let mut clone_last_created_readers = 0usize;
    let mut ungated_fixtures = 0usize;
    let mut violations: Vec<String> = Vec::new();

    for (label, text, types) in &joined {
        let defs = card_definitions("Repeat Probe", text, types);
        let mut has_gate = false;
        for def in &defs {
            walk(def, &mut |node| {
                has_gate |= node
                    .condition
                    .as_ref()
                    .is_some_and(|c| c.is_affirmative_reflexive_gate());
            });
        }
        if !has_gate {
            ungated_fixtures += 1;
            continue;
        }
        for def in defs {
            walk(&def, &mut |node| {
                if !is_clone_node(node) {
                    return;
                }
                clones_seen += 1;
                if !effect_reads_last_created(node) {
                    return;
                }
                clone_last_created_readers += 1;
                if node.sub_link == SubAbilityLink::SequentialSibling
                    && !node.is_self_gated_reflexive()
                {
                    violations.push(format!("{label}: {:?}", node.effect));
                }
            });
        }
    }

    assert!(
        clones_seen > 0,
        "non-vacuity: no replicated keyword counter materialised in ANY fixture"
    );
    assert!(
        clone_last_created_readers > 0,
        "non-vacuity: no clone read LastCreated, so the invariant checks nothing"
    );
    assert!(
        ungated_fixtures > 0 && ungated_fixtures < joined.len(),
        "the gate-presence filter is neither empty nor total (excluded {ungated_fixtures} of {})",
        joined.len()
    );
    assert!(
        violations.is_empty(),
        "a replicated clone landed off the gated instruction's continuation path while reading \
         LastCreated: {violations:#?}"
    );
}

/// One driven row of the T20 matrix.
struct CloneRow {
    runner: GameRunner,
    decoy: ObjectId,
    dlife: i32,
    dhand: i64,
    live: Vec<ObjectId>,
}

fn drive_clone(text: &str, types: &[&str], funded: bool) -> CloneRow {
    let mut scenario = GameScenario::new();
    fund(&mut scenario, funded);
    let mut runner = scenario.build();
    let source = add_creature(&mut runner, P0, "Synth");
    let decoy = pre_seed_decoy(&mut runner);
    add_object(&mut runner, P0, "Library Filler", Zone::Library);
    let life0 = runner.state().players[0].life;
    let hand0 = runner.state().players[0].hand.len();

    let def = parse_trigger(text, "Synth", types, 0);
    resolve_def(&mut runner, &def, source, vec![]);
    decide(&mut runner, true);

    let live = live_tokens(&runner, decoy);
    CloneRow {
        dlife: runner.state().players[0].life - life0,
        dhand: runner.state().players[0].hand.len() as i64 - hand0 as i64,
        runner,
        decoy,
        live,
    }
}

fn case_text(label: &str) -> (&'static str, &'static [&'static str]) {
    T20_CASES
        .iter()
        .find(|(l, _, _)| *l == label)
        .map(|(_, t, ty)| (*t, *ty))
        .unwrap_or_else(|| panic!("no T20 fixture named {label:?}"))
}

/// T20 — a "Repeat this process for …" clone must never transplant a gated
/// publisher's just-created-token referent to a slot the resolver reaches
/// without that token, and must never silently DROP a printed replication that
/// is honest where it lands.
///
/// (a) GATED-HAZARD, gate FALSE: the decoy gets no `Keyword(FirstStrike)`
///     counter, reach-guarded by `cost_payment_failed_flag` and "no live token
///     was created".
/// (b) NO-HAZARD (no token, no `LastCreated`, no gated publisher): every printed
///     replication is still emitted.
/// (c) LANDING-SLOT REACHED / SELF-GATED, gate TRUE: the live token gets BOTH
///     counters — paired with the negative `comma_gap_whenyoudo`, which gets only
///     `flying`, one connector word apart.
#[test]
fn repeat_process_clone_does_not_transplant_a_gated_referent() {
    // ── (a) the hazard rows, gate FALSE.
    for label in [
        "hazard_seq",
        "hazard_seq_whenyoudo",
        "hazard_2ndpub",
        "comma_gap_dosame",
        "adj",
        "comma_selfref",
    ] {
        let (text, types) = case_text(label);
        let r = drive_clone(text, types, false);
        assert!(
            r.runner.state().cost_payment_failed_flag,
            "[{label}] reach-guard: the payment failed, so the gate really went FALSE"
        );
        assert_eq!(
            r.live,
            Vec::<ObjectId>::new(),
            "[{label}] reach-guard: no token was created on the false gate"
        );
        assert_eq!(
            kw_counter(&r.runner, r.decoy, KeywordKind::FirstStrike),
            0,
            "[{label}] the stale decoy receives NO replicated first-strike counter"
        );
        assert_eq!(
            kw_counter(&r.runner, r.decoy, KeywordKind::Flying),
            0,
            "[{label}] nor the template's own flying counter"
        );
    }
    // The `*_gap` hazard rows carry an independent intervening instruction, so
    // its resolution is an extra reach-guard.
    for label in ["hazard_seq", "hazard_seq_whenyoudo"] {
        let (text, types) = case_text(label);
        assert_eq!(
            drive_clone(text, types, false).dlife,
            1,
            "[{label}] reach-guard: the intervening 'You gain 1 life.' really resolved"
        );
    }

    // ── (a) ungated control: BASE behaviour, benign — the live token keeps BOTH
    // printed counters because its publisher is unconditional.
    {
        let (text, types) = case_text("ungated_gap");
        let r = drive_clone(text, types, false);
        let ally = token_named(&r.runner, r.decoy, "Ally");
        assert_eq!(
            (
                kw_counter(&r.runner, ally, KeywordKind::Flying),
                kw_counter(&r.runner, ally, KeywordKind::FirstStrike),
            ),
            (1, 1),
            "ungated_gap: an UNGATED publisher's replication is never dropped"
        );
        assert_eq!(
            kw_counter(&r.runner, r.decoy, KeywordKind::FirstStrike),
            0,
            "ungated_gap: and nothing lands on the stale decoy"
        );
    }

    // ── (b) NO-HAZARD acceptance: every printed replication is emitted.
    for label in [
        "r8_gap_selfref",
        "r8_gap2_selfref",
        "r8_gap_selfref_dosame",
        "r8_adj_selfref",
    ] {
        let (text, types) = case_text(label);
        let r = drive_clone(text, types, false);
        let source = *r
            .runner
            .state()
            .battlefield
            .iter()
            .find(|id| r.runner.state().objects[id].name == "Synth")
            .expect("the source is on the battlefield");
        assert_eq!(
            (
                kw_counter(&r.runner, source, KeywordKind::Flying),
                kw_counter(&r.runner, source, KeywordKind::FirstStrike),
            ),
            (1, 1),
            "[{label}] both the template's and the replicated counter are placed"
        );
        if label != "r8_adj_selfref" {
            assert_eq!(
                r.dlife, 1,
                "[{label}] reach-guard: the intervening 'You gain 1 life.' really resolved"
            );
        }
        if label == "r8_gap2_selfref" {
            assert_eq!(
                r.dhand, 1,
                "[{label}] reach-guard: the second intervening 'Draw a card.' really resolved"
            );
        }
    }
    // Kathril's own templating: its gate needs a graveyard, so this row is
    // asserted on the parsed replication, with the intervening clause's
    // resolution as the reach-guard.
    {
        let (text, types) = case_text("r8_kathril_gap");
        let r = drive_clone(text, types, false);
        assert_eq!(
            r.dlife, 1,
            "r8_kathril_gap: reach-guard: the intervening clause really resolved"
        );
        let kinds: Vec<CounterType> = card_definitions("Kathril Gap", text, types)
            .iter()
            .flat_map(counter_types_of)
            .collect();
        for k in [
            KeywordKind::Flying,
            KeywordKind::FirstStrike,
            KeywordKind::Deathtouch,
        ] {
            assert!(
                kinds.contains(&CounterType::Keyword(k)),
                "r8_kathril_gap: the printed {k:?} replication is not dropped (got {kinds:?})"
            );
        }
    }

    // ── (c) landing slot reached / self-gated, gate TRUE.
    for (label, extra) in [
        ("cont_gap", None),
        ("cont_gap_2kw", Some(KeywordKind::Deathtouch)),
        ("comma_gap_ifyoudo", None),
        ("comma_gap_dosame", None),
    ] {
        let (text, types) = case_text(label);
        let r = drive_clone(text, types, true);
        let ally = token_named(&r.runner, r.decoy, "Ally");
        assert_eq!(
            (
                kw_counter(&r.runner, ally, KeywordKind::Flying),
                kw_counter(&r.runner, ally, KeywordKind::FirstStrike),
            ),
            (1, 1),
            "[{label}] the live token receives the template AND the replicated counter"
        );
        if let Some(k) = extra {
            assert_eq!(
                kw_counter(&r.runner, ally, k),
                1,
                "[{label}] and the second listed keyword"
            );
        }
    }

    // ── (c) THE PAIRED NEGATIVE, one connector word apart: `When you do` is not
    // position-independent (its evaluator keys on the effect it rides), so the
    // clone must NOT be emitted — and on the false path the decoy stays clean.
    {
        let (text, types) = case_text("comma_gap_whenyoudo");
        let r = drive_clone(text, types, true);
        let ally = token_named(&r.runner, r.decoy, "Ally");
        assert_eq!(
            kw_counter(&r.runner, ally, KeywordKind::Flying),
            1,
            "comma_gap_whenyoudo: reach-guard: the gate went TRUE and the template resolved"
        );
        assert_eq!(
            kw_counter(&r.runner, ally, KeywordKind::FirstStrike),
            0,
            "comma_gap_whenyoudo: a WhenYouDo-gated clone is NOT position-independent, so the \
             replication is declined rather than transplanted"
        );
        let f = drive_clone(text, types, false);
        assert!(
            f.runner.state().cost_payment_failed_flag,
            "comma_gap_whenyoudo: reach-guard: the false arm's payment really failed"
        );
        assert_eq!(
            kw_counter(&f.runner, f.decoy, KeywordKind::FirstStrike),
            0,
            "comma_gap_whenyoudo: and the stale decoy never receives the replicated counter"
        );
    }

    // ── AST-shape: the only `SequentialSibling` `LastCreated` readers in the
    // whole fixture set are `ungated_gap`'s (BASE behaviour, benign — its
    // publisher is unconditional) and the SELF-GATED clones.
    let mut ungated_seq = 0usize;
    let mut selfgated_seq = 0usize;
    for (label, text, types) in T20_CASES {
        for def in card_definitions("T20", text, types) {
            walk(&def, &mut |node| {
                if node.sub_link != SubAbilityLink::SequentialSibling
                    || !effect_reads_last_created(node)
                {
                    return;
                }
                if *label == "ungated_gap" {
                    ungated_seq += 1;
                } else if node.is_self_gated_reflexive() {
                    selfgated_seq += 1;
                } else {
                    panic!(
                        "[{label}] stale SequentialSibling LastCreated reader: {:?}",
                        node.effect
                    );
                }
            });
        }
    }
    assert_eq!(
        ungated_seq, 2,
        "ungated_gap keeps BOTH its template and its clone as SequentialSibling LastCreated \
         readers (benign: the publisher is unconditional)"
    );
    assert_eq!(
        selfgated_seq, 2,
        "exactly the two self-gated clones (comma_gap_ifyoudo, comma_gap_dosame) are exempt"
    );
}

/// The complete set of cards whose Oracle text contains "repeat this process
/// for " or "do the same for " — corpus-complete by construction, since
/// `try_parse_repeat_process_for_keywords` is anchored on exactly those two
/// tags and is the sole producer of `ReplicateKind::CounterPlacement`.
/// Embedded verbatim from `AtomicCards.json`, like T18's fixtures.
const REPEAT_PROCESS_CORPUS: &[(&str, &str, &[&str])] = &[
    (
        "Equipoise",
        "At the beginning of your upkeep, for each land target player controls in excess of the number you control, choose a land that player controls, then the chosen permanents phase out. Repeat this process for artifacts and creatures. (While they're phased out, they're treated as though they don't exist. They phase in before that player untaps during their next untap step.)",
        &["Enchantment"],
    ),
    (
        "Estrid, the Masked",
        "[+2]: Untap each enchanted permanent you control.\n[\u{2212}1]: Create a white Aura enchantment token named Mask attached to another target permanent. The token has enchant permanent and umbra armor.\n[\u{2212}7]: Mill seven cards. Return all non-Aura enchantment cards from your graveyard to the battlefield, then do the same for Aura cards.\nEstrid, the Masked can be your commander.",
        &["Planeswalker"],
    ),
    (
        "Estrid, the Masked // Estrid, the Masked",
        "[+2]: Untap each enchanted permanent you control.\n[\u{2212}1]: Create a white Aura enchantment token named Mask attached to another target permanent. The token has enchant permanent and umbra armor.\n[\u{2212}7]: Mill seven cards. Return all non-Aura enchantment cards from your graveyard to the battlefield, then do the same for Aura cards.\nEstrid, the Masked can be your commander.",
        &["Planeswalker"],
    ),
    (
        "Firemind's Foresight",
        "Search your library for an instant card with mana value 3, reveal it, and put it into your hand. Then repeat this process for instant cards with mana values 2 and 1. Then shuffle.",
        &["Instant"],
    ),
    (
        "Glimpse of Tomorrow",
        "Suspend 3\u{2014}{R}{R}\nShuffle all permanents you own into your library, then reveal that many cards from the top of your library. Put all non-Aura permanent cards revealed this way onto the battlefield, then do the same for Aura cards, then put the rest on the bottom of your library in a random order.",
        &["Sorcery"],
    ),
    (
        "Grim Captain's Call",
        "Return a Pirate card from your graveyard to your hand, then do the same for Vampire, Dinosaur, and Merfolk.",
        &["Sorcery"],
    ),
    (
        "Gruesome Menagerie",
        "Choose a creature card with mana value 1 in your graveyard, then do the same for creature cards with mana value 2 and 3. Return those cards to the battlefield.",
        &["Sorcery"],
    ),
    (
        "Invoke Despair",
        "Target opponent sacrifices a creature of their choice. If they can't, they lose 2 life and you draw a card. Then repeat this process for an enchantment and a planeswalker.",
        &["Sorcery"],
    ),
    (
        "Kathril, Aspect Warper",
        "When Kathril enters, put a flying counter on any creature you control if a creature card in your graveyard has flying. Repeat this process for first strike, double strike, deathtouch, hexproof, indestructible, lifelink, menace, reach, trample, and vigilance. Then put a +1/+1 counter on Kathril for each counter put on a creature this way.",
        &["Creature"],
    ),
    (
        "Queen Kayla bin-Kroog",
        "{4}, {T}: Discard all the cards in your hand, then draw that many cards. You may choose an artifact or creature card with mana value 1 you discarded this way, then do the same for artifact or creature cards with mana values 2 and 3. Return those cards to the battlefield. Activate only as a sorcery.",
        &["Creature"],
    ),
    (
        "Runed Terror",
        "Instead of taking turns as normal, players take their phases sequentially. (For example, you take your beginning phase as the active player, then the next player in turn order becomes the active player and takes their beginning phase. After each player had a beginning phase, do the same for first main, combat phase, second main, ending phase, and then beginning phase again. If this creature leaves the battlefield, the active player continues their turn as normal.)",
        &["Artifact", "Creature"],
    ),
    (
        "Super-Adaptoid",
        "Super-Adaptoid's power is equal to the number of legendary creatures you control.\nWhenever Super-Adaptoid enters or attacks, choose another target creature. If that creature has haste and Super-Adaptoid doesn't, put a haste counter on Super-Adaptoid. Do the same for flying, first strike, double strike, deathtouch, indestructible, lifelink, menace, reach, trample, and vigilance.",
        &["Artifact", "Creature"],
    ),
];

/// T21 — the shipped-card replication pin (round-8's restatement of the
/// round-7 adjacency backstop, which asserted two locals inside
/// `assemble_effect_chain` and was therefore not implementable from
/// `crates/engine/tests/`).
///
/// The observable proxy is the multiset of `PutCounter.counter_type` values
/// reachable from each card's parsed definition. It FAILS if a parser change
/// makes either shipped card bind differently or drop a listed keyword. It
/// deliberately does NOT claim to fire for a NEW card with a non-adjacent
/// directive: such a card emits no clones and no frozen-list test notices. The
/// §7.6 guard is what removes the need for that tripwire — a non-adjacent
/// directive is no longer dropped unless it would transplant a gated referent.
#[test]
fn repeat_process_replicates_every_listed_keyword() {
    fn kws(list: &[KeywordKind]) -> Vec<CounterType> {
        list.iter().copied().map(CounterType::Keyword).collect()
    }

    // Kathril: the flying template + its ten printed clones + the
    // "Then put a +1/+1 counter on Kathril" tail.
    let kathril = {
        let mut v = kws(&[
            KeywordKind::Flying,
            KeywordKind::FirstStrike,
            KeywordKind::DoubleStrike,
            KeywordKind::Deathtouch,
            KeywordKind::Hexproof,
            KeywordKind::Indestructible,
            KeywordKind::Lifelink,
            KeywordKind::Menace,
            KeywordKind::Reach,
            KeywordKind::Trample,
            KeywordKind::Vigilance,
        ]);
        v.push(CounterType::Plus1Plus1);
        v
    };
    // Super-Adaptoid: the haste template + its ten printed clones.
    let super_adaptoid = kws(&[
        KeywordKind::Haste,
        KeywordKind::Flying,
        KeywordKind::FirstStrike,
        KeywordKind::DoubleStrike,
        KeywordKind::Deathtouch,
        KeywordKind::Indestructible,
        KeywordKind::Lifelink,
        KeywordKind::Menace,
        KeywordKind::Reach,
        KeywordKind::Trample,
        KeywordKind::Vigilance,
    ]);

    let mut reached = 0usize;
    for (name, text, types) in REPEAT_PROCESS_CORPUS {
        let kinds: Vec<CounterType> = card_definitions(name, text, types)
            .iter()
            .flat_map(counter_types_of)
            .collect();
        let expected: &[CounterType] = match *name {
            "Kathril, Aspect Warper" => &kathril,
            "Super-Adaptoid" => &super_adaptoid,
            _ => &[],
        };
        if !expected.is_empty() {
            reached += 1;
        }
        assert_eq!(
            kinds, expected,
            "{name}: the parsed counter-placement multiset changed"
        );
    }

    assert_eq!(
        REPEAT_PROCESS_CORPUS.len(),
        12,
        "corpus-complete by construction: exactly the cards whose Oracle text carries one of the \
         two anchored directive tags"
    );
    assert_eq!(
        reached, 2,
        "non-vacuity: exactly two of the twelve reach the counter-placement binding"
    );
}
