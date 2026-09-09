import { useEffect, useState } from "react";
import { SelectionPopover, type SelectionOption } from "../../../components/SelectionPopover";
import styles from "./index.module.scss";

/** 参数表单的选择字段；键盘与浮层行为仍由统一 SelectionPopover 提供。 */
export function ModelFieldSelector<T extends string>(props: Readonly<{
  label: string; value: T; options: readonly SelectionOption<T>[]; onChange: (value: T) => void; disabled?: boolean;
}>) {
  const [open, setOpen] = useState(false);
  useEffect(() => { if (props.disabled) setOpen(false); }, [props.disabled]);
  return <div className={styles.form_field}><span>{props.label}</span><SelectionPopover aria_label={props.label} trigger_variant="field"
    open={open} on_open_change={setOpen} selected={props.value} options={props.options}
    disabled={props.disabled} trigger_content={props.options.find((option) => option.value === props.value)?.label ?? props.value}
    on_select={(value) => { if (!props.disabled) props.onChange(value); setOpen(false); }} /></div>;
}
