/**
 * 浮层原语用 data-overlay-region 声明实际绘制区域：Dialog 标记遮罩，其他浮层标记内容。
 * 只读 Shell 提供的 portal，不扫描业务页面、不推测 class/z-index，也不改写浮层的显隐。
 * 这些区域按 Shell 的层级契约位于正文上方；Tooltip 即使 pointer-events:none 仍会遮挡网页，
 * 因而不能用 elementFromPoint 的鼠标命中结果代替视觉相交。退场期间保留区域直到卸载。
 */
export function browserIsOccluded(viewport: DOMRect, root: HTMLElement | null): boolean {
  if (!root) return false;
  for (const region of root.querySelectorAll<HTMLElement>("[data-overlay-region]")) {
    if (region.dataset.positionReady === "false") continue;
    const style = getComputedStyle(region);
    if (style.display === "none" || style.visibility === "hidden") continue;
    const rect = region.getBoundingClientRect();
    const left = Math.max(0, viewport.left, rect.left);
    const top = Math.max(0, viewport.top, rect.top);
    const right = Math.min(window.innerWidth, viewport.right, rect.right);
    const bottom = Math.min(window.innerHeight, viewport.bottom, rect.bottom);
    if (left < right && top < bottom) return true;
  }
  return false;
}
