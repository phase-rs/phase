import { AnimatePresence, motion } from "framer-motion";
import { type RefObject, useCallback, useEffect, useRef, useState } from "react";

import {
  DAMAGE_FLURRY_SOURCE_SAMPLE_LIMIT,
  impactDelayMsForAnimationEvent,
  isPlayerDamageAnimationEvent,
  type StepEffect,
} from "../../animation/types.ts";
import { getCardColors } from "../../animation/wubrgColors.ts";
import { currentSnapshot } from "../../hooks/useGameDispatch.ts";
import { fetchCardImageUrl } from "../../services/scryfall.ts";
import { useAnimationStore } from "../../stores/animationStore.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import { usePreferencesStore } from "../../stores/preferencesStore.ts";
import { audioManager } from "../../audio/AudioManager.ts";
import { hexToRgb } from "./particleEffects.ts";
import { CardRevealBurst } from "./CardRevealBurst.tsx";
import { applyCardSlam } from "./CardSlamAnimation.tsx";
import { CastArcAnimation } from "./CastArcAnimation.tsx";
import { DamageVignette } from "./DamageVignette.tsx";
import { DeathShatter } from "./DeathShatter.tsx";
import { FloatingNumber } from "./FloatingNumber.tsx";
import { MillRevealAnimation } from "./MillRevealAnimation.tsx";
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

interface ActiveMillReveal {
  id: number;
  cards: { objectId: number; cardName: string; colors: string[] }[];
  from: { x: number; y: number };
  to: { x: number; y: number };
}

interface AnimationOverlayProps {
  containerRef: RefObject<HTMLDivElement | null>;
}

let floatIdCounter = 0;
let revealIdCounter = 0;
let shatterIdCounter = 0;
let castArcIdCounter = 0;
let millRevealIdCounter = 0;

/**
 * Resolve the rendered card element for an object id. Collapsed identical-
 * permanent groups (GroupedPermanent collapsed mode) render only their
 * representative card, which carries `data-grouped-ids` listing every id it
 * stands in for — so a non-rendered swarm member falls back to that
 * representative instead of resolving to nothing (no slam, no impact FX).
 */
function findCardElement(objectId: number): HTMLElement | null {
  return (
    document.querySelector<HTMLElement>(`[data-object-id="${objectId}"]`) ??
    document.querySelector<HTMLElement>(`[data-grouped-ids~="${objectId}"]`)
  );
}

export function AnimationOverlay({ containerRef }: AnimationOverlayProps) {
  const activeStep = useAnimationStore((s) => s.activeStep);
  const advanceStep = useAnimationStore((s) => s.advanceStep);
  const getPosition = useAnimationStore((s) => s.getPosition);
  const particleRef = useRef<ParticleCanvasHandle>(null);
  const stepTimeoutsRef = useRef<ReturnType<typeof setTimeout>[]>([]);
  const [activeFloats, setActiveFloats] = useState<ActiveFloat[]>([]);
  const [activeDeathClones, setActiveDeathClones] = useState<DeathClone[]>([]);
  const [activeVignette, setActiveVignette] = useState<{
    damageAmount: number;
  } | null>(null);
  const [activeReveals, setActiveReveals] = useState<ActiveReveal[]>([]);
  const [activeShatters, setActiveShatters] = useState<ActiveShatter[]>([]);
  const [activeCastArcs, setActiveCastArcs] = useState<ActiveCastArc[]>([]);
  const [activeMillReveals, setActiveMillReveals] = useState<ActiveMillReveal[]>([]);

  const vfxQuality = usePreferencesStore((s) => s.vfxQuality);
  const speedMultiplier = usePreferencesStore((s) => s.animationSpeedMultiplier);

  const getObjectPosition = useCallback(
    (objectId: number): { x: number; y: number } | null => {
      // Fallback chain: pre-dispatch snapshot, then live registry, then the
      // group representative for a collapsed swarm member that has no node of
      // its own (resolved live via data-grouped-ids — see findCardElement).
      const rect =
        currentSnapshot.get(objectId) ??
        getPosition(objectId) ??
        findCardElement(objectId)?.getBoundingClientRect();
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

  const scheduleStepTimeout = useCallback((callback: () => void, delay: number) => {
    const timeout = setTimeout(() => {
      stepTimeoutsRef.current = stepTimeoutsRef.current.filter((id) => id !== timeout);
      callback();
    }, delay);
    stepTimeoutsRef.current.push(timeout);
  }, []);

  const processEffect = useCallback(
    (effect: StepEffect, stepEffects: StepEffect[]) => {
      const { event } = effect;

      switch (event.type) {
        case "GroupedDamageFlurry": {
          const { player_id, source_ids, total_damage, hit_count } = event.data;
          const to = getPlayerHudPosition(player_id);
          const fromPoints = source_ids
            .slice(0, DAMAGE_FLURRY_SOURCE_SAMPLE_LIMIT)
            .map((sourceId) => getObjectPosition(sourceId))
            .filter((position): position is { x: number; y: number } => position != null);
          const origins = fromPoints.length > 0 ? fromPoints : [to];
          const impactDelay = impactDelayMsForAnimationEvent(event) * speedMultiplier;

          if (vfxQuality !== "minimal") {
            particleRef.current?.damageFlurry(origins, to, hit_count, total_damage, impactDelay);
          }

          scheduleStepTimeout(() => {
            audioManager.playSfx("DamageDealt");
            const id = ++floatIdCounter;
            setActiveFloats((prev) => [
              ...prev,
              { id, value: -total_damage, position: to, color: "#ef4444" },
            ]);

            if (vfxQuality !== "minimal") {
              particleRef.current?.playerDamage(to.x, to.y, total_damage);
              setActiveVignette({ damageAmount: total_damage });
              scheduleStepTimeout(() => setActiveVignette(null), 500 * speedMultiplier);
            }

            if (vfxQuality === "full" && containerRef.current) {
              const intensity = total_damage >= 20 ? "heavy" : total_damage >= 10 ? "medium" : "light";
              applyScreenShake(containerRef.current, intensity, speedMultiplier);
            }
          }, impactDelay);
          break;
        }

        case "DamageDealt": {
          const { source_id, target, amount } = event.data;
          let pos = { x: window.innerWidth / 2, y: window.innerHeight / 2 };
          let isPlayerTarget = false;

          if ("Object" in target) {
            const objPos = getObjectPosition(target.Object);
            if (objPos) pos = objPos;
          } else if ("Player" in target) {
            isPlayerTarget = true;
            pos = getPlayerHudPosition(target.Player);
          }

          // Creature-on-creature: slam the actual card element (Arena-style)
          if ("Object" in target && vfxQuality !== "minimal") {
            // For bidirectional pairs (combat or fight), only slam the first direction.
            // The second direction still gets its floating damage number below.
            const effectIndex = stepEffects.indexOf(effect);
            const isPairedReturn = stepEffects.slice(0, effectIndex).some(
              (e) =>
                e.event.type === "DamageDealt" &&
                "Object" in e.event.data.target &&
                e.event.data.source_id === (target as { Object: number }).Object &&
                (e.event.data.target as { Object: number }).Object === source_id,
            );

            // Resolve via the group representative when this attacker is a
            // non-rendered member of a collapsed swarm (findCardElement).
            const sourceEl = isPairedReturn ? null : findCardElement(source_id);
            const slammed = sourceEl
              ? applyCardSlam(sourceEl, pos.x, pos.y, speedMultiplier, () => {
                  // Impact effects: SFX, shockwave, floating number, screen shake
                  audioManager.playSfx("DamageDealt");
                  particleRef.current?.slamImpact(pos.x, pos.y, amount);

                  const id = ++floatIdCounter;
                  setActiveFloats((prev) => [
                    ...prev,
                    { id, value: -amount, position: pos, color: "#ef4444" },
                  ]);

                  if (vfxQuality === "full" && containerRef.current) {
                    const intensity = amount >= 7 ? "heavy" : amount >= 4 ? "medium" : "light";
                    applyScreenShake(containerRef.current, intensity, speedMultiplier);
                  }
                })
              : false;

            if (!slammed) {
              // Paired return, missing source element, or representative already
              // mid-slam: show the floating damage number (+ SFX) without a slam.
              audioManager.playSfx("DamageDealt");
              const floatId = ++floatIdCounter;
              setActiveFloats((prev) => [
                ...prev,
                { id: floatId, value: -amount, position: pos, color: "#ef4444" },
              ]);
            }
            break;
          }

          // Creature-to-player: card slam at the player HUD
          {
            // Resolve via the group representative for a collapsed swarm member.
            const sourceEl = vfxQuality !== "minimal" ? findCardElement(source_id) : null;
            const slammed = sourceEl
              ? applyCardSlam(sourceEl, pos.x, pos.y, speedMultiplier, () => {
                  audioManager.playSfx("DamageDealt");
                  particleRef.current?.playerDamage(pos.x, pos.y, amount);

                  const fid = ++floatIdCounter;
                  setActiveFloats((prev) => [
                    ...prev,
                    { id: fid, value: -amount, position: pos, color: "#ef4444" },
                  ]);

                  if (vfxQuality === "full" && containerRef.current) {
                    const intensity = amount >= 7 ? "heavy" : amount >= 4 ? "medium" : "light";
                    applyScreenShake(containerRef.current, intensity, speedMultiplier);
                  }

                  if (isPlayerTarget) {
                    setActiveVignette({ damageAmount: amount });
                    setTimeout(() => setActiveVignette(null), 500 * speedMultiplier);
                  }
                })
              : false;

            if (!slammed) {
              audioManager.playSfx("DamageDealt");
              const fid = ++floatIdCounter;
              setActiveFloats((prev) => [
                ...prev,
                { id: fid, value: -amount, position: pos, color: "#ef4444" },
              ]);
            }
          }
          break;
        }

        case "LifeChanged": {
          const { player_id, amount } = event.data;

          // Skip floating number when DamageDealt already covers this player
          // in the same step (avoids duplicate floating numbers)
          const hasDamageDealt = stepEffects.some(
            (e) => isPlayerDamageAnimationEvent(e.event, player_id),
          );
          const groupedDamageEvent = effect.displayOnly
            ? stepEffects.find((e) => e.event.type === "GroupedDamageFlurry")
            : undefined;
          const showLifeChange = () => {
            const { x, y } = getPlayerHudPosition(player_id);
            if (!hasDamageDealt) {
              const id = ++floatIdCounter;
              setActiveFloats((prev) => [
                ...prev,
                { id, value: amount, position: { x, y }, color: amount > 0 ? "#22c55e" : "#ef4444" },
              ]);
            }

            if (amount > 0 && vfxQuality !== "minimal") {
              particleRef.current?.healEffect(x, y, amount);
            }
          };

          if (groupedDamageEvent) {
            scheduleStepTimeout(
              showLifeChange,
              impactDelayMsForAnimationEvent(groupedDamageEvent.event) * speedMultiplier,
            );
          } else {
            showLifeChange();
          }
          break;
        }

        case "CreatureDestroyed":
        case "PermanentSacrificed": {
          const { object_id } = event.data;
          const pos = getObjectPosition(object_id);
          if (pos && vfxQuality !== "minimal") {
            const gameState = useGameStore.getState().gameState;
            const colors = gameState?.objects[object_id]?.color ?? [];
            const explosionColor = colors.length > 0 ? hexToRgb(getCardColors(colors)[0]) : undefined;
            particleRef.current?.explosion(pos.x, pos.y, explosionColor);
          }

          const snapshotRect = currentSnapshot.get(object_id);
          const registryRect = getPosition(object_id);
          const rect = snapshotRect ?? registryRect;
          if (rect) {
            const gameState = useGameStore.getState().gameState;
            const cardName = gameState?.objects[object_id]?.name ?? "Unknown";

            if (vfxQuality !== "minimal" && event.type === "CreatureDestroyed") {
              const shatterId = ++shatterIdCounter;
              fetchCardImageUrl(cardName, 0, "art_crop")
                .then((url) => {
                  setActiveShatters((prev) => [
                    ...prev,
                    { id: shatterId, position: { x: rect.x, y: rect.y, width: rect.width, height: rect.height }, imageUrl: url },
                  ]);
                })
                .catch(() => {
                  setActiveDeathClones((prev) => [...prev, { id: object_id, position: rect, cardName }]);
                });
            } else {
              setActiveDeathClones((prev) => [...prev, { id: object_id, position: rect, cardName }]);
            }
          }
          break;
        }

        case "SpellCast": {
          const { object_id } = event.data;
          const pos = getObjectPosition(object_id);
          if (pos) {
            const gameState = useGameStore.getState().gameState;
            const colors = gameState?.objects[object_id]?.color ?? [];
            const burstColor = getCardColors(colors)[0] ?? "#06b6d4";
            if (vfxQuality !== "minimal") {
              particleRef.current?.spellImpact(pos.x, pos.y, hexToRgb(burstColor));
              const cardName = gameState?.objects[object_id]?.name ?? "";
              const stackPos = { x: window.innerWidth * 0.75, y: window.innerHeight * 0.4 };
              const id = ++castArcIdCounter;
              setActiveCastArcs((prev) => [...prev, { id, from: pos, to: stackPos, cardName, mode: "cast" }]);
            }
          }
          break;
        }

        case "TurnStarted":
          // Handled directly in dispatch.ts via uiStore.flashTurnBanner
          break;

        case "ZoneChanged": {
          const { object_id, from: fromZone, to: toZone } = event.data;
          if (toZone === "Battlefield") {
            const pos = getObjectPosition(object_id);
            if (pos) {
              const gameState = useGameStore.getState().gameState;
              const colors = gameState?.objects[object_id]?.color ?? [];
              const id = ++revealIdCounter;
              setActiveReveals((prev) => [...prev, { id, position: pos, colors: getCardColors(colors) }]);

              if (vfxQuality !== "minimal") {
                const summonColor = colors.length > 0 ? hexToRgb(getCardColors(colors)[0]) : undefined;
                particleRef.current?.summonBurst(pos.x, pos.y, summonColor);

                if (fromZone === "Stack") {
                  const cardName = gameState?.objects[object_id]?.name ?? "";
                  const stackPos = { x: window.innerWidth * 0.75, y: window.innerHeight * 0.4 };
                  const arcId = ++castArcIdCounter;
                  setActiveCastArcs((prev) => [...prev, { id: arcId, from: stackPos, to: pos, cardName, mode: "resolve-permanent" }]);
                }
              }
            }
          } else if (fromZone === "Stack" && toZone === "Graveyard") {
            if (vfxQuality !== "minimal") {
              const gameState = useGameStore.getState().gameState;
              const cardName = gameState?.objects[object_id]?.name ?? "";
              const stackPos = { x: window.innerWidth * 0.75, y: window.innerHeight * 0.4 };
              const arcId = ++castArcIdCounter;
              setActiveCastArcs((prev) => [...prev, { id: arcId, from: stackPos, to: stackPos, cardName, mode: "resolve-spell" }]);
            }
          } else if (fromZone === "Library" && toZone === "Graveyard") {
            if (vfxQuality !== "minimal") {
              const oldState = useGameStore.getState().gameState;
              const newState = useAnimationStore.getState().animationNewState;
              const millCards: { objectId: number; cardName: string; colors: string[] }[] = [];
              for (const e of stepEffects) {
                if (e.event.type !== "ZoneChanged") continue;
                const d = e.event.data;
                if (d.from !== "Library" || d.to !== "Graveyard") continue;
                const obj = oldState?.objects[d.object_id] ?? newState?.objects[d.object_id];
                millCards.push({ objectId: d.object_id, cardName: obj?.name ?? "Unknown", colors: getCardColors(obj?.color ?? []) });
              }

              // Deduplicate: only process once per step (first Library→Graveyard event triggers the batch)
              if (object_id === millCards[0]?.objectId && millCards.length > 0) {
                const obj = oldState?.objects[object_id] ?? newState?.objects[object_id];
                const ownerId = obj?.owner ?? 0;

                const libEl = document.querySelector(`[data-library-pile="${ownerId}"]`);
                const gyEl = document.querySelector(`[data-graveyard-pile="${ownerId}"]`);
                const libRect = libEl?.getBoundingClientRect();
                const gyRect = gyEl?.getBoundingClientRect();

                const hudFallback = getPlayerHudPosition(ownerId);
                const fromPos = libRect
                  ? { x: libRect.x + libRect.width / 2, y: libRect.y + libRect.height / 2 }
                  : hudFallback;
                const toPos = gyRect
                  ? { x: gyRect.x + gyRect.width / 2, y: gyRect.y + gyRect.height / 2 }
                  : hudFallback;

                const id = ++millRevealIdCounter;
                setActiveMillReveals((prev) => [...prev, { id, cards: millCards, from: fromPos, to: toPos }]);
              }
            }
          }
          break;
        }

        case "TokenCreated": {
          const { object_id } = event.data;
          const pos = getObjectPosition(object_id);
          if (pos) {
            const gameState = useGameStore.getState().gameState;
            const colors = gameState?.objects[object_id]?.color ?? [];
            const id = ++revealIdCounter;
            setActiveReveals((prev) => [...prev, { id, position: pos, colors: getCardColors(colors) }]);

            if (vfxQuality !== "minimal") {
              const tokenColor = colors.length > 0 ? hexToRgb(getCardColors(colors)[0]) : undefined;
              particleRef.current?.summonBurst(pos.x, pos.y, tokenColor);
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
      scheduleStepTimeout,
    ],
  );

  // Process effects when activeStep changes, then advance after its duration
  useEffect(() => {
    if (!activeStep) return;

    for (const effect of activeStep.effects) {
      processEffect(effect, activeStep.effects);
    }

    const timer = setTimeout(advanceStep, activeStep.duration * speedMultiplier);
    return () => {
      clearTimeout(timer);
      for (const stepTimeout of stepTimeoutsRef.current) clearTimeout(stepTimeout);
      stepTimeoutsRef.current = [];
      setActiveVignette(null);
    };
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

  const handleMillRevealComplete = useCallback((id: number) => {
    setActiveMillReveals((prev) => prev.filter((m) => m.id !== id));
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

      {/* Mill reveal animations (z-45) */}
      {activeMillReveals.map((mill) => (
        <MillRevealAnimation
          key={`mill-${mill.id}`}
          cards={mill.cards}
          from={mill.from}
          to={mill.to}
          onComplete={() => handleMillRevealComplete(mill.id)}
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
