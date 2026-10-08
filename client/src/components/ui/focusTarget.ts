export type FocusTarget = HTMLElement | SVGElement;

export function isRendered(
  element: Element,
  container: Element | null = null,
): boolean {
  for (
    let current: Element | null = element;
    current;
    current = current.parentElement
  ) {
    const style = window.getComputedStyle(current);
    if (
      current.hasAttribute("hidden") ||
      current.hasAttribute("inert") ||
      current.getAttribute("aria-hidden") === "true" ||
      style.display === "none" ||
      style.visibility === "hidden" ||
      style.visibility === "collapse" ||
      style.contentVisibility === "hidden"
    ) {
      return false;
    }

    if (current instanceof HTMLDetailsElement && !current.open) {
      const summary = Array.from(current.children).find(
        (child) => child.tagName === "SUMMARY",
      );
      if (!summary?.contains(element)) return false;
    }
    if (current === container) break;
  }
  return true;
}

export function isEffectivelyDisabled(element: Element): boolean {
  if (element.matches(":disabled")) return true;

  for (
    let ancestor = element.parentElement;
    ancestor;
    ancestor = ancestor.parentElement
  ) {
    if (
      ancestor.tagName !== "FIELDSET" ||
      !ancestor.hasAttribute("disabled")
    ) {
      continue;
    }
    const firstLegend = Array.from(ancestor.children).find(
      (child) => child.tagName === "LEGEND",
    );
    if (!firstLegend?.contains(element)) return true;
  }
  return false;
}

export function isFocusTargetAvailable(
  target: FocusTarget | null | undefined,
): target is FocusTarget {
  return Boolean(
    target?.isConnected &&
      !(target instanceof HTMLInputElement && target.type === "hidden") &&
      !isEffectivelyDisabled(target) &&
      isRendered(target),
  );
}
