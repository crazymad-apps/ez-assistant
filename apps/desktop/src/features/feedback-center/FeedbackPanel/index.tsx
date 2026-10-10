import { useEffect, useState } from "react";
import { observer } from "mobx-react-lite";
import { Button } from "../../../components/Button";
import { Dialog } from "../../../components/Dialog";
import { SelectionPopover } from "../../../components/SelectionPopover";
import { FeedbackForm } from "../FeedbackForm";
import { FeedbackDetail } from "../FeedbackDetail";
import type { FeedbackCenterStore } from "../FeedbackCenterStore";
import { kinds, statuses } from "../model";
import styles from "./index.module.scss";

export const FeedbackPanel = observer(function FeedbackPanel({ store }: { store: FeedbackCenterStore }) {
  const [confirm, setConfirm] = useState<"close" | "back" | "new" | null>(null);
  const [picker, setPicker] = useState("");
  useEffect(() => { if (!store.open) { setConfirm(null); setPicker(""); } }, [store.open]);
  useEffect(() => {
    if (!store.open || (!store.dirty && !store.busy)) return;
    const guard = (event: BeforeUnloadEvent) => { event.preventDefault(); event.returnValue = ""; };
    window.addEventListener("beforeunload", guard); return () => window.removeEventListener("beforeunload", guard);
  }, [store.open, store.dirty, store.busy]);
  const leave = (target: "close" | "back") => { if (store.busy) return; if (store.dirty) setConfirm(target); else if (target === "close") store.close(); else store.back(); };
  return <Dialog open={store.open} aria_label="反馈中心" backdrop_class_name={styles.backdrop} dialog_class_name={styles.dialog} on_close={() => leave("close")} dismissible={!store.busy}>
    <header>{store.view !== "list" && <Button size="small" disabled={store.busy} onClick={() => leave("back")}>返回</Button>}<h3>{store.view === "list" ? "我的反馈" : store.view === "form" ? "新增反馈" : "反馈详情"}</h3><Button disabled={store.busy} onClick={() => leave("close")} aria-label="关闭反馈中心">关闭</Button></header>
    <div className={styles.body} aria-busy={store.busy}>
      {store.message && <p className={styles.notice} role="status">{store.message}</p>}
      {store.view === "list" && <>
        <div className={styles.filters}><SelectionPopover aria_label="筛选目标" trigger_variant="field" open={picker === "kind"} on_open_change={(open) => setPicker(open ? "kind" : "")} selected={store.kind} disabled={store.busy} options={[{ value: "", label: "全部目标" }, ...Object.entries(kinds).map(([value, label]) => ({ value, label }))]} on_select={(kind) => { store.filter(store.status, kind as typeof store.kind); setPicker(""); }} />
          <SelectionPopover aria_label="筛选状态" trigger_variant="field" open={picker === "status"} on_open_change={(open) => setPicker(open ? "status" : "")} selected={store.status} disabled={store.busy} options={[{ value: "", label: "全部状态" }, ...Object.entries(statuses).map(([value, label]) => ({ value, label }))]} on_select={(status) => { store.filter(status as typeof store.status, store.kind); setPicker(""); }} /></div>
        <ul className={styles.list}>{store.page?.items.map((item) => <li key={item.id}><button disabled={store.busy} onClick={() => void store.read(item.id)}><strong>{item.title}</strong><span>{kinds[item.target_kind]} / {item.target_name} · {statuses[item.status]}</span><small>{item.source === "agent" ? "Agent 提交" : "手动提交"} · {new Date(item.created_at).toLocaleString()}</small></button></li>)}</ul>
        {store.page?.items.length === 0 && <p className={styles.empty}>暂无符合条件的反馈</p>}
      </>}
      {store.view === "form" && <FeedbackForm store={store} />}
      {store.view === "detail" && store.detail && <FeedbackDetail detail={store.detail} />}
    </div>
    <footer>
      {store.view === "list" && <><span className={styles.page}>{store.page ? `共 ${store.page.total} 条` : ""}</span><Button disabled={store.busy || store.offset === 0} onClick={() => void store.load(Math.max(0, store.offset - 20))}>上一页</Button><Button disabled={store.busy || !store.page || store.offset + 20 >= store.page.total} onClick={() => void store.load(store.offset + 20)}>下一页</Button><Button disabled={store.busy} onClick={() => void store.load()}>刷新</Button><Button variant="primary" disabled={store.busy || !store.support?.available} onClick={() => store.create()}>新增反馈</Button></>}
      {store.view === "detail" && <Button disabled={store.busy} onClick={() => store.detail && void store.read(store.detail.id)}>刷新处理结果</Button>}
      {store.view === "form" && (store.unknown ? <><Button disabled={store.busy} onClick={() => setConfirm("new")}>修改为新反馈</Button><Button disabled={store.busy} variant="primary" onClick={() => void store.checkSubmission()}>检查提交结果</Button>{store.checked && <Button disabled={store.busy} onClick={() => void store.submit()}>重试本次提交</Button>}</> : <><Button disabled={store.busy} onClick={() => leave("back")}>取消</Button><Button type="submit" form="submit-feedback-form" variant="primary" disabled={store.busy || store.content_bytes > 12288}>{store.busy ? "提交中…" : "提交反馈"}</Button></>)}
    </footer>
    <Dialog open={!!confirm} aria_label="放弃本地草稿" backdrop_class_name={styles.backdrop} dialog_class_name={styles.confirm} on_close={() => setConfirm(null)}>
      <h3>{confirm === "new" ? "作为新反馈修改？" : "放弃本地草稿？"}</h3><p>{store.unknown ? "原请求可能已提交。建议先检查提交结果，也可稍后在“我的反馈”核对。" : "尚未提交的内容将被清除。"}</p>
      <footer><Button onClick={() => setConfirm(null)}>继续编辑</Button><Button variant="danger" onClick={() => { const target = confirm; setConfirm(null); if (target === "new") { const draft = store.draft; store.create(); store.edit(draft); } else if (target === "close") store.close(); else store.back(); }}>{confirm === "new" ? "作为新反馈修改" : "放弃草稿"}</Button></footer>
    </Dialog>
  </Dialog>;
});
