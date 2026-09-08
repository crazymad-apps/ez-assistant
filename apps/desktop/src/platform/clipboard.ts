/** Clipboard API 在普通 HTTP 下可能不可用；优先保留浏览器的复制能力。 */
export async function copyText(value: string): Promise<void> {
  if (navigator.clipboard?.writeText) {
    try {
      await navigator.clipboard.writeText(value);
      return;
    } catch {
      /* 回退到用户触发的复制 */
    }
  }
  const previous =
    document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null;
  const field = document.createElement("textarea");
  field.value = value;
  field.readOnly = true;
  field.setAttribute("aria-label", "待复制内容");
  field.style.cssText =
    "position:fixed;left:0;top:0;width:1px;height:1px;opacity:0";
  document.body.append(field);
  field.select();
  try {
    if (!document.execCommand?.("copy"))
      throw new Error(`自动复制不可用，请手动选择复制：${value}`);
  } finally {
    field.remove();
    previous?.focus();
  }
}
