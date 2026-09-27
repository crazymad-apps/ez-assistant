import { existsSync, readFileSync } from "node:fs";

const config_path = new URL("../src/features/composer/ComposerDock/quick-actions.json", import.meta.url);

export function readQuickActionsConfig(enabled) {
  if (!enabled || !existsSync(config_path)) return [];
  const actions = JSON.parse(readFileSync(config_path, "utf8"));
  if (!Array.isArray(actions)) throw new Error("快捷入口模板必须是数组");
  return actions;
}
