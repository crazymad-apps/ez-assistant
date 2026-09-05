import type { SessionResourceRootItem } from "./SessionResourceTree";
import type { editor } from "monaco-editor";
import type { SessionResourceLocator } from "../../generated/assistant-protocol";

/** 页面淘汰后保留的查看位置；不包含文件正文、目录结果、DOM 或原生句柄。 */
export type ResourceViewState = {
  roots?: readonly SessionResourceRootItem[];
  tree?: {
    expanded: readonly SessionResourceLocator[];
    focus_locator?: SessionResourceLocator | null;
    include_hidden: boolean;
    include_generated: boolean;
    scroll_top: number;
  };
  preview?: {
    scroll_top: number;
    scroll_left: number;
    word_wrap: boolean;
    image?: { scale: number; position_x: number; position_y: number };
    editor: editor.ICodeEditorViewState | null;
    initial_line?: number | null;
  };
};
