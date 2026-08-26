import { AdapterError } from "../adapter/types";
import i18n from "../i18n";
import { useAppNotificationStore, type AppNotificationAnchor } from "../stores/appToastStore";
import { objectAnchorSelector } from "../utils/objectAnchorSelector";

const TOAST_MAX_WIDTH_PX = 352;
const VIEWPORT_INSET_PX = 16;
const TARGET_GAP_PX = 12;
const TOAST_HEIGHT_BUDGET_PX = 144;

export type StructuredRejectionReport = "not-structured" | "stale" | "reported";

function genericActionFailureTitle(): string {
  return i18n.t("actionError.title", { action: i18n.t("actionError.genericAction") });
}

function clamp(value: number, min: number, max: number): number {
  return Math.min(Math.max(value, min), max);
}

function actionRejectionAnchor(err: AdapterError): AppNotificationAnchor | undefined {
  if (!err.rejection || typeof document === "undefined" || typeof window === "undefined") {
    return undefined;
  }

  for (const objectId of err.rejection.related_object_ids) {
    const anchor = document.querySelector<HTMLElement>(objectAnchorSelector(objectId));
    if (!anchor) continue;

    const rect = anchor.getBoundingClientRect();
    const viewportWidth = Math.max(0, window.innerWidth);
    const toastWidth = Math.min(
      TOAST_MAX_WIDTH_PX,
      Math.max(0, viewportWidth - VIEWPORT_INSET_PX * 2),
    );
    const halfToastWidth = Math.max(0, toastWidth / 2);
    const leftBound = Math.min(
      VIEWPORT_INSET_PX + halfToastWidth,
      viewportWidth - VIEWPORT_INSET_PX - halfToastWidth,
    );
    const rightBound = Math.max(
      VIEWPORT_INSET_PX + halfToastWidth,
      viewportWidth - VIEWPORT_INSET_PX - halfToastWidth,
    );
    const x = clamp(
      rect.left + rect.width / 2,
      leftBound,
      rightBound,
    );
    const placement = rect.top >= TOAST_HEIGHT_BUDGET_PX + VIEWPORT_INSET_PX + TARGET_GAP_PX
      ? "above"
      : "below";

    return {
      x,
      y: placement === "above" ? rect.top - TARGET_GAP_PX : rect.bottom + TARGET_GAP_PX,
      placement,
    };
  }

  return undefined;
}

/**
 * Presents an engine-owned rejection consistently for every direct action
 * submitter. The engine controls both the text and related object ordering;
 * this module only finds the first matching rendered anchor.
 */
export function reportStructuredActionRejection(
  err: unknown,
  title: string = genericActionFailureTitle(),
): StructuredRejectionReport {
  if (!(err instanceof AdapterError) || !err.rejection) return "not-structured";
  if (err.rejection.disposition === "stale") return "stale";

  const anchor = actionRejectionAnchor(err);
  useAppNotificationStore.getState().showNotification({
    title,
    description: err.rejection.message,
    ...(anchor ? { anchor } : {}),
  });
  return "reported";
}
