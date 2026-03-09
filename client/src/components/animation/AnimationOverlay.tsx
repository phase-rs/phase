import { AnimatePresence, motion } from "framer-motion";
import { type RefObject, useCallback, useEffect, useRef, useState } from "react";

import type { StepEffect } from "../../animation/types.ts";
import { SPEED_MULTIPLIERS } from "../../animation/types.ts";
import { getCardColors } from "../../animation/wubrgColors.ts";
import { currentSnapshot } from "../../hooks/useGameDispatch.ts";
import { useAnimationStore } from "../../stores/animationStore.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { CardRevealBurst } from "./CardRevealBurst.tsx";
import { DamageVignette } from "./DamageVignette.tsx";
import { FloatingNumber } from "./FloatingNumber.tsx";
import { ParticleCanvas } from "./ParticleCanvas.tsx";
import type { ParticleCanvasHandle } from "./ParticleCanvas.tsx";
import { applyScreenShake } from "./ScreenShake.tsx";
import { TurnBanner } from "./TurnBanner.tsx";

interface ActiveFloat {
  id: number;
  value: number;
  position: { x: number; y: number };
  color: string;
}

interface DeathClone {
  id: number;
  position: DOMRect;
  cardName: string;
}

interface ActiveReveal {
  id: number;
  position: { x: number; y: number };
  colors: string[];
}

interface AnimationOverlayProps {
  containerRef: RefObject<HTMLDivElement | null>;
}

let floatIdCounter = 0;
let revealIdCounter = 0;

export function AnimationOverlay({ containerRef }: AnimationOverlayProps) {
  const steps = useAnimationStore((s) => s.steps);
  const isPlaying = useAnimationStore((s) => s.isPlaying);
  const playNextStep = useAnimationStore((s) => s.playNextStep);
  const getPosition = useAnimationStore((s) => s.getPosition);
  const particleRef = useRef<ParticleCanvasHandle>(null);
  const [activeFloats, setActiveFloats] = useState<ActiveFloat[]>([]);
  const [activeDeathClones, setActiveDeathClones] = useState<DeathClone[]>([]);
  const [activeTurnBanner, setActiveTurnBanner] = useState<{
    turnNumber: number;
    isPlayerTurn: boolean;
  } | null>(null);
  const [activeVignette, setActiveVignette] = useState<{
    damageAmount: number;
  } | null>(null);
  const [activeReveals, setActiveReveals] = useState<ActiveReveal[]>([]);
  const processingRef = useRef(false);

  const vfxQuality = usePreferencesStore((s) => s.vfxQuality);
  const animationSpeed = usePreferencesStore((s) => s.animationSpeed);
  const speedMultiplier = SPEED_MULTIPLIERS[animationSpeed];

  const getObjectPosition = useCallback(
    (objectId: number): { x: number; y: number } | null => {
      // Check snapshot first (pre-dispatch positions), then live registry
      const snapshotRect = currentSnapshot.get(objectId);
      const registryRect = getPosition(objectId);
      const rect = snapshotRect ?? registryRect;
      if (!rect) return null;
      return { x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 };
    },
    [getPosition],
  );

  const processEffect = useCallback(
    (effect: StepEffect) => {
      const data = effect.data as Record<string, unknown>;

      switch (effect.type) {
        case "DamageDealt": {
          const target = data.target as
            | { Object: number }
            | { Player: number }
            | undefined;
          const amount = (data.amount as number) ?? 0;
          let pos = { x: window.innerWidth / 2, y: window.innerHeight / 2 };
          let isPlayerTarget = false;

          if (target && "Object" in target) {
            const objPos = getObjectPosition(target.Object);
            if (objPos) pos = objPos;
            // Emit particle burst at target
            particleRef.current?.emitBurst(pos.x, pos.y, "#ef4444", 12);
          } else if (target && "Player" in target) {
            isPlayerTarget = true;
            const playerId = target.Player;
            pos = {
              x: window.innerWidth - 140,
              y: playerId === 0 ? window.innerHeight - 120 : 80,
            };
          }

          // Floating number
          const id = ++floatIdCounter;
          setActiveFloats((prev) => [
            ...prev,
            { id, value: -amount, position: pos, color: "#ef4444" },
          ]);

          // Screen shake (full quality only)
          if (vfxQuality === "full" && containerRef.current) {
            const intensity =
              amount >= 7 ? "heavy" : amount >= 4 ? "medium" : "light";
            applyScreenShake(containerRef.current, intensity, speedMultiplier);
          }

          // Damage vignette for player damage
          if (isPlayerTarget) {
            setActiveVignette({ damageAmount: amount });
            setTimeout(
              () => setActiveVignette(null),
              500 * speedMultiplier,
            );
          }
          break;
        }

        case "LifeChanged": {
          const amount = (data.amount as number) ?? 0;
          const playerId = (data.player_id as number) ?? 0;
          const y = playerId === 0 ? window.innerHeight - 120 : 80;
          const x = window.innerWidth - 140;

          const id = ++floatIdCounter;
          setActiveFloats((prev) => [
            ...prev,
            {
              id,
              value: amount,
              position: { x, y },
              color: amount > 0 ? "#22c55e" : "#ef4444",
            },
          ]);
          break;
        }

        case "CreatureDestroyed":
        case "PermanentSacrificed": {
          const objectId = data.object_id as number | undefined;
          if (objectId != null) {
            // Particle burst
            const pos = getObjectPosition(objectId);
            if (pos) {
              particleRef.current?.emitBurst(pos.x, pos.y, "#ef4444", 16);
            }

            // Death clone
            const snapshotRect = currentSnapshot.get(objectId);
            const registryRect = getPosition(objectId);
            const rect = snapshotRect ?? registryRect;
            if (rect) {
              const gameState = useGameStore.getState().gameState;
              const cardName = gameState?.objects[objectId]?.name ?? "Unknown";
              setActiveDeathClones((prev) => [
                ...prev,
                { id: objectId, position: rect, cardName },
              ]);
            }
          }
          break;
        }

        case "SpellCast": {
          const cardId = data.card_id as number | undefined;
          if (cardId != null) {
            const pos = getObjectPosition(cardId);
            if (pos) {
              // Use card colors for WUBRG burst
              const gameState = useGameStore.getState().gameState;
              const colors = gameState?.objects[cardId]?.color ?? [];
              const burstColor = getCardColors(colors)[0] ?? "#06b6d4";
              particleRef.current?.emitBurst(pos.x, pos.y, burstColor, 12);
            }
          }
          break;
        }

        case "AttackersDeclared": {
          const attackerIds = (data.attacker_ids as number[]) ?? [];
          for (const attackerId of attackerIds) {
            const pos = getObjectPosition(attackerId);
            if (pos) {
              particleRef.current?.emitBurst(pos.x, pos.y, "#ffffff", 8);
            }
          }
          break;
        }

        case "TurnStarted": {
          const turnNumber = (data.turn_number as number) ?? 1;
          const activePlayer = (data.active_player as number) ?? 0;
          setActiveTurnBanner({
            turnNumber,
            isPlayerTurn: activePlayer === 0,
          });
          // Auto-clear after banner animation
          setTimeout(
            () => setActiveTurnBanner(null),
            1200 * speedMultiplier,
          );
          break;
        }

        case "ZoneChanged": {
          const toZone = data.to as string | undefined;
          if (toZone === "Battlefield") {
            const objectId = data.object_id as number | undefined;
            if (objectId != null) {
              const pos = getObjectPosition(objectId);
              if (pos) {
                const gameState = useGameStore.getState().gameState;
                const colors = gameState?.objects[objectId]?.color ?? [];
                const id = ++revealIdCounter;
                setActiveReveals((prev) => [
                  ...prev,
                  { id, position: pos, colors: getCardColors(colors) },
                ]);
              }
            }
          }
          break;
        }

        case "TokenCreated": {
          const objectId = data.object_id as number | undefined;
          if (objectId != null) {
            const pos = getObjectPosition(objectId);
            if (pos) {
              const gameState = useGameStore.getState().gameState;
              const colors = gameState?.objects[objectId]?.color ?? [];
              const id = ++revealIdCounter;
              setActiveReveals((prev) => [
                ...prev,
                { id, position: pos, colors: getCardColors(colors) },
              ]);
            }
          }
          break;
        }

        default:
          break;
      }
    },
    [
      getPosition,
      getObjectPosition,
      vfxQuality,
      speedMultiplier,
      containerRef,
    ],
  );

  useEffect(() => {
    if (!isPlaying || steps.length === 0 || processingRef.current) return;

    processingRef.current = true;
    const step = playNextStep();
    if (!step) {
      processingRef.current = false;
      return;
    }

    // Process all effects in the step in parallel
    for (const effect of step.effects) {
      processEffect(effect);
    }

    // Wait for step duration * speed multiplier before processing next step
    const timer = setTimeout(() => {
      processingRef.current = false;
    }, step.duration * speedMultiplier);

    return () => clearTimeout(timer);
  }, [isPlaying, steps, playNextStep, processEffect, speedMultiplier]);

  const handleFloatComplete = useCallback((id: number) => {
    setActiveFloats((prev) => prev.filter((f) => f.id !== id));
  }, []);

  const handleDeathCloneComplete = useCallback((id: number) => {
    setActiveDeathClones((prev) => prev.filter((c) => c.id !== id));
  }, []);

  const handleRevealComplete = useCallback((id: number) => {
    setActiveReveals((prev) => prev.filter((r) => r.id !== id));
  }, []);

  return (
    <>
      {/* Death clones overlay (z-45) */}
      <div
        style={{
          position: "fixed",
          inset: 0,
          pointerEvents: "none",
          zIndex: 45,
        }}
      >
        <AnimatePresence>
          {activeDeathClones.map((clone) => (
            <motion.div
              key={`death-${clone.id}`}
              initial={{ opacity: 1, scale: 1 }}
              exit={{ opacity: 0, scale: 0.8 }}
              animate={{ opacity: 1, scale: 1 }}
              transition={{ duration: 0.4 * speedMultiplier }}
              onAnimationComplete={() => {
                // Remove after exit animation duration
                setTimeout(
                  () => handleDeathCloneComplete(clone.id),
                  400 * speedMultiplier,
                );
              }}
              style={{
                position: "absolute",
                left: clone.position.x,
                top: clone.position.y,
                width: clone.position.width,
                height: clone.position.height,
                display: "flex",
                alignItems: "center",
                justifyContent: "center",
                fontSize: "0.75rem",
                color: "white",
                backgroundColor: "rgba(0,0,0,0.6)",
                borderRadius: "0.375rem",
                border: "1px solid rgba(239,68,68,0.4)",
              }}
            >
              {clone.cardName}
            </motion.div>
          ))}
        </AnimatePresence>
      </div>

      {/* Damage vignette (z-45) */}
      <DamageVignette
        active={activeVignette != null}
        damageAmount={activeVignette?.damageAmount ?? 0}
        speedMultiplier={speedMultiplier}
      />

      {/* Turn banner (z-50) */}
      <AnimatePresence>
        {activeTurnBanner && (
          <TurnBanner
            turnNumber={activeTurnBanner.turnNumber}
            isPlayerTurn={activeTurnBanner.isPlayerTurn}
            speedMultiplier={speedMultiplier}
            onComplete={() => setActiveTurnBanner(null)}
          />
        )}
      </AnimatePresence>

      {/* Card reveals */}
      <AnimatePresence>
        {activeReveals.map((reveal) => (
          <CardRevealBurst
            key={`reveal-${reveal.id}`}
            position={reveal.position}
            colors={reveal.colors}
            speedMultiplier={speedMultiplier}
            onComplete={() => handleRevealComplete(reveal.id)}
            particleRef={particleRef}
          />
        ))}
      </AnimatePresence>

      {/* Particles (z-55) */}
      <ParticleCanvas ref={particleRef} />

      {/* Floating numbers (z-60) */}
      <AnimatePresence>
        {activeFloats.map((f) => (
          <FloatingNumber
            key={f.id}
            value={f.value}
            position={f.position}
            color={f.color}
            onComplete={() => handleFloatComplete(f.id)}
            speedMultiplier={speedMultiplier}
          />
        ))}
      </AnimatePresence>
    </>
  );
}
