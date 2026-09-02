import { useEffect, useMemo, useRef, useState } from "react";
import type { SkillSummarySnapshot } from "../../../generated/assistant-protocol";
import { Icon } from "../../../components/Icon";
import { useInputMethodGuard } from "../../../components/InputMethodGuard";
import { usePresence } from "../../../components/Presence";
import styles from "./index.module.scss";

export function SkillPicker(props: Readonly<{
  skills: readonly SkillSummarySnapshot[];
  on_close: () => void;
  open: boolean;
  on_select: (skill: SkillSummarySnapshot) => void;
}>) {
  const [query, setQuery] = useState("");
  const [active_index, setActiveIndex] = useState(0);
  const input_ref = useRef<HTMLInputElement>(null);
  const input_method = useInputMethodGuard();
  const presence = usePresence(props.open, 90);
  const results = useMemo(() => {
    const normalized = query.trim().toLocaleLowerCase();
    return props.skills.filter((skill) => !normalized
      || skill.name.toLocaleLowerCase().includes(normalized)
      || skill.description.toLocaleLowerCase().includes(normalized));
  }, [props.skills, query]);

  useEffect(() => {
    if (props.open) input_ref.current?.focus();
  }, [props.open]);
  useEffect(() => setActiveIndex(0), [query]);

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
      aria-label="选择技能"
      className={styles.skill_picker}
      data-presence={presence.state}
      inert={presence.state === "exiting" ? true : undefined}
      onTransitionEnd={presence.onTransitionEnd}
      role="dialog"
    >
      <header>
        <Icon name="search" size={15} />
        <input
          aria-label="搜索技能"
          onChange={(event) => setQuery(event.target.value)}
          onCompositionEnd={input_method.onCompositionEnd}
          onCompositionStart={input_method.onCompositionStart}
          onKeyDown={handleKeyDown}
          onKeyUp={input_method.onKeyUp}
          placeholder="搜索技能名称或描述"
          ref={input_ref}
          value={query}
        />
        <button aria-label="关闭技能选择" onClick={props.on_close} type="button"><Icon name="x" size={14} /></button>
      </header>
      <div role="listbox">
        {results.map((skill, index) => (
          <button
            aria-selected={index === active_index}
            key={skill.name}
            onClick={() => props.on_select(skill)}
            onMouseEnter={() => setActiveIndex(index)}
            role="option"
            type="button"
          >
            <strong>{skill.name}</strong>
            <span>{skill.description || "暂无描述"}</span>
          </button>
        ))}
        {results.length === 0 && <p>没有匹配的技能。</p>}
      </div>
    </div>
  );
}
