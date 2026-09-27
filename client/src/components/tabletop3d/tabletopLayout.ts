import type { ObjectId, PlayerId } from "../../adapter/types.ts";
import type { GroupedPermanent } from "../../viewmodel/battlefieldProps.ts";
import type { PlayerBattlefieldView } from "../../viewmodel/gameStateView.ts";
import { TABLETOP_COLLAPSED_PERMANENT_DEPTH_RATIO } from "./tabletopCardCollapse.ts";

export type TabletopSeat = "local" | "far" | "left" | "right";
export type TabletopTableLayout = "duel" | "pod";
export type TabletopViewportLayout = "wide" | "compact";
export type TabletopLane = "creatures" | "support" | "lands";

/** Tabletop-style duplicate land piles show physical cards before using a count chip. */
export const TABLETOP_LAND_PILE_VISIBLE_LIMIT = 4;

export interface TabletopLandPileLayerTransform {
  x: number;
  y: number;
  z: number;
  rotation: number;
}

const TABLETOP_LAND_PILE_LAYER_TRANSFORMS:
  readonly TabletopLandPileLayerTransform[] = [
    { x: -0.052, y: -0.008, z: 0.056, rotation: -0.018 },
    { x: 0.076, y: -0.016, z: 0.112, rotation: 0.027 },
    { x: -0.104, y: -0.024, z: 0.168, rotation: -0.036 },
  ];

/**
 * Shared-scale offsets that make a land pile legible without widening its
 * reserved lane footprint.
 */
export function tabletopLandPileLayerTransforms(
  layerCount: number,
): readonly TabletopLandPileLayerTransform[] {
  return TABLETOP_LAND_PILE_LAYER_TRANSFORMS
    .slice(0, Math.max(0, Math.floor(layerCount)))
    .reverse();
}

export interface TabletopPilePresentation {
  visibleCardCount: number;
  badgeText: string | null;
}

/**
 * Converts an engine-authored group count into 3D presentation only. Land
 * piles expose as many as four physical layers; the badge counts only copies
 * beyond those visible layers. Other permanent groups keep their compact
 * total-count treatment.
 */
export function tabletopPilePresentation(
  lane: TabletopLane,
  pileCount: number,
): TabletopPilePresentation {
  const count = Math.max(1, Math.floor(pileCount));
  if (lane !== "lands") {
    return {
      visibleCardCount: 1,
      badgeText: count > 1 ? `×${count}` : null,
    };
  }

  const visibleCardCount = Math.min(
    count,
    TABLETOP_LAND_PILE_VISIBLE_LIMIT,
  );
  const overflowCount = count - visibleCardCount;
  return {
    visibleCardCount,
    badgeText: overflowCount > 0 ? `+${overflowCount}` : null,
  };
}

export const TABLETOP_CARD_WIDTH = 1.3;
export const TABLETOP_CARD_DEPTH = 1.82;
/** Library, graveyard, and exile piles are supporting table furniture, so their
 * visible cards sit slightly below the battlefield card scale. */
export const TABLETOP_ZONE_PILE_VISUAL_SCALE = 0.9;
export const TABLETOP_TAPPED_CARD_FOOTPRINT =
  (
    TABLETOP_CARD_WIDTH
    + TABLETOP_CARD_DEPTH * TABLETOP_COLLAPSED_PERMANENT_DEPTH_RATIO
  )
  / Math.SQRT2;

/** Keep the world-space opponent fan readable at the normal seven-card baseline. */
export const TABLETOP_MAX_VISIBLE_HELD_CARDS = 7;

export function tabletopVisibleHeldCardCount(totalCount: number): number {
  return Math.min(Math.max(totalCount, 0), TABLETOP_MAX_VISIBLE_HELD_CARDS);
}

/** Places public command-zone cards immediately after a concealed hand fan. */
export function tabletopHeldCommanderRow(
  visibleHandCount: number,
  commanderCount: number,
): TabletopHeldCardTransform[] {
  if (commanderCount <= 0) return [];
  const handFan = tabletopHeldCardFan(visibleHandCount);
  const handRightEdge = handFan[handFan.length - 1]?.x ?? 0;
  const startX = handRightEdge + TABLETOP_CARD_WIDTH * 0.92;
  const step = TABLETOP_CARD_WIDTH * 0.78;

  return Array.from({ length: commanderCount }, (_, index) => ({
    x: startX + index * step,
    y: 0.04,
    z: 0.22 + index * 0.012,
    rotationZ: 0,
    scale: 0.84,
  }));
}

export interface TabletopSeatAssignment {
  playerId: PlayerId;
  seat: Exclude<TabletopSeat, "local">;
}

export interface TabletopPlacement {
  objectId: ObjectId;
  pileCount: number;
  lane: TabletopLane;
  position: [number, number, number];
  faceAngle: number;
  attackVector: [number, number];
  cardScale: number;
}

interface TabletopAttachmentViewLike {
  cards: readonly { objectId: ObjectId }[];
}

const ATTACHMENT_DEPTH_STRIDE = 0.34;
const ATTACHMENT_RISE_STRIDE = 0.11;
const ATTACHMENT_MAX_DEPTH_SPAN = 1.36;
const ATTACHMENT_MAX_RISE = 0.44;
const ATTACHMENT_LATERAL_SWAY = 0.035;
const ATTACHMENT_ANGLE_SWAY = 0.035;

export interface TabletopZoneLayout {
  faceAngle: number;
  library: [number, number, number];
  graveyard: [number, number, number];
  exile: [number, number, number];
}

export interface TabletopHeldHandLayout {
  position: [number, number, number];
  faceAngle: number;
  scale: number;
}

export interface TabletopHeldCardTransform {
  x: number;
  y: number;
  z: number;
  rotationZ: number;
  scale: number;
}

export interface TabletopLaneZoneLayout {
  lane: TabletopLane;
  position: [number, number, number];
  faceAngle: number;
  width: number;
  depth: number;
}

interface TabletopSeatFrame {
  faceAngle: number;
  attackVector: [number, number];
  centers: Record<TabletopLane, [number, number]>;
  widths: Record<TabletopLane, number>;
}

const CARDINAL_SIDE_SEAT_ANGLE = Math.PI / 2;
const LANE_DEPTH = 1.98;
const LANE_EDGE_PADDING = 0.08;
const CARD_GAP = 0.12;
const CARD_OVERLAP_STRIDE_RATIO = 0.72;
const SIDE_ROW_SPLIT = 1.7;
const SIDE_CREATURE_CENTER_X = 3.5;
const SIDE_BACK_ROW_X = 5.62;
const SIDE_PILE_ROW_X = 7.65;
const LOCAL_BACK_ROW_Z = 2.75;
const POD_SIDE_HAND_X = 8.15;
const POD_HAND_CENTERLINE_Z = 0;
const POD_FAR_HAND_Z = -5.05;
const OPPONENT_HAND_SCALE = 0.9;
const ZONE_PILE_GAP = 1.45;
const LOCAL_ZONE_Z = 4.85;
const FAR_DUEL_ZONE_Z = -5.2;
const FAR_POD_ZONE_Z = -6.15;

const DUEL_WIDTHS: Record<TabletopLane, number> = {
  creatures: 9.4,
  support: 4,
  lands: 4,
};

const POD_LOCAL_WIDTHS: Record<TabletopLane, number> = {
  creatures: 4.6,
  support: 2.3,
  lands: 2.3,
};

const POD_SIDE_WIDTHS: Record<TabletopLane, number> = {
  creatures: 4.2,
  support: 3,
  lands: 3,
};

export function layoutTabletopSeat(
  view: PlayerBattlefieldView,
  seat: TabletopSeat,
  tableLayout: TabletopTableLayout = "duel",
): TabletopPlacement[] {
  const frame = tabletopSeatFrame(seat, tableLayout);
  const support = [...view.support, ...view.planeswalkers, ...view.other];
  const lanes: readonly [GroupedPermanent[], TabletopLane][] = [
    [view.creatures, "creatures"],
    [support, "support"],
    [view.lands, "lands"],
  ];
  const occupiedLaneScales = lanes
    .filter(([groups]) => groups.length > 0)
    .map(
      ([groups, lane]) =>
        fitTabletopLaneCards(groups.length, frame.widths[lane]).cardScale,
    );
  const sharedCardScale = occupiedLaneScales.length > 0
    ? Math.min(...occupiedLaneScales)
    : 1;

  return lanes.flatMap(([groups, lane]) =>
    layoutLane(groups, lane, frame, sharedCardScale),
  );
}

/**
 * Expands engine-authored attachment trees into shallow, seat-relative 3D
 * staircases. Every child remains an independent placement and therefore an
 * independent interaction surface; this helper changes presentation only.
 */
export function expandTabletopAttachmentPlacements(
  hosts: readonly TabletopPlacement[],
  attachmentViews: Readonly<Record<number, TabletopAttachmentViewLike>>,
): TabletopPlacement[] {
  const expanded: TabletopPlacement[] = [];
  const placed = new Set<ObjectId>();

  const appendTree = (host: TabletopPlacement) => {
    if (placed.has(host.objectId)) return;
    placed.add(host.objectId);
    expanded.push(host);

    const attachmentIds = attachmentViews[host.objectId]?.cards.map(
      ({ objectId }) => objectId,
    ) ?? [];
    for (const attachment of layoutTabletopAttachmentStaircase(
      host,
      attachmentIds,
    )) {
      appendTree(attachment);
    }
  };

  for (const host of hosts) appendTree(host);
  return expanded;
}

/**
 * Steps attachments toward their controller's seat along the host card's local
 * depth axis while lifting each successive card. A small alternating sway
 * keeps the stack from reading as one thick rectangular block.
 */
export function layoutTabletopAttachmentStaircase(
  host: TabletopPlacement,
  attachmentIds: readonly ObjectId[],
): TabletopPlacement[] {
  if (attachmentIds.length === 0) return [];

  const depthStride = Math.min(
    ATTACHMENT_DEPTH_STRIDE,
    ATTACHMENT_MAX_DEPTH_SPAN / attachmentIds.length,
  ) * host.cardScale;
  const riseStride = Math.min(
    ATTACHMENT_RISE_STRIDE,
    ATTACHMENT_MAX_RISE / attachmentIds.length,
  ) * host.cardScale;
  const tangentX = Math.cos(host.faceAngle);
  const tangentZ = -Math.sin(host.faceAngle);
  const depthX = Math.sin(host.faceAngle);
  const depthZ = Math.cos(host.faceAngle);

  return attachmentIds.map((objectId, index) => {
    const level = index + 1;
    const swayDirection = index % 2 === 0 ? -1 : 1;
    const lateralOffset =
      swayDirection * ATTACHMENT_LATERAL_SWAY * host.cardScale;
    return {
      ...host,
      objectId,
      pileCount: 1,
      position: [
        host.position[0]
          + depthX * depthStride * level
          + tangentX * lateralOffset,
        host.position[1] + riseStride * level,
        host.position[2]
          + depthZ * depthStride * level
          + tangentZ * lateralOffset,
      ],
      faceAngle:
        host.faceAngle + swayDirection * ATTACHMENT_ANGLE_SWAY,
    };
  });
}

export function assignTabletopOpponentSeats(
  seatOrder: PlayerId[],
  perspectivePlayerId: PlayerId,
  liveOpponentIds: PlayerId[],
): TabletopSeatAssignment[] {
  const perspectiveIndex = seatOrder.indexOf(perspectivePlayerId);
  const stableOrder = perspectiveIndex < 0
    ? [perspectivePlayerId, ...liveOpponentIds]
    : [
        ...seatOrder.slice(perspectiveIndex),
        ...seatOrder.slice(0, perspectiveIndex),
      ];
  const relativeOpponents = stableOrder.filter(
    (playerId) => playerId !== perspectivePlayerId,
  );
  const liveOpponents = new Set(liveOpponentIds);
  const seats: Exclude<TabletopSeat, "local">[] =
    relativeOpponents.length <= 1
      ? ["far"]
      : relativeOpponents.length === 2
        ? ["left", "right"]
        : ["left", "far", "right"];

  return relativeOpponents.flatMap((playerId, index) => {
    const seat = seats[index];
    return seat && liveOpponents.has(playerId) ? [{ playerId, seat }] : [];
  });
}

export function tabletopZoneLayout(
  seat: TabletopSeat,
  tableLayout: TabletopTableLayout = "duel",
  viewportLayout: TabletopViewportLayout = "wide",
): TabletopZoneLayout {
  if (seat === "local") {
    if (tableLayout === "duel" && viewportLayout === "compact") {
      return tabletopZoneRow(0, [-1.45, 0.08, LOCAL_ZONE_Z]);
    }
    const libraryX = tableLayout === "pod" ? -6.35 : -5.55;
    return tabletopZoneRow(0, [libraryX, 0.08, LOCAL_ZONE_Z]);
  }
  if (seat === "far") {
    // The directly opposite player's piles belong behind their upright hand,
    // near the rear camera crop. Wide screens intentionally trim the outer
    // card edge instead of pulling this public zone into the play lanes.
    const libraryX = tableLayout === "pod"
      ? 6.2
      : viewportLayout === "compact"
        ? 1.45
        : 5.55;
    const rowZ = tableLayout === "pod"
      ? FAR_POD_ZONE_Z
      : FAR_DUEL_ZONE_Z;
    return tabletopZoneRow(Math.PI, [libraryX, 0.08, rowZ]);
  }
  if (seat === "left") {
    const faceAngle = -CARDINAL_SIDE_SEAT_ANGLE;
    return tabletopZoneRow(faceAngle, [-SIDE_PILE_ROW_X, 0.08, -4.3]);
  }
  const faceAngle = CARDINAL_SIDE_SEAT_ANGLE;
  return tabletopZoneRow(faceAngle, [SIDE_PILE_ROW_X, 0.08, 3]);
}

/**
 * Anchors concealed hands above the plane at each seat's outer edge. Fans are
 * intentionally large enough to read against their own libraries, then
 * partially cropped by the viewport like the opponent hand in Magic Tabletop.
 */
export function tabletopHeldHandLayout(
  seat: TabletopSeat,
  tableLayout: TabletopTableLayout = "duel",
  viewportLayout: TabletopViewportLayout = "wide",
): TabletopHeldHandLayout {
  if (seat === "local") {
    return {
      position: [0, 0.1, tableLayout === "pod" ? 6.1 : 5.25],
      faceAngle: Math.PI,
      scale: tableLayout === "pod" ? 0.84 : 0.92,
    };
  }
  if (seat === "far") {
    if (viewportLayout === "compact") {
      return {
        position: [
          0,
          0.48,
          tableLayout === "pod" ? -4.45 : -4.15,
        ],
        faceAngle: 0,
        scale: OPPONENT_HAND_SCALE,
      };
    }
    return {
      position: [
        0,
        tableLayout === "pod" ? 1.02 : 0.98,
        tableLayout === "pod" ? POD_FAR_HAND_Z : -4.75,
      ],
      faceAngle: 0,
      scale: OPPONENT_HAND_SCALE,
    };
  }
  if (seat === "left") {
    return {
      position: [-POD_SIDE_HAND_X, 1, POD_HAND_CENTERLINE_Z],
      faceAngle: Math.PI / 2,
      scale: OPPONENT_HAND_SCALE,
    };
  }
  return {
    position: [POD_SIDE_HAND_X, 1, POD_HAND_CENTERLINE_Z],
    faceAngle: -Math.PI / 2,
    scale: OPPONENT_HAND_SCALE,
  };
}

/** Broad, bottom-pivoted fan geometry that keeps even large hands legible. */
export function tabletopHeldCardFan(count: number): TabletopHeldCardTransform[] {
  if (count <= 0) return [];
  const center = (count - 1) / 2;
  const step = count === 1 ? 0 : Math.min(0.38, 3.6 / (count - 1));
  const scale = count > 16 ? 0.68 : count > 11 ? 0.78 : 0.88;

  return Array.from({ length: count }, (_, index) => {
    const centeredIndex = index - center;
    const normalized = center === 0 ? 0 : centeredIndex / center;
    return {
      x: centeredIndex * step,
      y: (1 - Math.abs(normalized)) * 0.13,
      z: index * 0.012,
      rotationZ: -normalized * 0.24,
      scale,
    };
  });
}

function tabletopZoneRow(
  faceAngle: number,
  library: [number, number, number],
): TabletopZoneLayout {
  const rightX = Math.cos(faceAngle);
  const rightZ = -Math.sin(faceAngle);
  const offset = (distance: number): [number, number, number] => [
    library[0] + rightX * distance,
    library[1],
    library[2] + rightZ * distance,
  ];
  return {
    faceAngle,
    library,
    graveyard: offset(ZONE_PILE_GAP),
    exile: offset(ZONE_PILE_GAP * 2),
  };
}

export function tabletopLaneZoneLayouts(
  seat: TabletopSeat,
  tableLayout: TabletopTableLayout = "duel",
): TabletopLaneZoneLayout[] {
  const frame = tabletopSeatFrame(seat, tableLayout);
  const lanes: TabletopLane[] = ["creatures", "support", "lands"];
  return lanes.map((lane) => ({
    lane,
    position: [
      frame.centers[lane][0],
      0.018,
      frame.centers[lane][1],
    ],
    faceAngle: frame.faceAngle,
    width: frame.widths[lane],
    depth: LANE_DEPTH,
  }));
}

function layoutLane(
  groups: GroupedPermanent[],
  lane: TabletopLane,
  frame: TabletopSeatFrame,
  cardScaleLimit = 1,
): TabletopPlacement[] {
  const fit = fitTabletopLaneCards(
    groups.length,
    frame.widths[lane],
    cardScaleLimit,
  );
  const [centerX, centerZ] = frame.centers[lane];
  const tangentX = Math.cos(frame.faceAngle);
  const tangentZ = -Math.sin(frame.faceAngle);
  return groups.map((group, index) => ({
    objectId: group.ids[0],
    pileCount: group.count,
    lane,
    position: [
      centerX + tangentX * fit.offsets[index],
      // Cards hover above the stone rather than lying on it: enough clearance
      // for the sun to drop a readable shadow beside each permanent. A tiny,
      // deterministic rise also resolves depth ordering when tapped cards must
      // overlap on crowded lanes instead of letting their faces z-fight.
      0.16 + index * 0.003,
      centerZ + tangentZ * fit.offsets[index],
    ],
    faceAngle: frame.faceAngle,
    attackVector: frame.attackVector,
    cardScale: fit.cardScale,
  }));
}

export function fitTabletopLaneCards(
  count: number,
  laneWidth: number,
  cardScaleLimit = 1,
): { offsets: number[]; cardScale: number; gap: number } {
  if (count <= 0) {
    return { offsets: [], cardScale: 1, gap: 0 };
  }

  const innerWidth = Math.max(laneWidth - LANE_EDGE_PADDING * 2, 0);
  const depthScale = Math.max(
    (LANE_DEPTH - LANE_EDGE_PADDING * 2) / TABLETOP_CARD_DEPTH,
    0,
  );
  const gap =
    count === 1
      ? 0
      : Math.min(CARD_GAP, innerWidth / (count * 4));
  // Tabletop keeps sparse and moderately busy boards at a readable scale, then
  // progressively overlaps cards before resorting to aggressive shrinking.
  // Using the compact face width as the layout footprint also avoids making
  // every untapped card pay for a hypothetical 90-degree tap rotation.
  const fullScaleFootprint =
    TABLETOP_TAPPED_CARD_FOOTPRINT
    + (count - 1) * TABLETOP_CARD_WIDTH * CARD_OVERLAP_STRIDE_RATIO;
  const naturalScale = Math.max(
    innerWidth / fullScaleFootprint,
    0,
  );
  const crowdedFloor = count > 18 ? 0.7 : 0.82;
  const crowdedScale = Math.max(crowdedFloor, naturalScale);
  // Keep sparse boards at the shared full-card scale. From the fourth through
  // the eighth group, ease toward the crowded fit instead of snapping directly
  // to the floor when a narrow back row receives its next permanent.
  const densityProgress = Math.min(Math.max((count - 3) / 5, 0), 1);
  const densityScale = 1 + (crowdedScale - 1) * densityProgress;
  const cardScale = Math.min(
    1,
    depthScale,
    Math.max(0, cardScaleLimit),
    densityScale,
  );
  const naturalStride = TABLETOP_CARD_WIDTH * cardScale + gap;
  const centerSpan = Math.max(
    innerWidth - TABLETOP_TAPPED_CARD_FOOTPRINT * cardScale,
    0,
  );
  const stride = count === 1
    ? 0
    : Math.min(naturalStride, centerSpan / (count - 1));
  const start = -((count - 1) * stride) / 2;
  return {
    offsets: Array.from(
      { length: count },
      (_, index) => start + index * stride,
    ),
    cardScale,
    gap,
  };
}

function tabletopSeatFrame(
  seat: TabletopSeat,
  tableLayout: TabletopTableLayout,
): TabletopSeatFrame {
  if (tableLayout === "pod") {
    return podSeatFrame(seat);
  }
  if (seat === "local") {
    return {
      faceAngle: 0,
      attackVector: [0, -1],
      centers: {
        creatures: [0, 1.02],
        support: [2.4, LOCAL_BACK_ROW_Z],
        lands: [-2.4, LOCAL_BACK_ROW_Z],
      },
      widths: DUEL_WIDTHS,
    };
  }
  if (seat === "far") {
    const centers: Record<TabletopLane, [number, number]> = {
      creatures: [0, -1.9],
      support: [2.4, -4.2],
      lands: [-2.4, -4.2],
    };
    return {
      faceAngle: Math.PI,
      attackVector: [0, 1],
      centers,
      widths: DUEL_WIDTHS,
    };
  }
  return podSeatFrame(seat);
}

function podSeatFrame(seat: TabletopSeat): TabletopSeatFrame {
  if (seat === "local") {
    return {
      faceAngle: 0,
      attackVector: [0, -1],
      centers: {
        creatures: [0, 0.5],
        support: [1.22, LOCAL_BACK_ROW_Z],
        lands: [-1.22, LOCAL_BACK_ROW_Z],
      },
      widths: POD_LOCAL_WIDTHS,
    };
  }
  if (seat === "far") {
    const centers: Record<TabletopLane, [number, number]> = {
      creatures: [0, -2.42],
      support: [-1.22, -4.55],
      lands: [1.22, -4.55],
    };
    return {
      faceAngle: Math.PI,
      attackVector: [0, 1],
      centers,
      widths: POD_LOCAL_WIDTHS,
    };
  }
  return sideSeatFrame(seat, CARDINAL_SIDE_SEAT_ANGLE);
}

function sideSeatFrame(
  seat: Extract<TabletopSeat, "left" | "right">,
  sideAngle: number,
): TabletopSeatFrame {
  const side = seat === "left" ? -1 : 1;
  const faceAngle = side * sideAngle;
  const backRowX = side * SIDE_BACK_ROW_X;
  const centers: Record<TabletopLane, [number, number]> = {
    creatures: [side * SIDE_CREATURE_CENTER_X, 0],
    support: [backRowX, -side * SIDE_ROW_SPLIT],
    lands: [backRowX, side * SIDE_ROW_SPLIT],
  };
  return {
    faceAngle,
    attackVector: [-Math.sin(faceAngle), -Math.cos(faceAngle)],
    centers,
    widths: POD_SIDE_WIDTHS,
  };
}

export function spreadPositions(count: number, availableWidth: number): number[] {
  if (count <= 0) return [];
  if (count === 1) return [0];
  const gap = Math.min(2.02, availableWidth / (count - 1));
  const start = -((count - 1) * gap) / 2;
  return Array.from({ length: count }, (_, index) => start + index * gap);
}
