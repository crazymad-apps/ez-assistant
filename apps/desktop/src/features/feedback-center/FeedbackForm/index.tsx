import { useState } from "react";
import { observer } from "mobx-react-lite";
import { SelectionPopover } from "../../../components/SelectionPopover";
import type { FeedbackCenterStore } from "../FeedbackCenterStore";
import { kinds, type Draft } from "../model";
import styles from "./index.module.scss";

export const FeedbackForm = observer(function FeedbackForm({ store }: { store: FeedbackCenterStore }) {
  const [picker, setPicker] = useState("");
  const draft = store.draft;
  const environment = store.frozen?.client ?? store.environment;
  const text = (key: keyof Draft, label: string, max: number, required = false, multiline = false) => <label className={styles.field} key={key}>
    <span>{label}{required ? " *" : "（可选）"}</span>
    {multiline ? <textarea name={key} rows={key === "description" ? 5 : 3} maxLength={max} required={required} value={String(draft[key])} onChange={(event) => store.edit({ [key]: event.target.value })} />
      : <input name={key} maxLength={max} required={required} pattern={key === "reference" && draft.kind === "plugin" ? "[a-z0-9]+(-[a-z0-9]+)*" : undefined} value={String(draft[key])} onChange={(event) => store.edit({ [key]: event.target.value })} />}
  </label>;
  return <form id="submit-feedback-form" className={styles.form} onSubmit={(event) => { event.preventDefault(); if (!draft.kind) { document.querySelector<HTMLElement>('[aria-label="反馈目标"]')?.focus(); } void store.submit(); }}>
    <fieldset disabled={store.busy || store.unknown}>
      <label className={styles.field}><span>反馈目标 *</span><SelectionPopover aria_label="反馈目标" trigger_variant="field" placeholder="请选择目标" open={picker === "kind"} on_open_change={(open) => setPicker(open ? "kind" : "")} selected={draft.kind} options={Object.entries(kinds).map(([value, label]) => ({ value, label }))} on_select={(kind) => { store.edit({ kind: kind as Draft["kind"], name: "", reference: "", version: "", content_ref: "", location: "", specific: false }); setPicker(""); }} /></label>
      {draft.kind === "platform" && <p className={styles.hint}>EZ Assistant</p>}
      {draft.kind === "plugin" && <>
        <label className={styles.field}><span>从插件清单选择</span><SelectionPopover aria_label="选择插件" editable trigger_variant="field" placeholder="搜索插件，或在下方手动填写" open={picker === "plugin"} on_open_change={(open) => setPicker(open ? "plugin" : "")} selected={draft.reference} options={store.plugins.map((plugin) => ({ value: plugin.name, label: `${plugin.display_name} · ${plugin.name}` }))} on_select={(name) => { const plugin = store.plugins.find((item) => item.name === name); if (plugin) { store.edit({ reference: name, name: plugin.display_name, version: plugin.version ?? "" }); setPicker(""); } else store.edit({ reference: name }); }} /></label>
        <p className={styles.hint}>清单不可用或插件已下架时，可填写实际使用的标识与名称。</p>
      </>}
      {draft.kind && draft.kind !== "platform" && <div className={styles.pair}>{text("name", draft.kind === "plugin" ? "插件名称" : "知识库名称", 120, true)}{text("reference", draft.kind === "plugin" ? "插件标识" : "知识库标识或来源引用", draft.kind === "plugin" ? 64 : 512, draft.kind === "plugin")}</div>}
      {draft.kind === "plugin" && text("version", "实际使用版本", 64)}
      {draft.kind === "knowledge" && <>
        {!draft.reference.trim() && <p className={styles.hint}>将按名称记录，后续可人工核对。</p>}
        <label className={styles.checkbox}><input type="checkbox" checked={draft.specific} onChange={(event) => store.edit({ specific: event.target.checked, content_ref: "", location: "" })} />针对具体内容</label>
      </>}
      {draft.kind && (draft.kind !== "knowledge" || draft.specific) && <>
        {draft.kind === "knowledge" && text("content_ref", "文档或条目标识", 512)}
        {text("location", draft.kind === "knowledge" ? "内容位置或章节" : "工具、功能或位置", 512, draft.kind === "knowledge" && !draft.content_ref.trim())}
      </>}
      {text("title", "标题", 120, true)}
      {text("description", "问题描述", 4000, true, true)}
      <div className={styles.pair}>{text("reproduction", "复现步骤", 2000, false, true)}{text("expected_result", "期望结果", 2000, false, true)}</div>
    </fieldset>
    <p className={styles.hint}>内容空间：{store.content_bytes.toLocaleString()} / 12,288 字节</p>
    <details><summary>将附带的环境信息</summary><p className={styles.hint}>Host：{store.support?.host_version} · {store.support?.host_os} · {store.support?.host_arch}<br />客户端：{environment.client_kind === "desktop" ? "桌面端" : "Web 端"} {environment.client_version}<br />客户端系统：{environment.client_os ?? "未采集"}；架构：{environment.client_arch ?? "未采集"}</p></details>
  </form>;
});
