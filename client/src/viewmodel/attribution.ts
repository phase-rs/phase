import type {
  AttributionLayer,
  ContinuousModification,
  EffectRef,
  GameObject,
  Keyword,
  ObjectAttribution,
  ObjectId,
  TransientContinuousEffect,
} from "../adapter/types";
import { getKeywordName } from "./keywordProps";

/**
 * Resolves an `EffectRef` to the granted `ContinuousModification` plus the
 * display name of its source. Returns `null` when the referenced
 * static-definition slot or transient effect can't be found in the current
 * state (stale serialization, dead transient, etc.).
 *
 * Dereference is a pure lookup — no game logic. The engine writes
 * attribution at layer-application time so by the time the FE consumes it,
 * the references are valid against the *same* state snapshot the
 * attribution was computed from.
 */
export interface ResolvedAttribution {
  modification: ContinuousModification;
  sourceName: string;
  sourceId: ObjectId;
}

/**
 * The minimal state slice needed to resolve an `EffectRef`. Callers pass
 * narrowly-subscribed Zustand slices instead of the whole `GameState`,
 * which keeps `PermanentCard` re-renders bound to attribution-relevant
 * state changes only.
 */
export interface AttributionDeref {
  objects: Record<string, GameObject> | undefined;
  transientContinuousEffects: TransientContinuousEffect[] | undefined;
}

function resolveStatic(
  deref: AttributionDeref,
  source: ObjectId,
  defIndex: number,
  modIndex: number,
): ResolvedAttribution | null {
  const sourceObj = deref.objects?.[String(source)];
  if (!sourceObj) return null;
  const def = sourceObj.static_definitions[defIndex] as
    | { modifications?: ContinuousModification[] }
    | undefined;
  const mod = def?.modifications?.[modIndex];
  if (!mod) return null;
  return { modification: mod, sourceName: sourceObj.name, sourceId: source };
}

function resolveTransient(
  deref: AttributionDeref,
  id: number,
  modIndex: number,
): ResolvedAttribution | null {
  const tce = deref.transientContinuousEffects?.find((t) => t.id === id);
  if (!tce) return null;
  const mod = tce.modifications[modIndex];
  if (!mod) return null;
  return {
    modification: mod,
    sourceName: tce.source_name,
    sourceId: tce.source_id,
  };
}

export function resolveEffectRef(
  deref: AttributionDeref,
  ref: EffectRef,
): ResolvedAttribution | null {
  if (ref.type === "Transient") {
    return resolveTransient(deref, ref.data.id, ref.data.mod_index);
  }
  return resolveStatic(
    deref,
    ref.data.source,
    ref.data.def_index,
    ref.data.mod_index,
  );
}

/**
 * Composable primitive: resolves every `EffectRef` in one layer bucket on
 * the given object, filtering out self-grants (CR 113.3c — a permanent's
 * own static abilities granting itself a characteristic read as "base" to
 * the player). Returns dereferenced `ResolvedAttribution` records the
 * caller can filter/group by `modification.type`.
 *
 * All per-surface builders (`buildGrantedKeywordSources`, `buildPTSources`,
 * etc.) compose on top of this so layer/sublayer plumbing lives in exactly
 * one place.
 */
export function resolveLayerAttributions(
  attribution: ObjectAttribution | undefined,
  layer: AttributionLayer,
  objectId: ObjectId,
  deref: AttributionDeref,
): ResolvedAttribution[] {
  const refs = attribution?.by_layer?.[layer];
  if (!refs) return [];
  const out: ResolvedAttribution[] = [];
  for (const ref of refs) {
    const resolved = resolveEffectRef(deref, ref);
    if (!resolved) continue;
    if (resolved.sourceId === objectId) continue;
    out.push(resolved);
  }
  return out;
}

/**
 * Builds a `keyword_name → source_name` map for one object by dereferencing
 * every `EffectRef` in its `Layer::Ability` bucket that grants a keyword.
 *
 * Self-grants (source === objectId) are filtered out via
 * `resolveLayerAttributions`.
 *
 * Returns an empty map when the object has no attribution entries. Pass
 * `attribution` as `undefined` for the legacy-state case.
 */
export function buildGrantedKeywordSources(
  attribution: ObjectAttribution | undefined,
  objectId: ObjectId,
  deref: AttributionDeref,
): Map<string, string> {
  const result = new Map<string, string>();
  for (const r of resolveLayerAttributions(
    attribution,
    "Ability",
    objectId,
    deref,
  )) {
    const mod = r.modification;
    if (mod.type !== "AddKeyword") continue;
    const keyword = (mod as { type: "AddKeyword"; keyword: Keyword }).keyword;
    const name = getKeywordName(keyword);
    if (!result.has(name)) {
      result.set(name, r.sourceName);
    }
  }
  return result;
}

/**
 * One source's contribution to an object's P/T, aggregated across all
 * modifications that source provided in CR 613 layer 7c (ModifyPT). Static
 * anthems typically emit `AddPower{+1}` and `AddToughness{+1}` as two
 * modifications on the same `StaticDefinition`; we sum them per source so
 * the display shows "+1/+1 from Lord" rather than two separate entries.
 *
 * Dynamic `AddDynamicPower` / `AddDynamicToughness` are intentionally
 * omitted — their resolved per-target delta isn't surfaced in attribution
 * (would require re-resolving the quantity expression FE-side, which
 * violates the display-layer boundary). The static `Add{Power,Toughness}`
 * cases cover anthems, lords, and "+N/+N until end of turn" — the
 * overwhelming majority of P/T modifications.
 */
export interface PTContribution {
  sourceName: string;
  deltaPower: number;
  deltaToughness: number;
}

export function buildPTSources(
  attribution: ObjectAttribution | undefined,
  objectId: ObjectId,
  deref: AttributionDeref,
): PTContribution[] {
  const bySource = new Map<
    ObjectId,
    { sourceName: string; deltaPower: number; deltaToughness: number }
  >();
  for (const r of resolveLayerAttributions(
    attribution,
    "ModifyPT",
    objectId,
    deref,
  )) {
    const mod = r.modification;
    let dp = 0;
    let dt = 0;
    if (mod.type === "AddPower") {
      dp = (mod as { type: "AddPower"; value: number }).value;
    } else if (mod.type === "AddToughness") {
      dt = (mod as { type: "AddToughness"; value: number }).value;
    } else {
      continue;
    }
    const cur = bySource.get(r.sourceId) ?? {
      sourceName: r.sourceName,
      deltaPower: 0,
      deltaToughness: 0,
    };
    cur.deltaPower += dp;
    cur.deltaToughness += dt;
    bySource.set(r.sourceId, cur);
  }
  return [...bySource.values()].filter(
    (c) => c.deltaPower !== 0 || c.deltaToughness !== 0,
  );
}

/**
 * Formats a single contribution as "+N/+M" (signed). The two parts may
 * have different signs (e.g. a "-1/+1" tradeoff effect) so each component
 * is formatted independently.
 */
export function formatPTDelta(c: PTContribution): string {
  const fmt = (n: number) => (n >= 0 ? `+${n}` : String(n));
  return `${fmt(c.deltaPower)}/${fmt(c.deltaToughness)}`;
}
