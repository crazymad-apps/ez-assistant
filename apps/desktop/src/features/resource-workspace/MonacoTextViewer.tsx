import { forwardRef, useId, useEffect, useImperativeHandle, useRef, useState } from "react";
import type { ResourceViewState } from "./resourceViewState";
import styles from "./ResourceWorkspace/index.module.scss";

type MonacoApi = Pick<typeof import("monaco-editor"), "editor" | "languages" | "Uri">;
type MonacoEditor = ReturnType<MonacoApi["editor"]["create"]>;
type WorkerConstructor = new () => Worker;

export type MonacoTextViewerHandle = Readonly<{
  find: () => void;
}>;

export const MonacoTextViewer = forwardRef<MonacoTextViewerHandle, Readonly<{
  view_state?: NonNullable<ResourceViewState["preview"]>;
  file_name: string;
  initial_line: number | null;
  resource_key: string;
  text: string;
  word_wrap: boolean;
}>>(function MonacoTextViewer(props, ref) {
  const instance_id = useId();
  const container_ref = useRef<HTMLDivElement>(null);
  const editor_ref = useRef<MonacoEditor | null>(null);
  const [initialization_error, setInitializationError] = useState<string | null>(null);
  const word_wrap_ref = useRef(props.word_wrap);
  word_wrap_ref.current = props.word_wrap;

  useImperativeHandle(ref, () => ({
    find: () => void editor_ref.current?.getAction("actions.find")?.run(),
  }), []);

  useEffect(() => {
    let disposed = false;
    const listeners: { dispose: () => void }[] = [];
    let model: ReturnType<MonacoApi["editor"]["createModel"]> | null = null;
    setInitializationError(null);
    void loadMonaco()
      .then((monaco) => {
        if (disposed || !container_ref.current) return;
        const model_uri = monaco.Uri.parse(
          `inmemory://ez-resource/${encodeURIComponent(instance_id)}/${encodeURIComponent(props.resource_key)}/${encodeURIComponent(props.file_name)}`,
        );
        model = monaco.editor.createModel(
          props.text,
          undefined,
          model_uri,
        );
        const editor = monaco.editor.create(container_ref.current, {
          automaticLayout: true,
          contextmenu: true,
          folding: true,
          fontSize: 13,
          lineHeight: 20,
          minimap: { enabled: false },
          model,
          readOnly: true,
          renderValidationDecorations: "off",
          scrollBeyondLastLine: false,
          stickyScroll: { enabled: false },
          wordWrap: word_wrap_ref.current ? "on" : "off",
        });
        editor_ref.current = editor;
        const saved = props.view_state;
        if (saved?.editor) editor.restoreViewState(saved.editor);
        if (props.initial_line && props.initial_line > 0 && (!saved?.editor || saved.initial_line !== props.initial_line)) {
          editor.revealLineInCenter(props.initial_line);
          editor.setPosition({ lineNumber: props.initial_line, column: 1 });
        }
        if (saved) {
          saved.initial_line = props.initial_line;
          const capture = () => { saved.editor = editor.saveViewState(); };
          listeners.push(editor.onDidScrollChange(capture), editor.onDidChangeCursorPosition(capture));
          capture();
        }
      })
      .catch(() => {
        editor_ref.current?.dispose();
        editor_ref.current = null;
        model?.dispose();
        model = null;
        if (!disposed) {
          setInitializationError("代码查看器加载失败，请重新加载资源。");
        }
      });
    return () => {
      disposed = true;
      for (const listener of listeners) listener.dispose();
      if (props.view_state && editor_ref.current) props.view_state.editor = editor_ref.current.saveViewState();
      editor_ref.current?.dispose();
      editor_ref.current = null;
      model?.dispose();
    };
  }, [instance_id, props.file_name, props.initial_line, props.resource_key, props.text, props.view_state]);

  useEffect(() => {
    editor_ref.current?.updateOptions({ wordWrap: props.word_wrap ? "on" : "off" });
  }, [props.word_wrap]);

  return (
    <div className={styles.text_viewer}>
      {initialization_error
        ? <p className={styles.resource_state} role="alert">{initialization_error}</p>
        : <div className={styles.monaco_container} ref={container_ref} />}
    </div>
  );
});

let monaco_promise: Promise<MonacoApi> | null = null;

async function loadMonaco(): Promise<MonacoApi> {
  monaco_promise ??= Promise.all([
    import("monaco-editor/editor/editor.api"),
    import("monaco-editor/editor/editor.worker?worker"),
    import("monaco-editor/editor/contrib/find/browser/findController"),
    import("monaco-editor/basic-languages/monaco.contribution"),
    import("monaco-editor/language/json/monaco.contribution"),
    import("monaco-editor/language/json/json.worker?worker"),
  ]).then(([monaco, worker_module, , , , json_worker_module]) => {
    worker_constructors.set("editorWorkerService", worker_module.default);
    worker_constructors.set("json", json_worker_module.default);
    const root = globalThis as typeof globalThis & {
      MonacoEnvironment?: { getWorker: (_module_id: string, label: string) => Worker };
    };
    root.MonacoEnvironment = {
      getWorker: (_module_id, label) => new (worker_constructors.get(label) ?? worker_module.default)(),
    };
    return monaco as MonacoApi;
  });
  let monaco: MonacoApi;
  try {
    monaco = await monaco_promise;
  } catch (error) {
    monaco_promise = null;
    throw error;
  }
  return monaco;
}

const worker_constructors = new Map<string, WorkerConstructor>();
