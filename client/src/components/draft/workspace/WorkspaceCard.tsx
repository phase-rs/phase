import {
  useState,
  type CSSProperties,
  type KeyboardEvent,
  type PointerEvent,
  type RefCallback,
} from "react";
import { useTranslation } from "react-i18next";

import type { DraftCardInstance } from "../../../adapter/draft-adapter";
import { useDraftCardFace } from "../DraftCardFace.tsx";
import type { WorkspaceCardEntryModel } from "./workspacePlacement";
import type {
  DraftWorkspaceDragController,
  WorkspaceDragOrigin,
  WorkspaceDragSource,
  WorkspaceDragPreviewImage,
} from "./useDraftWorkspaceDrag";

const STACK_EXPOSED_WIDTH_RATIO = 0.16;

export interface WorkspaceCardDragCapability {
  controller: DraftWorkspaceDragController;
  touchDragEnabled?: boolean;
  touchScrollEnabled?: boolean;
  makeSource(
    card: WorkspaceCardEntryModel,
    width: number,
    height: number,
    previewImage: WorkspaceDragPreviewImage,
    origin: WorkspaceDragOrigin,
  ): WorkspaceDragSource;
}

export interface WorkspaceCardProps {
  card: WorkspaceCardEntryModel;
  stackIndex: number;
  interactionLocked?: boolean;
  registerCard?: RefCallback<HTMLButtonElement>;
  onHover?(card: WorkspaceCardEntryModel | null): void;
  onBlur?(): void;
  onActivate(card: WorkspaceCardEntryModel): void;
  onDoubleClick?(
    event: React.MouseEvent<HTMLButtonElement>,
    card: WorkspaceCardEntryModel,
  ): void;
  onKeyDown?(
    event: KeyboardEvent<HTMLButtonElement>,
    card: WorkspaceCardEntryModel,
  ): void;
  drag?: WorkspaceCardDragCapability;
  stackStyle?: CSSProperties;
  dragHidden?: boolean;
}

export interface WorkspaceDragProjection {
  readonly card: DraftCardInstance;
  readonly sourceInstanceId: string;
  readonly column: number;
  readonly row: number | null;
  readonly origin: WorkspaceDragOrigin;
  readonly previewImage?: WorkspaceDragPreviewImage;
}

export function WorkspaceDragProjectionSlot({ sourceInstanceId, stackIndex, placementStyle }: {
  sourceInstanceId: string;
  stackIndex: number;
  placementStyle?: CSSProperties;
}) {
  return (
    <div
      aria-hidden="true"
      data-drag-projection-slot={sourceInstanceId}
      className="pointer-events-none relative min-w-0 invisible"
      style={placementStyle ?? { marginTop: stackIndex === 0 ? undefined : "-123.3442622951%" }}
    >
      <span className="block aspect-[488/680] w-full" />
    </div>
  );
}

export function WorkspaceCard({
  card,
  stackIndex,
  interactionLocked = false,
  registerCard,
  onHover,
  onBlur,
  onActivate,
  onDoubleClick,
  onKeyDown,
  drag,
  stackStyle,
  dragHidden = false,
}: WorkspaceCardProps) {
  const { t } = useTranslation("draft");
  const { src, displayName, hasAlternateFace, toggleFace, advanceFailedSource } = useDraftCardFace(
    card.image.cardName,
    card.image.sourcePrinting,
  );
  const [hoverRevealed, setHoverRevealed] = useState(false);
  const updateHoverReveal = (event: PointerEvent<HTMLButtonElement>) => {
    if (event.pointerType === "touch") return;
    const rect = event.currentTarget.getBoundingClientRect();
    setHoverRevealed(event.clientY - rect.top <= rect.width * STACK_EXPOSED_WIDTH_RATIO);
  };

  return (
    <div
      className={`${dragHidden ? "pointer-events-none absolute inset-x-0 top-0 w-full opacity-0" : "relative"} min-w-0 ${hoverRevealed ? "z-10" : ""}`}
      style={dragHidden
        ? { ...(stackStyle ?? {}), marginTop: undefined }
        : stackStyle ?? { marginTop: stackIndex === 0 ? undefined : "-123.3442622951%" }}
      data-instance-id={card.instanceId}
    >
      <button
        ref={registerCard}
        type="button"
        disabled={interactionLocked}
        className={`block w-full overflow-hidden rounded-md bg-neutral-900 text-left shadow-lg focus-visible:z-10 focus-visible:outline-2 focus-visible:outline-amber-300 ${drag?.touchDragEnabled ? drag.touchScrollEnabled ? "touch-pan-y" : "touch-none" : ""}`}
        aria-label={t("workspace.card.inspect", { card: card.name })}
        onMouseEnter={() => onHover?.(card)}
        onMouseLeave={() => {
          setHoverRevealed(false);
          onHover?.(null);
        }}
        onFocus={() => onHover?.(card)}
        onBlur={() => onBlur?.()}
        onClick={(event) => {
          const pointerEvent = event.nativeEvent as MouseEvent & { pointerId?: number; pointerType?: string };
          if (drag?.controller.consumeCompatibilityActivation({
            kind: "click",
            detail: event.detail,
            pointerId: pointerEvent.pointerId ?? null,
            ...(pointerEvent.pointerType === undefined ? {} : { pointerType: pointerEvent.pointerType }),
            surface: "workspace",
            sourceInstanceId: card.instanceId,
          })) return;
          onActivate(card);
        }}
        onDoubleClick={(event) => {
          const pointerEvent = event.nativeEvent as MouseEvent & { pointerId?: number; pointerType?: string };
          if (drag?.controller.consumeCompatibilityActivation({
            kind: "double-click",
            detail: event.detail,
            pointerId: pointerEvent.pointerId ?? null,
            ...(pointerEvent.pointerType === undefined ? {} : { pointerType: pointerEvent.pointerType }),
            surface: "workspace",
            sourceInstanceId: card.instanceId,
          })) return;
          onDoubleClick?.(event, card);
        }}
        onKeyDown={(event) => onKeyDown?.(event, card)}
        onPointerDown={(event) => {
          if (drag !== undefined) {
            const rect = event.currentTarget.getBoundingClientRect();
            const origin = { left: rect.left, top: rect.top, width: rect.width, height: rect.height };
            const source = drag.makeSource(card, rect.width, rect.height, { src, alt: displayName }, origin);
            if (drag.touchDragEnabled) {
              drag.controller.handleWorkspacePointerDown(event, source, true);
            } else {
              drag.controller.handleWorkspacePointerDown(event, source);
            }
          }
        }}
        onPointerEnter={updateHoverReveal}
        onPointerMove={(event) => {
          updateHoverReveal(event);
          drag?.controller.handlePointerMove(event);
        }}
        onPointerUp={drag?.controller.handlePointerUp}
        onPointerCancel={drag?.controller.handlePointerCancel}
        onLostPointerCapture={drag?.controller.handleLostPointerCapture}
      >
        {src === null ? (
          <span className="flex aspect-[488/680] items-start px-2 py-1 text-xs text-white/80">
            {card.name}
          </span>
        ) : (
          <img
            key={src}
            src={src}
            alt={displayName}
            draggable={card.image.draggable}
            className="aspect-[488/680] w-full object-cover"
            onError={(event) => {
              const failedSrc = event.currentTarget.getAttribute("src");
              if (failedSrc !== null) advanceFailedSource?.(failedSrc);
            }}
          />
        )}
      </button>
      {hasAlternateFace && (
        <button
          type="button"
          disabled={interactionLocked}
          aria-label={`Show other face of ${card.name}`}
          data-card-face-toggle
          className="absolute -right-2 -top-2 rounded bg-black/70 px-1.5 py-1 text-xs text-white hover:bg-black focus-visible:outline-2 focus-visible:outline-amber-300"
          onClick={(event) => {
            event.preventDefault();
            event.stopPropagation();
            toggleFace();
          }}
          onPointerDown={(event) => event.stopPropagation()}
        >
          ↻
        </button>
      )}
    </div>
  );
}
