import { useEffect, useMemo } from "react";
import { useThree } from "@react-three/fiber";
import * as THREE from "three";

import type {
  GameObject,
  ObjectId,
  PlayerId,
} from "../../adapter/types.ts";
import { useCardImage } from "../../hooks/useCardImage.ts";
import { cardImageLookup } from "../../services/cardImageLookup.ts";
import { CARD_BACK_URL } from "../../services/scryfall.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { useUiStore } from "../../stores/uiStore.ts";
import { commandZoneLeaders } from "../../viewmodel/commanderColumn.ts";
import {
  TABLETOP_CARD_DEPTH,
  TABLETOP_MAX_VISIBLE_HELD_CARDS,
  TABLETOP_CARD_WIDTH,
  tabletopHeldCardFan,
  tabletopHeldCommanderRow,
  tabletopHeldHandLayout,
  tabletopVisibleHeldCardCount,
  type TabletopHeldCardTransform,
  type TabletopSeat,
  type TabletopTableLayout,
} from "./tabletopLayout.ts";
import { tabletopComposableArtSource } from "./tabletopArtSource.ts";
import {
  makeRoundedCardBodyGeometry,
  makeRoundedCardFaceGeometry,
} from "./tabletopCardFrame.ts";
import { useTabletopImageTexture } from "./useTabletopImageTexture.ts";

const HELD_CARD_WIDTH = TABLETOP_CARD_WIDTH * 0.92;
const HELD_CARD_HEIGHT = TABLETOP_CARD_DEPTH * 0.92;
const HELD_CARD_THICKNESS = 0.045;
const HELD_CARD_BODY_GEOMETRY = makeRoundedCardBodyGeometry(
  HELD_CARD_WIDTH,
  HELD_CARD_HEIGHT,
  HELD_CARD_THICKNESS,
  { bevelSize: 0.01, bevelThickness: 0.007 },
);
const HELD_CARD_FACE_GEOMETRY = makeRoundedCardFaceGeometry(
  HELD_CARD_WIDTH * 0.97,
  HELD_CARD_HEIGHT * 0.97,
);

interface TabletopHeldHandProps {
  playerId: PlayerId;
  seat: TabletopSeat;
  tableLayout: TabletopTableLayout;
  showCards?: boolean;
}

/**
 * A concealed hand belongs to the world, not the HUD. The restrained floating
 * fan sits above the material plane at its player's seat edge without adding a
 * holder, prop, or screen-space ribbon.
 */
export function TabletopHeldHand({
  playerId,
  seat,
  tableLayout,
  showCards = false,
}: TabletopHeldHandProps) {
  const gameState = useGameStore((state) => state.gameState);
  const inspectObject = useUiStore((state) => state.inspectObject);
  const player = gameState?.players[playerId];
  const backTexture = useTabletopImageTexture(
    tabletopComposableArtSource(CARD_BACK_URL),
  );
  const viewportLayout = useThree(({ size }) =>
    size.height < 500 ? "compact" : "wide",
  );
  const layout = tabletopHeldHandLayout(seat, tableLayout, viewportLayout);
  const totalCardCount = player?.hand.length ?? 0;
  const visibleCardCount = tabletopVisibleHeldCardCount(totalCardCount);
  const transforms = useMemo(
    () => tabletopHeldCardFan(visibleCardCount),
    [visibleCardCount],
  );
  const commanders = useMemo(
    () => (gameState ? commandZoneLeaders(gameState, playerId) : []),
    [gameState, playerId],
  );
  const commanderTransforms = useMemo(
    () => tabletopHeldCommanderRow(visibleCardCount, commanders.length),
    [commanders.length, visibleCardCount],
  );

  if (!player) return null;

  return (
    <group
      position={layout.position}
      rotation={[0, layout.faceAngle, 0]}
      scale={layout.scale}
    >
      {player.hand
        .slice(0, TABLETOP_MAX_VISIBLE_HELD_CARDS)
        .map((objectId, index) => {
          // Visibility is projected by the engine per viewer; the Three.js
          // hand reads the same authority as every DOM card surface.
          const isRevealed =
            gameState?.objects[objectId]?.display_visible_to_viewer ?? false;
          const transform = transforms[index];
          return transform ? (
            <HeldCard
              key={objectId}
              objectId={objectId}
              transform={transform}
              showFace={showCards || isRevealed}
              backTexture={backTexture}
            />
          ) : null;
        })}
      {commanders.map((commander, index) => {
        const transform = commanderTransforms[index];
        return transform ? (
          <HeldCard
            key={`commander-${commander.id}`}
            objectId={commander.id}
            transform={transform}
            showFace
            backTexture={backTexture}
            onInspect={() => inspectObject(commander.id)}
          />
        ) : null;
      })}
      {totalCardCount > TABLETOP_MAX_VISIBLE_HELD_CARDS ? (
        <TabletopHandCountBadge
          count={totalCardCount}
          transforms={transforms}
        />
      ) : null}
    </group>
  );
}

function TabletopHandCountBadge({
  count,
  transforms,
}: {
  count: number;
  transforms: TabletopHeldCardTransform[];
}) {
  const texture = useMemo(() => makeHandCountTexture(count), [count]);
  const leftmostCardX = transforms[0]?.x ?? 0;

  useEffect(() => () => texture.dispose(), [texture]);

  return (
    <sprite
      position={[
        leftmostCardX - HELD_CARD_WIDTH * 0.72,
        HELD_CARD_HEIGHT * 0.46,
        0.4,
      ]}
      scale={[0.66, 0.66, 1]}
      renderOrder={100}
    >
      <spriteMaterial
        map={texture}
        transparent
        depthTest={false}
        depthWrite={false}
        toneMapped={false}
      />
    </sprite>
  );
}

function makeHandCountTexture(count: number): THREE.CanvasTexture {
  const canvas = document.createElement("canvas");
  canvas.width = 128;
  canvas.height = 128;
  const context = canvas.getContext("2d");

  if (context) {
    context.beginPath();
    context.arc(64, 64, 54, 0, Math.PI * 2);
    context.fillStyle = "rgba(7, 12, 20, 0.9)";
    context.fill();
    context.lineWidth = 5;
    context.strokeStyle = "rgba(255, 255, 255, 0.32)";
    context.stroke();
    context.fillStyle = "#f8fafc";
    context.font = "700 54px system-ui, sans-serif";
    context.textAlign = "center";
    context.textBaseline = "middle";
    context.fillText(String(count), 64, 65);
  }

  const texture = new THREE.CanvasTexture(canvas);
  texture.colorSpace = THREE.SRGBColorSpace;
  texture.minFilter = THREE.LinearFilter;
  return texture;
}

function HeldCard({
  objectId,
  transform,
  showFace,
  backTexture,
  onInspect,
}: {
  objectId: ObjectId;
  transform: TabletopHeldCardTransform;
  showFace: boolean;
  backTexture: THREE.Texture | null;
  onInspect?: () => void;
}) {
  const object = useGameStore(
    (state) => state.gameState?.objects[objectId] ?? null,
  );

  useEffect(
    () => () => {
      if (onInspect) document.body.style.cursor = "";
    },
    [onInspect],
  );

  return (
    <group
      position={[transform.x, transform.y, transform.z]}
      rotation={[0, 0, transform.rotationZ]}
      scale={transform.scale}
      onClick={
        onInspect
          ? (event) => {
              event.stopPropagation();
              onInspect();
            }
          : undefined
      }
      onPointerOver={
        onInspect
          ? (event) => {
              event.stopPropagation();
              document.body.style.cursor = "pointer";
            }
          : undefined
      }
      onPointerOut={
        onInspect
          ? () => {
              document.body.style.cursor = "";
            }
          : undefined
      }
    >
      <mesh
        geometry={HELD_CARD_BODY_GEOMETRY}
        position={[0, HELD_CARD_HEIGHT / 2, 0]}
      >
        <meshStandardMaterial
          color="#0b0b0b"
          roughness={0.96}
          metalness={0}
        />
      </mesh>
      {showFace && object ? (
        <HeldFace
          object={object}
          fallbackTexture={backTexture}
        />
      ) : (
        <HeldCardSurface texture={backTexture} />
      )}
    </group>
  );
}

function HeldFace({
  object,
  fallbackTexture,
}: {
  object: GameObject;
  fallbackTexture: THREE.Texture | null;
}) {
  const lookup = cardImageLookup(object);
  const { src } = useCardImage(lookup.name, {
    size: "normal",
    oracleId: lookup.oracleId,
    faceName: lookup.faceName,
    faceIndex: lookup.faceIndex,
  });
  const texture = useTabletopImageTexture(
    src ? tabletopComposableArtSource(src) : null,
  );

  return <HeldCardSurface texture={texture ?? fallbackTexture} />;
}

function HeldCardSurface({ texture }: { texture: THREE.Texture | null }) {
  return (
    <mesh
      position={[0, HELD_CARD_HEIGHT / 2, 0.003]}
      geometry={HELD_CARD_FACE_GEOMETRY}
    >
      <meshLambertMaterial
        key={texture?.uuid ?? "held-card-loading"}
        map={texture}
        color={texture ? "#ffffff" : "#332f29"}
        emissive={texture ? "#18130e" : "#17130f"}
        emissiveIntensity={texture ? 0.3 : 0.18}
        transparent
        alphaTest={0.04}
        shadowSide={THREE.DoubleSide}
      />
    </mesh>
  );
}
