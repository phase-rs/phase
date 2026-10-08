/**
 * Use pointer coordinates when available and the launcher's visual center for
 * keyboard or assistive-technology clicks, whose synthetic coordinates are 0.
 */
export function debugContextMenuPoint(
  launcher: HTMLElement,
  clientX: number,
  clientY: number,
): { x: number; y: number } {
  if (clientX !== 0 || clientY !== 0) return { x: clientX, y: clientY };

  const bounds = launcher.getBoundingClientRect();
  return {
    x: bounds.left + bounds.width / 2,
    y: bounds.top + bounds.height / 2,
  };
}
