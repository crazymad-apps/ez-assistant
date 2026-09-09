import type { RuntimeHostHealth, RuntimeHostStartupStage, RuntimeHostStartupError } from "../generated/assistant-protocol";

const stages: Record<RuntimeHostStartupStage, string> = {
  database_check: "正在检查数据库…",
  database_backup: "正在备份数据库…",
  database_migration: "正在升级数据库，请稍候…",
  configuration: "正在检查配置…",
  recovery: "正在恢复 Runtime…",
};
const errors: Record<RuntimeHostStartupError, string> = {
  database_unavailable: "数据库无法打开或结构异常。请修复后重新启动 Host。",
  database_newer: "数据库版本高于当前软件，请升级软件后重新连接。",
  migration_failed: "数据库升级失败，已停止后续升级。请修复后重新启动 Host。",
  backup_failed: "数据库备份或校验失败，未继续升级。请检查磁盘后重新启动 Host。",
  configuration_invalid: "配置无法加载。请修复配置后重新启动 Host。",
  initialization_failed: "Host 初始化失败。请检查启动日志后重新启动 Host。",
};
export function startupMessage(health: RuntimeHostHealth): string {
  if (health.status === "unavailable") return health.error ? errors[health.error] ?? errors.initialization_failed : errors.initialization_failed;
  if (health.status === "starting") return health.stage ? stages[health.stage] ?? "Host 正在初始化…" : "Host 正在初始化…";
  return "Host 已就绪";
}
