import { AnimatePresence, motion } from "framer-motion";
import { type RefObject, useCallback, useEffect, useRef, useState } from "react";

import type { StepEffect } from "../../animation/types.ts";
import { SPEED_MULTIPLIERS } from "../../animation/types.ts";
import { getCardColors } from "../../animation/wubrgColors.ts";
import { currentSnapshot } from "../../hooks/useGameDispatch.ts";
import { fetchCardImageUrl } from "../../services/scryfall.ts";
import { useAnimationStore } from "../../stores/animationStore.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { hexToRgb } from "./particleEffects.ts";
import { CardRevealBurst } from "./CardRevealBurst.tsx";
import { CastArcAnimation } from "./CastArcAnimation.tsx";
import { DamageVignette } from "./DamageVignette.tsx";
import { DeathShatter } from "./DeathShatter.tsx";
import { FloatingNumber } from "./FloatingNumber.tsx";
import { ParticleCanvas } from "./ParticleCanvas.tsx";
import type { ParticleCanvasHandle } from "./ParticleCanvas.tsx";
import { applyScreenShake } from "./ScreenShake.tsx";


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

interface ActiveShatter {
  id: number;
  position: { x: number; y: number; width: number; height: number };
  imageUrl: string;
}

interface ActiveCastArc {
  id: number;
  from: { x: number; y: number };
  to: { x: number; y: number };
  cardName: string;
  mode: "cast" | "resolve-permanent" | "resolve-spell";
}

interface AnimationOverlayProps {
  containerRef: RefObject<HTMLDivElement | null>;
}

let floatIdCounter = 0;
let revealIdCounter = 0;
let shatterIdCounter = 0;
let castArcIdCounter = 0;

export function AnimationOverlay({ containerRef }: AnimationOverlayProps) {
  const activeStep = useAnimationStore((s) => s.activeStep);
  const advanceStep = useAnimationStore((s) => s.advanceStep);
  const getPosition = useAnimationStore((s) => s.getPosition);
  const particleRef = useRef<ParticleCanvasHandle>(null);
  const [activeFloats, setActiveFloats] = useState<ActiveFloat[]>([]);
  const [activeDeathClones, setActiveDeathClones] = useState<DeathClone[]>([]);
  const [activeVignette, setActiveVignette] = useState<{
    damageAmount: number;
  } | null>(null);
  const [activeReveals, setActiveReveals] = useState<ActiveReveal[]>([]);
  const [activeShatters, setActiveShatters] = useState<ActiveShatter[]>([]);
  const [activeCastArcs, setActiveCastArcs] = useState<ActiveCastArc[]>([]);

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

  /** Query the actual DOM position of a player's HUD element. */
  const getPlayerHudPosition = useCallback(
    (playerId: number): { x: number; y: number } => {
      const el = document.querySelector(`[data-player-hud="${playerId}"]`);
      if (el) {
        const rect = el.getBoundingClientRect();
        return { x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 };
      }
      // Fallback: center of screen
      return { x: window.innerWidth / 2, y: window.innerHeight / 2 };
    },
    [],
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
            if (vfxQuality !== "minimal") {
              particleRef.current?.damageFlash(pos.x, pos.y, amount);
            }
          } else if (target && "Player" in target) {
            isPlayerTarget = true;
            pos = getPlayerHudPosition(target.Player);
            if (vfxQuality !== "minimal") {
              particleRef.current?.playerDamage(pos.x, pos.y, amount);
            }
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
          const { x, y } = getPlayerHudPosition(playerId);

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

          // Heal particle effect for life gain
          if (amount > 0 && vfxQuality !== "minimal") {
            particleRef.current?.healEffect(x, y, amount);
          }
          break;
        }

        case "CreatureDestroyed":
        case "PermanentSacrificed": {
          const objectId = data.object_id as number | undefined;
          if (objectId != null) {
            // Explosion particle effect
            const pos = getObjectPosition(objectId);
            if (pos && vfxQuality !== "minimal") {
              const gameState = useGameStore.getState().gameState;
              const colors = gameState?.objects[objectId]?.color ?? [];
              const explosionColor = colors.length > 0 ? hexToRgb(getCardColors(colors)[0]) : undefined;
              particleRef.current?.explosion(pos.x, pos.y, explosionColor);
            }

            // Death shatter (full/reduced quality) or death clone (minimal)
            const snapshotRect = currentSnapshot.get(objectId);
            const registryRect = getPosition(objectId);
            const rect = snapshotRect ?? registryRect;
            if (rect) {
              const gameState = useGameStore.getState().gameState;
              const cardName = gameState?.objects[objectId]?.name ?? "Unknown";

              if (vfxQuality !== "minimal" && effect.type === "CreatureDestroyed") {
                // Fetch art_crop image for shatter effect
                const shatterId = ++shatterIdCounter;
                fetchCardImageUrl(cardName, 0, "art_crop")
                  .then((url) => {
                    setActiveShatters((prev) => [
                      ...prev,
                      {
                        id: shatterId,
                        position: { x: rect.x, y: rect.y, width: rect.width, height: rect.height },
                        imageUrl: url,
                      },
                    ]);
                  })
                  .catch(() => {
                    // Fallback to death clone if image fetch fails
                    setActiveDeathClones((prev) => [
                      ...prev,
                      { id: objectId, position: rect, cardName },
                    ]);
                  });
              } else {
                setActiveDeathClones((prev) => [
                  ...prev,
                  { id: objectId, position: rect, cardName },
                ]);
              }
            }
          }
          break;
        }

        case "SpellCast": {
          const cardId = data.card_id as number | undefined;
          if (cardId != null) {
            const pos = getObjectPosition(cardId);
            if (pos) {
              // Spell impact with WUBRG color
              const gameState = useGameStore.getState().gameState;
              const colors = gameState?.objects[cardId]?.color ?? [];
              const burstColor = getCardColors(colors)[0] ?? "#06b6d4";
              if (vfxQuality !== "minimal") {
                particleRef.current?.spellImpact(pos.x, pos.y, hexToRgb(burstColor));
              }

              // Cast arc animation (hand -> stack)
              if (vfxQuality !== "minimal") {
                const cardName = gameState?.objects[cardId]?.name ?? "";
                // Stack position is roughly right-center
                const stackPos = { x: window.innerWidth * 0.75, y: window.innerHeight * 0.4 };
                const id = ++castArcIdCounter;
                setActiveCastArcs((prev) => [
                  ...prev,
                  { id, from: pos, to: stackPos, cardName, mode: "cast" },
                ]);
              }
            }
          }
          break;
        }

        case "AttackersDeclared": {
          if (vfxQuality !== "minimal") {
            const attackerIds = (data.attacker_ids as number[]) ?? [];
            const defendingPlayer = data.defending_player as number | undefined;
            const defenderPos = defendingPlayer != null
              ? getPlayerHudPosition(defendingPlayer)
              : null;
            for (const attackerId of attackerIds) {
              const pos = getObjectPosition(attackerId);
              if (pos) {
                particleRef.current?.attackBurst(pos.x, pos.y);
                // Fire projectile toward the defending player's HUD
                if (defenderPos) {
                  particleRef.current?.projectile(
                    pos.x, pos.y,
                    defenderPos.x, defenderPos.y,
                    250,
                  );
                }
              }
            }
          }
          break;
        }

        case "TurnStarted":
          // Handled directly in dispatch.ts via uiStore.flashTurnBanner
          break;

        case "ZoneChanged": {
          const toZone = data.to as string | undefined;
          const fromZone = data.from as string | undefined;
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

                // Summon burst particle effect
                if (vfxQuality !== "minimal") {
                  const summonColor = colors.length > 0 ? hexToRgb(getCardColors(colors)[0]) : undefined;
                  particleRef.current?.summonBurst(pos.x, pos.y, summonColor);
                }

                // Resolve-permanent arc (stack -> battlefield)
                if (fromZone === "Stack" && vfxQuality !== "minimal") {
                  const cardName = gameState?.objects[objectId]?.name ?? "";
                  const stackPos = { x: window.innerWidth * 0.75, y: window.innerHeight * 0.4 };
                  const arcId = ++castArcIdCounter;
                  setActiveCastArcs((prev) => [
                    ...prev,
                    { id: arcId, from: stackPos, to: pos, cardName, mode: "resolve-permanent" },
                  ]);
                }
              }
            }
          } else if (fromZone === "Stack" && toZone === "Graveyard") {
            // Non-permanent spell resolved (instant/sorcery -> graveyard)
            if (vfxQuality !== "minimal") {
              const objectId = data.object_id as number | undefined;
              if (objectId != null) {
                const gameState = useGameStore.getState().gameState;
                const cardName = gameState?.objects[objectId]?.name ?? "";
                const stackPos = { x: window.innerWidth * 0.75, y: window.innerHeight * 0.4 };
                const arcId = ++castArcIdCounter;
                setActiveCastArcs((prev) => [
                  ...prev,
                  { id: arcId, from: stackPos, to: stackPos, cardName, mode: "resolve-spell" },
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

              // Summon burst for tokens
              if (vfxQuality !== "minimal") {
                const tokenColor = colors.length > 0 ? hexToRgb(getCardColors(colors)[0]) : undefined;
                particleRef.current?.summonBurst(pos.x, pos.y, tokenColor);
              }
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
      getPlayerHudPosition,
      vfxQuality,
      speedMultiplier,
      containerRef,
    ],
  );

  // Process effects when activeStep changes, then advance after its duration
  useEffect(() => {
    if (!activeStep) return;

    for (const effect of activeStep.effects) {
      processEffect(effect);
    }

    const timer = setTimeout(advanceStep, activeStep.duration * speedMultiplier);
    return () => clearTimeout(timer);
  }, [activeStep, advanceStep, processEffect, speedMultiplier]);

  const handleFloatComplete = useCallback((id: number) => {
    setActiveFloats((prev) => prev.filter((f) => f.id !== id));
  }, []);

  const handleDeathCloneComplete = useCallback((id: number) => {
    setActiveDeathClones((prev) => prev.filter((c) => c.id !== id));
  }, []);

  const handleRevealComplete = useCallback((id: number) => {
    setActiveReveals((prev) => prev.filter((r) => r.id !== id));
  }, []);

  const handleShatterComplete = useCallback((id: number) => {
    setActiveShatters((prev) => prev.filter((s) => s.id !== id));
  }, []);

  const handleCastArcComplete = useCallback((id: number) => {
    setActiveCastArcs((prev) => prev.filter((a) => a.id !== id));
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

      {/* Death shatter effects (z-46) */}
      {activeShatters.map((shatter) => (
        <DeathShatter
          key={`shatter-${shatter.id}`}
          position={shatter.position}
          imageUrl={shatter.imageUrl}
          onComplete={() => handleShatterComplete(shatter.id)}
        />
      ))}

      {/* Cast arc animations (z-45) */}
      {activeCastArcs.map((arc) => (
        <CastArcAnimation
          key={`arc-${arc.id}`}
          from={arc.from}
          to={arc.to}
          cardName={arc.cardName}
          mode={arc.mode}
          onComplete={() => handleCastArcComplete(arc.id)}
        />
      ))}

      {/* Damage vignette (z-45) */}
      <DamageVignette
        active={activeVignette != null}
        damageAmount={activeVignette?.damageAmount ?? 0}
        speedMultiplier={speedMultiplier}
      />

      {/* Card reveals */}
      <AnimatePresence>
        {activeReveals.map((reveal) => (
          <CardRevealBurst
            key={`reveal-${reveal.id}`}
            position={reveal.position}
            colors={reveal.colors}
            speedMultiplier={speedMultiplier}
            onComplete={() => handleRevealComplete(reveal.id)}
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
