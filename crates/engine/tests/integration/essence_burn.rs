//! Essence Burn exercises generic color-list grammar and target-bound death replacements.
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{AbilityDefinition, Effect, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const ESSENCE: &str = "Essence Burn deals 5 damage to target black or green creature or planeswalker. If that permanent would die this turn, exile it instead.";
const RITES: &str =
    "As an additional cost to cast this spell, sacrifice a creature.\nDraw two cards.";
const GUST: &str = "Choose target spell or permanent that's red or green. Its owner puts it on their choice of the top or bottom of their library.";

fn cost(generic: u32, shard: ManaCostShard) -> ManaCost {
    ManaCost::Cost {
        generic,
        shards: vec![shard],
    }
}
fn mana(color: ManaType) -> ManaUnit {
    ManaUnit::new(color, ObjectId(0), false, vec![])
}
fn board() -> GameScenario {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    s.with_mana_pool(
        P0,
        (0..12)
            .flat_map(|_| {
                [
                    mana(ManaType::Red),
                    mana(ManaType::Black),
                    mana(ManaType::Blue),
                ]
            })
            .collect(),
    );
    s.with_library_top(P0, &["One", "Two", "Three", "Four", "Five", "Six"]);
    s.with_library_top(P1, &["One", "Two", "Three", "Four"]);
    s
}
fn burn(s: &mut GameScenario) -> ObjectId {
    s.add_spell_to_hand_from_oracle(P0, "Essence Burn", true, ESSENCE)
        .with_mana_cost(cost(1, ManaCostShard::Red))
        .id()
}
fn creature(
    s: &mut GameScenario,
    player: PlayerId,
    name: &str,
    color: ManaColor,
    toughness: i32,
) -> ObjectId {
    s.add_creature(player, name, 2, toughness)
        .with_color(vec![color])
        .id()
}
fn walker(s: &mut GameScenario, name: &str, color: ManaColor, loyalty: u32) -> ObjectId {
    s.add_planeswalker_from_oracle(P1, name, "Test", loyalty, "")
        .with_color(vec![color])
        .id()
}
fn rites(s: &mut GameScenario) -> ObjectId {
    s.add_spell_to_hand_from_oracle(P0, "Village Rites", true, RITES)
        .with_mana_cost(cost(0, ManaCostShard::Black))
        .id()
}
fn announce(r: &mut GameRunner, spell: ObjectId) {
    let card_id = r.state().objects[&spell].card_id;
    r.act(GameAction::CastSpell {
        object_id: spell,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("production CastSpell reaches target selection");
}
fn legal(r: &GameRunner) -> Vec<TargetRef> {
    let WaitingFor::TargetSelection { target_slots, .. } = &r.state().waiting_for else {
        panic!("expected TargetSelection, got {:?}", r.state().waiting_for);
    };
    assert_eq!(target_slots.len(), 1);
    assert!(
        !target_slots[0].legal_targets.is_empty(),
        "positive target reach guard"
    );
    target_slots[0].legal_targets.clone()
}
fn reject(r: &mut GameRunner, id: ObjectId) {
    let err = r
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(id)),
        })
        .expect_err("illegal target must reject");
    assert!(
        format!("{err:?}").contains("Illegal target selected"),
        "{err:?}"
    );
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::TargetSelection { .. }
    ));
}
fn gaps(def: &AbilityDefinition) -> bool {
    matches!(def.effect.as_ref(), Effect::Unimplemented { .. })
        || def.sub_ability.as_deref().is_some_and(gaps)
        || def.else_ability.as_deref().is_some_and(gaps)
}

#[test]
fn essence_burn_exiles_lethal_black_creature() {
    let mut s = board();
    let spell = burn(&mut s);
    let victim = creature(&mut s, P1, "Victim", ManaColor::Black, 3);
    let bystander = creature(&mut s, P1, "Bystander", ManaColor::Black, 3);
    let mut r = s.build();
    let out = r.cast(spell).target_object(victim).resolve();
    // CR 608.2c + CR 700.4: the whole instruction installs the rider before lethal SBA.
    assert_eq!(
        out.zone_of(victim),
        Zone::Exile,
        "Essence Burn must exile its lethal black creature target"
    );
    out.assert_zone(&[spell], Zone::Graveyard);
    out.assert_zone(&[bystander], Zone::Battlefield);
    assert_eq!(out.damage_marked(bystander), 0);
    out.assert_stack_size(0);
}

#[test]
fn essence_burn_target_legality() {
    let mut s = board();
    let spell = burn(&mut s);
    let black = creature(&mut s, P0, "Black", ManaColor::Black, 6);
    let green = creature(&mut s, P1, "Green", ManaColor::Green, 6);
    let black_pw = walker(&mut s, "Black walker", ManaColor::Black, 7);
    let green_pw = walker(&mut s, "Green walker", ManaColor::Green, 7);
    let artifact = s
        .add_creature(P1, "Black artifact", 2, 6)
        .as_artifact()
        .with_color(vec![ManaColor::Black])
        .id();
    let enchantment = s
        .add_creature(P1, "Black enchantment", 2, 6)
        .as_enchantment()
        .with_color(vec![ManaColor::Black])
        .id();
    let land = s
        .add_creature(P1, "Black land", 2, 6)
        .as_land()
        .with_color(vec![ManaColor::Black])
        .id();
    let red = creature(&mut s, P1, "Red", ManaColor::Red, 6);
    let red_pw = walker(&mut s, "Red walker", ManaColor::Red, 7);
    let multicolor = s
        .add_creature(P1, "Black red", 2, 6)
        .with_color(vec![ManaColor::Black, ManaColor::Red])
        .id();
    let colorless = s.add_creature(P1, "Colorless", 2, 6).id();
    let mut r = s.build();
    announce(&mut r, spell);
    let targets = legal(&r);
    for id in [black, green, black_pw, green_pw, multicolor] {
        assert!(
            targets.contains(&TargetRef::Object(id)),
            "positive color/type combination"
        );
    }
    // CR 109.2 + CR 105.2: colors qualify BOTH concrete noun alternatives.
    assert!(
        !targets.contains(&TargetRef::Object(artifact)),
        "Essence Burn must reject a black noncreature nonplaneswalker target"
    );
    for id in [artifact, enchantment, land, red, red_pw, colorless] {
        assert!(!targets.contains(&TargetRef::Object(id)));
        reject(&mut r, id);
    }
    r.act(GameAction::ChooseTarget {
        target: Some(TargetRef::Object(black)),
    })
    .expect("legal selection");
    r.advance_until_stack_empty();
    assert_eq!(r.state().objects[&black].damage_marked, 5);
    assert_eq!(r.state().objects[&black].zone, Zone::Battlefield);
    assert_eq!(r.state().objects[&spell].zone, Zone::Graveyard);
    assert!(r.state().stack.is_empty());
}

#[test]
fn essence_burn_exiles_lethal_green_creature() {
    let mut s = board();
    let spell = burn(&mut s);
    let victim = creature(&mut s, P1, "Green", ManaColor::Green, 3);
    let mut r = s.build();
    let out = r.cast(spell).target_object(victim).resolve();
    out.assert_zone(&[victim], Zone::Exile);
    out.assert_zone(&[spell], Zone::Graveyard);
    out.assert_stack_size(0);
}
#[test]
fn essence_burn_exiles_lethal_green_planeswalker() {
    let mut s = board();
    let spell = burn(&mut s);
    let victim = walker(&mut s, "Green", ManaColor::Green, 5);
    let other = walker(&mut s, "Other", ManaColor::Green, 7);
    let mut r = s.build();
    let out = r.cast(spell).target_object(victim).resolve();
    out.assert_zone(&[victim], Zone::Exile);
    out.assert_zone(&[spell], Zone::Graveyard);
    out.assert_zone(&[other], Zone::Battlefield);
    out.assert_counters(other, CounterType::Loyalty, 7);
}
#[test]
fn essence_burn_nonlethal_creature_and_planeswalker_survive() {
    let mut s = board();
    let first = burn(&mut s);
    let second = burn(&mut s);
    let host = creature(&mut s, P1, "Black", ManaColor::Black, 6);
    let pw = walker(&mut s, "Black", ManaColor::Black, 7);
    let mut r = s.build();
    let out = r.cast(first).target_object(host).resolve();
    out.assert_zone(&[host], Zone::Battlefield);
    assert_eq!(out.damage_marked(host), 5);
    let out = r.cast(second).target_object(pw).resolve();
    out.assert_zone(&[pw], Zone::Battlefield);
    out.assert_counters(pw, CounterType::Loyalty, 2);
}
#[test]
fn essence_burn_replaces_later_same_turn_sacrifice() {
    let mut s = board();
    let spell = burn(&mut s);
    let first = rites(&mut s);
    let second = rites(&mut s);
    let host = creature(&mut s, P0, "Host", ManaColor::Black, 6);
    let other = creature(&mut s, P0, "Other", ManaColor::Black, 6);
    let mut r = s.build();
    let out = r.cast(spell).target_object(host).resolve();
    out.assert_zone(&[host], Zone::Battlefield);
    assert_eq!(out.damage_marked(host), 5);
    let out = r.cast(first).sacrifice_with(&[other]).resolve();
    out.assert_zone(&[other], Zone::Graveyard);
    out.assert_hand_drawn(P0, 2);
    out.assert_zone(&[host], Zone::Battlefield);
    let out = r.cast(second).sacrifice_with(&[host]).resolve();
    // CR 700.4: any battlefield-to-graveyard move is death, including a later cost.
    assert_eq!(
        out.zone_of(host),
        Zone::Exile,
        "Essence Burn must replace its target's later same-turn sacrifice"
    );
    out.assert_hand_drawn(P0, 2);
    out.assert_zone(&[second], Zone::Graveyard);
}
#[test]
fn essence_burn_expires_at_cleanup() {
    let mut s = board();
    let spell = burn(&mut s);
    let counterpart_spell = burn(&mut s);
    let sacrifice = rites(&mut s);
    let counterpart_sacrifice = rites(&mut s);
    let host = creature(&mut s, P0, "Host", ManaColor::Black, 6);
    let counterpart = creature(&mut s, P0, "Same turn counterpart", ManaColor::Green, 6);
    let mut r = s.build();
    let out = r.cast(spell).target_object(host).resolve();
    out.assert_zone(&[host], Zone::Battlefield);
    assert_eq!(out.damage_marked(host), 5);
    let out = r
        .cast(counterpart_spell)
        .target_object(counterpart)
        .resolve();
    assert_eq!(out.damage_marked(counterpart), 5);
    let out = r
        .cast(counterpart_sacrifice)
        .sacrifice_with(&[counterpart])
        .resolve();
    out.assert_zone(&[counterpart], Zone::Exile);
    out.assert_hand_drawn(P0, 2);
    out.assert_zone(&[host], Zone::Battlefield);
    let turn = r.state().turn_number;
    // CR 514.2: cross real turn transitions; both marked damage and the grant expire.
    for _ in 0..128 {
        if r.state().turn_number > turn
            && r.state().active_player == P0
            && r.state().phase == Phase::PreCombatMain
        {
            break;
        }
        match r.state().waiting_for {
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority).expect("turn priority");
            }
            WaitingFor::DeclareAttackers { .. } => {
                r.declare_attackers(&[]).expect("empty combat");
            }
            WaitingFor::DeclareBlockers { .. } => {
                r.declare_blockers(&[]).expect("empty blocks");
            }
            _ => panic!("unexpected turn prompt {:?}", r.state().waiting_for),
        }
    }
    assert!(
        r.state().turn_number > turn,
        "actual cleanup/turn advancement"
    );
    assert_eq!(r.state().phase, Phase::PreCombatMain);
    assert_eq!(r.state().objects[&host].damage_marked, 0);
    // Seed payment only after the real cleanup emptied the previous pool.
    r.state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .unwrap()
        .mana_pool
        .add(mana(ManaType::Black));
    let out = r.cast(sacrifice).sacrifice_with(&[host]).resolve();
    out.assert_zone(&[host], Zone::Graveyard);
    out.assert_hand_drawn(P0, 2);
}
#[test]
fn essence_burn_distinct_casts_keep_distinct_hosts() {
    let mut s = board();
    let a = burn(&mut s);
    let b = burn(&mut s);
    let ra = rites(&mut s);
    let rb = rites(&mut s);
    let first = creature(&mut s, P0, "First", ManaColor::Black, 6);
    let second = creature(&mut s, P0, "Second", ManaColor::Green, 6);
    let mut r = s.build();
    assert_eq!(
        r.cast(a)
            .target_object(first)
            .resolve()
            .damage_marked(first),
        5
    );
    assert_eq!(
        r.cast(b)
            .target_object(second)
            .resolve()
            .damage_marked(second),
        5
    );
    let out = r.cast(ra).sacrifice_with(&[first]).resolve();
    out.assert_zone(&[first], Zone::Exile);
    out.assert_hand_drawn(P0, 2);
    out.assert_zone(&[second], Zone::Battlefield);
    let out = r.cast(rb).sacrifice_with(&[second]).resolve();
    out.assert_zone(&[second], Zone::Exile);
    out.assert_hand_drawn(P0, 2);
}
#[test]
fn essence_burn_no_legal_target_cannot_commit() {
    let mut s = board();
    let spell = burn(&mut s);
    let red = creature(&mut s, P1, "Red", ManaColor::Red, 6);
    let mut r = s.build();
    assert!(r.cast(spell).target_object(red).try_resolve().is_err());
    assert!(r.state().stack.is_empty());
    assert_eq!(r.state().objects[&spell].zone, Zone::Hand);
    // Positive fresh-board counterpart proves ordinary announcement can reach the prompt.
    let mut s = board();
    let spell = burn(&mut s);
    let green = creature(&mut s, P1, "Green", ManaColor::Green, 6);
    let mut r = s.build();
    let out = r.cast(spell).target_object(green).resolve();
    assert_eq!(out.damage_marked(green), 5);
}
#[test]
fn essence_burn_vanished_target_does_not_rebind() {
    let mut s = board();
    let spell = burn(&mut s);
    let victim = creature(&mut s, P1, "Victim", ManaColor::Black, 6);
    let other = creature(&mut s, P1, "Other", ManaColor::Black, 6);
    let bounce = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Unsummon",
            true,
            "Return target creature to its owner's hand.",
        )
        .with_mana_cost(cost(0, ManaCostShard::Blue))
        .id();
    let mut r = s.build();
    let mut committed = r.cast(spell).target_object(victim).commit();
    assert_eq!(committed.state().objects[&spell].zone, Zone::Stack);
    let out = committed.cast(bounce).target_object(victim).resolve();
    out.assert_zone(&[victim], Zone::Hand);
    out.assert_zone(&[spell, bounce], Zone::Graveyard);
    out.assert_zone(&[other], Zone::Battlefield);
    assert_eq!(out.damage_marked(other), 0);
}

#[test]
fn essence_burn_target_becomes_illegal_after_turn_to_frog() {
    use engine::types::card_type::CoreType;
    let mut s = lifetime_board();
    let host = real(&mut s, P1, "Catacomb Slug", Zone::Battlefield);
    let bystander = real(&mut s, P1, "Grizzly Bears", Zone::Battlefield);
    let essence = real(&mut s, P0, "Essence Burn", Zone::Hand);
    let frog = real(&mut s, P1, "Turn to Frog", Zone::Hand);
    let mut r = s.build();
    let incarnation = r.state().objects[&host].incarnation;

    let mut committed = r.cast(essence).target_object(host).commit();
    committed
        .act(GameAction::PassPriority)
        .expect("P0 passes so P1 can respond");
    committed.cast(frog).target_object(host).resolve();
    let changed = &committed.state().objects[&host];
    assert_eq!(changed.incarnation, incarnation);
    assert_eq!(changed.color, vec![ManaColor::Blue]);
    assert!(changed.card_types.core_types.contains(&CoreType::Creature));
    assert!(changed
        .card_types
        .subtypes
        .iter()
        .any(|subtype| subtype == "Frog"));

    let out = committed.resolve();
    assert_eq!(out.state().objects[&essence].zone, Zone::Graveyard);
    assert_eq!(out.state().objects[&host].incarnation, incarnation);
    assert_eq!(out.state().objects[&host].damage_marked, 0);
    assert_eq!(live_resolution_count_in_state(out.state(), host), 0);
    assert_eq!(out.state().objects[&bystander].damage_marked, 0);
    assert_eq!(live_resolution_count_in_state(out.state(), bystander), 0);
}

#[test]
fn essence_burn_target_does_not_rebind_after_cloudshift() {
    let mut s = lifetime_board();
    let host = real(&mut s, P0, "Catacomb Slug", Zone::Battlefield);
    let bystander = real(&mut s, P1, "Grizzly Bears", Zone::Battlefield);
    let essence = real(&mut s, P1, "Essence Burn", Zone::Hand);
    let cloudshift = real(&mut s, P0, "Cloudshift", Zone::Hand);
    let mut r = s.build();
    let prior_turn = r.state().turn_number;
    advance_to_main(&mut r, P1, prior_turn);
    let incarnation = r.state().objects[&host].incarnation;

    let mut committed = r.cast(essence).target_object(host).commit();
    committed
        .act(GameAction::PassPriority)
        .expect("P1 passes so P0 can respond");
    committed.cast(cloudshift).target_object(host).resolve();
    assert_eq!(committed.state().objects[&host].zone, Zone::Battlefield);
    assert!(committed.state().objects[&host].incarnation > incarnation);
    let new_incarnation = committed.state().objects[&host].incarnation;

    let out = committed.resolve();
    assert_eq!(out.state().objects[&essence].zone, Zone::Graveyard);
    assert_eq!(out.state().objects[&host].zone, Zone::Battlefield);
    assert_eq!(out.state().objects[&host].incarnation, new_incarnation);
    assert_eq!(out.state().objects[&host].damage_marked, 0);
    assert_eq!(live_resolution_count_in_state(out.state(), host), 0);
    assert_eq!(out.state().objects[&bystander].damage_marked, 0);
    assert_eq!(live_resolution_count_in_state(out.state(), bystander), 0);
}

#[test]
fn essence_burn_battlefield_exit_replay_clears_damage_and_rider() {
    use engine::types::resolved_commands::ResolvedRulesCommand;

    let mut s = lifetime_board();
    let host = real(&mut s, P0, "Catacomb Slug", Zone::Battlefield);
    let essence = real(&mut s, P0, "Essence Burn", Zone::Hand);
    let unsummon = real(&mut s, P0, "Unsummon", Zone::Hand);
    let mut r = s.build();
    r.cast(essence).target_object(host).resolve();
    assert_eq!(r.state().objects[&host].damage_marked, 5);
    assert_eq!(live_resolution_count(&r, host), 1);

    // Commit Unsummon first so replay starts after its Hand-to-Stack zone
    // change; the recorded host-exit index is relative to that exact prestate.
    let mut committed = r.cast(unsummon).target_object(host).commit();
    let before_exit = committed.state().clone();
    let journal_start = committed.state().resolved_rules_journal.entries().len();
    let exited = committed.resolve();
    let live = &exited.state().objects[&host];
    assert_eq!(live.zone, Zone::Hand);
    assert_eq!(live.damage_marked, 0);
    assert_eq!(live_resolution_count_in_state(exited.state(), host), 0);

    let command = exited
        .state()
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(journal_start)
        .filter_map(|entry| entry.command.as_ref())
        .find_map(|command| match command {
            ResolvedRulesCommand::ZoneChange(command)
                if command.object.object_id == host
                    && command.from == Zone::Battlefield
                    && command.to == Zone::Hand =>
            {
                Some(command.as_ref().clone())
            }
            _ => None,
        })
        .expect("Unsummon records the exact Battlefield-to-Hand transition");
    let mut replay = before_exit;
    engine::game::zones::apply_resolved_zone_change(&mut replay, &command)
        .expect("the recorded battlefield exit replays");
    assert_eq!(replay.objects[&host].zone, Zone::Hand);
    assert_eq!(replay.objects[&host].damage_marked, 0);
    assert_eq!(live_resolution_count_in_state(&replay, host), 0);
}
#[test]
fn essence_burn_bounce_reentry_drops_old_grant() {
    let mut s = board();
    let spell = burn(&mut s);
    let sacrifice = rites(&mut s);
    let host = s
        .add_creature(P0, "Host", 2, 6)
        .with_mana_cost(cost(0, ManaCostShard::Black))
        .id();
    let bounce = s
        .add_spell_to_hand_from_oracle(
            P0,
            "Unsummon",
            true,
            "Return target creature to its owner's hand.",
        )
        .with_mana_cost(cost(0, ManaCostShard::Blue))
        .id();
    let mut r = s.build();
    let out = r.cast(spell).target_object(host).resolve();
    assert_eq!(out.damage_marked(host), 5);
    out.assert_zone(&[host], Zone::Battlefield);
    r.cast(bounce)
        .target_object(host)
        .resolve()
        .assert_zone(&[host], Zone::Hand);
    // CR 400.7: a normally recast permanent is a new object with no old grant.
    let out = r.cast(host).resolve();
    out.assert_zone(&[host], Zone::Battlefield);
    assert_eq!(out.damage_marked(host), 0);
    let out = r.cast(sacrifice).sacrifice_with(&[host]).resolve();
    out.assert_zone(&[host], Zone::Graveyard);
    out.assert_hand_drawn(P0, 2);
}

#[test]
fn essence_burn_inline_parse_is_complete() {
    let parsed =
        engine::parser::parse_oracle_text(ESSENCE, "Essence Burn", &[], &["Instant".into()], &[]);
    assert_eq!(parsed.abilities.len(), 1);
    let def = &parsed.abilities[0];
    assert!(matches!(def.effect.as_ref(), Effect::DealDamage { .. }));
    assert!(matches!(
        def.sub_ability
            .as_deref()
            .expect("positive rider")
            .effect
            .as_ref(),
        Effect::AddTargetReplacement { .. }
    ));
    assert!(!gaps(def));
    assert!(
        parsed.parse_warnings.is_empty(),
        "{:?}",
        parsed.parse_warnings
    );
}

#[test]
fn essence_burn_generated_fixture_exiles_lethal_creature() {
    let db = crate::support::shared_card_db().expect("generated integration fixture is required");
    let mut s = board();
    let spell = s.add_real_card(P0, "Essence Burn", Zone::Hand, db);
    let victim = creature(&mut s, P1, "Victim", ManaColor::Black, 3);
    let mut r = s.build();
    let out = r.cast(spell).target_object(victim).resolve();
    out.assert_zone(&[victim], Zone::Exile);
    out.assert_zone(&[spell], Zone::Graveyard);
    out.assert_stack_size(0);
}

#[test]
fn aether_gust_shared_color_target_legality() {
    let parsed =
        engine::parser::parse_oracle_text(GUST, "Aether Gust", &[], &["Instant".into()], &[]);
    assert_eq!(parsed.abilities.len(), 1);
    let Effect::TargetOnly { target } = parsed.abilities[0].effect.as_ref() else {
        panic!("positive spell/permanent target root");
    };
    assert!(matches!(target, TargetFilter::Or { .. }));
    assert!(!gaps(&parsed.abilities[0]));
    assert!(
        parsed.parse_warnings.is_empty(),
        "{:?}",
        parsed.parse_warnings
    );
    // CR 601.2c + CR 105.2: each pending spell uses its verified printed cost/color.
    for (name, oracle, shard, color, mana_color) in [
        ("Shock", "Shock deals 2 damage to any target.", ManaCostShard::Red, ManaColor::Red, ManaType::Red),
        ("Giant Growth", "Target creature gets +3/+3 until end of turn.", ManaCostShard::Green, ManaColor::Green, ManaType::Green),
        ("Opt", "Scry 1. (Look at the top card of your library. You may put that card on the bottom.)\nDraw a card.", ManaCostShard::Blue, ManaColor::Blue, ManaType::Blue),
    ] {
        let mut s = board(); s.with_mana_pool(P0, vec![mana(mana_color), mana(ManaType::Blue), mana(ManaType::Red)]);
        let red = creature(&mut s, P1, "Red", ManaColor::Red, 6);
        let green = creature(&mut s, P1, "Green", ManaColor::Green, 6);
        let blue = creature(&mut s, P1, "Blue", ManaColor::Blue, 6);
        let pending = s.add_spell_to_hand_from_oracle(P0, name, true, oracle).with_mana_cost(cost(0, shard)).id();
        let gust = s.add_spell_to_hand_from_oracle(P0, "Aether Gust", true, GUST).with_mana_cost(cost(1, ManaCostShard::Blue)).id();
        let mut r = s.build();
        {
            let committed = if name == "Opt" { r.cast(pending).commit() } else { r.cast(pending).target_object(red).commit() };
            assert_eq!(committed.state().objects[&pending].zone, Zone::Stack);
        }
        assert_eq!(r.state().objects[&pending].zone, Zone::Stack);
        assert_eq!(r.state().objects[&pending].color, vec![color]);
        announce(&mut r, gust); let targets = legal(&r);
        assert!(targets.contains(&TargetRef::Object(red))); assert!(targets.contains(&TargetRef::Object(green)));
        assert_eq!(targets.contains(&TargetRef::Object(pending)), color != ManaColor::Blue, "pending {name} color legality");
        assert!(!targets.contains(&TargetRef::Object(blue))); reject(&mut r, blue);
        if color == ManaColor::Blue { reject(&mut r, pending); }
        assert_eq!(r.state().objects[&pending].zone, Zone::Stack);
        // Stop at the live target prompt; library placement/owner choice is not asserted.
        assert!(matches!(r.state().waiting_for, WaitingFor::TargetSelection { .. }));
    }
}

// CR 614.3 + CR 615.3: these boards exercise duration-bound replacements through real
// parsed spells, face writers, cost announcements and zone transitions.
fn lifetime_board() -> GameScenario {
    let mut s = GameScenario::new();
    s.at_phase(Phase::PreCombatMain);
    for player in [P0, P1] {
        s.with_mana_pool(
            player,
            (0..24)
                .flat_map(|_| {
                    [
                        ManaType::White,
                        ManaType::Blue,
                        ManaType::Black,
                        ManaType::Red,
                        ManaType::Green,
                    ]
                    .map(mana)
                })
                .collect(),
        );
        s.with_library_top(
            player,
            &[
                "One", "Two", "Three", "Four", "Five", "Six", "Seven", "Eight", "Nine", "Ten",
                "Eleven", "Twelve",
            ],
        );
    }
    // Avacyn supplies the production prevention path used to keep low-toughness
    // targets alive while the independent Essence Burn rider is installed.
    real(&mut s, P0, "Avacyn, Guardian Angel", Zone::Battlefield);
    // A second legal creature proves Avacyn's target-specific shield stays on
    // the selected object rather than leaking to another eligible permanent.
    real(&mut s, P1, "Grizzly Bears", Zone::Battlefield);
    s
}
fn real(s: &mut GameScenario, player: PlayerId, name: &str, zone: Zone) -> ObjectId {
    let db = crate::support::shared_card_db().expect("required parsed-card fixture");
    let face = db
        .get_face_by_name(name)
        .expect("required real printed face");
    assert!(
        face.abilities.iter().all(|def| !gaps(def)),
        "unsupported real face: {name}"
    );
    s.add_real_card(player, name, zone, db)
}
fn priority_for(r: &mut GameRunner, player: PlayerId) {
    for _ in 0..2 {
        if matches!(r.state().waiting_for, WaitingFor::Priority { player: actual } if actual == player)
        {
            return;
        }
        r.act(GameAction::PassPriority)
            .expect("normal priority pass");
    }
    panic!(
        "expected actual priority for {player:?}, got {:?}",
        r.state().waiting_for
    );
}
fn live_resolution_count(r: &GameRunner, host: ObjectId) -> usize {
    live_resolution_count_in_state(r.state(), host)
}
fn live_resolution_count_in_state(
    state: &engine::types::game_state::GameState,
    host: ObjectId,
) -> usize {
    use engine::types::ability::{Effect, ReplacementEvent, RestrictionExpiry};
    state.objects[&host]
        .replacement_definitions
        .iter_unchecked()
        .filter(|def| {
            def.is_resolution_installed()
                && def.event == ReplacementEvent::Moved
                && def.valid_card == Some(TargetFilter::SelfRef)
                && def.expiry == Some(RestrictionExpiry::EndOfTurn)
                && def.consume_on_apply
                && def.execute.as_deref().is_some_and(|execute| {
                    matches!(
                        execute.effect.as_ref(),
                        Effect::ChangeZone {
                            origin: Some(Zone::Battlefield),
                            destination: Zone::Exile,
                            target: TargetFilter::SelfRef,
                            ..
                        }
                    )
                })
        })
        .count()
}
fn assert_intrinsic_base_in_state(state: &engine::types::game_state::GameState, host: ObjectId) {
    assert!(state.objects[&host]
        .base_replacement_definitions
        .iter()
        .all(|def| !def.is_resolution_installed()));
}
fn has_red_prevention(r: &GameRunner, host: ObjectId) -> bool {
    use engine::types::ability::{FilterProp, PreventionAmount, ShieldKind, TargetFilter};
    let incarnation = r.state().objects[&host].incarnation;
    r.state().objects[&host]
        .replacement_definitions
        .iter_unchecked()
        .any(|definition| {
            matches!(
                definition.shield_kind,
                ShieldKind::Prevention {
                    amount: PreventionAmount::All
                }
            ) && definition.target_recipient_incarnation == Some(incarnation)
                && definition.damage_source_filter.as_ref().is_some_and(|filter| {
                    matches!(filter, TargetFilter::Typed(typed) if typed.properties.contains(&FilterProp::HasColor { color: ManaColor::Red }))
                })
        })
}
fn protect_host_with_avacyn(r: &mut GameRunner, host: ObjectId) {
    use engine::types::card_type::CoreType;
    let avacyn = r
        .state()
        .objects
        .values()
        .find(|object| object.name == "Avacyn, Guardian Angel" && object.zone == Zone::Battlefield)
        .expect("real Avacyn is on the battlefield")
        .id;
    let incarnation = r.state().objects[&host].incarnation;
    let other_creature = r
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| {
            *id != host
                && *id != avacyn
                && r.state().objects[id]
                    .card_types
                    .core_types
                    .contains(&CoreType::Creature)
        })
        .expect("a second creature verifies exact recipient binding");

    priority_for(r, P0);
    r.act(GameAction::ActivateAbility {
        source_id: avacyn,
        ability_index: 0,
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("activate Avacyn's printed {1}{W} target-creature ability");
    let WaitingFor::TargetSelection { target_slots, .. } = &r.state().waiting_for else {
        panic!(
            "Avacyn must reach target selection, got {:?}",
            r.state().waiting_for
        );
    };
    assert!(target_slots[0]
        .legal_targets
        .contains(&TargetRef::Object(host)));
    r.act(GameAction::ChooseTarget {
        target: Some(TargetRef::Object(host)),
    })
    .expect("select the exact Essence Burn host");

    for _ in 0..4 {
        match &r.state().waiting_for {
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority)
                    .expect("pass priority to resolve Avacyn's ability");
            }
            WaitingFor::NamedChoice { options, .. } => {
                assert!(options.iter().any(|option| option == "Red"));
                break;
            }
            other => panic!("expected priority or Avacyn color choice, got {other:?}"),
        }
    }
    assert!(matches!(
        r.state().waiting_for,
        WaitingFor::NamedChoice { .. }
    ));
    r.act(GameAction::ChooseOption {
        choice: "Red".to_string(),
    })
    .expect("choose red as the prevented damage source color");
    r.advance_until_stack_empty();

    assert_eq!(r.state().objects[&host].incarnation, incarnation);
    assert!(has_red_prevention(r, host), "red shield is bound to host");
    assert!(
        !has_red_prevention(r, other_creature),
        "bystander gets no shield"
    );
}
fn assert_intrinsic_base(r: &GameRunner, host: ObjectId) {
    assert!(r.state().objects[&host]
        .base_replacement_definitions
        .iter()
        .all(|def| !def.is_resolution_installed()));
}
fn assert_cyber_body(r: &GameRunner, host: ObjectId, counters: i32) {
    use engine::types::card_type::CoreType;
    let obj = &r.state().objects[&host];
    assert_eq!(obj.zone, Zone::Battlefield);
    assert!(obj.face_down);
    assert_eq!(
        (obj.power, obj.toughness),
        (Some(2 + counters), Some(2 + counters))
    );
    assert!(obj.card_types.core_types.contains(&CoreType::Creature));
    assert!(obj.card_types.core_types.contains(&CoreType::Artifact));
    assert!(obj
        .card_types
        .subtypes
        .iter()
        .any(|subtype| subtype == "Cyberman"));
    assert_intrinsic_base(r, host);
}
fn burn_surviving_host(r: &mut GameRunner, spell: ObjectId, host: ObjectId) {
    priority_for(r, P0);
    let incarnation = r.state().objects[&host].incarnation;
    protect_host_with_avacyn(r, host);
    let out = r.cast(spell).target_object(host).resolve();
    out.assert_zone(&[host], Zone::Battlefield);
    out.assert_zone(&[spell], Zone::Graveyard);
    assert_eq!(
        out.damage_marked(host),
        0,
        "Avacyn prevents all five red damage"
    );
    out.assert_stack_size(0);
    assert_eq!(r.state().objects[&host].incarnation, incarnation);
    assert_eq!(live_resolution_count(r, host), 1);
    assert_intrinsic_base(r, host);
}
fn sacrifice_real(
    r: &mut GameRunner,
    spell: ObjectId,
    host: ObjectId,
    player: PlayerId,
    destination: Zone,
) {
    priority_for(r, player);
    let out = r.cast(spell).sacrifice_with(&[host]).resolve();
    out.assert_zone(&[host], destination);
    out.assert_zone(&[spell], Zone::Graveyard);
    out.assert_hand_drawn(player, 2);
    out.assert_stack_size(0);
}

#[test]
fn essence_burn_face_down_keeps_die_exile() {
    use engine::types::events::GameEvent;
    let mut s = lifetime_board();
    let host = real(&mut s, P1, "Grizzly Bears", Zone::Battlefield);
    let essence = real(&mut s, P0, "Essence Burn", Zone::Hand);
    let cyber = real(&mut s, P0, "Cyber Conversion", Zone::Hand);
    let open = real(&mut s, P0, "Break Open", Zone::Hand);
    let rites = real(&mut s, P1, "Village Rites", Zone::Hand);
    let mut r = s.build();
    assert_eq!(
        (
            r.state().objects[&host].power,
            r.state().objects[&host].toughness
        ),
        (Some(2), Some(2))
    );
    let incarnation = r.state().objects[&host].incarnation;
    burn_surviving_host(&mut r, essence, host);
    let out = r.cast(cyber).target_object(host).resolve();
    out.assert_zone(&[host], Zone::Battlefield);
    out.assert_zone(&[cyber], Zone::Graveyard);
    assert_eq!(out.damage_marked(host), 0);
    out.assert_stack_size(0);
    assert!(out.events().iter().any(
        |event| matches!(event, GameEvent::TurnedFaceDown { object_id } if *object_id == host)
    ));
    assert_cyber_body(&r, host, 0);
    assert_eq!(r.state().objects[&host].incarnation, incarnation);
    assert_eq!(r.state().objects[&host].damage_marked, 0);
    assert_eq!(live_resolution_count(&r, host), 1);
    assert_intrinsic_base(&r, host);

    priority_for(&mut r, P0);
    let out = r.cast(open).target_object(host).resolve();
    out.assert_zone(&[open], Zone::Graveyard);
    assert!(out
        .events()
        .iter()
        .any(|event| matches!(event, GameEvent::TurnedFaceUp { object_id } if *object_id == host)));
    assert_eq!(r.state().objects[&host].incarnation, incarnation);
    assert_eq!(r.state().objects[&host].damage_marked, 0);
    assert_eq!(live_resolution_count(&r, host), 1);
    assert_intrinsic_base(&r, host);
    sacrifice_real(&mut r, rites, host, P1, Zone::Exile);

    let mut s = lifetime_board();
    let host = real(&mut s, P0, "Catacomb Slug", Zone::Battlefield);
    let cyber = real(&mut s, P0, "Cyber Conversion", Zone::Hand);
    let rites = real(&mut s, P0, "Village Rites", Zone::Hand);
    let mut r = s.build();
    r.cast(cyber)
        .target_object(host)
        .resolve()
        .assert_zone(&[cyber], Zone::Graveyard);
    assert_cyber_body(&r, host, 0);
    assert_eq!(live_resolution_count(&r, host), 0);
    sacrifice_real(&mut r, rites, host, P0, Zone::Graveyard);
}

#[test]
fn brutal_expulsion_face_down_keeps_die_exile() {
    let mut s = lifetime_board();
    let host = real(&mut s, P0, "Catacomb Slug", Zone::Battlefield);
    let brutal = real(&mut s, P0, "Brutal Expulsion", Zone::Hand);
    let cyber = real(&mut s, P0, "Cyber Conversion", Zone::Hand);
    let mut r = s.build();
    let out = r.cast(brutal).modes(&[1]).target_object(host).resolve();
    out.assert_zone(&[host], Zone::Battlefield);
    out.assert_zone(&[brutal], Zone::Graveyard);
    assert_eq!(out.damage_marked(host), 2);
    assert_eq!(live_resolution_count(&r, host), 1);
    r.cast(cyber)
        .target_object(host)
        .resolve()
        .assert_zone(&[host], Zone::Exile);
    let mut s = lifetime_board();
    let host = real(&mut s, P0, "Catacomb Slug", Zone::Battlefield);
    let cyber = real(&mut s, P0, "Cyber Conversion", Zone::Hand);
    let rites = real(&mut s, P0, "Village Rites", Zone::Hand);
    let mut r = s.build();
    r.cast(cyber).target_object(host).resolve();
    assert_cyber_body(&r, host, 0);
    sacrifice_real(&mut r, rites, host, P0, Zone::Graveyard);
}

#[test]
fn essence_burn_face_up_keeps_die_exile() {
    use engine::types::events::GameEvent;
    for granted in [true, false] {
        let mut s = lifetime_board();
        let host = real(&mut s, P0, "Catacomb Slug", Zone::Battlefield);
        s.with_counter(host, CounterType::Plus1Plus1, 6);
        let essence = real(&mut s, P0, "Essence Burn", Zone::Hand);
        let cyber = real(&mut s, P0, "Cyber Conversion", Zone::Hand);
        let open = real(&mut s, P1, "Break Open", Zone::Hand);
        let rites = real(&mut s, P0, "Village Rites", Zone::Hand);
        let mut r = s.build();
        let incarnation = r.state().objects[&host].incarnation;
        if granted {
            burn_surviving_host(&mut r, essence, host);
        }
        let pre_face_down_count = live_resolution_count(&r, host);
        r.cast(cyber).target_object(host).resolve();
        assert_cyber_body(&r, host, 6);
        assert_eq!(r.state().objects[&host].incarnation, incarnation);
        assert_eq!(r.state().objects[&host].damage_marked, 0);
        assert_eq!(live_resolution_count(&r, host), pre_face_down_count);
        priority_for(&mut r, P1);
        let out = r.cast(open).target_object(host).resolve();
        out.assert_zone(&[open], Zone::Graveyard);
        assert!(out.events().iter().any(
            |event| matches!(event, GameEvent::TurnedFaceUp { object_id } if *object_id == host)
        ));
        let obj = &r.state().objects[&host];
        assert!(!obj.face_down);
        assert_eq!(obj.name, "Catacomb Slug");
        assert_eq!((obj.power, obj.toughness), (Some(8), Some(12)));
        assert_eq!(obj.counters[&CounterType::Plus1Plus1], 6);
        assert_eq!(obj.damage_marked, 0);
        assert_eq!(obj.incarnation, incarnation);
        assert_eq!(live_resolution_count(&r, host), usize::from(granted));
        sacrifice_real(
            &mut r,
            rites,
            host,
            P0,
            if granted {
                Zone::Exile
            } else {
                Zone::Graveyard
            },
        );
    }
}

#[test]
fn essence_burn_transform_keeps_die_exile() {
    use engine::types::events::GameEvent;
    use engine::types::keywords::Keyword;
    for granted in [true, false] {
        let mut s = lifetime_board();
        let host = real(&mut s, P0, "Elusive Tormentor", Zone::Battlefield);
        let essence = real(&mut s, P0, "Essence Burn", Zone::Hand);
        let discard = real(&mut s, P0, "Catacomb Slug", Zone::Hand);
        let rites = real(&mut s, P0, "Village Rites", Zone::Hand);
        let mut r = s.build();
        assert_eq!(
            (
                r.state().objects[&host].base_power,
                r.state().objects[&host].base_toughness
            ),
            (Some(4), Some(4))
        );
        if granted {
            burn_surviving_host(&mut r, essence, host);
            assert_eq!(
                (
                    r.state().objects[&host].power,
                    r.state().objects[&host].toughness
                ),
                (Some(4), Some(4))
            );
        }
        let incarnation = r.state().objects[&host].incarnation;
        let before_transform = granted.then(|| r.state().clone());
        let journal_start = r.state().resolved_rules_journal.entries().len();
        priority_for(&mut r, P0);
        let out = r.activate(host, 0).pay_with(&[discard]).resolve();
        out.assert_zone(&[discard], Zone::Graveyard);
        assert!(out.events().iter().any(
            |event| matches!(event, GameEvent::Transformed { object_id } if *object_id == host)
        ));
        let obj = &r.state().objects[&host];
        assert_eq!(obj.name, "Insidious Mist");
        assert_eq!(obj.zone, Zone::Battlefield);
        assert_eq!((obj.power, obj.toughness), (Some(0), Some(1)));
        assert!(obj.keywords.contains(&Keyword::Hexproof));
        assert!(obj.keywords.contains(&Keyword::Indestructible));
        assert_eq!(obj.damage_marked, 0);
        assert_eq!(obj.incarnation, incarnation);
        assert_eq!(live_resolution_count(&r, host), usize::from(granted));
        assert_intrinsic_base(&r, host);
        if let Some(mut replay) = before_transform {
            use engine::types::resolved_commands::ResolvedRulesCommand;
            let command = r
                .state()
                .resolved_rules_journal
                .entries()
                .iter()
                .skip(journal_start)
                .filter_map(|entry| entry.command.clone())
                .find_map(|command| match command {
                    ResolvedRulesCommand::ObjectTransform(command)
                        if command.object.object_id == host =>
                    {
                        Some(command)
                    }
                    _ => None,
                })
                .expect("real Elusive Tormentor activation records its transform");
            engine::game::transform::apply_resolved_transform(&mut replay, &command)
                .expect("the exact recorded transform replays");
            assert_eq!(replay.objects[&host].incarnation, incarnation);
            assert_eq!(replay.objects[&host].damage_marked, 0);
            assert_eq!(live_resolution_count_in_state(&replay, host), 1);
            assert_intrinsic_base_in_state(&replay, host);
            replay.layers_dirty.mark_full();
            engine::game::layers::evaluate_layers(&mut replay);
            assert_eq!(live_resolution_count_in_state(&replay, host), 1);
            assert_eq!(replay.objects[&host].damage_marked, 0);
            assert_intrinsic_base_in_state(&replay, host);
        }
        sacrifice_real(
            &mut r,
            rites,
            host,
            P0,
            if granted {
                Zone::Exile
            } else {
                Zone::Graveyard
            },
        );
    }
}

#[test]
fn essence_burn_flip_keeps_die_exile() {
    use engine::types::events::GameEvent;
    for (companions, granted, should_flip) in [(8, true, true), (7, true, false), (8, false, true)]
    {
        let mut s = lifetime_board();
        let host = real(&mut s, P0, "Orochi Eggwatcher", Zone::Battlefield);
        s.with_counter(host, CounterType::Plus1Plus1, 6);
        for _ in 0..companions {
            real(&mut s, P0, "Catacomb Slug", Zone::Battlefield);
        }
        let essence = real(&mut s, P0, "Essence Burn", Zone::Hand);
        let rites = real(&mut s, P0, "Village Rites", Zone::Hand);
        let mut r = s.build();
        let incarnation = r.state().objects[&host].incarnation;
        let before = r.state().battlefield.clone();
        assert_eq!(before.len(), companions + 3);
        assert!(!r.state().objects[&host].summoning_sick);
        let normal_cost = r.state().objects[&host].mana_cost.clone();
        assert_eq!(normal_cost, cost(2, ManaCostShard::Green));
        if granted {
            burn_surviving_host(&mut r, essence, host);
        }
        priority_for(&mut r, P0);
        let out = r.activate(host, 0).resolve();
        let tokens: Vec<_> = r
            .state()
            .battlefield
            .iter()
            .filter(|id| !before.contains(id))
            .copied()
            .collect();
        assert_eq!(tokens.len(), 1);
        let token = &r.state().objects[&tokens[0]];
        assert!(token.is_token);
        assert_eq!((token.power, token.toughness), (Some(1), Some(1)));
        assert_eq!(token.color, vec![ManaColor::Green]);
        assert!(token
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Snake"));
        assert_eq!(r.state().battlefield.len(), companions + 4);
        let obj = &r.state().objects[&host];
        assert!(obj.tapped);
        assert_eq!(obj.flipped, should_flip);
        assert_eq!(
            obj.name,
            if should_flip {
                "Shidako, Broodmistress"
            } else {
                "Orochi Eggwatcher"
            }
        );
        assert_eq!(
            (obj.power, obj.toughness),
            if should_flip {
                (Some(9), Some(9))
            } else {
                (Some(7), Some(7))
            }
        );
        assert_eq!(obj.color, vec![ManaColor::Green]);
        assert_eq!(obj.mana_cost, normal_cost);
        assert_eq!(obj.damage_marked, 0);
        assert_eq!(obj.incarnation, incarnation);
        assert_eq!(
            out.events().iter().any(
                |event| matches!(event, GameEvent::Flipped { object_id } if *object_id == host)
            ),
            should_flip
        );
        assert_eq!(live_resolution_count(&r, host), usize::from(granted));
        assert_intrinsic_base(&r, host);
        sacrifice_real(
            &mut r,
            rites,
            host,
            P0,
            if granted {
                Zone::Exile
            } else {
                Zone::Graveyard
            },
        );
    }
}

#[test]
fn essence_burn_copy_recipient_keeps_die_exile() {
    let mut s = lifetime_board();
    let host = real(&mut s, P0, "Catacomb Slug", Zone::Battlefield);
    let plain = real(&mut s, P0, "Catacomb Slug", Zone::Battlefield);
    let target = real(&mut s, P0, "Grizzly Bears", Zone::Battlefield);
    let essence = real(&mut s, P0, "Essence Burn", Zone::Hand);
    let mirror = real(&mut s, P0, "Mirrorweave", Zone::Hand);
    let rites = real(&mut s, P0, "Village Rites", Zone::Hand);
    let host_rites = real(&mut s, P0, "Village Rites", Zone::Hand);
    let mut r = s.build();
    burn_surviving_host(&mut r, essence, host);
    let out = r.cast(mirror).target_object(target).resolve();
    out.assert_zone(&[host], Zone::Battlefield);
    out.assert_zone(&[mirror], Zone::Graveyard);
    assert_eq!(out.damage_marked(host), 0);
    assert_eq!(live_resolution_count(&r, host), 1);
    assert_intrinsic_base(&r, host);
    assert_eq!(r.state().objects[&plain].name, "Grizzly Bears");
    assert_eq!(
        (
            r.state().objects[&plain].power,
            r.state().objects[&plain].toughness
        ),
        (Some(2), Some(2))
    );
    assert_eq!(live_resolution_count(&r, plain), 0);
    sacrifice_real(&mut r, rites, plain, P0, Zone::Graveyard);
    sacrifice_real(&mut r, host_rites, host, P0, Zone::Exile);
}

#[test]
fn essence_burn_copy_source_does_not_donate_die_exile() {
    let mut s = lifetime_board();
    let host = real(&mut s, P0, "Catacomb Slug", Zone::Battlefield);
    let recipient = real(&mut s, P0, "Grizzly Bears", Zone::Battlefield);
    let essence = real(&mut s, P0, "Essence Burn", Zone::Hand);
    let mirror = real(&mut s, P0, "Mirrorweave", Zone::Hand);
    let recipient_rites = real(&mut s, P0, "Village Rites", Zone::Hand);
    let source_rites = real(&mut s, P0, "Village Rites", Zone::Hand);
    let mut r = s.build();
    burn_surviving_host(&mut r, essence, host);
    r.cast(mirror).target_object(host).resolve();
    assert_eq!(r.state().objects[&host].damage_marked, 0);
    assert_eq!(r.state().objects[&recipient].name, "Catacomb Slug");
    assert_eq!(
        (
            r.state().objects[&recipient].power,
            r.state().objects[&recipient].toughness
        ),
        (Some(2), Some(6))
    );
    assert_eq!(live_resolution_count(&r, recipient), 0);
    assert_intrinsic_base(&r, recipient);
    assert!(!engine::game::printed_cards::intrinsic_copiable_values(
        &r.state().objects[&recipient]
    )
    .replacement_definitions
    .iter()
    .any(|def| def.is_resolution_installed()));
    sacrifice_real(&mut r, recipient_rites, recipient, P0, Zone::Graveyard);
    sacrifice_real(&mut r, source_rites, host, P0, Zone::Exile);
}

#[test]
fn essence_burn_survives_nezumi_graverobber_flip() {
    let mut s = lifetime_board();
    let host = real(&mut s, P0, "Nezumi Graverobber", Zone::Battlefield);
    let grave_card = real(&mut s, P1, "Grizzly Bears", Zone::Graveyard);
    let essence = real(&mut s, P0, "Essence Burn", Zone::Hand);
    let rites = real(&mut s, P0, "Village Rites", Zone::Hand);
    let mut r = s.build();
    let incarnation = r.state().objects[&host].incarnation;

    burn_surviving_host(&mut r, essence, host);
    let out = r.activate(host, 0).target_object(grave_card).resolve();
    out.assert_zone(&[grave_card], Zone::Exile);
    let flipped = &r.state().objects[&host];
    assert_eq!(flipped.name, "Nighteyes the Desecrator");
    assert!(flipped.flipped);
    assert_eq!(flipped.incarnation, incarnation);
    assert_eq!(flipped.damage_marked, 0);
    assert_eq!(live_resolution_count(&r, host), 1);
    assert_intrinsic_base(&r, host);

    sacrifice_real(&mut r, rites, host, P0, Zone::Exile);
}

#[test]
fn essence_burn_survives_lazav_multifarious_copy() {
    let mut s = lifetime_board();
    let host = real(&mut s, P0, "Lazav, the Multifarious", Zone::Battlefield);
    let grave_card = real(&mut s, P0, "Grizzly Bears", Zone::Graveyard);
    let essence = real(&mut s, P0, "Essence Burn", Zone::Hand);
    let rites = real(&mut s, P0, "Village Rites", Zone::Hand);
    let mut r = s.build();
    let incarnation = r.state().objects[&host].incarnation;

    burn_surviving_host(&mut r, essence, host);
    r.activate(host, 0).x(2).target_object(grave_card).resolve();
    let copied = &r.state().objects[&host];
    assert_eq!(copied.name, "Lazav, the Multifarious");
    assert_eq!(copied.incarnation, incarnation);
    assert_eq!(copied.damage_marked, 0);
    assert_eq!(live_resolution_count(&r, host), 1);
    assert_intrinsic_base(&r, host);

    sacrifice_real(&mut r, rites, host, P0, Zone::Exile);
}

#[test]
fn essence_burn_donor_does_not_give_spark_double_its_rider() {
    let mut s = lifetime_board();
    let donor = real(&mut s, P0, "Catacomb Slug", Zone::Battlefield);
    let spark = real(&mut s, P0, "Spark Double", Zone::Hand);
    let essence = real(&mut s, P0, "Essence Burn", Zone::Hand);
    let spark_rites = real(&mut s, P0, "Village Rites", Zone::Hand);
    let donor_rites = real(&mut s, P0, "Village Rites", Zone::Hand);
    let mut r = s.build();
    let donor_incarnation = r.state().objects[&donor].incarnation;

    burn_surviving_host(&mut r, essence, donor);
    assert_eq!(r.state().objects[&donor].damage_marked, 0);
    assert_eq!(live_resolution_count(&r, donor), 1);
    let hand_incarnation = r.state().objects[&spark].incarnation;
    let card_id = r.state().objects[&spark].card_id;
    r.act(GameAction::CastSpell {
        object_id: spark,
        card_id,
        targets: vec![],
        payment_mode: CastPaymentMode::Auto,
    })
    .expect("cast real Spark Double");
    r.advance_until_stack_empty();
    let WaitingFor::ReplacementChoice { candidates, .. } = &r.state().waiting_for else {
        panic!(
            "Spark Double must offer its printed copy replacement, got {:?}",
            r.state().waiting_for
        );
    };
    assert_eq!(candidates.len(), 2);
    r.act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept the printed enters-as-copy replacement");
    let WaitingFor::CopyTargetChoice { valid_targets, .. } = &r.state().waiting_for else {
        panic!(
            "Spark Double must offer copy targets, got {:?}",
            r.state().waiting_for
        );
    };
    assert!(valid_targets.contains(&donor));
    r.act(GameAction::ChooseTarget {
        target: Some(TargetRef::Object(donor)),
    })
    .expect("copy the protected donor");
    r.advance_until_stack_empty();

    let entered = &r.state().objects[&spark];
    assert_eq!(entered.zone, Zone::Battlefield);
    assert!(entered.incarnation > hand_incarnation);
    assert_eq!(live_resolution_count(&r, spark), 0);
    assert_intrinsic_base(&r, spark);
    assert_eq!(r.state().objects[&donor].incarnation, donor_incarnation);
    assert_eq!(r.state().objects[&donor].damage_marked, 0);
    assert_eq!(live_resolution_count(&r, donor), 1);

    sacrifice_real(&mut r, spark_rites, spark, P0, Zone::Graveyard);
    sacrifice_real(&mut r, donor_rites, donor, P0, Zone::Exile);
}

#[test]
fn essence_burn_rehydrate_keeps_die_exile() {
    let db = crate::support::shared_card_db().expect("required real-card DB");
    for granted in [true, false] {
        let mut s = lifetime_board();
        let host = real(&mut s, P0, "Catacomb Slug", Zone::Battlefield);
        let essence = real(&mut s, P0, "Essence Burn", Zone::Hand);
        let rites = real(&mut s, P0, "Village Rites", Zone::Hand);
        let mut r = s.build();
        if granted {
            burn_surviving_host(&mut r, essence, host);
        }
        let printed_ref = r.state().objects[&host]
            .printed_ref
            .clone()
            .expect("real printed identity");
        assert!(db.get_face_by_printed_ref(&printed_ref).is_some());
        engine::game::printed_cards::rehydrate_game_from_card_db(r.state_mut(), db);
        assert_eq!(
            r.state().objects[&host].printed_ref.as_ref(),
            Some(&printed_ref)
        );
        assert_eq!(r.state().objects[&host].zone, Zone::Battlefield);
        assert_eq!(r.state().objects[&host].damage_marked, 0);
        assert_eq!(live_resolution_count(&r, host), usize::from(granted));
        assert_intrinsic_base(&r, host);
        sacrifice_real(
            &mut r,
            rites,
            host,
            P0,
            if granted {
                Zone::Exile
            } else {
                Zone::Graveyard
            },
        );
    }
}

fn hand_size(r: &GameRunner, player: PlayerId) -> usize {
    r.state()
        .players
        .iter()
        .find(|p| p.id == player)
        .unwrap()
        .hand
        .len()
}
fn begin_real_rites_cost(
    r: &mut GameRunner,
    spell: ObjectId,
    host: ObjectId,
    player: PlayerId,
) -> engine::types::game_state::ActionResult {
    use engine::types::events::GameEvent;
    priority_for(r, player);
    let card_id = r.state().objects[&spell].card_id;
    let announced = r
        .act(GameAction::CastSpell {
            object_id: spell,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("normal Rites announcement before selecting the sacrifice cost");
    assert_eq!(r.state().objects[&spell].zone, Zone::Hand);
    assert!(!announced.events.iter().any(
        |event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == spell)
    ));
    assert!(
        matches!(&r.state().waiting_for, WaitingFor::PayCost { player: actual, choices, .. } if *actual == player && choices.contains(&host))
    );
    // CR 601.2a precedes cost payment (CR 601.2h); the real announcement is
    // already on the stack while object storage remains in Hand until finalize.
    assert_eq!(r.state().stack.len(), 1);
    let entry = &r.state().stack[0];
    assert_eq!(
        (entry.id, entry.source_id, entry.controller),
        (spell, spell, player)
    );
    assert!(
        matches!(&entry.kind, engine::types::game_state::StackEntryKind::Spell {
        card_id: actual_card, ability: None,
        casting_variant: engine::types::game_state::CastingVariant::Normal,
        actual_mana_spent: 0,
    } if *actual_card == card_id)
    );
    r.act(GameAction::SelectCards { cards: vec![host] })
        .expect("select the actual legal sacrifice cost")
}
fn assert_cost_cast_completed(
    r: &GameRunner,
    spell: ObjectId,
    completed: &engine::types::game_state::ActionResult,
) {
    use engine::types::events::GameEvent;
    assert_eq!(r.state().objects[&spell].zone, Zone::Stack);
    assert_eq!(r.state().stack.len(), 1);
    let entry = &r.state().stack[0];
    assert_eq!((entry.id, entry.source_id), (spell, spell));
    assert_eq!(entry.controller, r.state().objects[&spell].controller);
    assert!(
        matches!(&entry.kind, engine::types::game_state::StackEntryKind::Spell {
        card_id, ability: Some(_),
        casting_variant: engine::types::game_state::CastingVariant::Normal,
        actual_mana_spent: 1,
    } if *card_id == r.state().objects[&spell].card_id)
    );
    assert!(r.state().pending_cost_move_resume.is_none());
    assert!(r.state().pending_replacement.is_none());
    assert!(matches!(r.state().waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(completed.events.iter().filter(|event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == spell)).count(), 1);
}
fn finish_real_rites(r: &mut GameRunner, spell: ObjectId, player: PlayerId) {
    let before_draw = hand_size(r, player);
    r.advance_until_stack_empty();
    assert!(r.state().stack.is_empty());
    assert_eq!(r.state().objects[&spell].zone, Zone::Graveyard);
    assert_eq!(hand_size(r, player), before_draw + 2);
    assert!(r.state().pending_cost_move_resume.is_none());
}
fn has_reveal(events: &[engine::types::events::GameEvent], host: ObjectId) -> bool {
    use engine::types::events::GameEvent;
    events.iter().any(|event| matches!(event, GameEvent::CardsRevealed { card_ids, .. } if card_ids.contains(&host)))
}
fn has_shuffle(events: &[engine::types::events::GameEvent], owner: PlayerId) -> bool {
    use engine::types::events::{GameEvent, PlayerActionKind};
    events.iter().any(|event| matches!(event, GameEvent::PlayerPerformedAction { player_id, action: PlayerActionKind::ShuffledLibrary, .. } if *player_id == owner))
}
fn assert_real_cost_choice(
    r: &GameRunner,
    spell: ObjectId,
    host: ObjectId,
    player: PlayerId,
    count: usize,
) {
    use engine::types::game_state::PendingCostMoveResume;
    assert!(
        matches!(&r.state().waiting_for, WaitingFor::ReplacementChoice { player: actual, candidate_count, candidates, .. }
        if *actual == player && *candidate_count == count && candidates.len() == count)
    );
    assert_eq!(r.state().objects[&host].zone, Zone::Battlefield);
    assert_eq!(r.state().objects[&spell].zone, Zone::Hand);
    assert_eq!(r.state().stack.len(), 1);
    let entry = &r.state().stack[0];
    assert_eq!(
        (entry.id, entry.source_id, entry.controller),
        (spell, spell, player)
    );
    assert!(
        matches!(&entry.kind, engine::types::game_state::StackEntryKind::Spell {
        card_id, ability: None,
        casting_variant: engine::types::game_state::CastingVariant::Normal,
        actual_mana_spent: 0,
    } if *card_id == r.state().objects[&spell].card_id)
    );
    assert!(
        matches!(r.state().pending_cost_move_resume.as_ref(), Some(PendingCostMoveResume::SacrificeForCost { player: actual, chosen, paused_at_index: 0, .. }) if *actual == player && chosen == &vec![host])
    );
    assert_eq!(
        r.state()
            .pending_replacement
            .as_ref()
            .expect("real pending event")
            .candidates
            .len(),
        count
    );
}
fn choice_for_destination(r: &GameRunner, host: ObjectId, destination: Zone) -> usize {
    let pending = r
        .state()
        .pending_replacement
        .as_ref()
        .expect("actual offered replacement keys");
    pending.candidates.iter().position(|key| {
        assert_eq!(key.source, host);
        let def = &r.state().objects[&key.source].replacement_definitions[key.index];
        def.execute.as_deref().is_some_and(|execute| matches!(execute.effect.as_ref(), Effect::ChangeZone { destination: actual, .. } if *actual == destination))
    }).expect("actual definition with the selected execute destination")
}
fn assert_selected_actual_key(r: &GameRunner, index: usize, destination: Zone) {
    use engine::game::replacement::{continue_replacement, ReplacementResult};
    use engine::types::proposed_event::{AppliedReplacementKey, ProposedEvent};
    // Supplemental exact-key inspection starts from the REAL paused cost state.
    // The runtime witness below still uses GameAction::ChooseReplacement and
    // verifies delivered zones, effects and exactly one resumed cast.
    let pending = r
        .state()
        .pending_replacement
        .as_ref()
        .expect("actual cost pause");
    let chosen = pending.candidates[index];
    assert!(!pending
        .proposed
        .applied_set()
        .contains(&AppliedReplacementKey::object(chosen.source, chosen.index)));
    let mut inspection = r.state().clone();
    let mut events = Vec::new();
    let ReplacementResult::Execute(proposed) =
        continue_replacement(&mut inspection, index, &mut events)
    else {
        panic!("the selected real replacement must complete this cost event");
    };
    assert!(proposed
        .applied_set()
        .contains(&AppliedReplacementKey::object(chosen.source, chosen.index)));
    assert_eq!(proposed.applied_set().len(), 1);
    assert!(matches!(proposed, ProposedEvent::ZoneChange { to, .. } if to == destination));
}

#[test]
fn intrinsic_replacement_removed_face_down_and_restored_face_up() {
    use engine::types::keywords::Keyword;
    for restored in [false, true] {
        let mut s = lifetime_board();
        let host = real(&mut s, P0, "Darksteel Colossus", Zone::Battlefield);
        let cyber = real(&mut s, P0, "Cyber Conversion", Zone::Hand);
        let open = real(&mut s, P1, "Break Open", Zone::Hand);
        let rites = real(&mut s, P0, "Village Rites", Zone::Hand);
        let mut r = s.build();
        assert!(!r.state().objects[&host]
            .base_replacement_definitions
            .is_empty());
        r.cast(cyber).target_object(host).resolve();
        assert_cyber_body(&r, host, 0);
        assert!(r.state().objects[&host].replacement_definitions.is_empty());
        if restored {
            priority_for(&mut r, P1);
            r.cast(open).target_object(host).resolve();
            let obj = &r.state().objects[&host];
            assert!(!obj.face_down);
            assert_eq!(obj.name, "Darksteel Colossus");
            assert_eq!((obj.power, obj.toughness), (Some(11), Some(11)));
            assert!(obj.keywords.contains(&Keyword::Indestructible));
            assert_eq!(obj.replacement_definitions.len(), 1);
        }
        let completed = begin_real_rites_cost(&mut r, rites, host, P0);
        assert_eq!(
            r.state().objects[&host].zone,
            if restored {
                Zone::Library
            } else {
                Zone::Graveyard
            },
            "observe the sacrifice destination BEFORE Rites can draw the shuffled card"
        );
        assert_eq!(has_reveal(&completed.events, host), restored);
        assert_eq!(has_shuffle(&completed.events, P0), restored);
        assert_cost_cast_completed(&r, rites, &completed);
        finish_real_rites(&mut r, rites, P0);
    }
}

#[test]
fn essence_burn_multiple_live_replacements_keep_actual_choice() {
    for count in 0..=2 {
        let mut s = lifetime_board();
        let host = real(&mut s, P0, "Catacomb Slug", Zone::Battlefield);
        s.with_counter(host, CounterType::Plus1Plus1, 6);
        let essence = real(&mut s, P0, "Essence Burn", Zone::Hand);
        let brutal = real(&mut s, P0, "Brutal Expulsion", Zone::Hand);
        let cyber = real(&mut s, P0, "Cyber Conversion", Zone::Hand);
        let rites = real(&mut s, P0, "Village Rites", Zone::Hand);
        let mut r = s.build();
        if count >= 1 {
            burn_surviving_host(&mut r, essence, host);
        }
        let first = r.state().objects[&host]
            .replacement_definitions
            .as_slice()
            .to_vec();
        if count == 2 {
            let out = r.cast(brutal).modes(&[1]).target_object(host).resolve();
            out.assert_zone(&[brutal], Zone::Graveyard);
            assert_eq!(out.damage_marked(host), 0);
            assert_eq!(
                r.state().objects[&host].replacement_definitions[0],
                first[0]
            );
        }
        let before_rewrite = r.state().objects[&host]
            .replacement_definitions
            .as_slice()
            .to_vec();
        r.cast(cyber).target_object(host).resolve();
        assert_cyber_body(&r, host, 6);
        assert_eq!(live_resolution_count(&r, host), count);
        assert_eq!(
            r.state().objects[&host].replacement_definitions.as_slice(),
            before_rewrite.as_slice()
        );
        let paused = begin_real_rites_cost(&mut r, rites, host, P0);
        let completed = if count == 2 {
            use engine::types::events::GameEvent;
            assert_real_cost_choice(&r, rites, host, P0, 2);
            assert!(!paused.events.iter().any(|event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == rites)));
            let index = choice_for_destination(&r, host, Zone::Exile);
            assert_selected_actual_key(&r, index, Zone::Exile);
            r.act(GameAction::ChooseReplacement { index })
                .expect("select the actual external replacement")
        } else {
            paused
        };
        assert_eq!(
            r.state().objects[&host].zone,
            if count == 0 {
                Zone::Graveyard
            } else {
                Zone::Exile
            }
        );
        assert_cost_cast_completed(&r, rites, &completed);
        finish_real_rites(&mut r, rites, P0);
    }
}

#[test]
fn brutal_expulsion_intrinsic_and_external_choice_after_rewrites() {
    let mut s = lifetime_board();
    let host = real(&mut s, P0, "Darksteel Colossus", Zone::Battlefield);
    s.with_counter(host, CounterType::Plus1Plus1, 1);
    let brutal = real(&mut s, P0, "Brutal Expulsion", Zone::Hand);
    let cyber = real(&mut s, P0, "Cyber Conversion", Zone::Hand);
    let open = real(&mut s, P1, "Break Open", Zone::Hand);
    let rites = real(&mut s, P0, "Village Rites", Zone::Hand);
    let mut r = s.build();
    let out = r.cast(brutal).modes(&[1]).target_object(host).resolve();
    out.assert_zone(&[brutal], Zone::Graveyard);
    out.assert_zone(&[host], Zone::Battlefield);
    assert_eq!(out.damage_marked(host), 2);
    assert_eq!(live_resolution_count(&r, host), 1);
    r.cast(cyber).target_object(host).resolve();
    assert_cyber_body(&r, host, 1);
    assert_eq!(r.state().objects[&host].replacement_definitions.len(), 1);
    priority_for(&mut r, P1);
    r.cast(open).target_object(host).resolve();
    assert!(!r.state().objects[&host].face_down);
    assert_eq!(
        (
            r.state().objects[&host].power,
            r.state().objects[&host].toughness
        ),
        (Some(12), Some(12))
    );
    assert_eq!(r.state().objects[&host].replacement_definitions.len(), 2);
    assert_eq!(live_resolution_count(&r, host), 1);
    assert_intrinsic_base(&r, host);
    let actual_prepared_board = r.state().clone();
    for destination in [Zone::Exile, Zone::Library] {
        let mut r = GameRunner::from_state(actual_prepared_board.clone());
        let paused = begin_real_rites_cost(&mut r, rites, host, P0);
        assert_real_cost_choice(&r, rites, host, P0, 2);
        use engine::types::events::GameEvent;
        assert!(!paused.events.iter().any(
            |event| matches!(event, GameEvent::SpellCast { object_id, .. } if *object_id == rites)
        ));
        let exile = choice_for_destination(&r, host, Zone::Exile);
        let library = choice_for_destination(&r, host, Zone::Library);
        assert_ne!(exile, library);
        let pending = r.state().pending_replacement.as_ref().unwrap();
        assert_ne!(pending.candidates[exile], pending.candidates[library]);
        let index = if destination == Zone::Exile {
            exile
        } else {
            library
        };
        assert_selected_actual_key(&r, index, destination);
        let completed = r
            .act(GameAction::ChooseReplacement { index })
            .expect("normal actual intrinsic/external choice");
        assert_eq!(
            r.state().objects[&host].zone,
            destination,
            "observe the selected route before Rites draws"
        );
        assert_eq!(
            has_reveal(&completed.events, host),
            destination == Zone::Library
        );
        assert_eq!(
            has_shuffle(&completed.events, P0),
            destination == Zone::Library
        );
        assert_cost_cast_completed(&r, rites, &completed);
        finish_real_rites(&mut r, rites, P0);
    }
}

fn advance_to_main(r: &mut GameRunner, player: PlayerId, after_turn: u32) {
    for _ in 0..128 {
        if r.state().turn_number > after_turn
            && r.state().active_player == player
            && r.state().phase == Phase::PreCombatMain
        {
            break;
        }
        match r.state().waiting_for {
            WaitingFor::Priority { .. } => {
                r.act(GameAction::PassPriority)
                    .expect("ordinary turn priority");
            }
            WaitingFor::DeclareAttackers { .. } => {
                r.declare_attackers(&[]).expect("ordinary empty combat");
            }
            WaitingFor::DeclareBlockers { .. } => {
                r.declare_blockers(&[]).expect("ordinary empty blocks");
            }
            _ => panic!("unexpected actual turn prompt {:?}", r.state().waiting_for),
        }
    }
    assert!(r.state().turn_number > after_turn);
    assert_eq!(r.state().active_player, player);
    assert_eq!(r.state().phase, Phase::PreCombatMain);
    assert!(
        matches!(r.state().waiting_for, WaitingFor::Priority { player: actual } if actual == player)
    );
}
fn fund_after_cleanup(r: &mut GameRunner, player: PlayerId) {
    // Only payment is seeded; turn, faces, damage, counters and effects are real.
    let pool = &mut r
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .unwrap()
        .mana_pool;
    for _ in 0..12 {
        for color in [
            ManaType::Black,
            ManaType::Blue,
            ManaType::Red,
            ManaType::Green,
            ManaType::White,
        ] {
            pool.add(mana(color));
        }
    }
}

#[test]
fn essence_burn_rewrite_bounce_reentry_drops_old_grant() {
    // Independent normal spells reach the damage lifetime even with no grant.
    let mut ordinary = lifetime_board();
    let ordinary_host = real(&mut ordinary, P1, "Catacomb Slug", Zone::Battlefield);
    let shock = real(&mut ordinary, P0, "Shock", Zone::Hand);
    let unsummon = real(&mut ordinary, P0, "Unsummon", Zone::Hand);
    let mut normal = ordinary.build();
    let normal_card = normal.state().objects[&ordinary_host].card_id;
    let normal_incarnation = normal.state().objects[&ordinary_host].incarnation;
    let shock_outcome = normal.cast(shock).target_object(ordinary_host).resolve();
    shock_outcome.assert_zone(&[ordinary_host], Zone::Battlefield);
    shock_outcome.assert_zone(&[shock], Zone::Graveyard);
    shock_outcome.assert_stack_size(0);
    assert_eq!(normal.state().objects[&ordinary_host].damage_marked, 2);
    assert_eq!(live_resolution_count(&normal, ordinary_host), 0);
    assert!(normal.state().objects[&ordinary_host]
        .replacement_definitions
        .is_empty());
    let unsummon_outcome = normal.cast(unsummon).target_object(ordinary_host).resolve();
    unsummon_outcome.assert_zone(&[ordinary_host], Zone::Hand);
    unsummon_outcome.assert_zone(&[unsummon], Zone::Graveyard);
    unsummon_outcome.assert_stack_size(0);
    let bounced = &normal.state().objects[&ordinary_host];
    assert_eq!(bounced.card_id, normal_card);
    assert!(bounced.incarnation > normal_incarnation);
    assert_eq!(bounced.owner, P1);
    assert!(normal
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .unwrap()
        .hand
        .contains(&ordinary_host));
    assert_eq!(
        bounced.damage_marked, 0,
        "normal Shock then Unsummon must clear marked damage immediately in its owner's hand"
    );
    let db = crate::support::shared_card_db().expect("required real printed Slug");
    let face = db.get_face_by_name("Catacomb Slug").unwrap();
    let mut s = lifetime_board();
    // The existing legal initial-control builder sets both base and live control.
    // Its body, Oracle text and casting cost come from the real parsed Slug face.
    let host = s
        .add_creature_from_oracle(
            P1,
            "Catacomb Slug",
            2,
            6,
            face.oracle_text.as_deref().unwrap_or(""),
        )
        .with_mana_cost(face.mana_cost.clone())
        .with_color(vec![ManaColor::Black])
        .controlled_by(P0)
        .id();
    let counterpart = real(&mut s, P0, "Catacomb Slug", Zone::Battlefield);
    for id in [host, counterpart] {
        s.with_counter(id, CounterType::Plus1Plus1, 6);
    }
    let essence = real(&mut s, P0, "Essence Burn", Zone::Hand);
    let counterpart_burn = real(&mut s, P0, "Essence Burn", Zone::Hand);
    let cyber = real(&mut s, P0, "Cyber Conversion", Zone::Hand);
    let counterpart_cyber = real(&mut s, P0, "Cyber Conversion", Zone::Hand);
    let bounce = real(&mut s, P0, "Unsummon", Zone::Hand);
    let counterpart_rites = real(&mut s, P0, "Village Rites", Zone::Hand);
    let rites = real(&mut s, P1, "Village Rites", Zone::Hand);
    let mut r = s.build();
    assert_eq!(r.state().objects[&host].owner, P1);
    assert_eq!(r.state().objects[&host].controller, P0);
    assert_eq!(r.state().objects[&host].base_controller, Some(P0));
    let card_id = r.state().objects[&host].card_id;
    let old_incarnation = r.state().objects[&host].incarnation;
    burn_surviving_host(&mut r, essence, host);
    burn_surviving_host(&mut r, counterpart_burn, counterpart);
    for (spell, id) in [(cyber, host), (counterpart_cyber, counterpart)] {
        r.cast(spell).target_object(id).resolve();
        assert_cyber_body(&r, id, 6);
        assert_eq!(live_resolution_count(&r, id), 1);
    }
    // A second normal branch from this actual prepared board proves that the
    // stolen host's affected controller P0 chooses/pays its sacrifice, although
    // its owner is P1. The original branch below instead returns it to its owner.
    let mut control_branch = GameRunner::from_state(r.state().clone());
    let completed = begin_real_rites_cost(&mut control_branch, counterpart_rites, host, P0);
    assert_eq!(control_branch.state().objects[&host].zone, Zone::Exile);
    assert_eq!(control_branch.state().objects[&host].owner, P1);
    assert_cost_cast_completed(&control_branch, counterpart_rites, &completed);
    finish_real_rites(&mut control_branch, counterpart_rites, P0);
    let completed = begin_real_rites_cost(&mut r, counterpart_rites, counterpart, P0);
    assert_eq!(r.state().objects[&counterpart].zone, Zone::Exile);
    assert_cost_cast_completed(&r, counterpart_rites, &completed);
    finish_real_rites(&mut r, counterpart_rites, P0);
    priority_for(&mut r, P0);
    r.cast(bounce)
        .target_object(host)
        .resolve()
        .assert_zone(&[host], Zone::Hand);
    assert!(r
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .unwrap()
        .hand
        .contains(&host));
    assert!(!r
        .state()
        .players
        .iter()
        .find(|p| p.id == P0)
        .unwrap()
        .hand
        .contains(&host));
    assert_eq!(r.state().objects[&host].card_id, card_id);
    assert!(r.state().objects[&host].incarnation > old_incarnation);
    assert_eq!(r.state().objects[&host].damage_marked, 0);
    assert!(r.state().objects[&host].counters.is_empty());
    assert!(!r.state().objects[&host].face_down);
    assert_eq!(live_resolution_count(&r, host), 0);
    assert_intrinsic_base(&r, host);
    let bounced_incarnation = r.state().objects[&host].incarnation;
    let turn = r.state().turn_number;
    advance_to_main(&mut r, P1, turn);
    fund_after_cleanup(&mut r, P1);
    r.cast(host)
        .resolve()
        .assert_zone(&[host], Zone::Battlefield);
    let obj = &r.state().objects[&host];
    assert_eq!(obj.card_id, card_id);
    assert!(obj.incarnation > bounced_incarnation);
    assert_eq!(obj.controller, P1);
    assert_eq!((obj.power, obj.toughness), (Some(2), Some(6)));
    assert_eq!(obj.damage_marked, 0);
    assert!(obj.counters.is_empty());
    assert!(!obj.face_down && !obj.flipped);
    assert_eq!(live_resolution_count(&r, host), 0);
    let completed = begin_real_rites_cost(&mut r, rites, host, P1);
    assert_eq!(r.state().objects[&host].zone, Zone::Graveyard);
    assert_cost_cast_completed(&r, rites, &completed);
    finish_real_rites(&mut r, rites, P1);
}

#[test]
fn essence_burn_rewrite_expires_at_cleanup() {
    let mut s = lifetime_board();
    let host = real(&mut s, P0, "Catacomb Slug", Zone::Battlefield);
    let counterpart = real(&mut s, P0, "Catacomb Slug", Zone::Battlefield);
    for id in [host, counterpart] {
        s.with_counter(id, CounterType::Plus1Plus1, 6);
    }
    let essence = real(&mut s, P0, "Essence Burn", Zone::Hand);
    let counterpart_burn = real(&mut s, P0, "Essence Burn", Zone::Hand);
    let cyber = real(&mut s, P0, "Cyber Conversion", Zone::Hand);
    let counterpart_cyber = real(&mut s, P0, "Cyber Conversion", Zone::Hand);
    let same_turn_rites = real(&mut s, P0, "Village Rites", Zone::Hand);
    let later_rites = real(&mut s, P0, "Village Rites", Zone::Hand);
    let mut r = s.build();
    burn_surviving_host(&mut r, essence, host);
    burn_surviving_host(&mut r, counterpart_burn, counterpart);
    for (spell, id) in [(cyber, host), (counterpart_cyber, counterpart)] {
        r.cast(spell).target_object(id).resolve();
        assert_cyber_body(&r, id, 6);
        assert_eq!(live_resolution_count(&r, id), 1);
    }
    sacrifice_real(&mut r, same_turn_rites, counterpart, P0, Zone::Exile);
    let turn = r.state().turn_number;
    advance_to_main(&mut r, P0, turn);
    assert_cyber_body(&r, host, 6);
    assert_eq!(r.state().objects[&host].damage_marked, 0);
    assert_eq!(live_resolution_count(&r, host), 0);
    assert!(r.state().objects[&host].replacement_definitions.is_empty());
    assert!(r.state().objects[&host]
        .base_replacement_definitions
        .is_empty());
    fund_after_cleanup(&mut r, P0);
    sacrifice_real(&mut r, later_rites, host, P0, Zone::Graveyard);
}

#[test]
fn essence_burn_and_rest_in_peace_choice_survives_restore() {
    use engine::types::events::GameEvent;
    use engine::types::game_state::PersistedGameState;

    // Catacomb Slug is 2/6, so Essence Burn leaves it on the battlefield with
    // five marked damage and installs the rider before the later sacrifice.
    let mut s = lifetime_board();
    let host = real(&mut s, P0, "Catacomb Slug", Zone::Battlefield);
    let rest = real(&mut s, P1, "Rest in Peace", Zone::Battlefield);
    let essence = real(&mut s, P0, "Essence Burn", Zone::Hand);
    let rites = real(&mut s, P0, "Village Rites", Zone::Hand);
    let mut r = s.build();

    let burned = r.cast(essence).target_object(host).resolve();
    burned.assert_zone(&[host], Zone::Battlefield);
    assert_eq!(burned.damage_marked(host), 5);
    assert_eq!(live_resolution_count(&r, host), 1);
    assert_intrinsic_base(&r, host);

    begin_real_rites_cost(&mut r, rites, host, P0);
    assert_real_cost_choice(&r, rites, host, P0, 2);
    let sources: Vec<_> = r
        .state()
        .pending_replacement
        .as_ref()
        .expect("real replacement pause")
        .candidates
        .iter()
        .map(|candidate| candidate.source)
        .collect();
    assert!(
        sources.contains(&rest),
        "Rest in Peace is an offered source"
    );
    assert!(
        sources.contains(&host),
        "Essence Burn rider is an offered source"
    );

    let persisted = PersistedGameState::capture(r.state().clone());
    let restored_state = persisted
        .into_game_state()
        .expect("restore the pending replacement-choice state");
    let mut restored = GameRunner::from_state(restored_state);
    assert_real_cost_choice(&restored, rites, host, P0, 2);
    let pending = restored.state().pending_replacement.as_ref().unwrap();
    let host_rider_index = pending
        .candidates
        .iter()
        .position(|candidate| candidate.source == host)
        .expect("the restored choice retains the target-bound rider identity");

    // Both effects send this move to Exile. This proves restored choice/source
    // identity and exactly one delivered transition; it does not compare ordering
    // between distinct destinations.
    let chosen = restored
        .act(GameAction::ChooseReplacement {
            index: host_rider_index,
        })
        .expect("choose the restored Essence Burn replacement through GameAction");
    assert_eq!(restored.state().objects[&host].zone, Zone::Exile);
    assert_eq!(
        chosen
            .events
            .iter()
            .filter(|event| matches!(event, GameEvent::ZoneChanged { object_id, from: Some(Zone::Battlefield), to: Zone::Exile, .. } if *object_id == host))
            .count(),
        1,
        "the selected shared-destination replacement delivers one zone-change event"
    );
    assert_cost_cast_completed(&restored, rites, &chosen);
    finish_real_rites(&mut restored, rites, P0);
}
