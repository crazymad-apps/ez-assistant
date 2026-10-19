import type { ComponentProps } from "react";
import styles from "./index.module.scss";

/** 普通单行表单字段；复用应用控件尺寸、主题与焦点边框，不用于菜单搜索。 */
export function TextInput(props: Readonly<ComponentProps<"input">>) {
  const { className, placeholder, ...input } = props;
  return <input {...input} placeholder={placeholder?.trim() ? placeholder : "请输入"} className={[styles.input, className].filter(Boolean).join(" ")} />;
}
