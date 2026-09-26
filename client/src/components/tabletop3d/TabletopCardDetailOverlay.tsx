import { useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { AnimatePresence, motion, useReducedMotion } from "framer-motion";
import { useTranslation } from "react-i18next";

import type { ObjectId } from "../../adapter/types.ts";
import {
  useCardParseDetails,
  useEngineCardData,
} from "../../hooks/useEngineCardData.ts";
import { objectImageProps } from "../../services/cardImageLookup.ts";
import { useGameStore } from "../../stores/gameStore.ts";
import {
  getKeywordDisplayText,
  getKeywordIconClass,
  getKeywordName,
  getKeywordReminderText,
  sortKeywords,
} from "../../viewmodel/keywordProps.ts";
import { ManaFontIcon } from "../icons/ManaFontIcon.tsx";
import { RichLabel } from "../mana/RichLabel.tsx";
import { CardImage } from "../card/CardImage.tsx";
import { ParsedAbilitiesPanel } from "../card/CardPreview.tsx";
import { TabletopCardFace } from "./TabletopCardFace.tsx";

interface TabletopCardDetailOverlayProps {
  objectId: ObjectId | null;
  onClose: () => void;
}

type TabletopCardDetailView = "live" | "original" | "parse";

/**
 * Tabletop-style, screen-space inspection for a visible Three.js card. The live
 * card face and keyword list both read the engine-authored GameObject; this
 * overlay only chooses how those values are presented.
 */
export function TabletopCardDetailOverlay({
  objectId,
  onClose,
}: TabletopCardDetailOverlayProps) {
  const { t } = useTranslation("game");
  const object = useGameStore((state) =>
    objectId == null ? null : state.gameState?.objects[objectId] ?? null,
  );
  const closeButtonRef = useRef<HTMLButtonElement>(null);
  const shouldReduceMotion = useReducedMotion();
  const [view, setView] = useState<TabletopCardDetailView>("live");
  const activeView = object?.face_down ? "live" : view;
  const visibleKeywords = useMemo(
    () => object && !object.face_down ? sortKeywords(object.keywords) : [],
    [object],
  );
  const parseName = object && !object.face_down ? object.name : null;
  const parseDetails = useCardParseDetails(parseName);
  const engineFace = useEngineCardData(parseName);

  useEffect(() => setView("live"), [objectId]);

  useEffect(() => {
    if (!object) return undefined;

    const previousFocus = document.activeElement as HTMLElement | null;
    closeButtonRef.current?.focus();

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    document.addEventListener("keydown", handleKeyDown);

    return () => {
      document.removeEventListener("keydown", handleKeyDown);
      previousFocus?.focus();
    };
  }, [object, onClose]);

  if (typeof document === "undefined") return null;

  const displayName = object?.face_down
    ? t("card.faceDownName")
    : object?.name ?? "";

  return createPortal(
    <AnimatePresence>
      {object ? (
        <motion.div
          key={object.id}
          role="dialog"
          aria-modal="true"
          aria-label={t("preview.detailAriaLabel", { name: displayName })}
          className="game-no-select fixed inset-0 z-[140] flex items-center justify-center overflow-y-auto bg-black/80 px-4 py-5 backdrop-blur-[2px]"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ duration: shouldReduceMotion ? 0 : 0.16 }}
          data-tabletop-card-detail
          onPointerDown={(event) => {
            if (event.target === event.currentTarget) onClose();
          }}
          // The portal lives directly under document.body rather than inside
          // GamePage's protected game surface. Cancel WebKit's native long-
          // press callout here too, so inspection cannot turn into the iOS
          // text-selection loupe after the overlay mounts beneath the finger.
          onContextMenu={(event) => event.preventDefault()}
        >
          <button
            ref={closeButtonRef}
            type="button"
            aria-label={t("preview.closeDetails")}
            className="fixed right-[max(1rem,env(safe-area-inset-right))] top-[max(1rem,env(safe-area-inset-top))] z-10 grid h-11 w-11 place-items-center rounded-full border border-white/25 bg-black/70 text-2xl leading-none text-white shadow-xl backdrop-blur transition-colors hover:bg-white/15 focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-white"
            onClick={onClose}
          >
            <span aria-hidden>×</span>
          </button>

          <motion.div
            className="flex max-w-[min(94vw,980px)] flex-col items-center gap-3"
            initial={
              shouldReduceMotion
                ? false
                : { opacity: 0, scale: 0.96, y: 10 }
            }
            animate={{ opacity: 1, scale: 1, y: 0 }}
            exit={{ opacity: 0, scale: shouldReduceMotion ? 1 : 0.98 }}
            transition={{
              duration: shouldReduceMotion ? 0 : 0.2,
              ease: [0.22, 1, 0.36, 1],
            }}
            onPointerDown={(event) => event.stopPropagation()}
          >
            {!object.face_down ? (
              <div
                role="group"
                aria-label={t("preview.detailViewOptions")}
                className="flex rounded-full border border-white/15 bg-black/70 p-1 shadow-lg backdrop-blur"
              >
                {(["live", "original", "parse"] as const).map((option) => (
                  <button
                    key={option}
                    type="button"
                    aria-pressed={activeView === option}
                    className={`rounded-full px-3 py-1.5 text-xs font-semibold transition-colors sm:px-4 ${
                      activeView === option
                        ? "bg-stone-100 text-stone-950 shadow"
                        : "text-stone-300 hover:bg-white/10 hover:text-white"
                    }`}
                    onClick={() => setView(option)}
                  >
                    {t(`preview.detailView.${option}`)}
                  </button>
                ))}
              </div>
            ) : null}

            <div
              className="flex flex-col items-center justify-center gap-4 md:flex-row md:items-start md:gap-5"
              data-tabletop-card-detail-view={activeView}
            >
              {activeView === "parse" ? (
                <ParsedAbilitiesPanel
                  name={engineFace?.name ?? object.name}
                  cardTypes={engineFace?.card_type ?? object.card_types}
                  keywords={object.keywords}
                  localizedTypeLine={engineFace?.localized_type_line}
                  parseDetails={parseDetails}
                  maxHeight="min(76dvh, 680px)"
                />
              ) : activeView === "original" ? (
                <div
                  className="shrink-0"
                  style={{
                    "--card-h":
                      "min(100.8vw, calc(100dvh - 6rem), 602px)",
                    "--card-w":
                      "min(72vw, calc((100dvh - 6rem) * 5 / 7), 430px)",
                  } as React.CSSProperties}
                >
                  <CardImage
                    {...objectImageProps(object)}
                    faceDown={object.face_down}
                    tapIndicator={false}
                    className="!rounded-[4.4%/3.15%]"
                  />
                </div>
              ) : (
                <div
                  className="shrink-0"
                  style={{
                    width:
                      "min(72vw, calc((100dvh - 6rem) * 5 / 7), 430px)",
                    height:
                      "min(100.8vw, calc(100dvh - 6rem), 602px)",
                  }}
                  data-tabletop-live-card-detail-frame
                >
                  <TabletopCardFace
                    objectId={object.id}
                    mode="inspection"
                    className="!h-full !w-full"
                  />
                </div>
              )}

              {activeView !== "parse" && !object.face_down ? (
                <div
                  className="flex w-[min(86vw,380px)] max-h-[min(76dvh,680px)] flex-col gap-3 overflow-y-auto pr-1 md:pt-1"
                  data-tabletop-card-detail-info-rail
                >
                  {visibleKeywords.map((keyword, index) => {
                    const name = getKeywordName(keyword);
                    const iconClass = getKeywordIconClass(keyword);
                    const reminder = getKeywordReminderText(keyword);
                    return (
                      <motion.section
                        key={`${name}-${index}`}
                        className="rounded-lg border border-white/15 bg-[#151719]/95 px-3.5 py-3 text-white shadow-[0_10px_24px_rgba(0,0,0,0.34)]"
                        initial={shouldReduceMotion ? false : { opacity: 0, x: 10 }}
                        animate={{ opacity: 1, x: 0 }}
                        transition={{
                          delay: shouldReduceMotion ? 0 : 0.05 + index * 0.035,
                          duration: shouldReduceMotion ? 0 : 0.16,
                        }}
                      >
                        <div className="flex items-start gap-3">
                          <span className="grid h-8 w-8 shrink-0 place-items-center rounded-md bg-white/10 text-xl text-stone-100 ring-1 ring-white/15">
                            {iconClass ? (
                              <ManaFontIcon
                                iconClass={iconClass}
                                fallbackText="◆"
                                label={name}
                              />
                            ) : (
                              <span aria-hidden className="text-sm">◆</span>
                            )}
                          </span>
                          <div className="min-w-0">
                            <RichLabel
                              text={getKeywordDisplayText(keyword)}
                              className="block font-semibold leading-tight text-stone-100"
                            />
                            {reminder ? (
                              <RichLabel
                                text={reminder}
                                size="sm"
                                className="mt-1 block leading-snug text-stone-300"
                              />
                            ) : null}
                          </div>
                        </div>
                      </motion.section>
                    );
                  })}
                  <ParsedAbilitiesPanel
                    name={engineFace?.name ?? object.name}
                    cardTypes={engineFace?.card_type ?? object.card_types}
                    keywords={object.keywords}
                    localizedTypeLine={engineFace?.localized_type_line}
                    parseDetails={parseDetails}
                    maxHeight="min(42dvh, 360px)"
                    className="!w-full shrink-0"
                  />
                </div>
              ) : null}
            </div>
          </motion.div>
        </motion.div>
      ) : null}
    </AnimatePresence>,
    document.body,
  );
}
