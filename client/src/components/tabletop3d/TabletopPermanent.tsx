import { useEffect, useLayoutEffect, useMemo, useRef } from "react";
import { useReducedMotion } from "framer-motion";
import { useFrame, useThree, type ThreeEvent } from "@react-three/fiber";
import * as THREE from "three";

import {
  TABLETOP_CARD_DEPTH,
  TABLETOP_CARD_WIDTH,
  tabletopLandPileLayerTransforms,
  tabletopPilePresentation,
  type TabletopPlacement,
} from "./tabletopLayout.ts";
import {
  TABLETOP_BOTTOM_FRAME_DEPTH_RATIO,
  TABLETOP_COLLAPSED_PERMANENT_DEPTH_RATIO,
  tabletopCardCollapseDuration,
  tabletopCardCollapseProgress,
  tabletopCardRestPose,
  tabletopCardCollapseTransform,
  tabletopCardSettleResponse,
  tabletopCardStatUv,
  collapsedTabletopCardV,
} from "./tabletopCardCollapse.ts";
import { TABLETOP_CARD_STAT_RECT } from "./tabletopCardCanvas.ts";
import {
  makeRoundedCardBodyGeometry,
  makeRoundedCardFaceGeometry,
  makeRoundedRectangleShape,
} from "./tabletopCardFrame.ts";
import { TabletopCardGlow } from "./TabletopCardGlow.tsx";
import { useTabletopCardTextures } from "./useTabletopCardTexture.ts";
import { useTabletopCardHold } from "./useTabletopCardHold.ts";
import { useTabletopPermanentInteraction } from "./useTabletopPermanentInteraction.ts";
import {
  tabletopFlyingBobOffset,
  isFlyingCreature,
} from "./tabletopAmbientMotion.ts";
import {
  TABLETOP_ATTACK_STRIKE_DURATION_SECONDS,
  tabletopAttackStrikeTransform,
  type TabletopAttackStrikeTarget,
} from "./tabletopAttackAnimation.ts";
import type { ObjectId } from "../../adapter/types.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";

const CARD_WIDTH = TABLETOP_CARD_WIDTH;
const CARD_HEIGHT = TABLETOP_CARD_DEPTH;
const CARD_THICKNESS = 0.03;
const STAT_BADGE_TARGET_WIDTH_RATIO = 0.39;
const STAT_BADGE_SOURCE_ASPECT_RATIO = 322 / 176;
const STAT_BADGE_TARGET_DEPTH_RATIO =
  CARD_WIDTH * STAT_BADGE_TARGET_WIDTH_RATIO
  / STAT_BADGE_SOURCE_ASPECT_RATIO
  / CARD_HEIGHT;
const STAT_BADGE_RIGHT_MARGIN_RATIO = 0.018;
const CARD_FACE_GEOMETRY = makeRoundedCardFaceGeometry(
  CARD_WIDTH,
  CARD_HEIGHT,
);
const CARD_BODY_GEOMETRY = makeRoundedCardBodyGeometry(
  CARD_WIDTH,
  CARD_HEIGHT,
  CARD_THICKNESS,
);
const CARD_BOTTOM_FRAME_GEOMETRY = makeBottomFrameGeometry();
const CARD_STAT_BADGE_GEOMETRY = makeStatBadgeGeometry(false);
const CARD_STAT_BADGE_FALLBACK_GEOMETRY = makeStatBadgeGeometry(true);
const PILE_BADGE_WIDTH = 0.48;
const PILE_BADGE_DEPTH = 0.25;
const PILE_BADGE_GEOMETRY = new THREE.PlaneGeometry(
  PILE_BADGE_WIDTH,
  PILE_BADGE_DEPTH,
);

interface TabletopPermanentProps extends TabletopPlacement {
  pileCount: number;
  attackStrikeTarget?: TabletopAttackStrikeTarget;
  onShowDetails: (objectId: ObjectId) => void;
}

export function TabletopPermanent({
  objectId,
  pileCount,
  lane,
  position,
  faceAngle,
  attackVector,
  cardScale,
  attackStrikeTarget,
  onShowDetails,
}: TabletopPermanentProps) {
  const object = useGameStore((state) => state.gameState?.objects[objectId]);
  // TabletopPermanent only renders engine-authored battlefield objects, so every
  // mounted face uses the same compact battlefield treatment.
  const collapseOnBattlefield = object != null;
  const showStatBadge =
    (
      object?.power != null
      && object.toughness != null
    )
    || object?.loyalty != null;
  const pilePresentation = tabletopPilePresentation(lane, pileCount);
  const textures = useTabletopCardTextures(
    objectId,
    collapseOnBattlefield,
  );
  // Permanents retain the exact composed face used in hand for their entire
  // battlefield lifetime. Arrival progress changes only geometry and UV crop;
  // it must never swap the crown/name treatment for a compact replacement.
  const faceTexture = collapseOnBattlefield
    ? textures.fullCard
    : textures.battlefield;
  const statBadgeTexture = textures.statBadge ?? faceTexture;
  const interaction = useTabletopPermanentInteraction(objectId);
  const hold = useTabletopCardHold({
    onHold: () => onShowDetails(objectId),
  });
  const animationSpeedMultiplier = usePreferencesStore(
    (state) => state.animationSpeedMultiplier,
  );
  const shouldReduceMotion = useReducedMotion();
  const floats = isFlyingCreature(object)
    && !shouldReduceMotion
    && animationSpeedMultiplier > 0;
  const groupRef = useRef<THREE.Group>(null);
  const surfaceRef = useRef<THREE.Group>(null);
  const bottomFrameRef = useRef<THREE.Mesh>(null);
  const statBadgeRef = useRef<THREE.Mesh>(null);
  const faceMaterialRef = useRef<THREE.MeshBasicMaterial>(null);
  const arrivalProgressRef = useRef(0);
  const attackStrikeAgeRef = useRef<number | null>(null);
  const previousAttackTargetKeyRef = useRef(
    attackStrikeTarget?.key ?? null,
  );
  const previousPileCountRef = useRef(pileCount);
  const initialPlacementRef = useRef({
    attackVector,
    cardScale,
    faceAngle,
    position,
  });
  const faceGeometry = useMemo(
    () => CARD_FACE_GEOMETRY.clone(),
    [],
  );
  const { invalidate } = useThree();

  useLayoutEffect(() => {
    const group = groupRef.current;
    if (!group) return;
    const initial = initialPlacementRef.current;
    group.position.set(
      initial.position[0] + initial.attackVector[0] * 0.14,
      initial.position[1] + 0.34,
      initial.position[2] + initial.attackVector[1] * 0.14,
    );
    group.rotation.y = initial.faceAngle;
    group.scale.setScalar(initial.cardScale * 0.94);
    applyTabletopCardCollapse(
      surfaceRef.current,
      bottomFrameRef.current,
      statBadgeRef.current,
      faceGeometry,
      0,
      collapseOnBattlefield,
    );
    invalidate();
  }, [collapseOnBattlefield, faceGeometry, invalidate]);

  useEffect(
    () => () => {
      faceGeometry.dispose();
    },
    [faceGeometry],
  );

  useEffect(() => {
    const previousPileCount = previousPileCountRef.current;
    previousPileCountRef.current = pileCount;
    if (!collapseOnBattlefield || pileCount <= previousPileCount) return;

    const group = groupRef.current;
    if (!group) return;
    arrivalProgressRef.current = 0;
    group.position.set(
      position[0] + attackVector[0] * 0.14,
      position[1] + 0.34,
      position[2] + attackVector[1] * 0.14,
    );
    group.rotation.y = faceAngle;
    group.scale.setScalar(cardScale * 0.94);
    applyTabletopCardCollapse(
      surfaceRef.current,
      bottomFrameRef.current,
      statBadgeRef.current,
      faceGeometry,
      0,
      true,
    );
    invalidate();
  }, [
    attackVector,
    cardScale,
    collapseOnBattlefield,
    faceAngle,
    faceGeometry,
    invalidate,
    pileCount,
    position,
  ]);

  useEffect(() => invalidate(), [
    attackStrikeTarget?.key,
    faceAngle,
    cardScale,
    animationSpeedMultiplier,
    interaction.hasProminentAction,
    interaction.isActionable,
    interaction.isAttacking,
    interaction.isBlocking,
    interaction.isHovered,
    interaction.isSelected,
    interaction.isValidTarget,
    invalidate,
    object?.tapped,
    position,
    faceTexture,
    floats,
  ]);

  useEffect(() => {
    const nextKey = attackStrikeTarget?.key ?? null;
    if (nextKey == null) {
      previousAttackTargetKeyRef.current = null;
      attackStrikeAgeRef.current = null;
      return;
    }
    if (
      previousAttackTargetKeyRef.current == null
      && !shouldReduceMotion
      && animationSpeedMultiplier > 0
    ) {
      attackStrikeAgeRef.current = 0;
      invalidate();
    }
    previousAttackTargetKeyRef.current = nextKey;
  }, [
    animationSpeedMultiplier,
    attackStrikeTarget?.key,
    invalidate,
    shouldReduceMotion,
  ]);

  const visuallyTapped = object?.tapped === true || interaction.isAttacking;

  useFrame((state, delta) => {
    const group = groupRef.current;
    if (!group) return;

    const arrivalDuration = tabletopCardCollapseDuration(
      animationSpeedMultiplier,
    );
    const arrivalReady =
      !collapseOnBattlefield || textures.fullCard != null;
    if (arrivalReady) {
      arrivalProgressRef.current =
        arrivalDuration <= 0
          ? 1
          : Math.min(
              1,
              arrivalProgressRef.current + delta / arrivalDuration,
            );
    }
    const arrivalProgress = arrivalProgressRef.current;
    const collapseProgress = tabletopCardCollapseProgress(
      arrivalProgress,
      1,
    );
    applyTabletopCardCollapse(
      surfaceRef.current,
      bottomFrameRef.current,
      statBadgeRef.current,
      faceGeometry,
      collapseProgress,
      collapseOnBattlefield,
    );

    const faceMaterial = faceMaterialRef.current;
    if (faceMaterial) {
      const nextTexture = faceTexture;
      if (faceMaterial.map !== nextTexture) {
        faceMaterial.map = nextTexture;
        faceMaterial.color.set(nextTexture ? "#ffffff" : "#171a20");
        faceMaterial.needsUpdate = true;
      }
    }

    const response = tabletopCardSettleResponse(
      delta,
      animationSpeedMultiplier,
    );
    const targetPose = tabletopCardRestPose(
      position,
      faceAngle,
      object?.tapped === true,
      interaction.isAttacking,
      interaction.isHovered,
    );
    let targetX = targetPose.x;
    let targetY = targetPose.y + (
      floats
        ? tabletopFlyingBobOffset(
            state.clock.elapsedTime,
            objectId,
            animationSpeedMultiplier,
          )
        : 0
    );
    let targetZ = targetPose.z;
    const targetRotation = targetPose.rotationY;
    let targetScale = cardScale;

    const strikeAge = attackStrikeAgeRef.current;
    const strikeDuration =
      TABLETOP_ATTACK_STRIKE_DURATION_SECONDS * animationSpeedMultiplier;
    const strikeActive =
      strikeAge != null
      && attackStrikeTarget != null
      && strikeDuration > 0;
    if (strikeActive) {
      const nextStrikeAge = Math.min(strikeDuration, strikeAge + delta);
      attackStrikeAgeRef.current =
        nextStrikeAge < strikeDuration ? nextStrikeAge : null;
      const strike = tabletopAttackStrikeTransform(
        nextStrikeAge / strikeDuration,
        [targetPose.x, targetPose.z],
        [attackStrikeTarget.x, attackStrikeTarget.z],
      );
      targetX += strike.offsetX;
      targetY += strike.lift;
      targetZ += strike.offsetZ;
      targetScale *= strike.scale;

      // The attack arc is a keyed animation rather than a spring target. Set
      // its pose directly so the impact arrives on time on every frame rate.
      group.position.set(targetX, targetY, targetZ);
      group.rotation.y = targetRotation;
      group.scale.setScalar(targetScale);
    } else {
      group.position.x = THREE.MathUtils.lerp(group.position.x, targetX, response);
      group.position.y = THREE.MathUtils.lerp(group.position.y, targetY, response);
      group.position.z = THREE.MathUtils.lerp(group.position.z, targetZ, response);
      group.rotation.y = lerpAngle(group.rotation.y, targetRotation, response);
      const nextScale = THREE.MathUtils.lerp(
        group.scale.x,
        targetScale,
        response,
      );
      group.scale.setScalar(nextScale);
    }

    const unsettled =
      Math.abs(group.position.x - targetX) > 0.001
      || Math.abs(group.position.y - targetY) > 0.001
      || Math.abs(group.position.z - targetZ) > 0.001
      || Math.abs(angleDelta(group.rotation.y, targetRotation)) > 0.001
      || Math.abs(group.scale.x - targetScale) > 0.001;
    if (
      floats
      || strikeActive
      || unsettled
      || (arrivalReady && arrivalProgress < 1)
    ) {
      invalidate();
    }
  });

  if (!object) return null;

  const visualState = permanentVisualState(interaction);
  const restingCardHeight = collapseOnBattlefield
    ? CARD_HEIGHT * TABLETOP_COLLAPSED_PERMANENT_DEPTH_RATIO
    : CARD_HEIGHT;
  const tappedHitFootprint =
    (CARD_WIDTH + restingCardHeight) / Math.SQRT2;

  const handlePointerOver = (event: ThreeEvent<PointerEvent>) => {
    event.stopPropagation();
    const pointerType = event.pointerType || event.nativeEvent.pointerType;
    if (pointerType === "touch" || pointerType === "pen") return;
    interaction.onPointerEnter();
    document.body.style.cursor = interaction.isActionable ? "pointer" : "zoom-in";
  };

  const handlePointerOut = (event: ThreeEvent<PointerEvent>) => {
    event.stopPropagation();
    interaction.onPointerLeave();
    document.body.style.cursor = "";
  };

  return (
    <group
      onClick={(event) => {
        event.stopPropagation();
        if (hold.consumeClick()) return;
        interaction.onClick();
      }}
      {...hold.handlers}
      onPointerOver={handlePointerOver}
      onPointerOut={handlePointerOut}
    >
      {/* Keep a stationary raycast surface over the permanent's resting slot.
          Visual scale, arrival, tap, and combat motion must not move the hover
          target out from under the cursor and dismiss the inspection preview. */}
      <mesh
        rotation={[-Math.PI / 2, 0, 0]}
        position={[position[0], position[1] + 0.24, position[2]]}
      >
        <planeGeometry
          args={[
            (visuallyTapped ? tappedHitFootprint : CARD_WIDTH) * cardScale * 1.2,
            (visuallyTapped ? tappedHitFootprint : restingCardHeight) * cardScale * 1.2,
          ]}
        />
        <meshBasicMaterial
          transparent
          opacity={0}
          depthWrite={false}
        />
      </mesh>

      <group ref={groupRef}>
        <group ref={surfaceRef}>
          {lane === "lands" && pilePresentation.visibleCardCount > 1 && (
            <TabletopLandPileLayers
              count={pilePresentation.visibleCardCount - 1}
            />
          )}
          {visualState.underglow && (
            <TabletopCardGlow
              width={CARD_WIDTH}
              height={CARD_HEIGHT}
              padding={visualState.underglow.padding}
              color={visualState.underglow.color}
              opacity={visualState.underglow.opacity}
              y={-0.004}
            />
          )}

          {visualState.bracketColor && (
            <TabletopTargetBrackets color={visualState.bracketColor} />
          )}

          <mesh
            geometry={CARD_BODY_GEOMETRY}
            rotation={[-Math.PI / 2, 0, 0]}
            castShadow
            receiveShadow
          >
            <meshStandardMaterial
              color="#0b0b0b"
              roughness={0.78}
              metalness={0}
            />
          </mesh>

          <mesh
            geometry={faceGeometry}
            rotation={[-Math.PI / 2, 0, 0]}
            position={[0, 0.003, 0]}
          >
            <meshBasicMaterial
              ref={faceMaterialRef}
              key={faceTexture?.uuid ?? "tabletop-loading"}
              map={faceTexture}
              color={faceTexture ? "#ffffff" : "#171a20"}
              toneMapped={false}
            />
          </mesh>
          {visuallyTapped && (
            <mesh
              geometry={faceGeometry}
              rotation={[-Math.PI / 2, 0, 0]}
              position={[0, 0.007, 0]}
              renderOrder={3}
            >
              <meshBasicMaterial
                color="#7f8585"
                transparent
                opacity={0.34}
                depthWrite={false}
                toneMapped={false}
              />
            </mesh>
          )}
        </group>

        {collapseOnBattlefield && (
          <mesh
            ref={bottomFrameRef}
            geometry={CARD_BOTTOM_FRAME_GEOMETRY}
            rotation={[-Math.PI / 2, 0, 0]}
            position={[0, 0.004, 0]}
            renderOrder={1}
          >
            <meshBasicMaterial
              key={faceTexture?.uuid ?? "tabletop-bottom-frame-loading"}
              map={faceTexture}
              color={faceTexture ? "#ffffff" : "#171a20"}
              transparent
              alphaTest={0.06}
              alphaToCoverage
              toneMapped={false}
            />
          </mesh>
        )}

        {showStatBadge && (
          <mesh
            ref={statBadgeRef}
            geometry={
              textures.statBadge
                ? CARD_STAT_BADGE_GEOMETRY
                : CARD_STAT_BADGE_FALLBACK_GEOMETRY
            }
            rotation={[-Math.PI / 2, 0, 0]}
            position={[0, 0.007, 0]}
            renderOrder={3}
          >
            <meshBasicMaterial
              key={statBadgeTexture?.uuid ?? "tabletop-stat-loading"}
              map={statBadgeTexture}
              color={statBadgeTexture ? "#ffffff" : "#171a20"}
              transparent
              alphaTest={0.04}
              alphaToCoverage
              toneMapped={false}
            />
          </mesh>
        )}

        {pilePresentation.badgeText && (
          <TabletopPileBadge text={pilePresentation.badgeText} />
        )}
      </group>
    </group>
  );
}

function TabletopPileBadge({ text }: { text: string }) {
  const texture = useMemo(() => makePileBadgeTexture(text), [text]);

  useEffect(() => () => texture.dispose(), [texture]);

  return (
    <mesh
      geometry={PILE_BADGE_GEOMETRY}
      rotation={[-Math.PI / 2, 0, 0]}
      position={[
        CARD_WIDTH / 2 - PILE_BADGE_WIDTH / 2 - 0.035,
        0.018,
        -CARD_HEIGHT * 0.34,
      ]}
      renderOrder={5}
    >
      <meshBasicMaterial
        map={texture}
        transparent
        alphaTest={0.04}
        alphaToCoverage
        depthWrite={false}
        toneMapped={false}
      />
    </mesh>
  );
}

function makePileBadgeTexture(text: string): THREE.CanvasTexture {
  const canvas = document.createElement("canvas");
  canvas.width = 512;
  canvas.height = 256;
  const context = canvas.getContext("2d");
  if (!context) throw new Error("2D canvas unavailable");

  context.clearRect(0, 0, canvas.width, canvas.height);
  context.shadowColor = "rgba(0, 0, 0, 0.9)";
  context.shadowBlur = 22;
  context.beginPath();
  context.roundRect(22, 22, 468, 212, 106);
  context.fillStyle = "#080b0e";
  context.fill();
  context.shadowBlur = 0;
  context.strokeStyle = "#f5e6a8";
  context.lineWidth = 13;
  context.stroke();
  context.fillStyle = "#ffffff";
  context.font = '900 112px "JetBrains Mono", monospace';
  context.textAlign = "center";
  context.textBaseline = "middle";
  context.fillText(text, canvas.width / 2, canvas.height / 2 + 5);

  const texture = new THREE.CanvasTexture(canvas);
  texture.colorSpace = THREE.SRGBColorSpace;
  texture.minFilter = THREE.LinearFilter;
  texture.magFilter = THREE.LinearFilter;
  texture.needsUpdate = true;
  return texture;
}

interface TabletopLandPileLayersProps {
  count: number;
}

/** Decorative, non-interactive black card silhouettes beneath the top card. */
function TabletopLandPileLayers({ count }: TabletopLandPileLayersProps) {
  const layers = tabletopLandPileLayerTransforms(count);

  return (
    <group>
      {layers.map((layer) => (
        <group
          key={`${layer.x}-${layer.z}`}
          position={[layer.x, layer.y, layer.z]}
          rotation={[0, layer.rotation, 0]}
        >
          <mesh
            geometry={CARD_BODY_GEOMETRY}
            rotation={[-Math.PI / 2, 0, 0]}
            castShadow
            receiveShadow
          >
            <meshStandardMaterial
              color="#030405"
              roughness={0.84}
              metalness={0}
            />
          </mesh>
        </group>
      ))}
    </group>
  );
}

interface TabletopPermanentVisualState {
  bracketColor: string | null;
  underglow: {
    color: string;
    opacity: number;
    padding: number;
  } | null;
}

function permanentVisualState(
  interaction: TabletopPermanentInteractionLike,
): TabletopPermanentVisualState {
  if (interaction.isBlocking) {
    return {
      bracketColor: null,
      underglow: { color: "#c5784c", opacity: 0.24, padding: 0.2 },
    };
  }
  if (interaction.isValidTarget) {
    return { bracketColor: "#8bcbbd", underglow: null };
  }
  if (interaction.hasProminentAction) {
    return {
      bracketColor: null,
      underglow: { color: "#72b9ca", opacity: 0.26, padding: 0.28 },
    };
  }
  if (interaction.isSelected) {
    return { bracketColor: "#d8cfb2", underglow: null };
  }
  return { bracketColor: null, underglow: null };
}

interface TabletopPermanentInteractionLike {
  hasProminentAction: boolean;
  isAttacking: boolean;
  isBlocking: boolean;
  isSelected: boolean;
  isValidTarget: boolean;
}

function TabletopTargetBrackets({ color }: { color: string }) {
  const cornerX = CARD_WIDTH / 2 + 0.035;
  const cornerZ = CARD_HEIGHT / 2 + 0.035;
  const bracketLength = 0.21;
  const bracketWidth = 0.045;

  return (
    <group position={[0, 0.013, 0]}>
      {([-1, 1] as const).flatMap((xDirection) =>
        ([-1, 1] as const).flatMap((zDirection) => [
          <mesh
            key={`horizontal-${xDirection}-${zDirection}`}
            position={[
              xDirection * (cornerX - bracketLength / 2),
              0,
              zDirection * cornerZ,
            ]}
          >
            <boxGeometry args={[bracketLength, 0.012, bracketWidth]} />
            <meshBasicMaterial
              color={color}
              transparent
              opacity={0.84}
              toneMapped={false}
            />
          </mesh>,
          <mesh
            key={`vertical-${xDirection}-${zDirection}`}
            position={[
              xDirection * cornerX,
              0,
              zDirection * (cornerZ - bracketLength / 2),
            ]}
          >
            <boxGeometry args={[bracketWidth, 0.012, bracketLength]} />
            <meshBasicMaterial
              color={color}
              transparent
              opacity={0.84}
              toneMapped={false}
            />
          </mesh>,
        ]),
      )}
    </group>
  );
}

function makeBottomFrameGeometry(): THREE.PlaneGeometry {
  const geometry = new THREE.PlaneGeometry(
    CARD_WIDTH,
    CARD_HEIGHT * TABLETOP_BOTTOM_FRAME_DEPTH_RATIO,
  );
  const uvs = geometry.getAttribute("uv");
  for (let index = 0; index < uvs.count; index += 1) {
    uvs.setY(index, uvs.getY(index) * TABLETOP_BOTTOM_FRAME_DEPTH_RATIO);
  }
  uvs.needsUpdate = true;
  return geometry;
}

function makeStatBadgeGeometry(
  useCardTextureUv: boolean,
): THREE.ShapeGeometry {
  const width = CARD_WIDTH * STAT_BADGE_TARGET_WIDTH_RATIO;
  const height = CARD_HEIGHT * STAT_BADGE_TARGET_DEPTH_RATIO;
  const geometry = new THREE.ShapeGeometry(
    makeRoundedRectangleShape(width, height, height * 0.26),
    8,
  );
  const positions = geometry.getAttribute("position");
  const uvs = new Float32Array(positions.count * 2);
  for (let index = 0; index < positions.count; index += 1) {
    const baseU = (positions.getX(index) + width / 2) / width;
    const baseV = (positions.getY(index) + height / 2) / height;
    const mapped = useCardTextureUv
      ? tabletopCardStatUv(baseU, baseV)
      : { u: baseU, v: baseV };
    uvs[index * 2] = mapped.u;
    uvs[index * 2 + 1] = mapped.v;
  }
  geometry.setAttribute("uv", new THREE.BufferAttribute(uvs, 2));
  return geometry;
}

function applyTabletopCardCollapse(
  surface: THREE.Group | null,
  bottomFrame: THREE.Mesh | null,
  statBadge: THREE.Mesh | null,
  faceGeometry: THREE.ShapeGeometry,
  progress: number,
  enabled: boolean,
): void {
  if (!surface) return;
  const transform = tabletopCardCollapseTransform(
    enabled ? progress : 0,
    enabled ? TABLETOP_COLLAPSED_PERMANENT_DEPTH_RATIO : 1,
  );
  surface.scale.set(1, 1, transform.depthScale);
  surface.position.z =
    transform.centerOffsetInCardDepths * CARD_HEIGHT;
  const bottomEdge =
    (transform.centerOffsetInCardDepths + transform.depthScale / 2)
    * CARD_HEIGHT;

  if (bottomFrame) {
    bottomFrame.visible = enabled;
    const frameHeight =
      CARD_HEIGHT * TABLETOP_BOTTOM_FRAME_DEPTH_RATIO;
    bottomFrame.position.z = bottomEdge - frameHeight / 2;
  }

  if (statBadge) {
    const sourceScaleX =
      TABLETOP_CARD_STAT_RECT.width / STAT_BADGE_TARGET_WIDTH_RATIO;
    const sourceScaleZ =
      TABLETOP_CARD_STAT_RECT.height / STAT_BADGE_TARGET_DEPTH_RATIO;
    const scaleX = THREE.MathUtils.lerp(
      sourceScaleX,
      1,
      transform.easedProgress,
    );
    const scaleZ = THREE.MathUtils.lerp(
      sourceScaleZ,
      1,
      transform.easedProgress,
    );

    const sourceCenterX =
      (
        TABLETOP_CARD_STAT_RECT.x
        + TABLETOP_CARD_STAT_RECT.width / 2
        - 0.5
      )
      * CARD_WIDTH;
    const targetCenterX =
      CARD_WIDTH / 2
      - CARD_WIDTH * STAT_BADGE_RIGHT_MARGIN_RATIO
      - CARD_WIDTH * STAT_BADGE_TARGET_WIDTH_RATIO / 2;
    const positionX = THREE.MathUtils.lerp(
      sourceCenterX,
      targetCenterX,
      transform.easedProgress,
    );

    const sourceCenterZ =
      (
        TABLETOP_CARD_STAT_RECT.y
        + TABLETOP_CARD_STAT_RECT.height / 2
        - 0.5
      )
      * CARD_HEIGHT;
    const targetCenterZ =
      bottomEdge - CARD_HEIGHT * STAT_BADGE_TARGET_DEPTH_RATIO / 2;
    const positionZ = THREE.MathUtils.lerp(
      sourceCenterZ,
      targetCenterZ,
      transform.easedProgress,
    );
    applyStatBadgeTransform(statBadge, scaleX, scaleZ, positionX, positionZ);
  }

  const positions = faceGeometry.getAttribute("position");
  const uvs = faceGeometry.getAttribute("uv");
  const visibleTextureRatio = enabled ? transform.visibleTextureRatio : 1;
  for (let index = 0; index < uvs.count; index += 1) {
    const baseV =
      (positions.getY(index) + CARD_HEIGHT / 2) / CARD_HEIGHT;
    uvs.setY(index, collapsedTabletopCardV(baseV, visibleTextureRatio));
  }
  uvs.needsUpdate = true;
}

function applyStatBadgeTransform(
  badge: THREE.Mesh | null,
  scaleX: number,
  scaleZ: number,
  positionX: number,
  positionZ: number,
): void {
  if (!badge) return;
  badge.scale.set(scaleX, scaleZ, 1);
  badge.position.x = positionX;
  badge.position.z = positionZ;
}

function angleDelta(from: number, to: number): number {
  return Math.atan2(Math.sin(to - from), Math.cos(to - from));
}

function lerpAngle(from: number, to: number, amount: number): number {
  return from + angleDelta(from, to) * amount;
}
