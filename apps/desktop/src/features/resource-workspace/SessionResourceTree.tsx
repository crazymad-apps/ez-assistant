import { useCallback, useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent, type MouseEvent } from "react";
import type {
  SessionId,
  SessionResourceEntry,
  SessionResourceLocator,
  SessionResourceRoot,
} from "../../generated/assistant-protocol";
import { Icon } from "../../components/Icon";
import { InlineIconButton } from "../../components/InlineIconButton";
import { resolveMaterialFileIcon, resolveMaterialFolderIcon } from "./materialResourceIcon";
import { listSessionResourceFiles } from "../../native-bridge/nativeResource";
import type { ResourceMenuLocation } from "./ResourceContextMenu";
import type { ResourceViewState } from "./resourceViewState";
import styles from "./ResourceWorkspace/index.module.scss";

export type SessionResourceRootItem = Readonly<{
  id: string;
  label: string;
  detail: string;
  locator: SessionResourceLocator | null;
  path?: string;
}>;

type DirectoryState = Readonly<{
  status: "loading" | "ready" | "error";
  entries: readonly SessionResourceEntry[];
  truncated: boolean;
  error: string | null;
}>;

const DIRECTORY_LOADING_DELAY_MS = 100;

export function SessionResourceTree(props: Readonly<{
  view_state?: ResourceViewState;
  focus_locator: SessionResourceLocator | null;
  roots: readonly SessionResourceRootItem[];
  session_id: SessionId | null;
  on_open_file?: (entry: SessionResourceEntry) => void;
  on_open_resource_menu?: (entry: SessionResourceEntry, location: ResourceMenuLocation) => void;
}>) {
  const [include_hidden, setIncludeHidden] = useState(props.view_state?.tree?.include_hidden ?? false);
  const [include_generated, setIncludeGenerated] = useState(props.view_state?.tree?.include_generated ?? false);
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set(props.view_state?.tree?.expanded.map(locatorKey)));
  const [directories, setDirectories] = useState<ReadonlyMap<string, DirectoryState>>(new Map());
  const directories_ref = useRef<ReadonlyMap<string, DirectoryState>>(directories);
  const request_generation = useRef(0);
  const locator_registry = useRef(new Map<string, SessionResourceLocator>(props.view_state?.tree?.expanded.map((locator) => [locatorKey(locator), locator])));

  const loadDirectory = useCallback(async (locator: SessionResourceLocator) => {
    if (!props.session_id) return null;
    const key = locatorKey(locator);
    const generation = request_generation.current;
    setDirectories((current) => {
      const next = new Map(current).set(key, {
        status: "loading" as const,
        entries: current.get(key)?.entries ?? [],
        truncated: current.get(key)?.truncated ?? false,
        error: null,
      });
      directories_ref.current = next;
      return next;
    });
    try {
      const result = await listSessionResourceFiles(props.session_id, {
        locator,
        include_hidden,
        include_generated,
      });
      if (generation !== request_generation.current) return;
      setDirectories((current) => {
        const next = new Map(current).set(key, {
          status: "ready" as const,
          entries: result.entries,
          truncated: result.truncated,
          error: null,
        });
        directories_ref.current = next;
        return next;
      });
      return result;
    } catch (error: unknown) {
      if (generation !== request_generation.current) return null;
      setDirectories((current) => {
        const next = new Map(current).set(key, {
          status: "error" as const,
          entries: [],
          truncated: false,
          error: error instanceof Error ? error.message : "无法读取该目录。",
        });
        directories_ref.current = next;
        return next;
      });
      return null;
    }
  }, [include_generated, include_hidden, props.session_id]);

  const scroll_ref = useRef<HTMLDivElement>(null);
  const restore_scroll = useRef<number | null>(props.view_state?.tree?.scroll_top ?? 0);

  useEffect(() => {
    request_generation.current += 1;
    directories_ref.current = new Map();
    setDirectories(new Map());
    for (const key of expanded) {
      const locator = locator_registry.current.get(key);
      if (locator) void loadDirectory(locator);
    }
    return () => { request_generation.current += 1; };
    // 展开操作自行加载；仅会话或过滤变化才刷新已展开目录。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [loadDirectory]);

  useLayoutEffect(() => {
    if (!props.view_state) return;
    props.view_state.tree = {
      focus_locator: props.view_state.tree?.focus_locator,
      expanded: [...expanded].flatMap((key) => locator_registry.current.get(key) ?? []),
      include_hidden, include_generated,
      scroll_top: props.view_state.tree?.scroll_top ?? 0,
    };
  }, [expanded, include_hidden, include_generated, props.view_state]);

  useLayoutEffect(() => {
    const element = scroll_ref.current;
    if (!element || restore_scroll.current === null) return;
    element.scrollTop = restore_scroll.current;
    if ([...expanded].every((key) => directories.has(key)) && ![...directories.values()].some((directory) => directory.status === "loading")) restore_scroll.current = null;
  }, [directories, expanded]);

  useEffect(() => {
    if (!props.focus_locator || props.view_state?.tree?.focus_locator === props.focus_locator) return;
    if (props.view_state?.tree) props.view_state.tree.focus_locator = props.focus_locator;
    const locators = directoryAncestors(props.focus_locator);
    for (const locator of locators) locator_registry.current.set(locatorKey(locator), locator);
    setExpanded((current) => {
      const next = new Set(current);
      for (const locator of locators) next.add(locatorKey(locator));
      return next;
    });
    for (const locator of locators) {
      const state = directories_ref.current.get(locatorKey(locator));
      if (!state || state.status === "error") void loadDirectory(locator);
    }
    // 定位对象表达一次跳转意图；过滤变化由上一个 effect 统一刷新，避免同一目录请求两次。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [props.focus_locator, props.session_id]);

  function toggle(locator: SessionResourceLocator) {
    const key = locatorKey(locator);
    locator_registry.current.set(key, locator);
    const opening = !expanded.has(key);
    setExpanded((current) => {
      const next = new Set(current);
      if (opening) next.add(key);
      else next.delete(key);
      return next;
    });
    const state = directories_ref.current.get(key);
    if (opening && (!state || state.status === "error")) void loadDirectory(locator);
  }

  async function expandAll() {
    if (!props.session_id) return;
    const generation = request_generation.current;
    const queue = props.roots.flatMap((root) => root.locator ? [root.locator] : []);
    const visited = new Set<string>();

    while (queue.length > 0) {
      if (generation !== request_generation.current) return;
      const locator = queue.shift();
      if (!locator) continue;
      const key = locatorKey(locator);
      if (visited.has(key)) continue;
      visited.add(key);
      locator_registry.current.set(key, locator);
      setExpanded((current) => new Set(current).add(key));

      const cached = directories_ref.current.get(key);
      const result = cached?.status === "ready" ? cached : await loadDirectory(locator);
      if (generation !== request_generation.current) return;
      if (!result) continue;
      for (const entry of result.entries) {
        if (entry.kind === "directory" && entry.state === "available" && !entry.is_symbolic_link) {
          queue.push(entry.locator);
        }
      }
    }
  }

  function collapseAll() {
    request_generation.current += 1;
    setExpanded(new Set());
    const ready = new Map([...directories_ref.current].filter(([, state]) => state.status !== "loading"));
    directories_ref.current = ready;
    setDirectories(ready);
  }

  return (
    <div className={styles.tree_view}>
      <div className={styles.tree_toolbar}>
        <label><input checked={include_hidden} onChange={(event) => setIncludeHidden(event.target.checked)} type="checkbox" />显示隐藏项</label>
        <label><input checked={include_generated} onChange={(event) => setIncludeGenerated(event.target.checked)} type="checkbox" />显示生成目录</label>
        <div className={styles.tree_toolbar_actions}>
          <InlineIconButton
            disabled={!props.session_id || !props.roots.some((root) => root.locator)}
            icon="expand-all"
            label="全部展开"
            onClick={() => void expandAll()}
            size={17}
          />
          <InlineIconButton
            disabled={expanded.size === 0}
            icon="collapse-all"
            label="全部收起"
            onClick={collapseAll}
            size={17}
          />
        </div>
      </div>
      <div aria-label="工作空间目录" className={styles.tree_roots} role="tree" ref={scroll_ref}
        onWheel={() => { restore_scroll.current = null; }}
        onPointerDown={() => { restore_scroll.current = null; }}
        onScroll={(event) => { if (props.view_state?.tree && restore_scroll.current === null) props.view_state.tree.scroll_top = event.currentTarget.scrollTop; }}>
        {props.roots.map((root) => {
          const locator = root.locator;
          const key = locator ? locatorKey(locator) : root.id;
          const open = locator ? expanded.has(key) : false;
          const state = locator ? directories.get(key) : undefined;
          return (
            <div aria-expanded={locator ? open : undefined} className={styles.tree_root} key={root.id} role="treeitem">
              <div className={styles.tree_root_row}>
                {locator ? (
                  <button className={styles.tree_root_control} onClick={() => toggle(locator)} type="button">
                    <span className={styles.tree_disclosure}><Icon name={open ? "chevron-down" : "chevron-right"} size={15} /></span>
                    <ResourceFolderIcon folder_name={root.label} open={open} root />
                    <span className={styles.tree_root_name}><strong>{root.label}</strong><small>{root.detail}</small></span>
                  </button>
                ) : (
                  <div className={styles.tree_root_control}>
                    <span className={styles.tree_disclosure} />
                    <ResourceFolderIcon folder_name={root.label} open={false} root />
                    <span className={styles.tree_root_name}><strong>{root.label}</strong><small>{root.detail}</small></span>
                  </div>
                )}
                {locator && open && (
                  <InlineIconButton className={styles.tree_refresh} icon="refresh" label={`刷新${root.label}`} onClick={() => void loadDirectory(locator)} />
                )}
              </div>
              {!locator && <p className={styles.tree_notice}>创建会话后可浏览</p>}
              {locator && open && (
                <DirectoryChildren
                  directories={directories}
                  depth={1}
                  expanded={expanded}
                  locator={locator}
                  on_refresh={(value) => void loadDirectory(value)}
                  on_open_file={props.on_open_file}
                  on_open_resource_menu={props.on_open_resource_menu}
                  on_toggle={toggle}
                  state={state}
                />
              )}
            </div>
          );
        })}
      </div>
      {props.roots.length === 0 && <p className={styles.tree_empty}>当前没有可浏览的文件根。</p>}
    </div>
  );
}

function DirectoryChildren(props: Readonly<{
  directories: ReadonlyMap<string, DirectoryState>;
  depth: number;
  expanded: ReadonlySet<string>;
  locator: SessionResourceLocator;
  on_refresh: (locator: SessionResourceLocator) => void;
  on_open_file?: (entry: SessionResourceEntry) => void;
  on_open_resource_menu?: (entry: SessionResourceEntry, location: ResourceMenuLocation) => void;
  on_toggle: (locator: SessionResourceLocator) => void;
  state: DirectoryState | undefined;
}>) {
  if (!props.state || (props.state.status === "loading" && props.state.entries.length === 0)) {
    return <DirectoryLoadingIndicator />;
  }
  if (props.state.status === "error") {
    return (
      <div className={styles.tree_error}>
        <span>{props.state.error}</span>
        <button onClick={() => props.on_refresh(props.locator)} type="button">重试</button>
      </div>
    );
  }
  if (props.state.entries.length === 0 && !props.state.truncated) return null;
  return (
    <div className={styles.tree_children} role="group">
      {props.state.entries.map((entry) => (
        <ResourceEntryRow
          directories={props.directories}
          depth={props.depth}
          entry={entry}
          expanded={props.expanded}
          key={locatorKey(entry.locator)}
          on_refresh={props.on_refresh}
          on_open_file={props.on_open_file}
          on_open_resource_menu={props.on_open_resource_menu}
          on_toggle={props.on_toggle}
        />
      ))}
      {props.state.truncated && <p className={styles.tree_limit}>目录内容过多，请使用 Finder 查看。</p>}
    </div>
  );
}

function DirectoryLoadingIndicator() {
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    const timer = window.setTimeout(() => setVisible(true), DIRECTORY_LOADING_DELAY_MS);
    return () => window.clearTimeout(timer);
  }, []);

  return visible ? <p className={styles.tree_notice}>正在读取…</p> : null;
}

function ResourceEntryRow(props: Readonly<{
  directories: ReadonlyMap<string, DirectoryState>;
  depth: number;
  entry: SessionResourceEntry;
  expanded: ReadonlySet<string>;
  on_refresh: (locator: SessionResourceLocator) => void;
  on_open_file?: (entry: SessionResourceEntry) => void;
  on_open_resource_menu?: (entry: SessionResourceEntry, location: ResourceMenuLocation) => void;
  on_toggle: (locator: SessionResourceLocator) => void;
}>) {
  const directory = props.entry.kind === "directory";
  const available = props.entry.state === "available";
  const key = locatorKey(props.entry.locator);
  const open = directory && props.expanded.has(key);
  const state = props.directories.get(key);
  const reason = props.entry.state === "outside_root"
    ? "目标位于当前根之外"
    : props.entry.state === "unsupported" ? "该文件类型不可访问" : null;
  const content = (
    <>
      <span className={styles.tree_disclosure}>{directory ? <Icon name={open ? "chevron-down" : "chevron-right"} size={15} /> : null}</span>
      {directory
        ? <ResourceFolderIcon folder_name={props.entry.display_name} open={open} root={false} />
        : <ResourceFileIcon file_name={props.entry.display_name} />}
      <span className={styles.tree_entry_name}>{props.entry.display_name}</span>
    </>
  );
  const openMenuFromPointer = (event: MouseEvent<HTMLElement>) => {
    if (!props.on_open_resource_menu) return;
    event.preventDefault();
    event.currentTarget.focus();
    props.on_open_resource_menu(props.entry, { x: event.clientX, y: event.clientY });
  };
  const openMenuFromKeyboard = (event: KeyboardEvent<HTMLElement>) => {
    if (!props.on_open_resource_menu
      || (event.key !== "ContextMenu" && !(event.shiftKey && event.key === "F10"))) return;
    event.preventDefault();
    const bounds = event.currentTarget.getBoundingClientRect();
    props.on_open_resource_menu(props.entry, { x: bounds.left + 16, y: bounds.bottom });
  };
  return (
    <div aria-expanded={directory && available ? open : undefined} className={styles.tree_entry} role="treeitem">
      <div className={styles.tree_entry_row} data-unavailable={!available || undefined}>
        {directory && available ? (
          <button
            className={styles.tree_entry_control}
            onClick={() => props.on_toggle(props.entry.locator)}
            onContextMenu={openMenuFromPointer}
            onKeyDown={openMenuFromKeyboard}
            style={{ paddingLeft: props.depth * 20 }}
            type="button"
          >
            {content}
          </button>
        ) : available ? (
          <button
            className={styles.tree_entry_control}
            onDoubleClick={() => props.on_open_file?.(props.entry)}
            onKeyDown={(event) => {
              if (event.key === "Enter") props.on_open_file?.(props.entry);
              openMenuFromKeyboard(event);
            }}
            onContextMenu={openMenuFromPointer}
            style={{ paddingLeft: props.depth * 20 }}
            type="button"
          >
            {content}
          </button>
        ) : <div className={styles.tree_entry_control} style={{ paddingLeft: props.depth * 20 }}>{content}</div>}
        {props.entry.size_bytes !== undefined && <small>{formatBytes(props.entry.size_bytes)}</small>}
        {directory && open && <InlineIconButton className={styles.tree_refresh} icon="refresh" label={`刷新${props.entry.display_name}`} onClick={() => props.on_refresh(props.entry.locator)} />}
      </div>
      {reason && <p className={styles.tree_reason}>{reason}</p>}
      {directory && available && open && (
        <DirectoryChildren
          directories={props.directories}
          depth={props.depth + 1}
          expanded={props.expanded}
          locator={props.entry.locator}
          on_refresh={props.on_refresh}
          on_open_file={props.on_open_file}
          on_open_resource_menu={props.on_open_resource_menu}
          on_toggle={props.on_toggle}
          state={state}
        />
      )}
    </div>
  );
}

function ResourceFileIcon(props: Readonly<{ file_name: string }>) {
  const icon_url = resolveMaterialFileIcon(props.file_name);
  return icon_url
    ? <img alt="" aria-hidden="true" className={styles.tree_file_icon} src={icon_url} />
    : <Icon name="file" size={16} />;
}

function ResourceFolderIcon(props: Readonly<{
  folder_name: string;
  open: boolean;
  root: boolean;
}>) {
  const icon_url = resolveMaterialFolderIcon(props.folder_name, props.open, props.root);
  return icon_url
    ? <img alt="" aria-hidden="true" className={styles.tree_folder_icon} src={icon_url} />
    : <Icon name="folder" size={17} />;
}

export function locatorKey(locator: SessionResourceLocator): string {
  return `${rootKey(locator.root)}:${locator.relative_path}`;
}

function directoryAncestors(locator: SessionResourceLocator): SessionResourceLocator[] {
  const segments = locator.relative_path.split("/").filter(Boolean);
  return [
    { ...locator, relative_path: "" },
    ...segments.map((_, index) => ({
      ...locator,
      relative_path: segments.slice(0, index + 1).join("/"),
    })),
  ];
}

function rootKey(root: SessionResourceRoot): string {
  return root.type === "workspace_additional"
    ? `${root.type}:${root.directory_index}`
    : root.type;
}

function formatBytes(value: number): string {
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
  return `${(value / (1024 * 1024)).toFixed(1)} MB`;
}
