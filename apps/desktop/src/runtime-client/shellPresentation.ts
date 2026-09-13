import type { ShellKind } from "@ez-assistant/protocol";

/** Host Shell 类型的跨设置、会话和终端展示名称。 */
export const shellLabels: Readonly<Record<ShellKind, string>> = {
  windows_powershell_51: "Windows PowerShell 5.1",
  cmd: "CMD",
  powershell_7: "PowerShell 7",
  git_bash: "Git Bash",
  posix_sh: "/bin/sh",
};

export const shellBadges: Readonly<Record<ShellKind, string>> = {
  windows_powershell_51: "PS 5.1", cmd: "CMD", powershell_7: "PS 7", git_bash: "Bash", posix_sh: "sh",
};
