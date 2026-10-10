import type { SubmittedFeedback } from "@ez-assistant/protocol";
import { kinds, statuses } from "../model";
import styles from "./index.module.scss";

export function FeedbackDetail({ detail }: { detail: SubmittedFeedback }) {
  return <article className={styles.detail}>
    <div><h3>{detail.title}</h3><p>{statuses[detail.status]} · {detail.source === "agent" ? "Agent 提交" : "手动提交"}</p></div>
    <dl><dt>反馈编号</dt><dd>{detail.id}</dd><dt>反馈目标</dt><dd>{kinds[detail.target.kind]} / {detail.target.name}{detail.target.version ? ` · ${detail.target.version}` : ""}</dd>
      <dt>{detail.target.reference ? "来源引用" : "对象说明"}</dt><dd>{detail.target.reference ?? "手动描述的对象"}</dd>
      {detail.target.content_ref && <><dt>具体内容</dt><dd>{detail.target.content_ref}</dd></>}{detail.target.location && <><dt>位置</dt><dd>{detail.target.location}</dd></>}
      <dt>提交时间</dt><dd>{new Date(detail.created_at).toLocaleString()}</dd><dt>更新时间</dt><dd>{new Date(detail.updated_at).toLocaleString()}</dd></dl>
    <section><h4>问题描述</h4><p>{detail.description}</p></section>
    {detail.reproduction && <section><h4>复现步骤</h4><p>{detail.reproduction}</p></section>}
    {detail.expected_result && <section><h4>期望结果</h4><p>{detail.expected_result}</p></section>}
    <section className={styles.handling}><h4>处理说明</h4><p>{detail.handling_note ?? "暂无处理说明"}</p></section>
    <details><summary>环境信息</summary><p>Host：{[detail.environment.host_version, detail.environment.host_os, detail.environment.host_arch].filter(Boolean).join(" · ")}<br />客户端：{[detail.environment.client_kind, detail.environment.client_version, detail.environment.client_os, detail.environment.client_arch].filter(Boolean).join(" · ")}</p></details>
  </article>;
}
