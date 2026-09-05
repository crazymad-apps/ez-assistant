import { useEffect, useRef } from "react";
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

  useEffect(() => {
    menu_ref.current?.querySelector<HTMLButtonElement>("button:not(:disabled)")?.focus();
    const close = (event: PointerEvent) => {
      if (event.target instanceof Node && !menu_ref.current?.contains(event.target)) on_close();
    };
    document.addEventListener("pointerdown", close, true);
    return () => {
      document.removeEventListener("pointerdown", close, true);
      if (opener_ref.current?.isConnected) opener_ref.current.focus();
    };
  }, [on_close]);

  return (
    <div
      className={styles.menu}
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.preventDefault();
          on_close();
        }
      }}
      ref={menu_ref}
      role="menu"
      style={{
        left: Math.min(props.location.x, Math.max(8, window.innerWidth - 212)),
        top: Math.min(props.location.y, Math.max(8, window.innerHeight - (props.items.length * 30 + 18))),
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
    </div>
  );
}
