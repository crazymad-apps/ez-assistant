import type { ModelConfigOrigin, ModelConfigurationSummary } from "@ez-assistant/protocol";
import styles from "./index.module.scss";

/** 创建来源与参数状态分别展示，固定记录不会因在线返回变化而改标。 */
export function ModelStatusTags(props: Readonly<{
  origin: ModelConfigOrigin;
  customized: boolean;
  configuration?: ModelConfigurationSummary | null;
  offline?: boolean;
}>) {
  return <span className={styles.model_status_tags}>
    <span className={styles.model_tag}>{props.origin === "manual" ? "手动添加" : "在线发现"}</span>
    {props.customized ? <span className={styles.model_tag} data-tone="accent">已自定义</span> : <>
      {props.configuration?.uses_template && <span className={styles.model_tag} data-tone="accent">模板预填</span>}
      {props.configuration?.requires_configuration !== false
        ? <span className={styles.model_tag} data-tone="warning">待补全</span>
        : !props.configuration?.uses_template && <span className={styles.model_tag}>在线参数</span>}
    </>}
    {props.offline && props.origin === "online" && <span className={styles.model_tag}>本次未返回</span>}
    <span aria-hidden="true">›</span>
  </span>;
}
