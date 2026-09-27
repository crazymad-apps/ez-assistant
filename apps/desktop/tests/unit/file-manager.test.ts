import { afterEach, expect, it } from "vitest";
import { revealFileLabel } from "../../src/platform/fileManager";

afterEach(() => { delete document.documentElement.dataset.platform; });

it.each([
  ["windows", "在资源管理器中显示"],
  ["macos", "在 Finder 中显示"],
  ["linux", "在文件管理器中显示"],
  ["unsupported", "在文件管理器中显示"],
])("uses the native operation's client platform: %s", (platform, label) => {
  document.documentElement.dataset.platform = platform;
  expect(revealFileLabel()).toBe(label);
});
