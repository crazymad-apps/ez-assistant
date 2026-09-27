import { useEffect, useId, useRef, useState, type ReactNode } from 'react';
import { AnchoredOverlay } from '../../../../components/AnchoredOverlay';
import { Icon, type IconName } from '../../../../components/Icon';
import { useInputMethodGuard } from '../../../../components/InputMethodGuard';
import styles from './index.module.scss';

export type ContextPickerOption = Readonly<{
  value: string;
  label: string;
  detail?: string;
  hints?: readonly string[];
  search_text: string;
}>;

/** 草稿工作空间与智能体共用的搜索、选中标签和清空交互。 */
export function ContextPicker(props: Readonly<{
  label: string;
  trigger_label: string;
  dialog_label: string;
  icon: IconName;
  disabled: boolean;
  open: boolean;
  selected: ContextPickerOption | null;
  options: readonly ContextPickerOption[];
  notice?: ReactNode;
  empty_text?: string;
  action_label: string;
  action_icon: IconName;
  on_action: () => void;
  on_open_change: (open: boolean) => void;
  on_select: (value: string | null) => void;
}>) {
  const trigger_ref = useRef<HTMLDivElement>(null);
  const select_ref = useRef<HTMLButtonElement>(null);
  const overlay_ref = useRef<HTMLDivElement>(null);
  const search_ref = useRef<HTMLInputElement>(null);
  const input_method = useInputMethodGuard();
  const popover_id = useId();
  const listbox_id = useId();
  const [query, setQuery] = useState('');
  const [active_index, setActiveIndex] = useState<number | null>(null);
  const normalized_query = query.trim().toLocaleLowerCase();
  const results = props.options.filter((option) => !normalized_query
    || option.search_text.toLocaleLowerCase().includes(normalized_query));
  const selected_index = Math.max(0, results.findIndex((option) => option.value === props.selected?.value));

  useEffect(() => {
    setActiveIndex(null);
    if (!props.open) {
      setQuery('');
      return undefined;
    }
    const frame = requestAnimationFrame(() => search_ref.current?.focus());
    return () => cancelAnimationFrame(frame);
  }, [props.open, props.selected?.value]);

  useEffect(() => {
    if (props.open && active_index !== null) {
      document.getElementById(`${listbox_id}-${active_index}`)?.scrollIntoView?.({ block: 'nearest' });
    }
  }, [active_index, listbox_id, props.open, results.length]);

  function close(restore_focus: boolean) {
    props.on_open_change(false);
    if (restore_focus) select_ref.current?.focus();
  }

  function select(value: string | null) {
    // 草稿切换或冲突弹窗接管前，先把焦点还给触发器。
    close(true);
    if (value !== (props.selected?.value ?? null)) props.on_select(value);
  }

  function handleSearchKeyDown(event: React.KeyboardEvent<HTMLInputElement>) {
    if (input_method.shouldIgnoreKeyDown(event)) return;
    if (event.key === 'Escape') {
      event.preventDefault();
      close(true);
    } else if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      if (results.length > 0) {
        setActiveIndex((current) => {
          if (current === null) return selected_index;
          return (current + (event.key === 'ArrowDown' ? 1 : -1) + results.length) % results.length;
        });
      }
    } else if (event.key === 'Enter' && results[active_index ?? selected_index]) {
      event.preventDefault();
      select(results[active_index ?? selected_index].value);
    }
  }

  return (
    <div className={styles.root} data-selected={Boolean(props.selected)} ref={trigger_ref}>
      <button aria-controls={props.open ? popover_id : undefined} aria-expanded={props.open}
        aria-haspopup="dialog" aria-label={props.trigger_label} className={styles.trigger}
        disabled={props.disabled} onClick={() => props.on_open_change(!props.open)} ref={select_ref} type="button">
        <Icon name={props.icon} size={16} />
        <span className={styles.trigger_name}>{props.selected?.label ?? `选择${props.label}`}</span>
        {props.selected?.detail && <span className={styles.trigger_detail}>{props.selected.detail}</span>}
        {props.selected?.hints?.map((hint) => <span className={styles.trigger_detail} key={hint}>{hint}</span>)}
        {!props.selected && <Icon name="chevron-down" size={14} />}
      </button>
      {props.selected && <button aria-label={`清空${props.label}选择`} className={styles.clear}
        disabled={props.disabled} onClick={() => select(null)} type="button"><Icon name="x" size={14} /></button>}
      <AnchoredOverlay aria_label={props.dialog_label} class_name={styles.popover}
        on_request_close={() => close(false)} open={props.open} overlay_ref={overlay_ref}
        placement="above" trigger_ref={trigger_ref}>
        <div id={popover_id} role="dialog" aria-label={props.dialog_label}>
          <div className={styles.search_row}>
            <Icon name="search" size={17} />
            <input aria-activedescendant={active_index !== null && results[active_index] ? `${listbox_id}-${active_index}` : undefined}
              aria-controls={listbox_id} aria-expanded={props.open} aria-label={`搜索${props.label}`}
              onChange={(event) => { setQuery(event.target.value); setActiveIndex(null); }}
              onCompositionEnd={input_method.onCompositionEnd} onCompositionStart={input_method.onCompositionStart}
              onKeyDown={handleSearchKeyDown} onKeyUp={input_method.onKeyUp}
              placeholder={`搜索${props.label}`} ref={search_ref} role="combobox" value={query} />
          </div>
          {props.notice && <p className={styles.notice} role="status">{props.notice}</p>}
          <div aria-label={props.label} className={styles.list} id={listbox_id}
            onMouseLeave={() => setActiveIndex(null)} role="listbox">
            {results.map((option, index) => <button aria-selected={option.value === props.selected?.value}
              className={styles.option} data-active={index === active_index} id={`${listbox_id}-${index}`}
              key={option.value} onClick={() => select(option.value)} onMouseEnter={() => setActiveIndex(index)}
              role="option" tabIndex={-1} type="button">
              <Icon name={props.icon} size={17} />
              <span className={styles.option_text}>
                <strong>{option.label}</strong>
                {option.detail && <span>{option.detail}</span>}
                {option.hints?.map((hint) => <small key={hint}>{hint}</small>)}
              </span>
              {option.value === props.selected?.value && <Icon name="check" size={16} />}
            </button>)}
            {results.length === 0 && <p className={styles.empty}>{props.empty_text ?? `没有匹配的${props.label}`}</p>}
          </div>
          <div className={styles.footer}>
            <button onClick={() => { close(false); props.on_action(); }} type="button">
              <Icon name={props.action_icon} size={17} />{props.action_label}
            </button>
          </div>
        </div>
      </AnchoredOverlay>
    </div>
  );
}
