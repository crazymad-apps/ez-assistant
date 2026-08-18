import { observer } from "mobx-react-lite";
import { useEffect, useMemo, useState } from "react";
import type { PinnedMemorySnapshot } from "../../../generated/assistant-protocol";
import { Icon } from "../../../components/Icon";
import { useRootStore } from "../../../stores/RootStoreContext";
import styles from "./index.module.scss";

type Props = Readonly<{ onDirtyChange: (dirty: boolean) => void }>;

type MemoryDraft = Readonly<{
  memory: PinnedMemorySnapshot | null;
  category: string;
  content: string;
}>;

export const MemorySettingsPage = observer(function MemorySettingsPage(props: Props) {
  const memory_store = useRootStore().memory_settings;
  const { onDirtyChange } = props;
  const [persona_enabled, setPersonaEnabled] = useState(false);
  const [persona_content, setPersonaContent] = useState("");
  const [persona_dirty, setPersonaDirty] = useState(false);
  const [memory_draft, setMemoryDraft] = useState<MemoryDraft | null>(null);
  const [query, setQuery] = useState("");
  const collection = memory_store.collection;
  const persona = memory_store.persona;

  useEffect(() => {
    if (!persona || persona_dirty) return;
    setPersonaEnabled(persona.enabled);
    setPersonaContent(persona.content);
  }, [persona, persona_dirty]);

  useEffect(() => {
    onDirtyChange(persona_dirty || memory_draft !== null);
    return () => onDirtyChange(false);
  }, [memory_draft, onDirtyChange, persona_dirty]);

  const filtered_memories = useMemo(() => {
    const normalized = query.trim().toLocaleLowerCase();
    if (!normalized) return collection?.items ?? [];
    return (collection?.items ?? []).filter((memory) => (
      memory.category.toLocaleLowerCase().includes(normalized)
      || memory.content.toLocaleLowerCase().includes(normalized)
    ));
  }, [collection, query]);

  async function savePersona() {
    const saved = await memory_store.savePersona(persona_enabled, persona_content);
    if (saved) setPersonaDirty(false);
  }

  async function savePinnedMemory() {
    if (!memory_draft) return;
    const category = memory_draft.category.trim();
    const content = memory_draft.content.trim();
    if (!category || !content) {
      memory_store.showError("请填写分类和正文。");
      return;
    }
    const saved = memory_draft.memory
      ? await memory_store.updatePinnedMemory(memory_draft.memory, category, content)
      : await memory_store.createPinnedMemory(category, content);
    if (saved) setMemoryDraft(null);
  }

  async function deletePinnedMemory(memory: PinnedMemorySnapshot) {
    if (!window.confirm(`删除“${memory.category}”中的这条 Pinned Memory？`)) return;
    await memory_store.deletePinnedMemory(memory);
  }

  const persona_bytes = new TextEncoder().encode(persona_content).length;
  const persona_limit = memory_store.capabilities?.max_persona_bytes ?? 0;

  return (
    <section>
      <div className={styles.page_header}>
        <h3>记忆</h3>
        <button disabled={memory_store.loading} onClick={() => void memory_store.load()} type="button">
          <Icon name="refresh" size={14} />重新加载
        </button>
      </div>

      <article className={styles.memory_persona}>
        <div className={styles.memory_section_header}>
          <div>
            <h4>Persona</h4>
            <p>称呼、语言和长期协作偏好。修改只影响之后新建的会话。</p>
          </div>
          <label className={styles.memory_toggle}>
            <input
              checked={persona_enabled}
              onChange={(event) => {
                setPersonaEnabled(event.currentTarget.checked);
                setPersonaDirty(true);
              }}
              type="checkbox"
            />
            启用
          </label>
        </div>
        <textarea
          aria-label="Persona 内容"
          maxLength={persona_limit || undefined}
          onChange={(event) => {
            setPersonaContent(event.currentTarget.value);
            setPersonaDirty(true);
          }}
          placeholder="例如：默认使用简体中文，结论优先；涉及取舍时说明理由。"
          value={persona_content}
        />
        <div className={styles.memory_persona_footer}>
          <span>{persona_bytes}{persona_limit ? ` / ${persona_limit}` : ""} 字节</span>
          <button
            className={styles.primary_button}
            disabled={!persona || !persona_dirty || memory_store.pending_action === "persona:save" || persona_bytes > persona_limit}
            onClick={() => void savePersona()}
            type="button"
          >保存 Persona</button>
        </div>
      </article>

      <div className={styles.memory_list_header}>
        <div>
          <h4>Pinned Memory</h4>
          <span>{collection?.items.length ?? 0} / {collection?.capabilities.max_pinned_entries ?? 0}</span>
        </div>
        <div>
          <label className={styles.memory_search}>
            <Icon name="search" size={14} />
            <input aria-label="搜索 Pinned Memory" onChange={(event) => setQuery(event.currentTarget.value)} placeholder="搜索分类或正文" value={query} />
          </label>
          {!memory_draft && (
            <button onClick={() => setMemoryDraft({ memory: null, category: "", content: "" })} type="button">
              <Icon name="plus" size={14} />添加
            </button>
          )}
        </div>
      </div>

      {memory_draft && (
        <PinnedMemoryEditor
          draft={memory_draft}
          on_cancel={() => setMemoryDraft(null)}
          on_change={setMemoryDraft}
          on_save={() => void savePinnedMemory()}
          pending={memory_store.pending_action?.startsWith("pinned:") ?? false}
        />
      )}

      {!memory_draft && (
        <div className={styles.memory_rows}>
          {filtered_memories.map((memory) => (
            <article key={memory.id}>
              <div className={styles.memory_row_body}>
                <div>
                  <strong>{memory.category}</strong>
                  <small>{createdByLabel(memory)} · {formatTime(memory.updated_at_ms)}</small>
                </div>
                <p>{memory.content}</p>
              </div>
              <div className={styles.memory_row_actions}>
                <button aria-label="编辑 Pinned Memory" onClick={() => setMemoryDraft({ memory, category: memory.category, content: memory.content })} type="button">
                  <Icon name="edit" size={14} />
                </button>
                <button aria-label="删除 Pinned Memory" onClick={() => void deletePinnedMemory(memory)} type="button">
                  <Icon name="trash" size={14} />
                </button>
              </div>
            </article>
          ))}
          {filtered_memories.length === 0 && (
            <p className={styles.permission_empty}>{query ? "没有匹配的 Pinned Memory。" : "尚未添加 Pinned Memory。"}</p>
          )}
        </div>
      )}

      {memory_store.conflict_message && <p className={styles.memory_conflict}>{memory_store.conflict_message}</p>}
      {memory_store.error_message && <p className={styles.error_message}>{memory_store.error_message}</p>}
      {memory_store.notice_message && <p className={styles.notice_message}>{memory_store.notice_message}</p>}
    </section>
  );
});

function PinnedMemoryEditor(props: Readonly<{
  draft: MemoryDraft;
  on_cancel: () => void;
  on_change: (draft: MemoryDraft) => void;
  on_save: () => void;
  pending: boolean;
}>) {
  return (
    <div className={styles.memory_editor}>
      <strong>{props.draft.memory ? "编辑 Pinned Memory" : "添加 Pinned Memory"}</strong>
      <label>
        分类
        <input
          onChange={(event) => props.on_change({ ...props.draft, category: event.currentTarget.value })}
          placeholder="例如：协作偏好、常用约定"
          value={props.draft.category}
        />
      </label>
      <label>
        正文
        <textarea
          onChange={(event) => props.on_change({ ...props.draft, content: event.currentTarget.value })}
          placeholder="记录需要跨会话长期保留的稳定信息"
          value={props.draft.content}
        />
      </label>
      <div>
        <button onClick={props.on_cancel} type="button">取消</button>
        <button className={styles.primary_button} disabled={props.pending} onClick={props.on_save} type="button">保存</button>
      </div>
    </div>
  );
}

function createdByLabel(memory: PinnedMemorySnapshot): string {
  return memory.created_by.type === "user" ? "用户添加" : "Agent 添加";
}

function formatTime(timestamp_ms: number): string {
  return new Intl.DateTimeFormat("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  }).format(new Date(timestamp_ms));
}
