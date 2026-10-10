import type { FeedbackClientEnvironment, FeedbackContent, FeedbackFailure, FeedbackTargetKind, SubmittedFeedbackStatus } from "@ez-assistant/protocol";
import { isTauri } from "@tauri-apps/api/core";

export const kinds: Record<FeedbackTargetKind, string> = { platform: "平台", plugin: "插件", knowledge: "知识库" };
export const statuses: Record<SubmittedFeedbackStatus, string> = { pending: "待处理", processing: "处理中", resolved: "已解决", closed: "已关闭" };
export const failures: Record<FeedbackFailure, string> = {
  unsupported: "当前中心尚未开放反馈，请联系管理员升级中心。", invalid_input: "请检查目标、标题和描述是否完整，字段是否过长。",
  too_large: "内容过长，请精简后提交。", not_found: "未找到这条反馈。", conflict: "本次提交的内容与原记录不一致，请先检查提交结果。",
  unavailable: "暂时无法读取反馈，请稍后重试。", identity_changed: "登录已失效或切换，请重新登录后查看我的反馈。",
  submission_unknown: "尚未确认提交结果。请先检查提交结果，原请求可能已经保存。",
};
export type Draft = { kind: FeedbackTargetKind | ""; name: string; reference: string; version: string; content_ref: string; location: string; specific: boolean; title: string; description: string; reproduction: string; expected_result: string };
export const emptyDraft = (): Draft => ({ kind: "", name: "", reference: "", version: "", content_ref: "", location: "", specific: false, title: "", description: "", reproduction: "", expected_result: "" });
export function contentOf(draft: Draft): FeedbackContent | null {
  if (!draft.kind || (draft.kind === "knowledge" && draft.specific && !draft.content_ref.trim() && !draft.location.trim())) return null;
  const optional = (value: string) => value.trim() || null;
  return { target: { kind: draft.kind, name: draft.kind === "platform" ? "EZ Assistant" : draft.name.trim(),
    reference: draft.kind === "platform" ? "ez-assistant" : optional(draft.reference), version: draft.kind === "plugin" ? optional(draft.version) : null,
    content_ref: optional(draft.content_ref), location: optional(draft.location) }, title: draft.title.trim(), description: draft.description.trim(),
    reproduction: optional(draft.reproduction), expected_result: optional(draft.expected_result) };
}
export function clientEnvironment(): FeedbackClientEnvironment {
  // 浏览器无法可靠判断 OS/架构，不从远程 Host 或 UA 猜测本机事实。
  return { client_kind: isTauri() ? "desktop" : "web", client_version: __APP_VERSION__, client_os: null, client_arch: null };
}
