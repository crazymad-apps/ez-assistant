import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import styles from "./ResourceContextMenu.module.scss";

export type ResourceMenuLocation = Readonly<{ x: number; y: number }>;

export type ResourceMenuItem = Readonly<{
  disabled?: boolean;
  label: string;
  on_select: () => void;
}>;

export function ResourceContextMenu(props: Readonly<{
  items: readonly ResourceMenuItem[];
  location: ResourceMenuLocation;
  on_close: () => void;
}>) {
  const menu_ref = useRef<HTMLDivElement>(null);
  const opener_ref = useRef<HTMLElement | null>(
    document.activeElement instanceof HTMLElement ? document.activeElement : null,
  );
  const { on_close } = props;
  const [position, setPosition] = useState({ left: 0, top: 0, ready: false });

  useLayoutEffect(() => {
    const menu = menu_ref.current;
    if (!menu) return;
    const update = () => {
      const bounds = menu.getBoundingClientRect();
      setPosition({
        left: Math.max(8, Math.min(props.location.x, window.innerWidth - bounds.width - 8)),
        top: Math.max(8, Math.min(props.location.y, window.innerHeight - bounds.height - 8)),
        ready: true,
      });
    };
    update();
    const observer = new ResizeObserver(update);
    observer.observe(menu);
    window.addEventListener("resize", update);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", update);
    };
  }, [props.location.x, props.location.y]);

  useEffect(() => {
    menu_ref.current?.querySelector<HTMLButtonElement>("button:not(:disabled)")?.focus({ preventScroll: true });
    const close = (event: PointerEvent) => {
      if (event.target instanceof Node && !menu_ref.current?.contains(event.target)) on_close();
    };
    // 菜单绑定的是打开时的视口坐标，正文滚动后关闭，避免留在已经移走的资源旁。
    const closeOnScroll = (event: Event) => {
      if (!(event.target instanceof Node) || !menu_ref.current?.contains(event.target)) on_close();
    };
    document.addEventListener("pointerdown", close, true);
    document.addEventListener("scroll", closeOnScroll, true);
    return () => {
      document.removeEventListener("pointerdown", close, true);
      document.removeEventListener("scroll", closeOnScroll, true);
      if (opener_ref.current?.isConnected) opener_ref.current.focus({ preventScroll: true });
    };
  }, [on_close]);

  // 消息的 content-visibility 会建立 fixed 定位包含块；Portal 保证 clientX/Y 始终对应视口。
  return createPortal(
    <div
      className={styles.menu}
      data-overlay-region="floating"
      data-position-ready={position.ready}
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.preventDefault();
          on_close();
        }
      }}
      ref={menu_ref}
      role="menu"
      style={{
        left: position.left,
        top: position.top,
      }}
    >
      {props.items.map((item) => (
        <button
          disabled={item.disabled}
          key={item.label}
          onClick={() => {
            item.on_select();
            on_close();
          }}
          role="menuitem"
          type="button"
        >
          {item.label}
        </button>
      ))}
    </div>,
    document.querySelector("#overlay-root") ?? document.body,
  );
}
