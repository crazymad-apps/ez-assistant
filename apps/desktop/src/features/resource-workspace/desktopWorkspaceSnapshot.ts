import type { ApprovalMode, ModelSelection } from "@ez-assistant/protocol";
import type { DesktopPins } from "../../stores/DesktopPinsStore";
import { parseResourceSnapshot, type ResourceWorkspaceSnapshot } from "./resourceWorkspaceSnapshot";

export type DesktopWorkspaceSnapshot = Readonly<{
  version: 1;
  owner: string;
  pins: DesktopPins;
  devices: readonly Readonly<{ device: string | null; expanded: readonly string[]; defaults?: Readonly<{ approval: ApprovalMode; model: ModelSelection | null }>; resources: ResourceWorkspaceSnapshot | null }>[];
}>;

/** 原身份分区之外的描述不能解释为当前设备；拒绝整段新状态但保留磁盘原记录。 */
export function parseDesktopWorkspace(value: unknown, owner: string): DesktopWorkspaceSnapshot | null {
  if (value == null) return null;
  if (!record(value) || value.version !== 1 || value.owner !== owner || !record(value.pins)
    || typeof value.pins.imported !== "boolean" || !Array.isArray(value.pins.items) || value.pins.items.length > 2048
    || !Array.isArray(value.devices) || value.devices.length > 64) throw invalid();
  const pins = new Set<string>();
  for (const pin of value.pins.items) {
    if (!record(pin) || !device(pin.device) || !text(pin.session_id) || !text(pin.title) || !text(pin.device_name)) throw invalid();
    const key = JSON.stringify([pin.device, pin.session_id]);
    if (pins.has(key)) throw invalid();
    pins.add(key);
  }
  const devices = new Set<string | null>();
  for (const item of value.devices) {
    if (!record(item) || !device(item.device) || devices.has(item.device) || !Array.isArray(item.expanded)
      || item.expanded.length > 256 || !item.expanded.every(text)) throw invalid();
    if (item.defaults !== undefined && (!record(item.defaults) || !["ask", "auto"].includes(String(item.defaults.approval))
      || (item.defaults.model !== null && (!record(item.defaults.model) || !text(item.defaults.model.provider_instance_id) || !text(item.defaults.model.model_id))))) throw invalid();
    devices.add(item.device);
    const snapshot = parseResourceSnapshot(item.resources);
    if (item.device && snapshot?.groups.some((group) => group.tabs.some((tab) => tab.page.type === "resource" && tab.page.source.type === "local_file"))) throw invalid();
  }
  // 旧固定选择不再恢复；仅保留各设备有效会话／草稿的标签描述。
  const snapshot = value as DesktopWorkspaceSnapshot;
  return {
    version: 1, owner: snapshot.owner, pins: snapshot.pins,
    devices: snapshot.devices.map((item) => ({ ...item, resources: parseResourceSnapshot(item.resources) })),
  };
}

function record(value: unknown): value is Record<string, unknown> { return !!value && typeof value === "object" && !Array.isArray(value); }

function text(value: unknown): value is string { return typeof value === "string" && value.length > 0 && value.length <= 16384; }

function device(value: unknown): value is string | null { return value === null || (typeof value === "string" && /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(value)); }

function invalid(): Error { return new Error("本机工作状态损坏或身份不匹配，已保留原记录，未覆盖。"); }
