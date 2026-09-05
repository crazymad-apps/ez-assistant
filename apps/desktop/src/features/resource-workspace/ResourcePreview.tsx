import { useCallback, useEffect, useLayoutEffect, useRef, useState, type KeyboardEvent, type MouseEvent, type RefObject } from "react";
import type { ReactZoomPanPinchRef } from "react-zoom-pan-pinch";
import { ResourceImageViewer } from "./ResourceImageViewer";
import type {
  AttachmentSummary,
  ConversationOwner,
  MessageId,
  PreviewSessionResourceFileResult,
  SessionResourceEntry,
  SessionResourceLocator,
  ToolFileReference,
} from "../../generated/assistant-protocol";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "../../components/DropdownMenu";
import { Icon, type IconName } from "../../components/Icon";
import { MarkdownContent } from "../../components/MarkdownContent";
import { PdfViewer } from "../../components/PdfViewer";
import {
  copyAttachmentPath,
  copyLocalResourcePath,
  copySessionResourcePath,
  copyToolFilePath,
  openAttachmentInSystem,
  listLocalResourceSiblings,
  listSessionResourceFiles,
  openLocalResourceInSystem,
  openSessionResourceInSystem,
  openToolFileInSystem,
  previewAttachment,
  previewLocalResource,
  previewSessionResourceFile,
  previewToolFile,
  registerLocalFileUri,
  registerLocalResourceSibling,
  registerRelativeLocalResource,
  revealAttachmentInDirectory,
  revealLocalResourceInDirectory,
  revealSessionResourceInDirectory,
  revealToolFileInDirectory,
  type AttachmentPreview,
  type LocalResourcePreview,
  type RegisteredLocalResource,
} from "../../native-bridge/nativeResource";
import {
  isPreviewableResource,
  type ResourceHandle,
  type ResourceScopeKey,
  type ResourceTab,
} from "./ResourceWorkspaceStore";
import { createResourceObjectUrl } from "../../native-bridge/resourceObjectUrl";
import { MonacoTextViewer, type MonacoTextViewerHandle } from "./MonacoTextViewer";
import { resolveMaterialFileIcon, resolveMaterialFolderIcon } from "./materialResourceIcon";
import {
  ResourceContextMenu,
  type ResourceMenuItem,
  type ResourceMenuLocation,
} from "./ResourceContextMenu";
import type { ResourceViewState } from "./resourceViewState";
import styles from "./ResourceWorkspace/index.module.scss";

export function ResourcePreview(props: Readonly<{
  active?: boolean;
  view_state?: ResourceViewState;
  roots: readonly Readonly<{ label: string; locator: SessionResourceLocator | null; path?: string }>[];
  tab: Extract<ResourceTab, { type: "text" | "markdown" | "image" | "pdf" }>;
  on_focus_workspace: (scope_key: ResourceScopeKey, locator: SessionResourceLocator) => void;
  on_open_attachment: (attachment: AttachmentSummary, siblings: readonly AttachmentSummary[]) => void;
  on_open_file: (entry: SessionResourceEntry) => void;
  on_open_local_resource: (resource: RegisteredLocalResource) => void;
  on_open_tool_resource: (
    owner: ConversationOwner,
    message_id: MessageId,
    file: ToolFileReference,
    siblings: readonly ToolFileReference[],
  ) => void;
}>) {
  const { resource } = props.tab;
  const source = resource.source;
  const [revision, setRevision] = useState(0);
  const [preview, setPreview] = useState<PreviewSessionResourceFileResult | LocalResourcePreview | AttachmentPreview | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [view_state] = useState(() => {
    const value = props.view_state?.preview ?? { scroll_top: 0, scroll_left: 0, word_wrap: false, editor: null };
    if (props.view_state) props.view_state.preview = value;
    return value;
  });
  const image_ref = useRef<ReactZoomPanPinchRef>(null);
  const [word_wrap, setWordWrap] = useState(view_state.word_wrap);
  const body_ref = useRef<HTMLDivElement>(null);
  const restore_scroll = useRef(true);
  useEffect(() => { view_state.word_wrap = word_wrap; }, [word_wrap, view_state]);
  useLayoutEffect(() => {
    const body = body_ref.current;
    if (!body || !restore_scroll.current || loading) return;
    const restore = () => { body.scrollTop = view_state.scroll_top; body.scrollLeft = view_state.scroll_left; };
    restore();
    const observer = new ResizeObserver(restore);
    if (body.firstElementChild) observer.observe(body.firstElementChild);
    return () => observer.disconnect();
  }, [loading, preview, view_state]);
  const [action_error, setActionError] = useState<string | null>(null);
  const monaco_ref = useRef<MonacoTextViewerHandle>(null);
  const [context_menu, setContextMenu] = useState<Readonly<{
    items: readonly ResourceMenuItem[];
    location: ResourceMenuLocation;
  }> | null>(null);

  useEffect(() => {
    let active = true;
    setLoading(true);
    setError(null);
    const request = loadPreview(resource);
    void request
      .then((result) => {
        if (active) setPreview(result);
      })
      .catch((failure: unknown) => {
        if (active) {
          setPreview(null);
          setError(failure instanceof Error ? failure.message : "无法读取该资源。");
        }
      })
      .finally(() => {
        if (active) setLoading(false);
      });
    return () => { active = false; };
  }, [resource.resource_key, revision, source]);

  useEffect(() => { if (props.active === false) setContextMenu(null); }, [props.active]);

  const resolveRelativeLocator = useCallback((reference: string): SessionResourceLocator | null => {
    return source.type === "session_file" ? resolveRelativeResource(source.locator, reference) : null;
  }, [source]);

  function openInSystem() {
    switch (source.type) {
      case "session_file": return openSessionResourceInSystem(source.session_id, source.locator);
      case "local_file": return openLocalResourceInSystem(resource.resource_key);
      case "attachment": return openAttachmentInSystem(source.session_id, source.attachment_id);
      case "tool_file": return openToolFileInSystem(source.owner, source.message_id, source.resource_ref_id);
    }
  }

  function revealInDirectory() {
    switch (source.type) {
      case "session_file": return revealSessionResourceInDirectory(source.session_id, source.locator);
      case "local_file": return revealLocalResourceInDirectory(resource.resource_key);
      case "attachment": return revealAttachmentInDirectory(source.session_id, source.attachment_id);
      case "tool_file": return revealToolFileInDirectory(source.owner, source.message_id, source.resource_ref_id);
    }
  }

  function copyFullPath() {
    switch (source.type) {
      case "session_file": return copySessionResourcePath(source.session_id, source.locator);
      case "local_file": return copyLocalResourcePath(resource.resource_key);
      case "attachment": return copyAttachmentPath(source.session_id, source.attachment_id);
      case "tool_file": return copyToolFilePath(source.owner, source.message_id, source.resource_ref_id);
    }
  }

  function runAction(request: Promise<void>, fallback: string) {
    setActionError(null);
    void request.catch((failure: unknown) => {
      setActionError(failure instanceof Error ? failure.message : fallback);
    });
  }

  function showContextMenu(event: MouseEvent<HTMLElement>) {
    event.preventDefault();
    event.currentTarget.focus();
    setContextMenu({
      items: resourceActionItems(openInSystem, revealInDirectory, runAction),
      location: { x: event.clientX, y: event.clientY },
    });
  }

  function handleContextMenuKey(event: KeyboardEvent<HTMLElement>) {
    if (event.key !== "ContextMenu" && !(event.shiftKey && event.key === "F10")) return;
    event.preventDefault();
    const bounds = event.currentTarget.getBoundingClientRect();
    setContextMenu({
      items: resourceActionItems(openInSystem, revealInDirectory, runAction),
      location: { x: bounds.left + 20, y: bounds.top + 20 },
    });
  }

  function showLinkedResourceMenu(reference: string, location: ResourceMenuLocation) {
    const openRegistered = (registered: RegisteredLocalResource) => {
      setContextMenu({
        location,
        items: [
          { label: "在资源栏打开", on_select: () => props.on_open_local_resource(registered) },
          {
            label: "使用系统应用打开",
            on_select: () => runAction(openLocalResourceInSystem(registered.resource_key), "无法使用系统应用打开。"),
          },
          {
            label: "在 Finder 中显示",
            on_select: () => runAction(revealLocalResourceInDirectory(registered.resource_key), "无法在 Finder 中显示。"),
          },
        ],
      });
    };
    if (isFileUri(reference)) {
      void registerLocalFileUri(reference).then(openRegistered).catch(showResourceFailure);
      return;
    }
    if (source.type === "local_file") {
      void registerRelativeLocalResource(resource.resource_key, reference)
        .then(openRegistered)
        .catch(showResourceFailure);
      return;
    }
    if (source.type !== "session_file") return;
    const locator = resolveRelativeLocator(reference);
    if (!locator) {
      setActionError("本地资源路径无效。");
      return;
    }
    const entry = sessionFileEntry(locator, reference);
    setContextMenu({
      location,
      items: [
        { label: "在资源栏打开", on_select: () => props.on_open_file(entry) },
        {
          label: "使用系统应用打开",
          on_select: () => runAction(openSessionResourceInSystem(source.session_id, locator), "无法使用系统应用打开。"),
        },
        {
          label: "在 Finder 中显示",
          on_select: () => runAction(revealSessionResourceInDirectory(source.session_id, locator), "无法在 Finder 中显示。"),
        },
      ],
    });
  }

  function showResourceFailure(failure: unknown) {
    setActionError(failure instanceof Error ? failure.message : "无法打开本地资源菜单。");
  }

  return (
    <div className={styles.resource_preview}>
      <div className={styles.resource_header} key={String(props.active ?? true)}>
        {source.type === "session_file" ? (
          <ResourceBreadcrumb
            handle={resource}
            on_focus_workspace={props.on_focus_workspace}
            on_open_file={props.on_open_file}
            roots={props.roots}
          />
        ) : source.type === "local_file" ? (
          <LocalResourceBreadcrumb
            handle={resource}
            on_open_resource={props.on_open_local_resource}
            roots={props.roots}
          />
        ) : (
          <OpaqueResourceBreadcrumb
            label={source.type === "attachment" ? "会话附件" : "工具产物"}
            name={resource.display_name}
            on_select={(item) => {
              if (source.type === "attachment" && item.type === "attachment") {
                props.on_open_attachment(item.value, source.siblings);
              }
              if (source.type === "tool_file" && item.type === "tool_file") {
                props.on_open_tool_resource(source.owner, source.message_id, item.value, source.siblings);
              }
            }}
            siblings={source.type === "attachment"
              ? source.siblings.map((attachment) => ({
                  disabled: !isPreviewableResource(attachment.original_name, attachment.media_type),
                  key: attachment.attachment_id,
                  name: attachment.original_name,
                  type: "attachment" as const,
                  value: attachment,
                }))
              : source.siblings.map((file) => ({
                  disabled: file.state !== "available" || !isPreviewableResource(file.display_name, file.media_type),
                  key: file.resource_ref_id,
                  name: file.display_name,
                  type: "tool_file" as const,
                  value: file,
                }))}
          />
        )}
        {action_error && <span className={styles.resource_action_error} role="alert">{action_error}</span>}
        <ResourceActionsMenu
          file_size={preview?.size_bytes ?? null}
          image_available={props.tab.type === "image" && preview?.kind === "image"}
          on_copy_path={() => runAction(copyFullPath(), "无法复制完整路径。")}
          on_find={() => monaco_ref.current?.find()}
          on_open_system={() => runAction(openInSystem(), "无法使用系统应用打开。")}
          on_refresh={() => setRevision((value) => value + 1)}
          on_reveal={() => runAction(revealInDirectory(), "无法在 Finder 中显示。")}
          image_ref={image_ref}
          on_toggle_wrap={() => setWordWrap((value) => !value)}
          text_available={props.tab.type === "text" && preview?.kind === "text"}
          word_wrap={word_wrap}
        />
      </div>
      <div className={styles.resource_body} ref={body_ref}
        onWheel={() => { restore_scroll.current = false; }}
        onPointerDown={() => { restore_scroll.current = false; }}
        onScroll={(event) => { if (!restore_scroll.current) { view_state.scroll_top = event.currentTarget.scrollTop; view_state.scroll_left = event.currentTarget.scrollLeft; } }} onContextMenu={showContextMenu} onKeyDown={(event) => { restore_scroll.current = false; handleContextMenuKey(event); }} tabIndex={0}>
        {loading ? <p className={styles.resource_state}>正在读取…</p> : error ? (
          <div className={styles.resource_state}><p>{error}</p><button onClick={() => setRevision((value) => value + 1)} type="button">重试</button></div>
        ) : preview ? renderPreview(
          props.tab,
          preview,
          image_ref,
          props.active !== false,
          word_wrap,
          monaco_ref,
          view_state,
          resolveRelativeLocator,
          props.on_open_file,
          props.on_open_local_resource,
          (message) => setActionError(message),
          showLinkedResourceMenu,
        ) : null}
      </div>
      {props.active !== false && context_menu && (
        <ResourceContextMenu
          items={context_menu.items}
          location={context_menu.location}
          on_close={() => setContextMenu(null)}
        />
      )}
    </div>
  );
}

function loadPreview(resource: ResourceHandle): Promise<PreviewSessionResourceFileResult | LocalResourcePreview | AttachmentPreview> {
  const source = resource.source;
  switch (source.type) {
    case "session_file": return previewSessionResourceFile(source.session_id, { locator: source.locator });
    case "local_file": return previewLocalResource(resource.resource_key);
    case "attachment": return previewAttachment(source.session_id, source.attachment_id);
    case "tool_file": return previewToolFile(source.owner, source.message_id, source.resource_ref_id);
  }
}

function ResourceActionsMenu(props: Readonly<{
  file_size: number | null;
  image_ref: RefObject<ReactZoomPanPinchRef | null>;
  image_available: boolean;
  on_copy_path: () => void;
  on_find: () => void;
  on_open_system: () => void;
  on_refresh: () => void;
  on_reveal: () => void;
  on_toggle_wrap: () => void;
  text_available: boolean;
  word_wrap: boolean;
}>) {
  return (
    <DropdownMenu className={styles.resource_more_menu}>
      <DropdownMenuTrigger
        aria-label="更多资源操作"
        className={styles.resource_more_trigger}
        iconOnly
        variant="text"
      >
        <Icon name="more" size={17} />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className={styles.resource_actions_menu}>
        {props.file_size !== null && (
          <div className={styles.resource_menu_meta}>文件大小 {formatBytes(props.file_size)}</div>
        )}
        {props.text_available && (
          <>
            <ResourceActionMenuItem icon="search" label="查找" on_select={props.on_find} />
            <ResourceActionMenuItem
              checked={props.word_wrap}
              icon="file"
              label="自动换行"
              on_select={props.on_toggle_wrap}
            />
          </>
        )}
        {props.image_available && (
          <>
            <ResourceActionMenuItem icon="zoom-in" label="放大" on_select={() => { void props.image_ref.current?.zoomIn(); }} />
            <ResourceActionMenuItem icon="zoom-out" label="缩小" on_select={() => { void props.image_ref.current?.zoomOut(); }} />
            <ResourceActionMenuItem icon="image" label="适应窗口" on_select={() => { void props.image_ref.current?.fitToView(); }} />
            <ResourceActionMenuItem icon="image" label="原始尺寸（100%）" on_select={() => { void props.image_ref.current?.centerView(1); }} />
          </>
        )}
        <ResourceActionMenuItem icon="refresh" label="重新加载" on_select={props.on_refresh} />
        <ResourceActionMenuItem icon="external-link" label="使用系统应用打开" on_select={props.on_open_system} />
        <ResourceActionMenuItem icon="folder" label="在 Finder 中显示" on_select={props.on_reveal} />
        <ResourceActionMenuItem icon="copy" label="复制完整路径" on_select={props.on_copy_path} />
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function ResourceActionMenuItem(props: Readonly<{
  checked?: boolean;
  icon: IconName;
  label: string;
  on_select: () => void;
}>) {
  return (
    <DropdownMenuItem className={styles.resource_action_menu_item} onSelect={props.on_select}>
      <Icon name={props.icon} size={16} />
      <span>{props.label}</span>
      {props.checked && <Icon name="check" size={14} />}
    </DropdownMenuItem>
  );
}

type OpaqueSibling = Readonly<
  | { disabled: boolean; key: string; name: string; type: "attachment"; value: AttachmentSummary }
  | { disabled: boolean; key: string; name: string; type: "tool_file"; value: ToolFileReference }
>;

function OpaqueResourceBreadcrumb(props: Readonly<{
  label: string;
  name: string;
  on_select: (item: OpaqueSibling) => void;
  siblings: readonly OpaqueSibling[];
}>) {
  return (
    <nav aria-label="资源路径" className={styles.breadcrumb}>
      <span className={styles.breadcrumb_plain_node}>
        <span>{props.label}</span>
        <Icon name="chevron-right" size={13} />
      </span>
      <DropdownMenu>
        <DropdownMenuTrigger aria-label={`${props.label}中的资源`} className={styles.breadcrumb_node_trigger} data-current="true">
          <span>{props.name}</span>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="start" className={styles.breadcrumb_menu}>
          {props.siblings.map((item) => (
            <DropdownMenuItem disabled={item.disabled} key={item.key} onSelect={() => props.on_select(item)}>
              <MaterialIcon url={resolveMaterialFileIcon(item.name)} fallback="file" />
              <span>{item.name}</span>
              {item.name === props.name && <Icon name="check" size={14} />}
            </DropdownMenuItem>
          ))}
        </DropdownMenuContent>
      </DropdownMenu>
    </nav>
  );
}

function renderPreview(
  tab: Extract<ResourceTab, { type: "text" | "markdown" | "image" | "pdf" }>,
  preview: PreviewSessionResourceFileResult | LocalResourcePreview | AttachmentPreview,
  image_ref: RefObject<ReactZoomPanPinchRef | null>,
  active: boolean,
  word_wrap: boolean,
  monaco_ref: RefObject<MonacoTextViewerHandle | null>,
  view_state: NonNullable<ResourceViewState["preview"]>,
  resolve_relative: (reference: string) => SessionResourceLocator | null,
  on_open_file: (entry: SessionResourceEntry) => void,
  on_open_local_resource: (resource: RegisteredLocalResource) => void,
  on_error: (message: string) => void,
  on_local_resource_menu: (reference: string, location: ResourceMenuLocation) => void,
) {
  if (preview.kind === "pdf" && "data_base64" in preview && preview.data_base64) {
    return <PdfViewer base64={preview.data_base64} title={`${tab.resource.display_name} PDF 预览`} />;
  }
  if (tab.type === "pdf") {
    return <p className={styles.resource_state}>文件内容不是受支持的 PDF。</p>;
  }
  if (preview.kind === "image") {
    return <ResourceImageViewer
      active={active}
      alt={tab.resource.display_name}
      base64={"data_base64" in preview ? preview.data_base64 : undefined}
      data_url={"data_url" in preview ? preview.data_url : undefined}
      media_type={preview.media_type}
      ref={image_ref}
      view_state={view_state}
    />;
  }
  if (tab.type === "image") {
    return <p className={styles.resource_state}>文件内容不是受支持的图片。</p>;
  }
  if (preview.kind !== "text" || typeof preview.text !== "string") {
    return <p className={styles.resource_state}>该文件不支持应用内预览。</p>;
  }
  if (tab.type === "markdown") {
    const source = tab.resource.source;
    const supports_relative_resources = source.type === "session_file" || source.type === "local_file";
    return (
      <div className={styles.markdown_viewer}>
        <MarkdownContent
          allow_relative_local_resources={supports_relative_resources}
          load_local_image={supports_relative_resources ? async (reference) => {
            let image: PreviewSessionResourceFileResult | LocalResourcePreview;
            if (isFileUri(reference)) {
              image = await registerLocalFileUri(reference)
                .then((registered) => previewLocalResource(registered.resource_key));
            } else if (source.type === "session_file") {
              const locator = resolve_relative(reference);
              if (!locator) throw new Error("本地图片路径无效。");
              image = await previewSessionResourceFile(source.session_id, { locator });
            } else {
              image = await registerRelativeLocalResource(tab.resource.resource_key, reference)
                .then((registered) => previewLocalResource(registered.resource_key));
            }
            if (image.kind !== "image" || !image.data_base64) throw new Error("该资源不是可预览图片。");
            return createResourceObjectUrl(image.data_base64, image.media_type);
          } : undefined}
          on_local_resource_open={supports_relative_resources ? (reference) => {
            if (isFileUri(reference)) {
              void registerLocalFileUri(reference).then(on_open_local_resource).catch((failure: unknown) => {
                on_error(failure instanceof Error ? failure.message : "无法打开本地资源。");
              });
            } else if (source.type === "session_file") {
              const locator = resolve_relative(reference);
              if (!locator) return;
              const display_name = locator.relative_path.split("/").at(-1) ?? reference;
              on_open_file({
                locator,
                display_name,
                kind: "file",
                state: "available",
                is_symbolic_link: false,
                is_hidden: false,
                is_generated: false,
              });
            } else {
              void registerRelativeLocalResource(tab.resource.resource_key, reference)
                .then(on_open_local_resource)
                .catch((failure: unknown) => {
                  on_error(failure instanceof Error ? failure.message : "无法打开本地资源。");
                });
            }
          } : undefined}
          on_local_resource_context_menu={supports_relative_resources ? on_local_resource_menu : undefined}
          text={preview.text}
        />
      </div>
    );
  }
  return (
    <MonacoTextViewer
      view_state={view_state}
      file_name={tab.resource.display_name}
      initial_line={tab.type === "text" ? tab.line : null}
      ref={monaco_ref}
      resource_key={tab.resource.resource_key}
      text={preview.text}
      word_wrap={word_wrap}
    />
  );
}

function resourceActionItems(
  open_in_system: () => Promise<void>,
  reveal_in_directory: () => Promise<void>,
  run_action: (request: Promise<void>, fallback: string) => void,
): readonly ResourceMenuItem[] {
  return [
    {
      label: "使用系统应用打开",
      on_select: () => run_action(open_in_system(), "无法使用系统应用打开。"),
    },
    {
      label: "在 Finder 中显示",
      on_select: () => run_action(reveal_in_directory(), "无法在 Finder 中显示。"),
    },
  ];
}

function sessionFileEntry(locator: SessionResourceLocator, reference: string): SessionResourceEntry {
  return {
    locator,
    display_name: locator.relative_path.split("/").at(-1) ?? reference,
    kind: "file",
    state: "available",
    is_symbolic_link: false,
    is_hidden: false,
    is_generated: false,
  };
}

function LocalResourceBreadcrumb(props: Readonly<{
  handle: ResourceHandle;
  on_open_resource: (resource: RegisteredLocalResource) => void;
  roots: readonly Readonly<{ label: string; path?: string }>[];
}>) {
  const source = props.handle.source;
  if (source.type !== "local_file") return null;
  const path_segments = localBreadcrumbSegments(source.path_segments, props.roots);
  return (
    <nav aria-label="资源路径" className={styles.breadcrumb}>
      {path_segments.map((segment, index) => {
        const current = index === path_segments.length - 1;
        if (current) {
          return (
            <LocalSiblingMenu
              current_name={props.handle.display_name}
              key={`${segment}:${index}`}
              label={segment}
              on_open_resource={props.on_open_resource}
              resource_key={props.handle.resource_key}
              show_arrow={false}
            />
          );
        }
        return (
          <span className={styles.breadcrumb_plain_node} key={`${segment}:${index}`}>
            <span>{segment}</span>
            <Icon name="chevron-right" size={13} />
          </span>
        );
      })}
    </nav>
  );
}

function LocalSiblingMenu(props: Readonly<{
  current_name: string;
  label: string;
  on_open_resource: (resource: RegisteredLocalResource) => void;
  resource_key: string;
  show_arrow: boolean;
}>) {
  const [siblings, setSiblings] = useState<Awaited<ReturnType<typeof listLocalResourceSiblings>> | null>(null);
  const [error, setError] = useState(false);
  function load() {
    setError(false);
    void listLocalResourceSiblings(props.resource_key).then(setSiblings).catch(() => setError(true));
  }
  return (
    <DropdownMenu>
      <DropdownMenuTrigger aria-label="查看同级项目" className={styles.breadcrumb_node_trigger} onClick={load}>
        <span>{props.label}</span>
        {props.show_arrow && <Icon name="chevron-right" size={13} />}
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className={styles.breadcrumb_menu}>
        {error ? <div className={styles.breadcrumb_menu_state}>无法读取同级项目</div> : siblings === null ? (
          <div className={styles.breadcrumb_menu_state}>正在读取…</div>
        ) : siblings.length === 0 ? <div className={styles.breadcrumb_menu_state}>没有其他项目</div> : siblings.map((entry) => (
          <DropdownMenuItem
            disabled={entry.kind === "directory"}
            key={`${entry.kind}:${entry.display_name}`}
            onSelect={() => {
              if (entry.kind !== "file") return;
              void registerLocalResourceSibling(props.resource_key, entry.display_name)
                .then(props.on_open_resource);
            }}
          >
            {entry.kind === "directory"
              ? <MaterialIcon url={resolveMaterialFolderIcon(entry.display_name, false, false)} fallback="folder" />
              : <MaterialIcon url={resolveMaterialFileIcon(entry.display_name)} fallback="file" />}
            <span>{entry.display_name}</span>
            {(entry.current || entry.display_name === props.current_name) && <Icon name="check" size={14} />}
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function ResourceBreadcrumb(props: Readonly<{
  handle: ResourceHandle;
  roots: readonly Readonly<{ label: string; locator: SessionResourceLocator | null }>[];
  on_focus_workspace: (scope_key: ResourceScopeKey, locator: SessionResourceLocator) => void;
  on_open_file: (entry: SessionResourceEntry) => void;
}>) {
  const source = props.handle.source;
  if (source.type !== "session_file") return null;
  const root = props.roots.find((item) => item.locator && sameRoot(item.locator, source.locator));
  const path = source.locator.relative_path.split("/").filter(Boolean);
  const items = [{ name: root?.label ?? "资源根", path: "", directory: true }];
  path.forEach((name, index) => items.push({
    name,
    path: path.slice(0, index + 1).join("/"),
    directory: index < path.length - 1,
  }));
  return (
    <nav aria-label="资源路径" className={styles.breadcrumb}>
      {items.map((item, index) => {
        const locator = { ...source.locator, relative_path: item.path };
        return index === 0 ? (
          <RootBreadcrumbSiblings
            current_name={item.name}
            key={`${item.path}:${index}`}
            on_focus_workspace={(value) => props.on_focus_workspace(props.handle.scope_key, value)}
            roots={props.roots}
          />
        ) : (
          <BreadcrumbSiblings
            current_name={item.name}
            directory={item.directory}
            key={`${item.path}:${index}`}
            locator={locator}
            on_focus_workspace={(value) => props.on_focus_workspace(props.handle.scope_key, value)}
            on_open_file={props.on_open_file}
            session_id={source.session_id}
            show_arrow={index < items.length - 1}
          />
        );
      })}
    </nav>
  );
}

function RootBreadcrumbSiblings(props: Readonly<{
  current_name: string;
  on_focus_workspace: (locator: SessionResourceLocator) => void;
  roots: readonly Readonly<{ label: string; locator: SessionResourceLocator | null }>[];
}>) {
  const available_roots = props.roots.filter(
    (root): root is Readonly<{ label: string; locator: SessionResourceLocator }> => root.locator !== null,
  );
  return (
    <DropdownMenu>
      <DropdownMenuTrigger aria-label="切换资源根" className={styles.breadcrumb_node_trigger}>
        <span>{props.current_name}</span>
        <Icon name="chevron-right" size={13} />
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className={styles.breadcrumb_menu}>
        {available_roots.map((root) => (
          <DropdownMenuItem key={root.label} onSelect={() => props.on_focus_workspace(root.locator)}>
            <MaterialIcon url={resolveMaterialFolderIcon(root.label, false, true)} fallback="folder" />
            <span>{root.label}</span>
            {root.label === props.current_name && <Icon name="check" size={14} />}
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function BreadcrumbSiblings(props: Readonly<{
  current_name: string;
  directory: boolean;
  locator: SessionResourceLocator;
  on_focus_workspace: (locator: SessionResourceLocator) => void;
  on_open_file: (entry: SessionResourceEntry) => void;
  session_id: string;
  show_arrow: boolean;
}>) {
  const [entries, setEntries] = useState<readonly SessionResourceEntry[] | null>(null);
  const [error, setError] = useState(false);
  const parent_path = props.locator.relative_path.split("/").filter(Boolean).slice(0, -1).join("/");
  const parent = { ...props.locator, relative_path: parent_path };
  function load() {
    setError(false);
    void listSessionResourceFiles(props.session_id, {
      locator: props.locator.relative_path ? parent : props.locator,
      include_hidden: false,
      include_generated: false,
    }).then((result) => setEntries(result.entries)).catch(() => setError(true));
  }
  return (
    <DropdownMenu>
      <DropdownMenuTrigger
        aria-label={`${props.current_name}的同级项目`}
        className={styles.breadcrumb_node_trigger}
        data-current={!props.directory || undefined}
        onClick={load}
      >
        <span>{props.current_name}</span>
        {props.show_arrow && <Icon name="chevron-right" size={13} />}
      </DropdownMenuTrigger>
      <DropdownMenuContent align="start" className={styles.breadcrumb_menu}>
        {error ? <div className={styles.breadcrumb_menu_state}>无法读取同级项目</div> : entries === null ? (
          <div className={styles.breadcrumb_menu_state}>正在读取…</div>
        ) : entries.length === 0 ? <div className={styles.breadcrumb_menu_state}>没有其他项目</div> : entries.map((entry) => (
          <DropdownMenuItem
            disabled={entry.state !== "available"}
            key={`${entry.locator.relative_path}:${entry.kind}`}
            onSelect={() => entry.kind === "directory" ? props.on_focus_workspace(entry.locator) : props.on_open_file(entry)}
          >
            {entry.kind === "directory"
              ? <MaterialIcon url={resolveMaterialFolderIcon(entry.display_name, false, false)} fallback="folder" />
              : <MaterialIcon url={resolveMaterialFileIcon(entry.display_name)} fallback="file" />}
            <span>{entry.display_name}</span>
            {entry.display_name === props.current_name && <Icon name="check" size={14} />}
          </DropdownMenuItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  );
}

function MaterialIcon(props: Readonly<{ fallback: "file" | "folder"; url: string | null }>) {
  return props.url ? <img alt="" aria-hidden="true" className={styles.breadcrumb_file_icon} src={props.url} /> : <Icon name={props.fallback} size={16} />;
}

function resolveRelativeResource(base: SessionResourceLocator, reference: string): SessionResourceLocator | null {
  let value = reference;
  try {
    const parsed = new URL(reference);
    if (parsed.protocol !== "file:") return null;
    return null;
  } catch {
    value = reference.split(/[?#]/, 1)[0] ?? reference;
  }
  if (!value || value.startsWith("/") || value.startsWith("#") || value.includes("\\")) return null;
  const parent = base.relative_path.split("/").filter(Boolean).slice(0, -1);
  for (const part of value.split("/")) {
    if (!part || part === ".") continue;
    if (part === "..") {
      if (parent.length === 0) return null;
      parent.pop();
    } else {
      try {
        parent.push(decodeURIComponent(part));
      } catch {
        return null;
      }
    }
  }
  return { root: base.root, relative_path: parent.join("/") };
}

function isFileUri(reference: string): boolean {
  try {
    return new URL(reference).protocol === "file:";
  } catch {
    return false;
  }
}

function sameRoot(left: SessionResourceLocator, right: SessionResourceLocator): boolean {
  return JSON.stringify(left.root) === JSON.stringify(right.root);
}

function formatBytes(value: number | null): string {
  if (value === null) return "";
  if (value < 1024) return `${value} B`;
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
  return `${(value / (1024 * 1024)).toFixed(1)} MB`;
}

function localBreadcrumbSegments(
  path_segments: readonly string[],
  roots: readonly Readonly<{ label: string; path?: string }>[],
): readonly string[] {
  const normalized_path = displayPathFromSegments(path_segments);
  const root = roots
    .filter((candidate): candidate is Readonly<{ label: string; path: string }> => Boolean(candidate.path))
    .map((candidate) => ({ ...candidate, path: candidate.path.replace(/\/+$/, "") }))
    .filter((candidate) => normalized_path === candidate.path || normalized_path.startsWith(`${candidate.path}/`))
    .sort((left, right) => right.path.length - left.path.length)[0];
  if (root) {
    const suffix = normalized_path.slice(root.path.length).split("/").filter(Boolean);
    return [root.label, ...suffix];
  }
  return path_segments.filter((segment) => segment !== "/").slice(-2);
}

function displayPathFromSegments(path_segments: readonly string[]): string {
  if (path_segments[0] === "/") return `/${path_segments.slice(1).join("/")}`;
  return path_segments.join("/");
}
