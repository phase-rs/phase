//! Regression coverage for continuations that belong to delayed-trigger payloads.

use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, SubAbilityLink, TargetFilter,
};
use engine::types::zones::Zone;

const CODIE: &str = "You can't cast permanent spells.\n{4}, {T}: Add {W}{U}{B}{R}{G}. When you next cast a spell this turn, exile cards from the top of your library until you exile an instant or sorcery card with lesser mana value. Until end of turn, you may cast that card without paying its mana cost. Put each other card exiled this way on the bottom of your library in a random order.";
const POWER_PACK: &str = "Flying, vigilance, trample, haste\nWhenever Power Pack deals combat damage to a player, exile target instant or sorcery card from your graveyard chosen at random. At the beginning of your next upkeep, you may cast that card without paying its mana cost. If that spell would be put into your graveyard, exile it instead.";
const KYLOX: &str = "Collect evidence 6: This Vehicle becomes an artifact creature until end of turn.\nWhenever this Vehicle attacks, you may cast an instant or sorcery spell from among cards exiled with it. If that spell would be put into a graveyard, put it on the bottom of its owner's library instead.\nCrew 2";
const GOBLIN_KITES: &str = "{R}: Target creature you control with toughness 2 or less gains flying until end of turn. Flip a coin at the beginning of the next end step. If you lose the flip, sacrifice that creature.";
const ELEMENTAL_APPEAL: &str = "Kicker {5} (You may pay an additional {5} as you cast this spell.)\nCreate a 7/1 red Elemental creature token with trample and haste. Exile it at the beginning of the next end step. If this spell was kicked, that creature gets +7/+0 until end of turn.";
const BUMIS_FEAST_LECTURE: &str = "Create a Food token. Then earthbend X, where X is twice the number of Foods you control. (A Food token is an artifact with \"{2}, {T}, Sacrifice this token: You gain 3 life.\" To earthbend X, target land you control becomes a 0/0 creature with haste that's still a land. Put X +1/+1 counters on it. When it dies or is exiled, return it to the battlefield tapped.)";

fn parse_card(name: &str, text: &str) -> engine::parser::oracle::ParsedAbilities {
    parse_oracle_text(text, name, &[], &[], &[])
}

fn spine(def: &AbilityDefinition) -> Vec<&AbilityDefinition> {
    let mut spine = vec![def];
    while let Some(next) = spine.last().and_then(|node| node.sub_ability.as_deref()) {
        spine.push(next);
    }
    spine
}

fn delayed_payload(def: &AbilityDefinition) -> &AbilityDefinition {
    for node in spine(def) {
        if let Effect::CreateDelayedTrigger { effect, .. } = node.effect.as_ref() {
            return effect;
        }
    }
    panic!("expected a delayed-trigger installer on the ability spine: {def:#?}")
}

fn find_effect<'a>(
    def: &'a AbilityDefinition,
    predicate: &impl Fn(&Effect) -> bool,
) -> Option<&'a AbilityDefinition> {
    if predicate(def.effect.as_ref()) {
        return Some(def);
    }
    if let Effect::CreateDelayedTrigger { effect, .. } = def.effect.as_ref() {
        if let Some(found) = find_effect(effect, predicate) {
            return Some(found);
        }
    }
    def.sub_ability
        .as_deref()
        .and_then(|child| find_effect(child, predicate))
        .or_else(|| {
            def.else_ability
                .as_deref()
                .and_then(|child| find_effect(child, predicate))
        })
}

fn codie_activation() -> AbilityDefinition {
    parse_card("Codie, Vociferous Codex", CODIE)
        .abilities
        .into_iter()
        .find(|ability| ability.kind == AbilityKind::Activated)
        .expect("Codie's verbatim Oracle text must produce an activated ability")
}

#[test]
fn codie_relocates_two_continuations_in_printed_order() {
    let activated = codie_activation();
    let top_level = spine(&activated);
    let payload = delayed_payload(&activated);
    let payload_spine = spine(payload);

    assert_eq!(
        top_level.len(),
        2,
        "T-AR1: Codie's top-level chain must shrink by its two relocated definitions"
    );
    assert_eq!(
        payload_spine.len(),
        3,
        "T-PL2/T-PLoc2: the delayed payload must contain exactly three definitions"
    );
    assert!(matches!(
        payload_spine[0].effect.as_ref(),
        Effect::ExileFromTopUntil { .. }
    ));
    assert!(matches!(
        payload_spine[1].effect.as_ref(),
        Effect::CastFromZone { .. }
    ));
    assert!(matches!(
        payload_spine[2].effect.as_ref(),
        Effect::PutAtLibraryPosition { .. }
    ));
    assert_eq!(
        payload_spine[1].kind,
        AbilityKind::Spell,
        "T-KIND: Codie's relocated CastFromZone must be a spell continuation"
    );
    assert_eq!(
        payload_spine[2].kind,
        AbilityKind::Spell,
        "T-KIND: Codie's relocated library placement must be a spell continuation"
    );
    assert_eq!(
        activated.kind,
        AbilityKind::Activated,
        "T-KIND control: the activation's chain head is not relocated"
    );
    assert_eq!(
        payload_spine[0].kind,
        AbilityKind::Spell,
        "T-KIND control: Codie's payload head is already a spell and is not mover-owned"
    );
    assert_eq!(
        payload_spine[2].sub_link,
        SubAbilityLink::SequentialSibling,
        "T-SL: the mover preserves the parsed sentence boundary"
    );
}

#[test]
fn power_pack_relocates_the_exile_replacement_rider_but_kylox_keeps_its_folded_rider() {
    let power_pack = parse_card("Power Pack", POWER_PACK);
    let power_execute = power_pack
        .triggers
        .first()
        .and_then(|trigger| trigger.execute.as_deref())
        .expect("Power Pack's combat-damage trigger must execute an ability");
    let power_payload = delayed_payload(power_execute);
    assert!(
        find_effect(power_payload, &|effect| matches!(
            effect,
            Effect::ChangeZone {
                destination: Zone::Exile,
                target: TargetFilter::ParentTarget,
                ..
            }
        ))
        .is_some(),
        "T-PP: Power Pack's exile replacement rider must live in its delayed payload; \
         payload = {power_payload:#?}; execute = {power_execute:#?}"
    );

    let kylox = parse_card("Kylox's Voltstrider", KYLOX);
    let kylox_execute = kylox
        .triggers
        .first()
        .and_then(|trigger| trigger.execute.as_deref())
        .expect("Kylox's attack trigger must execute an ability");
    let cast = find_effect(kylox_execute, &|effect| {
        matches!(effect, Effect::CastFromZone { .. })
    })
    .expect("Kylox must produce its cast-from-exile instruction");
    assert!(matches!(
        cast.sub_ability
            .as_deref()
            .map(|child| child.effect.as_ref()),
        Some(Effect::PutAtLibraryPosition { .. })
    ));
    assert!(
        !matches!(cast.effect.as_ref(), Effect::CreateDelayedTrigger { .. }),
        "T-PP control: Kylox's folded rider remains attached to CastFromZone, not a delayed payload"
    );
}

#[test]
fn goblin_kites_payload_head_is_not_stamped_as_a_spell() {
    let kites = parse_card("Goblin Kites", GOBLIN_KITES);
    let activated = kites
        .abilities
        .iter()
        .find(|ability| ability.kind == AbilityKind::Activated)
        .expect("Goblin Kites must produce its activated ability");
    let payload = delayed_payload(activated);

    assert_eq!(
        payload.kind,
        AbilityKind::Activated,
        "T-KIND control: Goblin Kites detects an over-stamping mover at the payload head"
    );
}

#[test]
fn elemental_appeal_keeps_its_kicker_pump_outside_the_delayed_payload() {
    let elemental = parse_card("Elemental Appeal", ELEMENTAL_APPEAL);
    let root = elemental
        .abilities
        .first()
        .expect("Elemental Appeal must produce its spell ability");
    let outer_spine = spine(root);
    let payload = delayed_payload(root);

    assert!(matches!(root.effect.as_ref(), Effect::Token { .. }));
    assert!(matches!(
        outer_spine.last().map(|node| node.effect.as_ref()),
        Some(Effect::Pump {
            target: TargetFilter::LastCreated,
            ..
        })
    ));
    assert!(matches!(
        payload.effect.as_ref(),
        Effect::ChangeZone {
            target: TargetFilter::LastCreated,
            ..
        }
    ));
    assert!(
        !find_effect(payload, &|effect| matches!(effect, Effect::Pump { .. })).is_some(),
        "T-ROW5: Elemental Appeal's kicker pump is a top-level sibling, not a delayed payload child"
    );

    let bumi = parse_card("Bumi's Feast Lecture", BUMIS_FEAST_LECTURE);
    let bumi_root = bumi
        .abilities
        .first()
        .expect("Bumi's Feast Lecture must produce its spell ability");
    assert!(matches!(bumi_root.effect.as_ref(), Effect::Token { .. }));
    assert!(
        find_effect(bumi_root, &|effect| matches!(
            effect,
            Effect::RegisterBending { .. }
        ))
        .is_some(),
        "T-ROW5 hostile control: Bumi's post-token earthbend remains RegisterBending"
    );
}
