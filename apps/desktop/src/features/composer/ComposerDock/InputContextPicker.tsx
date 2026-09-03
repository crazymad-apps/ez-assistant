import { useEffect, useId, useMemo, useRef, useState } from "react";
import { Icon } from "../../../components/Icon";
import { useInputMethodGuard } from "../../../components/InputMethodGuard";
import { usePresence } from "../../../components/Presence";
import styles from "./index.module.scss";

export function InputContextPicker<T extends { name: string; description: string }>(props: Readonly<{
  options: readonly T[];
  label: string;
  loading?: boolean;
  error?: string | null;
  on_retry?: () => void;
  empty_action?: Readonly<{ label: string; on_select: () => void }>;
  option_label?: (option: T) => string;
  on_close: () => void;
  open: boolean;
  on_select: (option: T) => void;
}>) {
  const [query, setQuery] = useState("");
  const [active_index, setActiveIndex] = useState(0);
  const input_ref = useRef<HTMLInputElement>(null);
  const listbox_id = useId();
  const input_method = useInputMethodGuard();
  const presence = usePresence(props.open, 90);
  const results = useMemo(() => {
    const normalized = query.trim().toLocaleLowerCase();
    return props.options.filter((skill) => !normalized
      || skill.name.toLocaleLowerCase().includes(normalized)
      || props.option_label?.(skill).toLocaleLowerCase().includes(normalized)
      || skill.description.toLocaleLowerCase().includes(normalized));
  }, [props.options, props.option_label, query]);

  useEffect(() => {
    if (props.open) input_ref.current?.focus();
  }, [props.open]);
  useEffect(() => setActiveIndex(0), [query]);
  useEffect(() => {
    document.getElementById(`${listbox_id}-${active_index}`)?.scrollIntoView?.({ block: "nearest" });
  }, [active_index, listbox_id]);

  function handleKeyDown(event: React.KeyboardEvent<HTMLInputElement>) {
    if (input_method.shouldIgnoreKeyDown(event)) return;
    if (event.key === "Escape") {
      event.preventDefault();
      props.on_close();
    } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      if (results.length > 0) {
        setActiveIndex((current) => (current + (event.key === "ArrowDown" ? 1 : -1) + results.length) % results.length);
      }
    } else if (event.key === "Enter" && results[active_index]) {
      event.preventDefault();
      props.on_select(results[active_index]);
    }
  }

  if (!presence.mounted) return null;
  return (
    <div
      aria-hidden={presence.state === "exiting" ? true : undefined}
      aria-label={`选择${props.label}`}
      className={styles.skill_picker}
      data-presence={presence.state}
      inert={presence.state === "exiting" ? true : undefined}
      onTransitionEnd={presence.onTransitionEnd}
      role="dialog"
    >
      <header>
        <Icon name="search" size={15} />
        <input
          aria-activedescendant={results[active_index] ? `${listbox_id}-${active_index}` : undefined}
          aria-controls={listbox_id}
          aria-expanded={props.open}
          aria-label={`搜索${props.label}`}
          onChange={(event) => setQuery(event.target.value)}
          onCompositionEnd={input_method.onCompositionEnd}
          onCompositionStart={input_method.onCompositionStart}
          onKeyDown={handleKeyDown}
          onKeyUp={input_method.onKeyUp}
          placeholder={`搜索${props.label}名称或描述`}
          ref={input_ref}
          role="combobox"
          value={query}
        />
        <button aria-label={`关闭${props.label}选择`} onClick={props.on_close} type="button"><Icon name="x" size={14} /></button>
      </header>
      <div aria-label={props.label} id={listbox_id} role="listbox">
        {results.map((skill, index) => (
          <button
            aria-selected={index === active_index}
            key={skill.name}
            id={`${listbox_id}-${index}`}
            onClick={() => props.on_select(skill)}
            onMouseEnter={() => setActiveIndex(index)}
            role="option"
            type="button"
          >
            <strong>{props.option_label?.(skill) ?? skill.name}</strong>
            <span>{skill.description || "暂无描述"}</span>
          </button>
        ))}
      </div>
      {props.loading && <p role="status">正在读取{props.label}…</p>}
      {props.error && <p role="alert">{props.error}<button onClick={props.on_retry} type="button">重试</button></p>}
      {!props.loading && !props.error && results.length === 0 && <p>
        没有匹配的{props.label}。
        {props.empty_action && <button onClick={props.empty_action.on_select} type="button">{props.empty_action.label}</button>}
      </p>}
    </div>
  );
}
