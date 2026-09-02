import { useRef } from "react";
import { Icon } from "../../../components/Icon";
import { usePresence } from "../../../components/Presence";
import type { SlashCommandItem } from "./composerOptions";
import styles from "./index.module.scss";

export function SlashCommandMenu(props: Readonly<{
  active_index: number;
  items: readonly SlashCommandItem[];
  menu_ref: React.RefObject<HTMLDivElement | null>;
  open: boolean;
  on_select: (command: SlashCommandItem) => void;
}>) {
  const presence = usePresence(props.open, 90);
  const retained_items_ref = useRef(props.items);
  if (props.open) retained_items_ref.current = props.items;
  if (!presence.mounted) return null;
  const items = props.open ? props.items : retained_items_ref.current;
  return (
    <div
      aria-hidden={presence.state === "exiting" ? true : undefined}
      className={styles.slash_menu}
      data-presence={presence.state}
      inert={presence.state === "exiting" ? true : undefined}
      onTransitionEnd={presence.onTransitionEnd}
      ref={props.menu_ref}
      role="listbox"
    >
      <strong>指令</strong>
      {items.map((command, index) => (
        <button
          aria-selected={index === props.active_index}
          disabled={Boolean(command.disabled_reason)}
          data-slash-index={index}
          key={command.name}
          onClick={() => props.on_select(command)}
          role="option"
          type="button"
        >
          <b>{command.name}</b>
          <span>{command.disabled_reason ?? command.description}</span>
        </button>
      ))}
    </div>
  );
}

export function SlashCommandHelp({ on_close, open }: Readonly<{ on_close: () => void; open: boolean }>) {
  const presence = usePresence(open, 90);
  if (!presence.mounted) return null;
  return (
    <div
      aria-hidden={presence.state === "exiting" ? true : undefined}
      className={styles.slash_help}
      data-presence={presence.state}
      inert={presence.state === "exiting" ? true : undefined}
      onTransitionEnd={presence.onTransitionEnd}
    >
      <strong>斜杠指令</strong>
      <span>↑↓ 选择 · Enter 确认 · Esc 关闭</span>
      <button aria-label="关闭指令帮助" onClick={on_close} type="button">
        <Icon name="x" size={14} />
      </button>
    </div>
  );
}
