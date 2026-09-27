import { describe, expect, it } from "vitest";
import { hostFilePath, hostFileUri } from "../../src/runtime-client/hostFilePath";

describe("Host file paths independent of client OS", () => {
  it.each([
    ["C:\\项目 空格\\图.png", "C:/项目 空格/图.png"],
    ["C:/项目 空格/图.png", "C:/项目 空格/图.png"],
    ["file:///C:/%E9%A1%B9%E7%9B%AE%20%E7%A9%BA%E6%A0%BC/%E5%9B%BE.png", "C:/项目 空格/图.png"],
    ["\\\\?\\C:\\项目\\图.png", "C:/项目/图.png"],
    ["file:///Users/test/a%20b.png", "/Users/test/a b.png"],
  ])("resolves %s", (reference, expected) => {
    expect(hostFilePath(reference)).toBe(expected);
    expect(hostFilePath(hostFileUri(expected))).toBe(expected);
  });

  it("resolves relative URI paths from a Windows document or directory", () => {
    expect(hostFilePath("../图%20片.png", "C:\\项目\\docs\\readme.md")).toBe("C:/项目/图 片.png");
    expect(hostFilePath(".\\图.png", "\\\\?\\C:\\项目\\")).toBe("C:/项目/图.png");
    expect(hostFileUri("C:/项目/a#100%.png")).toBe("file:///C:/%E9%A1%B9%E7%9B%AE/a%23100%25.png");
  });

  it.each(["C:relative.png", "file://server/share/a", "\\\\server\\share\\a", "\\\\.\\C:\\a", "file:///C:/a?x", "file:///C:/a#x", "file:///C:/a%00b", "https://example.com/a", "file:///C:/a%2fb", "file:///C:/a%5cb"])("rejects ambiguous or unsafe references: %s", (reference) => {
    expect(() => hostFilePath(reference)).toThrow();
  });
});
