import { useEffect, useMemo, useRef } from "react";
import { useFrame, useThree } from "@react-three/fiber";
import * as THREE from "three";

const FLOOR_Y = -0.62;
const FLOOR_RADIUS = 48;
const MOTE_TICK_SECONDS = 1 / 30;

/**
 * Woodland clearing around the stone table: a mossy forest floor, a ring of
 * shadowed canopies closing in through the mist, and a slow drift of
 * pollen-gold and faintly arcane motes.
 * Everything ambient lives here so the gameplay meshes stay state-driven.
 */
export function TabletopForest() {
  const floorTexture = useMemo(makeForestFloorTexture, []);
  const trees = useMemo(makeTreeLine, []);
  const treeGeometries = useMemo(
    () => ({
      trunk: new THREE.CylinderGeometry(0.26, 0.58, 1, 7),
      canopy: new THREE.IcosahedronGeometry(1, 1),
    }),
    [],
  );
  const treeMaterials = useMemo(
    () => ({
      trunk: new THREE.MeshStandardMaterial({
        color: "#171b19",
        roughness: 0.98,
      }),
      canopies: ["#173027", "#1c382d", "#244236", "#12281f", "#2b493b"].map(
        (color) =>
          new THREE.MeshStandardMaterial({ color, roughness: 0.9, flatShading: true }),
      ),
      undergrowth: new THREE.MeshStandardMaterial({
        color: "#10231d",
        roughness: 1,
        flatShading: true,
      }),
    }),
    [],
  );

  useEffect(
    () => () => {
      floorTexture.dispose();
      treeGeometries.trunk.dispose();
      treeGeometries.canopy.dispose();
      treeMaterials.trunk.dispose();
      treeMaterials.undergrowth.dispose();
      for (const material of treeMaterials.canopies) material.dispose();
    },
    [floorTexture, treeGeometries, treeMaterials],
  );

  return (
    <group>
      {/* Forest floor: moss and leaf litter fading into the mist. */}
      <mesh
        rotation={[-Math.PI / 2, 0, 0]}
        position={[0, FLOOR_Y, 0]}
        receiveShadow
      >
        <circleGeometry args={[FLOOR_RADIUS, 48]} />
        <meshStandardMaterial
          map={floorTexture}
          color="#ffffff"
          roughness={1}
          metalness={0}
        />
      </mesh>

      {trees.map((tree) => (
        <group
          key={tree.key}
          position={tree.position}
          rotation={[0, tree.turn, tree.lean]}
          scale={tree.scale}
        >
          <mesh
            geometry={treeGeometries.trunk}
            material={treeMaterials.trunk}
            position={[0, tree.trunkHeight / 2 + FLOOR_Y, 0]}
            scale={[1, tree.trunkHeight, 1]}
          />
          {tree.blobs.map((blob, index) => (
            <mesh
              key={index}
              geometry={treeGeometries.canopy}
              material={
                blob.undergrowth
                  ? treeMaterials.undergrowth
                  : treeMaterials.canopies[blob.materialIndex]
              }
              position={[
                blob.offset[0],
                blob.offset[1] + (blob.undergrowth ? FLOOR_Y : 0),
                blob.offset[2],
              ]}
              scale={blob.scale}
            />
          ))}
        </group>
      ))}

      <MagicMotes />
    </group>
  );
}

/**
 * Firefly-scale points: warm pollen near the light pools, a sparser cold
 * glimmer for the hint of magic. Drift runs on a 30 fps tick so the demand
 * frameloop never spins faster than the ambience needs.
 */
function MagicMotes() {
  const { invalidate } = useThree();
  const warmRef = useRef<THREE.Points>(null);
  const arcaneRef = useRef<THREE.Points>(null);
  const tickRef = useRef(0);
  const clockRef = useRef(0);
  const sprite = useMemo(makeMoteSprite, []);
  const clouds = useMemo(
    () => ({
      warm: makeMoteCloud(28, 0x5eed1, ["#eacb86", "#dcb76a", "#f3dca8"]),
      arcane: makeMoteCloud(42, 0x5eed2, ["#72e0cc", "#a3efe1", "#46c5b2"]),
    }),
    [],
  );
  const materials = useMemo(
    () => ({
      warm: new THREE.PointsMaterial({
        map: sprite,
        size: 0.13,
        transparent: true,
        opacity: 0.54,
        vertexColors: true,
        blending: THREE.AdditiveBlending,
        depthWrite: false,
        sizeAttenuation: true,
      }),
      arcane: new THREE.PointsMaterial({
        map: sprite,
        size: 0.19,
        transparent: true,
        opacity: 0.52,
        vertexColors: true,
        blending: THREE.AdditiveBlending,
        depthWrite: false,
        sizeAttenuation: true,
      }),
    }),
    [sprite],
  );

  useEffect(
    () => () => {
      sprite.dispose();
      clouds.warm.geometry.dispose();
      clouds.arcane.geometry.dispose();
      materials.warm.dispose();
      materials.arcane.dispose();
    },
    [clouds, materials, sprite],
  );

  useFrame((_state, delta) => {
    tickRef.current += delta;
    if (tickRef.current < MOTE_TICK_SECONDS) return;
    clockRef.current += tickRef.current;
    tickRef.current = 0;
    const time = clockRef.current;

    driftCloud(warmRef.current, clouds.warm, time, 0.32, 0.55);
    driftCloud(arcaneRef.current, clouds.arcane, time, 0.22, 0.34);
    materials.warm.opacity = 0.4 + Math.sin(time * 0.6) * 0.09;
    materials.arcane.opacity = 0.42 + Math.sin(time * 0.83 + 1.7) * 0.11;
    invalidate();
  });

  return (
    <>
      <points
        ref={warmRef}
        geometry={clouds.warm.geometry}
        material={materials.warm}
      />
      <points
        ref={arcaneRef}
        geometry={clouds.arcane.geometry}
        material={materials.arcane}
      />
    </>
  );
}

interface TreeBlob {
  offset: [number, number, number];
  scale: number;
  materialIndex: number;
  undergrowth?: boolean;
}

interface TreePlacement {
  key: string;
  position: [number, number, number];
  turn: number;
  lean: number;
  scale: number;
  trunkHeight: number;
  blobs: TreeBlob[];
}

function makeTreeLine(): TreePlacement[] {
  const random = mulberry32(0x7ea5);
  const trees: TreePlacement[] = [];
  const treeCount = 15;
  for (let index = 0; index < treeCount; index += 1) {
    let angle = (index / treeCount) * Math.PI * 2 + (random() - 0.5) * 0.42;
    // Keep the canopy ring out of the camera sector behind the local seat:
    // the rig hovers over +z, and a tall crown there swallows the lens.
    while (Math.sin(angle) > 0.35) angle = random() * Math.PI * 2;
    const radius = 13.5 + random() * 10;
    // Far-side trees tower a little higher so their crowns rise into the top
    // of the frame past the table's far edge.
    const farSide = Math.sin(angle) < -0.45;
    const scale = (0.85 + random() * 0.75) * (farSide ? 1.3 : 1);
    const trunkHeight = 6.5 + random() * 3.5;
    const blobs: TreeBlob[] = [];
    const blobCount = 3 + Math.floor(random() * 3);
    for (let blob = 0; blob < blobCount; blob += 1) {
      const blobAngle = random() * Math.PI * 2;
      const blobRadius = random() * 1.9;
      blobs.push({
        offset: [
          Math.cos(blobAngle) * blobRadius,
          trunkHeight + 0.5 + random() * 2,
          Math.sin(blobAngle) * blobRadius,
        ],
        scale: (1.9 + random() * 1.6) * (blob === 0 ? 1.35 : 1),
        materialIndex: Math.floor(random() * 5),
      });
    }
    trees.push({
      key: `tree-${index}`,
      position: [
        Math.cos(angle) * radius,
        0,
        Math.sin(angle) * radius,
      ],
      turn: random() * Math.PI * 2,
      lean: (random() - 0.5) * 0.06,
      scale,
      trunkHeight,
      blobs,
    });
    // Undergrowth crouches between the trunks, never under the slab.
    if (index % 2 === 0) {
      const bushAngle = angle + 0.24;
      const bushRadius = Math.max(radius - 3.5 - random() * 3, 11.8);
      trees.push({
        key: `bush-${index}`,
        position: [
          Math.cos(bushAngle) * bushRadius,
          0,
          Math.sin(bushAngle) * bushRadius,
        ],
        turn: random() * Math.PI * 2,
        lean: 0,
        scale: 1,
        trunkHeight: 0,
        blobs: [
          {
            offset: [0, 0.35, 0],
            scale: 0.9 + random() * 0.8,
            materialIndex: 0,
            undergrowth: true,
          },
        ],
      });
    }
  }
  // A loose inner fringe of shrubs closes the gap between clearing and
  // treeline, so the slab reads as nested inside the wood.
  for (let shrub = 0; shrub < 8; shrub += 1) {
    let angle = random() * Math.PI * 2;
    while (Math.sin(angle) > 0.35) angle = random() * Math.PI * 2;
    const radius = 11 + random() * 3;
    trees.push({
      key: `shrub-${shrub}`,
      position: [Math.cos(angle) * radius, 0, Math.sin(angle) * radius],
      turn: random() * Math.PI * 2,
      lean: 0,
      scale: 1,
      trunkHeight: 0,
      blobs: [
        {
          offset: [0, 0.3, 0],
          scale: 1.1 + random() * 1.1,
          materialIndex: 0,
          undergrowth: true,
        },
        {
          offset: [(random() - 0.5) * 1.4, 0.55, (random() - 0.5) * 1.4],
          scale: 0.7 + random() * 0.7,
          materialIndex: 0,
          undergrowth: true,
        },
      ],
    });
  }
  return trees;
}

interface MoteCloud {
  geometry: THREE.BufferGeometry;
  base: Float32Array;
  phase: Float32Array;
}

function makeMoteCloud(
  count: number,
  seed: number,
  palette: string[],
): MoteCloud {
  const random = mulberry32(seed);
  const base = new Float32Array(count * 3);
  const phase = new Float32Array(count);
  const colors = new Float32Array(count * 3);
  const color = new THREE.Color();
  for (let index = 0; index < count; index += 1) {
    base[index * 3] = (random() - 0.5) * 19;
    base[index * 3 + 1] = 0.35 + random() * random() * 4.6;
    base[index * 3 + 2] = (random() - 0.5) * 19;
    phase[index] = random() * Math.PI * 2;
    color.set(palette[Math.floor(random() * palette.length)]);
    colors[index * 3] = color.r;
    colors[index * 3 + 1] = color.g;
    colors[index * 3 + 2] = color.b;
  }
  const geometry = new THREE.BufferGeometry();
  geometry.setAttribute(
    "position",
    new THREE.BufferAttribute(base.slice(), 3),
  );
  geometry.setAttribute("color", new THREE.BufferAttribute(colors, 3));
  return { geometry, base, phase };
}

function driftCloud(
  points: THREE.Points | null,
  cloud: MoteCloud,
  time: number,
  lift: number,
  sway: number,
): void {
  if (!points) return;
  const attribute = points.geometry.getAttribute(
    "position",
  ) as THREE.BufferAttribute;
  const array = attribute.array as Float32Array;
  for (let index = 0; index < cloud.phase.length; index += 1) {
    const phase = cloud.phase[index];
    array[index * 3] = cloud.base[index * 3]
      + Math.sin(time * 0.19 + phase * 1.7) * sway;
    array[index * 3 + 1] = cloud.base[index * 3 + 1]
      + Math.sin(time * 0.31 + phase) * lift;
    array[index * 3 + 2] = cloud.base[index * 3 + 2]
      + Math.cos(time * 0.16 + phase * 2.3) * sway;
  }
  attribute.needsUpdate = true;
}

function makeForestFloorTexture(): THREE.CanvasTexture {
  const canvas = document.createElement("canvas");
  canvas.width = 512;
  canvas.height = 512;
  const context = canvas.getContext("2d");
  if (!context) throw new Error("2D canvas unavailable");

  context.fillStyle = "#07110f";
  context.fillRect(0, 0, canvas.width, canvas.height);

  // Overlapping moss and leaf-litter blotches keep the tile organic.
  const random = mulberry32(0xf10);
  const blotches = ["#0d1d18", "#10251e", "#14291f", "#081612", "#18241b"];
  for (let index = 0; index < 240; index += 1) {
    context.fillStyle = blotches[Math.floor(random() * blotches.length)];
    context.globalAlpha = 0.16 + random() * 0.22;
    context.beginPath();
    context.ellipse(
      random() * canvas.width,
      random() * canvas.height,
      12 + random() * 46,
      8 + random() * 30,
      random() * Math.PI,
      0,
      Math.PI * 2,
    );
    context.fill();
  }
  // Spring shoots and scattered light flecks.
  context.globalAlpha = 1;
  for (let index = 0; index < 520; index += 1) {
    const bright = random() < 0.24;
    context.fillStyle = bright ? "#3c6e57" : "#050d0b";
    context.globalAlpha = 0.16 + random() * 0.24;
    context.fillRect(
      random() * canvas.width,
      random() * canvas.height,
      1 + random() * 2,
      1 + random() * 2,
    );
  }
  context.globalAlpha = 1;

  const texture = new THREE.CanvasTexture(canvas);
  texture.colorSpace = THREE.SRGBColorSpace;
  texture.wrapS = THREE.RepeatWrapping;
  texture.wrapT = THREE.RepeatWrapping;
  texture.repeat.set(9, 9);
  texture.anisotropy = 4;
  return texture;
}

function makeMoteSprite(): THREE.CanvasTexture {
  const canvas = document.createElement("canvas");
  canvas.width = 64;
  canvas.height = 64;
  const context = canvas.getContext("2d");
  if (!context) throw new Error("2D canvas unavailable");

  const glow = context.createRadialGradient(32, 32, 0, 32, 32, 32);
  glow.addColorStop(0, "rgba(255, 255, 255, 1)");
  glow.addColorStop(0.35, "rgba(255, 255, 255, 0.55)");
  glow.addColorStop(1, "rgba(255, 255, 255, 0)");
  context.fillStyle = glow;
  context.fillRect(0, 0, 64, 64);

  const texture = new THREE.CanvasTexture(canvas);
  texture.colorSpace = THREE.SRGBColorSpace;
  return texture;
}

function mulberry32(seed: number): () => number {
  let state = seed;
  return () => {
    state = (state + 0x6d2b79f5) | 0;
    let value = Math.imul(state ^ (state >>> 15), 1 | state);
    value = (value + Math.imul(value ^ (value >>> 7), 61 | value)) ^ value;
    return ((value ^ (value >>> 14)) >>> 0) / 4294967296;
  };
}
