import type { ReactNode } from "react";
import styles from "./index.module.scss";

/** 连接或读取失败的顶部提示；操作由所属连接／查询调用方提供。 */
export function ConnectionNotice(props: Readonly<{ children: ReactNode; role?: "alert" | "status"; className?: string }>) {
  return <div className={[styles.notice, props.className].filter(Boolean).join(" ")} role={props.role ?? "status"}>{props.children}</div>;
}
