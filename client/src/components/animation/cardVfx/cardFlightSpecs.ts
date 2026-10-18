import type { GameState, ManaColor, ObjectId, PlayerId, Zone } from "../../../adapter/types.ts";
import { resolvingEntry } from "../../../animation/damageCause.ts";
import type { AnimationEvent } from "../../../animation/types.ts";
import {
  type AnimationImageSnapshot,
  visibleAnimationImageSnapshot,
} from "../ResolvedAnimationImage.tsx";

/** The zones a card flight can land in. Every per-destination table is keyed
 *  by this, so a new destination is a compile error until each has its entry. */
export type FlightDestination = Extract<Zone, "Stack" | "Battlefield" | "Graveyard" | "Hand" | "Library" | "Exile">;

/** Where a card flight goes: from the card's surface in one zone to its own
 *  surface in another. `ownerId` locates per-player surfaces (hand, exile; library and
 *  graveyard unless the format shares them), which belong to the card's owner whoever controls it. */
export interface CardFlightRoute {
  from: Zone;
  to: FlightDestination;
  ownerId: PlayerId;
}

/** The zone changes a card flight presents, by origin. A move between zones
 *  that already have surfaces needs only its entry here. Casting is not a zone
 *  change event: `castOf` routes it. A permanent leaving the battlefield for a
 *  graveyard is not either: its destruction breaks it where it lies, while
 *  sacrifice and exile dissolve. */
const FLIGHT_ZONE_CHANGES: { readonly [From in Zone]?: readonly FlightDestination[] } = {
  // CR 608.2n / CR 608.3: a resolving spell goes to the battlefield or its
  // owner's graveyard; a countered or bounced one to its hand, library or exile.
  Stack: ["Battlefield", "Graveyard", "Hand", "Library", "Exile"],
  // CR 121.1: a drawn card moves from the library to the hand; CR 701.17a: a
  // milled one to the graveyard. Effects also exile or put cards onto the battlefield.
  Library: ["Hand", "Graveyard", "Exile", "Battlefield"],
  // CR 305.1: a played land (or a card an effect puts onto the battlefield)
  // moves from the hand to the battlefield; CR 701.9a: a discarded card to the graveyard.
  Hand: ["Battlefield", "Graveyard", "Library", "Exile"],
  Graveyard: ["Hand", "Battlefield", "Library", "Exile"],
  Exile: ["Hand", "Battlefield", "Graveyard", "Library"],
  Battlefield: ["Hand", "Library"],
};

/** Whether a card flight presents a move from `from` to `to`. */
export function flightPresents(from: Zone, to: Zone): boolean {
  return FLIGHT_ZONE_CHANGES[from]?.some((destination) => destination === to) ?? false;
}

/** How a counter's ripple looks: water from a blue source, otherwise a pale
 *  disturbance with no water in it. */
export type RippleLook = "water" | "pale";

export function rippleLookFor(colors: readonly ManaColor[]): RippleLook {
  return colors.includes("Blue") ? "water" : "pale";
}

/** A counter's ripple travels to the spell it counters in this long, before pace. */
export const COUNTER_RIPPLE_MS = 380;
/** The countered spell washes out over this long once the ripple reaches it,
 *  before pace; then it leaves the stack. */
export const COUNTER_WASH_MS = 300;

/** How long after its ripple sets out a counter and the spell it counters
 *  may leave the stack: once the countered spell has washed out. */
export const counterLeaveMs = (pace: number) => (COUNTER_RIPPLE_MS + COUNTER_WASH_MS) * pace;

/** A countered spell washing out where it lies before it leaves. */
export interface WashSpec {
  /** When the wash starts, after the flight is presented, already paced. */
  atMs: number;
  /** How long it takes to wash out, already paced. */
  durationMs: number;
  look: RippleLook;
}

/** How a front sweeping the board looks: a wave for a blue spell returning
 *  permanents to hand, light or smoke for a white or black one destroying them. */
export type SweepLook = "tsunami" | "light" | "smoke";

/** A front the resolving spell sends across the board, reaching each of the
 *  permanents it moves or destroys together in turn. */
export interface SweepSpec {
  look: SweepLook;
  /** The spell's controller, from whose side the front comes. */
  casterId: PlayerId;
  /** Every permanent the front reaches, this one among them. */
  memberIds: readonly ObjectId[];
}

/** CR 608.1 + CR 608.2c: the spell on top of the stack resolves, and its
 *  controller follows its instructions; a front sweeps the permanents one of
 *  them moves or destroys together, when there are several and the spell's
 *  colours give it a look. */
export function sweepOf(
  memberIds: readonly ObjectId[],
  lookFor: (colors: readonly ManaColor[]) => SweepLook | null,
  pre: GameState | null,
): SweepSpec | null {
  if (memberIds.length < 2 || !pre) return null;
  const entry = resolvingEntry(pre);
  const source = entry && pre.objects[entry.source_id];
  const look = source ? lookFor(source.color) : null;
  return entry && look ? { look, casterId: entry.controller, memberIds } : null;
}

const tsunamiLook = (colors: readonly ManaColor[]): SweepLook | null => (colors.includes("Blue") ? "tsunami" : null);

/** A blue spell returning several permanents to their owners' hands at once
 *  sends a wave over them. */
function tsunamiOf(route: CardFlightRoute, { pre, stepEvents }: CardFlightSpecContext): SweepSpec | null {
  if (route.from !== "Battlefield" || route.to !== "Hand") return null;
  const members = stepEvents.flatMap((event) =>
    event.type === "ZoneChanged" && event.data.from === "Battlefield" && event.data.to === "Hand"
      ? [event.data.object_id]
      : [],
  );
  return sweepOf(members, tsunamiLook, pre);
}

/** The most flights one batch sends (a mass mill or graveyard exile); the
 *  rest present Classic. */
export const FLIGHT_BATCH_MAX = 12;

export interface CardFlightSpec {
  kind: "flight";
  objectId: ObjectId;
  /** The object whose surface the flight leaves from: the card itself, or the
   *  source a token comes out of. */
  sourceId: ObjectId;
  route: CardFlightRoute;
  /** The face the viewer may see before the event, from the engine's
   *  per-viewer visibility; `null` shows the card back. */
  startFace: AnimationImageSnapshot | null;
  /** The face the viewer may see after the event; `null` shows the card back. */
  endFace: AnimationImageSnapshot | null;
  /** The engine's colours for the card after the event, or `null` when the
   *  viewer may not see its face. */
  endColors: readonly ManaColor[] | null;
  /** The animation-speed multiplier every flight duration is scaled by. */
  pace: number;
  /** The owning step's scaled duration, which bounds face readiness. */
  owningStepMs: number;
  /** The snapshot the owning step animates (`QueuedStep.snapshotSeq`). */
  snapshotSeq: number;
  /** How long after it is presented this flight leaves: draws in one step
   *  leave one after another, all inside the step's first half. */
  delayMs: number;
  /** A countered spell's wash before it leaves; `null` for any other flight. */
  wash: WashSpec | null;
  /** The front that sets the card off when it reaches it, after `delayMs`;
   *  `null` when none does. */
  sweep: SweepSpec | null;
}

export interface CardFlightSpecContext {
  /** The committed state the event starts from. */
  pre: GameState | null;
  /** The state the event produces. */
  post: GameState | null;
  pace: number;
  owningStepMs: number;
  /** The snapshot the owning step animates (`QueuedStep.snapshotSeq`). */
  snapshotSeq: number;
  /** Every event in the owning step, in order. */
  stepEvents: readonly AnimationEvent[];
}

/** The gap between consecutive flights of one zone change in a step, before pace. */
export const FLIGHT_STAGGER_MS = 90;

interface RoutedObject {
  objectId: ObjectId;
  sourceId: ObjectId;
  route: CardFlightRoute;
}

/** CR 601.2a: casting moves the card from where it is to the stack. The event
 *  announcing the cast (or, for a cast that completes in one step, the cast
 *  itself) starts the flight from the zone the card is cast from. */
function castOf(objectId: ObjectId, pre: GameState | null, post: GameState | null): RoutedObject | null {
  const from = pre?.objects[objectId]?.zone;
  const owner = post?.objects[objectId]?.owner;
  return from !== undefined && owner !== undefined
    ? { objectId, sourceId: objectId, route: { from, to: "Stack", ownerId: owner } }
    : null;
}

function zoneChangeOf(event: AnimationEvent, post: GameState | null): RoutedObject | null {
  if (event.type !== "ZoneChanged" || event.data.from === null) return null;
  const { object_id: objectId, from } = event.data;
  const to = FLIGHT_ZONE_CHANGES[from]?.find((destination) => destination === event.data.to);
  const owner = post?.objects[objectId]?.owner;
  return to && owner !== undefined ? { objectId, sourceId: objectId, route: { from, to, ownerId: owner } } : null;
}

/** CR 111.1: an effect puts a token onto the battlefield; it comes out of the
 *  source that created it, when that source is a spell on the stack or a
 *  permanent. */
function tokenOf(objectId: ObjectId, sourceId: ObjectId, pre: GameState | null, post: GameState | null): RoutedObject | null {
  const from = pre?.objects[sourceId]?.zone;
  const owner = post?.objects[objectId]?.owner;
  return (from === "Stack" || from === "Battlefield") && owner !== undefined
    ? { objectId, sourceId, route: { from, to: "Battlefield", ownerId: owner } }
    : null;
}

/** CR 601.2a: whether a spell was announced in an earlier batch whose cast
 *  paused for a choice; it went to the stack at its announcement. */
export function castAnnounced(objectId: ObjectId, pre: GameState | null): boolean {
  return pre?.stack.some((entry) => entry.id === objectId) ?? false;
}

function routedObjectFor(event: AnimationEvent, pre: GameState | null, post: GameState | null): RoutedObject | null {
  switch (event.type) {
    case "SpellCast":
      return castAnnounced(event.data.object_id, pre) ? null : castOf(event.data.object_id, pre, post);
    case "StackPushed":
      return castOf(event.data.object_id, pre, post);
    case "TokenCreated":
      return tokenOf(event.data.object_id, event.data.source_id, pre, post);
    default:
      return zoneChangeOf(event, post);
  }
}

/** The flights of one step that leave one after another: one zone change's
 *  cards, one step's tokens. */
function batchKey(event: AnimationEvent): string | null {
  switch (event.type) {
    case "ZoneChanged":
      return `${event.data.from}>${event.data.to}`;
    case "TokenCreated":
      return event.type;
    default:
      return null;
  }
}

/** Where `event` falls in its step's batch of flights (see `batchKey`). */
function batchIndex(event: AnimationEvent, stepEvents: readonly AnimationEvent[]): { index: number; size: number } {
  const key = batchKey(event);
  const batch = key === null ? [event] : stepEvents.filter((other) => batchKey(other) === key);
  return { index: Math.max(batch.indexOf(event), 0), size: batch.length };
}

/** When a flight leaves after its step starts. Flights of one batch in a step
 *  (a multi-card draw) leave one after another: the nth leaves n gaps in, and
 *  the gaps shrink so the last still leaves within the step's first half. */
function delayFor({ index, size }: { index: number; size: number }, pace: number, owningStepMs: number) {
  if (index === 0) return 0;
  const flights = Math.min(size, FLIGHT_BATCH_MAX);
  const gap = Math.min(FLIGHT_STAGGER_MS * pace, owningStepMs / 2 / (flights - 1));
  return index * gap;
}

/** CR 701.6a: a countered spell is removed from the stack and put into its
 *  owner's graveyard. Its counter's ripple reaches it and washes it out before
 *  it leaves. (The countering spell leaves once it has, CR 608.2n; the engine
 *  can report that move a step later, so the ripple carries its time.) `null`
 *  for a move from the stack that no counter in the step made. */
function counteredLeave(
  objectId: ObjectId,
  route: CardFlightRoute,
  { pre, pace, stepEvents }: CardFlightSpecContext,
): Pick<CardFlightSpec, "delayMs" | "wash"> | null {
  if (route.from !== "Stack") return null;
  for (const event of stepEvents) {
    if (event.type !== "SpellCountered" || event.data.object_id !== objectId) continue;
    const look = rippleLookFor(pre?.objects[event.data.countered_by]?.color ?? []);
    const wash = { atMs: COUNTER_RIPPLE_MS * pace, durationMs: COUNTER_WASH_MS * pace, look };
    return { delayMs: counterLeaveMs(pace), wash };
  }
  return null;
}

/** The card flight that presents `event`, or `null` when the event has no
 *  flight (it then presents Classic). A zero or negative pace has no flight,
 *  matching the step timers' instant mode. */
export function cardFlightSpecFor(event: AnimationEvent, context: CardFlightSpecContext): CardFlightSpec | null {
  const { pre, post, pace, owningStepMs, snapshotSeq, stepEvents } = context;
  if (pace <= 0) return null;
  const routed = routedObjectFor(event, pre, post);
  if (!routed) return null;
  const batch = batchIndex(event, stepEvents);
  if (batch.index >= FLIGHT_BATCH_MAX) return null;
  const { objectId } = routed;
  const endObject = post?.objects[objectId];
  const endFace = visibleAnimationImageSnapshot(endObject);
  // A token has no face before it exists; it comes out of its source already showing it.
  const startFace = routed.sourceId === objectId ? visibleAnimationImageSnapshot(pre?.objects[objectId]) : endFace;
  const sweep = tsunamiOf(routed.route, context);
  return {
    kind: "flight",
    ...routed,
    startFace,
    endFace,
    endColors: endFace && endObject ? endObject.color : null,
    pace,
    owningStepMs,
    snapshotSeq,
    // A swept card leaves when the front reaches it, not in its batch's turn.
    ...(counteredLeave(objectId, routed.route, context) ?? {
      delayMs: sweep ? 0 : delayFor(batch, pace, owningStepMs),
      wash: null,
    }),
    sweep,
  };
}
