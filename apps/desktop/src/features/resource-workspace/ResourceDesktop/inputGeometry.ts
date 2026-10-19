/** object-fit: contain 的实际画面区域；留白不映射为远端坐标。 */
export function desktopPoint(rect: Pick<DOMRect, "left" | "top" | "width" | "height">, width: number, height: number, clientX: number, clientY: number): { x: number; y: number } | null {
  if (width <= 0 || height <= 0 || rect.width <= 0 || rect.height <= 0) return null;
  const scale = Math.min(rect.width / width, rect.height / height);
  const contentWidth = width * scale;
  const contentHeight = height * scale;
  const x = (clientX - rect.left - (rect.width - contentWidth) / 2) / contentWidth;
  const y = (clientY - rect.top - (rect.height - contentHeight) / 2) / contentHeight;
  return Number.isFinite(x) && Number.isFinite(y) && x >= 0 && x <= 1 && y >= 0 && y <= 1 ? { x, y } : null;
}

export function desktopKey(code: string): boolean {
  return /^(Key[A-Z]|Digit[0-9]|F([1-9]|1[0-2]))$/.test(code) || new Set([
    "Backquote", "Minus", "Equal", "BracketLeft", "BracketRight", "Backslash", "Semicolon", "Quote", "Comma", "Period", "Slash", "Space", "Enter", "Tab", "Backspace", "Delete", "Insert", "Home", "End", "PageUp", "PageDown", "ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "ShiftLeft", "ShiftRight", "ControlLeft", "ControlRight", "AltLeft", "AltRight", "MetaLeft", "MetaRight", "CapsLock",
  ]).has(code);
}
