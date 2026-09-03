import { observer } from "mobx-react-lite";
import { useEffect, useRef, useState } from "react";
import { Dialog } from "../../../../components/Dialog";
import type { PreviewMcpImportResult } from "../../../../generated/assistant-protocol";
import { useRootStore } from "../../../../stores/RootStoreContext";
import { SessionActionDialog } from "../../../sessions/SessionActionDialog";
import shared from "../index.module.scss";
import styles from "./index.module.scss";

export const ImportDialog = observer(function ImportDialog(props: Readonly<{
  revision: string; onClose: () => void; onDirtyChange: (dirty: boolean) => void;
}>) {
  const settings = useRootStore().settings.mcp;
  const [document, setDocument] = useState("");
  const [preview, setPreview] = useState<PreviewMcpImportResult | null>(null);
  const [replacements, setReplacements] = useState<string[]>([]);
  const [discard, setDiscard] = useState(false);
  const input_ref = useRef<HTMLTextAreaElement>(null);
  const request = useRef(0);
  const pending = settings.pending_action !== null;
  useEffect(() => { props.onDirtyChange(document.length > 0); }, [document, props.onDirtyChange]);
  useEffect(() => () => { ++request.current; props.onDirtyChange(false); }, [props.onDirtyChange]);
  function close() { if (document) setDiscard(true); else props.onClose(); }
  async function loadPreview() {
    const current = ++request.current;
    const result = await settings.previewImport(document);
    if (request.current === current) { setPreview(result); setReplacements([]); }
  }
  async function submit() {
    if (await settings.mutate(props.revision, { type: "import", payload: { document, replace_server_keys: replacements } })) props.onClose();
  }
  const count = preview?.entries.filter((entry) => !entry.conflicts_with_existing || replacements.includes(entry.server_key)).length ?? 0;
  return <>
    <Dialog aria_label="导入 MCP 配置" backdrop_class_name={shared.confirm_backdrop} dialog_class_name={`${shared.confirm_dialog} ${styles.import_dialog}`} initial_focus_ref={input_ref} dismissible={!pending} on_close={close}>
      <header><h4>导入 MCP 配置</h4></header>
      <p>粘贴 mcpServers 对象或服务列表。同名服务默认跳过，勾选后才会替换。</p>
      <textarea aria-label="MCP 配置 JSON" disabled={pending} onChange={(event) => { ++request.current; setDocument(event.target.value); setPreview(null); setReplacements([]); }} placeholder={'{ "mcpServers": { "example": { "command": "server" } } }'} ref={input_ref} rows={8} spellCheck={false} value={document} />
      {settings.error_message && <p className={styles.error} role="alert">{settings.error_message}</p>}
      {settings.configuration_conflict && <p className={styles.warning}>配置已被其他窗口修改。请关闭并重新读取配置，再核对导入；当前内容没有写入。</p>}
      {preview && <div className={styles.preview}>
        {preview.diagnostics.map((diagnostic, index) => <p className={styles.error} key={index}>{diagnostic.message}</p>)}
        {preview.entries.map((entry) => <article key={entry.server_key}>
          <strong>{entry.display_name} <code>{entry.server_key}</code></strong>
          <small>{entry.transport === "stdio" ? "本地 stdio" : "Streamable HTTP"} · {entry.conflicts_with_existing ? "同名，默认跳过" : "新增"}</small>
          {entry.conflicts_with_existing && <label><input checked={replacements.includes(entry.server_key)} disabled={pending} onChange={(event) => setReplacements((values) => event.target.checked ? [...values, entry.server_key] : values.filter((key) => key !== entry.server_key))} type="checkbox" />替换 {entry.server_key}</label>}
          {entry.warnings.length > 0 && <p className={styles.warning}>未知扩展字段将保留，但本版本不使用。</p>}
        </article>)}
        {replacements.length > 0 && <button disabled={pending} onClick={() => setReplacements([])} type="button">全部跳过</button>}
      </div>}
      <footer><button disabled={pending} onClick={close} type="button">取消</button><button disabled={pending || !document.trim()} onClick={() => void loadPreview()} type="button">预览导入</button><button className={shared.primary_button} disabled={pending || !preview || preview.diagnostics.length > 0 || count === 0} onClick={() => void submit()} type="button">导入 {count} 个服务</button></footer>
    </Dialog>
    {discard && <SessionActionDialog title="放弃导入草稿？" confirm_label="放弃草稿" is_danger is_pending={false} on_cancel={() => setDiscard(false)} on_confirm={props.onClose}><p>本次粘贴的配置和凭据不会保存。</p></SessionActionDialog>}
  </>;
});
