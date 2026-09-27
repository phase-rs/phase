import { useEffect, useMemo, useState } from "react";
import { type ThreeEvent, useThree } from "@react-three/fiber";
import * as THREE from "three";

import type { PlayerId } from "../../adapter/types.ts";
import { useCardImage } from "../../hooks/useCardImage.ts";
import { cardImageLookup } from "../../services/cardImageLookup.ts";
import { CARD_BACK_URL } from "../../services/scryfall.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { getPlayerZoneIds } from "../../viewmodel/gameStateView.ts";
import { tabletopComposableArtSource } from "./tabletopArtSource.ts";
import {
  TABLETOP_CARD_DEPTH,
  TABLETOP_CARD_WIDTH,
  TABLETOP_ZONE_PILE_VISUAL_SCALE,
  tabletopZoneLayout,
  type TabletopSeat,
  type TabletopTableLayout,
} from "./tabletopLayout.ts";
import {
  makeRoundedCardBodyGeometry,
  makeRoundedCardFaceGeometry,
} from "./tabletopCardFrame.ts";
import { useTabletopImageTexture } from "./useTabletopImageTexture.ts";

const ZONE_CARD_FACE_GEOMETRY = makeRoundedCardFaceGeometry(
  TABLETOP_CARD_WIDTH,
  TABLETOP_CARD_DEPTH,
);

type ViewableZone = "graveyard" | "exile" | "library";
type TabletopCardZoneKind = "graveyard" | "library";

interface TabletopZonePilesProps {
  playerId: PlayerId;
  seat: TabletopSeat;
  tableLayout?: TabletopTableLayout;
  onViewZone?: (zone: ViewableZone, playerId: PlayerId) => void;
}

export function TabletopZonePiles({
  playerId,
  seat,
  tableLayout = "duel",
  onViewZone,
}: TabletopZonePilesProps) {
  const gameState = useGameStore((state) => state.gameState);
  const viewportLayout = useThree(({ size }) =>
    size.width / Math.max(size.height, 1) < 1 ? "compact" : "wide"
  );
  const library = getPlayerZoneIds(gameState, "library", playerId);
  const graveyard = getPlayerZoneIds(gameState, "graveyard", playerId);
  const exile = getPlayerZoneIds(gameState, "exile", playerId);
  const graveyardTopId = graveyard[graveyard.length - 1];
  const graveyardTop = graveyardTopId == null
    ? null
    : gameState?.objects[graveyardTopId] ?? null;
  const graveyardLookup = graveyardTop ? cardImageLookup(graveyardTop) : null;
  const { src: graveyardImage } = useCardImage(graveyardLookup?.name ?? "", {
    size: "normal",
    oracleId: graveyardLookup?.oracleId,
    faceName: graveyardLookup?.faceName,
    faceIndex: graveyardLookup?.faceIndex,
  });
  const layout = tabletopZoneLayout(seat, tableLayout, viewportLayout);
  const handleView = (zone: ViewableZone) => onViewZone?.(zone, playerId);

  return (
    <group>
      <TabletopCardZone
        kind="library"
        count={library.length}
        position={layout.library}
        faceAngle={layout.faceAngle}
        imageSource={
          library.length > 0
            ? tabletopComposableArtSource(CARD_BACK_URL)
            : null
        }
        stack={library.length > 0}
        onClick={() => handleView("library")}
      />
      <TabletopCardZone
        kind="graveyard"
        count={graveyard.length}
        position={layout.graveyard}
        faceAngle={layout.faceAngle}
        imageSource={
          graveyardImage
            ? tabletopComposableArtSource(graveyardImage)
            : null
        }
        onClick={() => handleView("graveyard")}
      />
      {exile.length > 0 && (
        <TabletopExileZone
          count={exile.length}
          position={layout.exile}
          faceAngle={layout.faceAngle}
          onClick={() => handleView("exile")}
        />
      )}
    </group>
  );
}

interface TabletopCardZoneProps {
  kind: TabletopCardZoneKind;
  count: number;
  position: [number, number, number];
  faceAngle: number;
  imageSource: string | null;
  stack?: boolean;
  onClick: () => void;
}

function TabletopCardZone({
  kind,
  count,
  position,
  faceAngle,
  imageSource,
  stack = false,
  onClick,
}: TabletopCardZoneProps) {
  const texture = useTabletopImageTexture(imageSource);
  const emptyTexture = useMemo(() => makeEmptyZoneTexture(kind), [kind]);
  const [hovered, setHovered] = useState(false);
  const stackHeight = stack
    ? Math.min(0.09 + count * 0.002, 0.2)
    : count > 0
      ? 0.045
      : 0;
  const bodyGeometry = useMemo(
    () =>
      count > 0
        ? makeRoundedCardBodyGeometry(
            TABLETOP_CARD_WIDTH,
            TABLETOP_CARD_DEPTH,
            stackHeight,
            {
              bevelSize: 0.01,
              bevelThickness: Math.min(0.007, stackHeight * 0.12),
            },
          )
        : null,
    [count, stackHeight],
  );

  useEffect(() => () => emptyTexture.dispose(), [emptyTexture]);
  useEffect(() => () => bodyGeometry?.dispose(), [bodyGeometry]);

  const pointerOver = (event: ThreeEvent<PointerEvent>) => {
    event.stopPropagation();
    setHovered(true);
    document.body.style.cursor = "pointer";
  };
  const pointerOut = (event: ThreeEvent<PointerEvent>) => {
    event.stopPropagation();
    setHovered(false);
    document.body.style.cursor = "";
  };

  return (
    <group
      position={position}
      rotation={[0, faceAngle, 0]}
      onClick={(event) => {
        event.stopPropagation();
        onClick();
      }}
      onPointerOver={pointerOver}
      onPointerOut={pointerOut}
    >
      {/* The visual pile is slightly smaller than a battlefield card, while
          this full-size transparent face preserves the existing pointer target
          on compact touch screens. */}
      <mesh
        geometry={ZONE_CARD_FACE_GEOMETRY}
        rotation={[-Math.PI / 2, 0, 0]}
        position={[0, 0.004, 0]}
      >
        <meshBasicMaterial
          transparent
          opacity={0}
          depthWrite={false}
          colorWrite={false}
        />
      </mesh>
      <group scale={TABLETOP_ZONE_PILE_VISUAL_SCALE}>
        {count > 0 ? (
          <>
            <mesh
              geometry={bodyGeometry ?? undefined}
              rotation={[-Math.PI / 2, 0, 0]}
              position={[0, stackHeight, 0]}
              castShadow
              receiveShadow
            >
              <meshStandardMaterial
                color={stack ? "#0b0e0f" : "#15191a"}
                roughness={0.98}
                metalness={0}
              />
            </mesh>
            <mesh
              geometry={ZONE_CARD_FACE_GEOMETRY}
              rotation={[-Math.PI / 2, 0, 0]}
              position={[0, stackHeight + (hovered ? 0.055 : 0.006), 0]}
              castShadow
              receiveShadow
            >
              <meshLambertMaterial
                key={texture?.uuid ?? "tabletop-zone-loading"}
                map={texture}
                color={texture ? "#ffffff" : "#2f2a21"}
                transparent
                alphaTest={0.04}
                emissive={texture ? "#050505" : "#080b10"}
                emissiveIntensity={texture ? 0.02 : 0.16}
                shadowSide={THREE.DoubleSide}
              />
            </mesh>
          </>
        ) : (
          <mesh
            geometry={ZONE_CARD_FACE_GEOMETRY}
            rotation={[-Math.PI / 2, 0, 0]}
            position={[0, 0.006, 0]}
          >
            <meshBasicMaterial
              map={emptyTexture}
              transparent
              opacity={hovered ? 0.72 : 0.46}
              depthWrite={false}
              toneMapped={false}
            />
          </mesh>
        )}
      </group>
    </group>
  );
}

interface TabletopExileZoneProps {
  count: number;
  position: [number, number, number];
  faceAngle: number;
  onClick: () => void;
}

function TabletopExileZone({
  count,
  position,
  faceAngle,
  onClick,
}: TabletopExileZoneProps) {
  const [hovered, setHovered] = useState(false);
  const depth = 0.036 + Math.min(count, 8) * 0.002;
  const bodyGeometry = useMemo(
    () =>
      makeRoundedCardBodyGeometry(
        TABLETOP_CARD_WIDTH,
        TABLETOP_CARD_DEPTH,
        depth,
        {
          bevelSize: 0.01,
          bevelThickness: Math.min(0.007, depth * 0.12),
        },
      ),
    [depth],
  );

  useEffect(() => () => bodyGeometry.dispose(), [bodyGeometry]);

  return (
    <group
      position={position}
      rotation={[0, faceAngle, 0]}
      onClick={(event) => {
        event.stopPropagation();
        onClick();
      }}
      onPointerOver={(event) => {
        event.stopPropagation();
        setHovered(true);
        document.body.style.cursor = "pointer";
      }}
      onPointerOut={(event) => {
        event.stopPropagation();
        setHovered(false);
        document.body.style.cursor = "";
      }}
    >
      <mesh
        geometry={ZONE_CARD_FACE_GEOMETRY}
        rotation={[-Math.PI / 2, 0, 0]}
        position={[0, 0.004, 0]}
      >
        <meshBasicMaterial
          transparent
          opacity={0}
          depthWrite={false}
          colorWrite={false}
        />
      </mesh>
      <group scale={TABLETOP_ZONE_PILE_VISUAL_SCALE}>
        <mesh
          geometry={bodyGeometry}
          rotation={[-Math.PI / 2, 0, 0]}
          position={[0, depth, 0]}
          castShadow
          receiveShadow
        >
          <meshStandardMaterial
            color={hovered ? "#302d45" : "#211f31"}
            roughness={0.98}
            metalness={0}
          />
        </mesh>
      </group>
    </group>
  );
}

function makeEmptyZoneTexture(kind: TabletopCardZoneKind): THREE.CanvasTexture {
  const canvas = document.createElement("canvas");
  canvas.width = 390;
  canvas.height = 546;
  const context = canvas.getContext("2d");
  if (!context) throw new Error("2D canvas unavailable");

  context.fillStyle = "rgba(5, 10, 17, 0.34)";
  context.beginPath();
  context.roundRect(8, 8, 374, 530, 28);
  context.fill();
  context.strokeStyle = kind === "graveyard"
    ? "rgba(145, 152, 166, 0.24)"
    : "rgba(122, 153, 174, 0.24)";
  context.lineWidth = 5;
  context.stroke();

  const texture = new THREE.CanvasTexture(canvas);
  texture.colorSpace = THREE.SRGBColorSpace;
  texture.minFilter = THREE.LinearFilter;
  return texture;
}
