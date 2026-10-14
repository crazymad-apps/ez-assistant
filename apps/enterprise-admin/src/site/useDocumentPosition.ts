import { useEffect, useState } from 'react';
import { useLocation } from 'react-router';

/** 等异步正文落到 DOM 后再定位深链接，避免首次加载丢失锚点。 */
export function useDocumentPosition(
  content: unknown,
  { pageTop = false, behavior = 'instant' }: { pageTop?: boolean; behavior?: ScrollBehavior } = {},
) {
  const location = useLocation();
  const key = location.pathname + location.hash;
  const [missing, setMissing] = useState<string>();
  useEffect(() => {
    if (!content) return;
    // 普通页面无锚点时由页面壳保持页顶，不能在内容加载后再次滚到主标题。
    if (!location.hash && !pageTop) return;
    const frame = requestAnimationFrame(() => {
      let id = 'main';
      try {
        id = decodeURIComponent(location.hash.slice(1)) || 'main';
      } catch {
        /* 畸形锚点回到正文 */
      }
      const anchor = document.getElementById(id);
      setMissing(!anchor && location.hash ? key : undefined);
      const main = document.getElementById('main');
      const target = id === 'main' || !anchor ? (main?.querySelector('h1') ?? main) : anchor;
      if (target) {
        const scrollBehavior = window.matchMedia('(prefers-reduced-motion: reduce)').matches ? 'instant' : behavior;
        // 手册文章切换回到整页顶部，段落深链接仍精确定位；焦点不触发第二次滚动。
        if (pageTop && (id === 'main' || !anchor)) window.scrollTo({ top: 0, behavior: scrollBehavior });
        else target.scrollIntoView({ behavior: scrollBehavior, block: 'start' });
        target.setAttribute('tabindex', '-1');
        target.focus({ preventScroll: true });
      }
    });
    return () => cancelAnimationFrame(frame);
  }, [location.pathname, location.hash, content, key, pageTop, behavior]);
  return missing === key;
}
