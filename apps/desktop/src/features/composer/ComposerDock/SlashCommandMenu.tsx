import { Icon } from "../../../components/Icon";
import type { SlashCommandItem } from "./composerOptions";
import styles from "./index.module.scss";

export function SlashCommandMenu(props: Readonly<{
  active_index: number;
  items: readonly SlashCommandItem[];
  menu_ref: React.RefObject<HTMLDivElement | null>;
  on_select: (command: SlashCommandItem) => void;
}>) {
  return (
    <div className={styles.slash_menu} ref={props.menu_ref} role="listbox">
      <strong>指令</strong>
      {props.items.map((command, index) => (
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

export function SlashCommandHelp({ on_close }: Readonly<{ on_close: () => void }>) {
  return (
    <div className={styles.slash_help}>
      <strong>斜杠指令</strong>
      <span>↑↓ 选择 · Enter 确认 · Esc 关闭</span>
      <button aria-label="关闭指令帮助" onClick={on_close} type="button">
        <Icon name="x" size={14} />
      </button>
    </div>
  );
}
