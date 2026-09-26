import { useEffect, useMemo } from "react";
import { useThree } from "@react-three/fiber";
import * as THREE from "three";

import { usePerspectivePlayerId } from "../../hooks/usePlayerId.ts";
import {
  useAnimationStore,
  type CardMotionTarget,
} from "../../stores/animationStore.ts";
import {
  buildPlayerBattlefieldView,
  getOpponentIds,
  getSeatCount,
} from "../../viewmodel/gameStateView.ts";
import { projectedCardMotionTarget } from "../animation/cardMotion.ts";
import {
  TABLETOP_CARD_DEPTH,
  TABLETOP_CARD_WIDTH,
  tabletopZoneLayout,
  assignTabletopOpponentSeats,
  layoutTabletopSeat,
  type TabletopPlacement,
  type TabletopSeat,
  type TabletopTableLayout,
} from "./tabletopLayout.ts";

interface ProjectedPose {
  position: [number, number, number];
  faceAngle: number;
  scale: number;
}

/**
 * Projects post-action engine placements into screen space while the old state
 * is still mounted. The transition overlay can therefore land the same card at
 * the exact pose where TabletopPermanent or a zone pile will appear on commit.
 */
export function TabletopFlightDestinations() {
  const nextState = useAnimationStore((state) => state.animationNewState);
  const perspectivePlayerId = usePerspectivePlayerId();
  const { camera, gl, size } = useThree();
  const projection = useMemo(() => {
    if (!nextState) return null;
    const opponents = getOpponentIds(nextState, perspectivePlayerId);
    const seatOrder =
      nextState.seat_order
      ?? nextState.players.map((player) => player.id);
    const tableLayout: TabletopTableLayout =
      getSeatCount(nextState) > 2 ? "pod" : "duel";
    const opponentSeats = assignTabletopOpponentSeats(
      seatOrder,
      perspectivePlayerId,
      opponents,
    );
    const seatByPlayer = new Map<number, TabletopSeat>([
      [perspectivePlayerId, "local"],
      ...opponentSeats.map(
        ({ playerId, seat }) => [playerId, seat] as const,
      ),
    ]);
    const placements = [
      ...layoutTabletopSeat(
        buildPlayerBattlefieldView(nextState, perspectivePlayerId),
        "local",
        tableLayout,
      ),
      ...opponentSeats.flatMap(({ playerId, seat }) =>
        layoutTabletopSeat(
          buildPlayerBattlefieldView(nextState, playerId),
          seat,
          tableLayout,
        )
      ),
    ];

    return { placements, seatByPlayer, tableLayout };
  }, [nextState, perspectivePlayerId]);

  useEffect(() => {
    if (!nextState || !projection) {
      useAnimationStore.getState().setCardMotionDestinations(
        new Map(),
        new Map(),
      );
      return;
    }

    camera.updateMatrixWorld();
    const canvasRect = gl.domElement.getBoundingClientRect();
    const project = (point: THREE.Vector3) => {
      const projected = point.clone().project(camera);
      return {
        x: canvasRect.left + (projected.x + 1) * canvasRect.width / 2,
        y: canvasRect.top + (1 - projected.y) * canvasRect.height / 2,
      };
    };
    const projectPose = (pose: ProjectedPose) => {
      const [x, y, z] = pose.position;
      const center = new THREE.Vector3(x, y, z);
      const right = new THREE.Vector3(
        Math.cos(pose.faceAngle),
        0,
        -Math.sin(pose.faceAngle),
      );
      const down = new THREE.Vector3(
        Math.sin(pose.faceAngle),
        0,
        Math.cos(pose.faceAngle),
      );
      const halfWidth = TABLETOP_CARD_WIDTH * pose.scale / 2;
      const halfDepth = TABLETOP_CARD_DEPTH * pose.scale / 2;

      return projectedCardMotionTarget({
        center: project(center),
        left: project(center.clone().addScaledVector(right, -halfWidth)),
        right: project(center.clone().addScaledVector(right, halfWidth)),
        top: project(center.clone().addScaledVector(down, -halfDepth)),
        bottom: project(center.clone().addScaledVector(down, halfDepth)),
      });
    };
    const cards = new Map(
      projection.placements.map((placement) => [
        placement.objectId,
        projectPose(arrivalPose(placement)),
      ]),
    );
    const zones = new Map<string, CardMotionTarget>();
    const viewportLayout =
      size.width / Math.max(size.height, 1) < 1 ? "compact" : "wide";
    for (const [playerId, seat] of projection.seatByPlayer) {
      const layout = tabletopZoneLayout(
        seat,
        projection.tableLayout,
        viewportLayout,
      );
      for (const zone of ["library", "graveyard", "exile"] as const) {
        zones.set(
          `${playerId}:${zone}`,
          projectPose({
            position: [
              layout[zone][0],
              layout[zone][1] + 0.05,
              layout[zone][2],
            ],
            faceAngle: layout.faceAngle,
            scale: 1,
          }),
        );
      }
    }

    useAnimationStore.getState().setCardMotionDestinations(cards, zones);
  }, [camera, gl, nextState, projection, size.height, size.width]);

  return null;
}

function arrivalPose(placement: TabletopPlacement): ProjectedPose {
  return {
    position: [
      placement.position[0] + placement.attackVector[0] * 0.14,
      placement.position[1] + 0.42,
      placement.position[2] + placement.attackVector[1] * 0.14,
    ],
    faceAngle: placement.faceAngle,
    scale: placement.cardScale * 0.86,
  };
}
