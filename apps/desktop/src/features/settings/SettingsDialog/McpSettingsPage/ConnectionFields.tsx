import { useState } from "react";
import { SelectionPopover, type SelectionOption } from "../../../../components/SelectionPopover";
import type { McpFieldChange, McpSecretChange } from "../../../../generated/assistant-protocol";
import styles from "./index.module.scss";

export function ConnectionField(props: Readonly<{ label: string; change: McpFieldChange<string>; onChange: (change: McpFieldChange<string>) => void; removable?: boolean }>) {
  const [open, setOpen] = useState(false);
  const options: SelectionOption<McpFieldChange<string>["mode"]>[] = [
    { value: "keep", label: "保持原值" },
    { value: "replace", label: "替换" },
  ];
  if (props.removable) options.push({ value: "remove", label: "移除" });

  return <div className={styles.connection_field}>
    <label>{props.label}<input aria-label={props.label} disabled={props.change.mode !== "replace"} onChange={(event) => props.onChange({ mode: "replace", value: event.target.value })} placeholder={props.change.mode === "keep" ? "已配置，保持原值" : ""} value={props.change.mode === "replace" ? props.change.value : ""} /></label>
    <SelectionPopover
      aria_label={`${props.label}修改方式`}
      content_width="content"
      on_open_change={setOpen}
      on_select={(mode) => {
        if (mode === "replace") props.onChange({ mode: "replace", value: "" });
        else props.onChange({ mode });
      }}
      open={open}
      options={options}
      selected={props.change.mode}
      trigger_variant="field"
    />
  </div>;
}

export function ArgsFields(props: Readonly<{ change: McpFieldChange<string[]>; onChange: (change: McpFieldChange<string[]>) => void }>) {
  const change = props.change;
  return <fieldset><legend>参数 args</legend>
    {change.mode === "keep" && <p>现有参数不回显，将保持原值。<button onClick={() => props.onChange({ mode: "replace", value: [] })} type="button">替换参数</button></p>}
    {change.mode === "replace" && <>
      {change.value.map((argument, index) => <div className={styles.inline_row} key={index}>
        <input aria-label={`参数 ${index + 1}`} onChange={(event) => props.onChange({ mode: "replace", value: change.value.map((value, position) => position === index ? event.target.value : value) })} value={argument} />
        <button aria-label={`移除参数 ${index + 1}`} onClick={() => props.onChange({ mode: "replace", value: change.value.filter((_, position) => position !== index) })} type="button">移除</button>
      </div>)}
      <div className={styles.inline_row}><button onClick={() => props.onChange({ mode: "replace", value: [...change.value, ""] })} type="button">添加参数</button><button onClick={() => props.onChange({ mode: "keep" })} type="button">保持现有参数</button></div>
    </>}
  </fieldset>;
}

export type SecretRow = { id: string; name: string; change: McpSecretChange; existing: boolean };

export function SecretFields(props: Readonly<{ rows: readonly SecretRow[]; onChange: (rows: SecretRow[]) => void; label: string }>) {
  function update(id: string, patch: Partial<SecretRow>) { props.onChange(props.rows.map((row) => row.id === id ? { ...row, ...patch } : row)); }
  return <fieldset><legend>{props.label}</legend>
    <p>已有值不回显；留空保持，可填写新值或明确移除。支持 ${"{VAR}"} 引用。</p>
    {props.rows.map((row, index) => <div className={styles.secret_row} key={row.id}>
      <input aria-label={`${props.label}名称 ${index + 1}`} disabled={row.existing} onChange={(event) => update(row.id, { name: event.target.value })} placeholder="名称" value={row.name} />
      <input aria-label={`${props.label}值 ${index + 1}`} autoComplete="off" disabled={row.change.mode === "remove"} onChange={(event) => update(row.id, { change: event.target.value || !row.existing ? { mode: "replace", value: event.target.value } : { mode: "keep" } })} placeholder={row.existing ? "已配置，留空保持" : "值"} type="password" value={row.change.mode === "replace" ? row.change.value : ""} />
      <button onClick={() => {
        if (!row.existing) props.onChange(props.rows.filter((candidate) => candidate.id !== row.id));
        else update(row.id, { change: { mode: row.change.mode === "remove" ? "keep" : "remove" } });
      }} type="button">{row.change.mode === "remove" ? "恢复保持" : "移除"}</button>
    </div>)}
    <button onClick={() => props.onChange([...props.rows, { id: crypto.randomUUID(), name: "", change: { mode: "replace", value: "" }, existing: false }])} type="button">添加{props.label}</button>
  </fieldset>;
}
