import { memo, useCallback, useEffect, useMemo, useState } from "react";
import { Canvas, useThree } from "@react-three/fiber";
import * as THREE from "three";

import type { ObjectId, PlayerId } from "../../adapter/types.ts";
import { usePerspectivePlayerId } from "../../hooks/usePlayerId.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import {
  buildPlayerBattlefieldView,
  getOpponentIds,
  getSeatCount,
} from "../../viewmodel/gameStateView.ts";
import { TabletopHeldHand } from "./TabletopHeldHand.tsx";
import { TabletopCardDetailOverlay } from "./TabletopCardDetailOverlay.tsx";
import { TabletopFlightDestinations } from "./TabletopFlightDestinations.tsx";
import { TabletopMaterialPlane } from "./TabletopMaterialPlane.tsx";
import { TabletopPermanent } from "./TabletopPermanent.tsx";
import { TabletopZonePiles } from "./TabletopZonePiles.tsx";
import { tabletopAttackStrikeTargets } from "./tabletopAttackAnimation.ts";
import {
  assignTabletopOpponentSeats,
  expandTabletopAttachmentPlacements,
  layoutTabletopSeat,
  type TabletopSeatAssignment,
  type TabletopTableLayout,
} from "./tabletopLayout.ts";

const TABLETOP_CAMERA_FOV = 32;

interface TabletopGameBoardProps {
  renderOpponentHud?: (
    seats: readonly TabletopSeatAssignment[],
  ) => React.ReactNode;
  playerHud?: React.ReactNode;
  showOpponentCards?: boolean;
  onKickPlayer?: (playerId: PlayerId) => void;
  onViewZone?: (
    zone: "graveyard" | "exile" | "library",
    playerId: PlayerId,
  ) => void;
}

/**
 * Three-dimensional gameplay renderer. The canvas owns spatial presentation while
 * the existing Phase HUD, hand, stack, dialogs, and dispatch pipeline remain
 * normal DOM layers around it.
 */
export const TabletopGameBoard = memo(function TabletopGameBoard(
  props: TabletopGameBoardProps,
) {
  const gameState = useGameStore((state) => state.gameState);
  const attachmentViews = useGameStore(
    (state) => state.viewerInteraction?.attachmentViews,
  );
  const dismissPreview = useUiStore((state) => state.dismissPreview);
  const [detailObjectId, setDetailObjectId] = useState<ObjectId | null>(null);
  const perspectivePlayerId = usePerspectivePlayerId();
  const opponents = useMemo(
    () => getOpponentIds(gameState, perspectivePlayerId),
    [gameState, perspectivePlayerId],
  );
  const seatOrder = useMemo(
    () =>
      gameState?.seat_order
      ?? gameState?.players.map((player) => player.id)
      ?? [],
    [gameState],
  );
  const tableLayout: TabletopTableLayout =
    getSeatCount(gameState) > 2 ? "pod" : "duel";
  const opponentSeats = useMemo(
    () =>
      assignTabletopOpponentSeats(
        seatOrder,
        perspectivePlayerId,
        opponents,
      ),
    [opponents, perspectivePlayerId, seatOrder],
  );
  const playerView = useMemo(
    () => buildPlayerBattlefieldView(gameState, perspectivePlayerId),
    [gameState, perspectivePlayerId],
  );
  const opponentViews = useMemo(
    () =>
      new Map(
        opponentSeats.map(({ playerId }) => [
          playerId,
          buildPlayerBattlefieldView(gameState, playerId),
        ]),
      ),
    [gameState, opponentSeats],
  );
  const placements = useMemo(() => {
    const hosts = [
      ...layoutTabletopSeat(playerView, "local", tableLayout),
      ...opponentSeats.flatMap(({ playerId, seat }) => {
        const view = opponentViews.get(playerId);
        return view ? layoutTabletopSeat(view, seat, tableLayout) : [];
      }),
    ];
    return expandTabletopAttachmentPlacements(hosts, attachmentViews ?? {});
  }, [
    attachmentViews,
    opponentSeats,
    opponentViews,
    playerView,
    tableLayout,
  ]);
  const attackStrikeTargets = useMemo(
    () =>
      tabletopAttackStrikeTargets(
        gameState?.combat?.attackers ?? [],
        placements,
        perspectivePlayerId,
        opponentSeats,
        tableLayout,
      ),
    [
      gameState?.combat?.attackers,
      opponentSeats,
      perspectivePlayerId,
      placements,
      tableLayout,
    ],
  );
  const showCardDetails = useCallback(
    (objectId: ObjectId) => {
      dismissPreview();
      document.body.style.cursor = "";
      setDetailObjectId(objectId);
    },
    [dismissPreview],
  );
  const closeCardDetails = useCallback(() => setDetailObjectId(null), []);

  if (!gameState) return null;

  return (
    <div
      className="relative h-full min-h-0 w-full overflow-visible"
      data-tabletop-stage-layout={tableLayout}
    >
      <div
        className="pointer-events-none absolute inset-0 z-30"
        data-tabletop-screen-space-ui="opponent-hud"
      >
        {props.renderOpponentHud?.(opponentSeats)}
      </div>
      <div
        className="pointer-events-none absolute inset-x-0 bottom-0 z-40 flex justify-center"
        data-tabletop-screen-space-ui="player-hud"
      >
        <div className="contents pointer-events-auto">{props.playerHud}</div>
      </div>
      <div className="absolute inset-0 overflow-hidden">
        <Canvas
          shadows
          frameloop="demand"
          dpr={[1, 2]}
          camera={{ fov: TABLETOP_CAMERA_FOV, near: 0.1, far: 90 }}
          gl={{
            antialias: true,
            alpha: true,
            powerPreference: "high-performance",
          }}
          style={{ position: "absolute", inset: 0 }}
        >
          <TabletopCameraRig tableLayout={tableLayout} />
          <TabletopFlightDestinations />
          <TabletopRendererSettings />
          <color attach="background" args={["#111a1c"]} />
          <fog attach="fog" args={["#111a1c", 34, 62]} />
          <ambientLight intensity={0.72} color="#a7b0ad" />
          <hemisphereLight args={["#c1c5bc", "#1d292a", 0.96]} />
          {/* One broad warm key establishes the painterly upper-left value
              mass while retaining readable shadows beneath matte cards. */}
          <directionalLight
            position={[-9, 18, -7]}
            intensity={2.85}
            color="#ead6b8"
            castShadow
            shadow-mapSize-width={2048}
            shadow-mapSize-height={2048}
            shadow-camera-left={-10}
            shadow-camera-right={10}
            shadow-camera-top={11}
            shadow-camera-bottom={-11}
            shadow-bias={-0.00015}
            shadow-normalBias={0.025}
            shadow-radius={3}
          />
          {/* A restrained cool fill separates the far edge without creating a
              second theatrical light source. */}
          <directionalLight
            position={[9, 7, 9]}
            intensity={0.44}
            color="#799aa5"
          />
          <directionalLight
            position={[-4, 8, -12]}
            intensity={0.54}
            color="#a5bdbe"
          />

          <TabletopMaterialPlane />
          {opponentSeats.map(({ playerId, seat }) => (
            <TabletopHeldHand
              key={`held-hand-${playerId}`}
              playerId={playerId}
              seat={seat}
              tableLayout={tableLayout}
              showCards={props.showOpponentCards}
            />
          ))}
          <TabletopZonePiles
            playerId={perspectivePlayerId}
            seat="local"
            tableLayout={tableLayout}
            onViewZone={props.onViewZone}
          />
          {opponentSeats.map(({ playerId, seat }) => (
            <TabletopZonePiles
              key={playerId}
              playerId={playerId}
              seat={seat}
              tableLayout={tableLayout}
              onViewZone={props.onViewZone}
            />
          ))}
          {placements.map((placement) => (
            <TabletopPermanent
              key={placement.objectId}
              {...placement}
              attackStrikeTarget={attackStrikeTargets.get(placement.objectId)}
              onShowDetails={showCardDetails}
            />
          ))}
        </Canvas>
      </div>
      <TabletopCardDetailOverlay
        objectId={detailObjectId}
        onClose={closeCardDetails}
      />
    </div>
  );
});

function TabletopCameraRig({
  tableLayout,
}: {
  tableLayout: TabletopTableLayout;
}) {
  const { camera, size, invalidate } = useThree();

  useEffect(() => {
    const perspective = camera as THREE.PerspectiveCamera;
    const aspect = Math.max(size.width / Math.max(size.height, 1), 0.4);
    const compact = aspect < 1.35;
    const target = compact
      ? new THREE.Vector3(0, 0, 0.4)
      : new THREE.Vector3(0, 0, tableLayout === "pod" ? 0.55 : 0.72);
    const direction = compact
      ? new THREE.Vector3(0, 0.86, 0.51).normalize()
      : new THREE.Vector3(0, 0.8, 0.6).normalize();
    const halfFov = (TABLETOP_CAMERA_FOV * Math.PI) / 360;
    const fitRadius = tableLayout === "pod" ? 9.3 : 8.7;
    const horizontalDistance = fitRadius / (Math.tan(halfFov) * aspect);
    const verticalDistance = fitRadius / Math.tan(halfFov);
    // Compact screens intentionally crop the environment instead of zooming
    // the cards down to fit an artificial perimeter. The interactive play lanes
    // remain large while the material plane continues beyond the viewport;
    // the mobile hand drawer carries the primary narrow-screen interaction.
    const distance = compact
      ? verticalDistance * (tableLayout === "pod" ? 0.72 : 0.68)
      : Math.max(horizontalDistance, verticalDistance * 0.48);
    const cameraPosition = direction.multiplyScalar(distance).add(target);

    perspective.position.copy(cameraPosition);
    perspective.fov = TABLETOP_CAMERA_FOV;
    perspective.aspect = aspect;
    perspective.updateProjectionMatrix();
    perspective.lookAt(target);
    invalidate();
  }, [camera, invalidate, size.height, size.width, tableLayout]);

  return null;
}

function TabletopRendererSettings() {
  const { gl, invalidate } = useThree();

  useEffect(() => {
    gl.outputColorSpace = THREE.SRGBColorSpace;
    gl.toneMapping = THREE.ACESFilmicToneMapping;
    gl.toneMappingExposure = 1.18;
    gl.shadowMap.type = THREE.PCFSoftShadowMap;
    invalidate();
  }, [gl, invalidate]);

  return null;
}
