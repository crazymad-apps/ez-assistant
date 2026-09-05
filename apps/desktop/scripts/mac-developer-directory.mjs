import { spawnSync } from "node:child_process";
import { existsSync, readdirSync } from "node:fs";
import { homedir } from "node:os";
import { join } from "node:path";
import { env, platform } from "node:process";

/**
 * 系统升级后，xcode-select 可能仍指向尚未完成许可配置的旧 Xcode。这里仅为当前
 * 子进程选择一个能实际解析 macOS SDK 的 Developer 目录，不修改系统级 xcode-select。
 */
export function selectMacDeveloperDirectory(log_prefix) {
  if (platform !== "darwin" || env.DEVELOPER_DIR || macSdkIsAvailable()) {
    return;
  }

  for (const developer_directory of macDeveloperDirectoryCandidates()) {
    if (!macSdkIsAvailable(developer_directory)) {
      continue;
    }
    env.DEVELOPER_DIR = developer_directory;
    console.info(`[${log_prefix}] 使用 Xcode Developer 目录：${developer_directory}`);
    return;
  }
}

function macSdkIsAvailable(developer_directory) {
  const environment = developer_directory
    ? { ...env, DEVELOPER_DIR: developer_directory }
    : env;
  const result = spawnSync("/usr/bin/xcrun", ["--sdk", "macosx", "--show-sdk-path"], {
    env: environment,
    stdio: "ignore",
  });
  return result.status === 0;
}

function macDeveloperDirectoryCandidates() {
  const roots = ["/Applications", join(homedir(), "Applications"), join(homedir(), "Downloads")];
  const applications = [];
  for (const root of roots) {
    if (!existsSync(root)) continue;
    for (const entry of readdirSync(root, { withFileTypes: true })) {
      if (entry.isDirectory() && /^Xcode(?:[- ].+)?\.app$/i.test(entry.name)) {
        applications.push(join(root, entry.name));
      }
    }
  }
  applications.sort((left, right) => {
    const left_is_beta = /beta/i.test(left);
    const right_is_beta = /beta/i.test(right);
    if (left_is_beta !== right_is_beta) return left_is_beta ? -1 : 1;
    return left.localeCompare(right);
  });
  return [...new Set(applications.map((application) => join(application, "Contents", "Developer")))];
}
