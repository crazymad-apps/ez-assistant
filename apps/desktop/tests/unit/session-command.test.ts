import { describe, expect, it } from "vitest";
import { parseMcpRefreshCommand } from "../../src/features/composer/ComposerDock/sessionCommand";

describe("MCP refresh command", () => {
  it("converts only exact commands to structured requests", () => {
    expect(parseMcpRefreshCommand(" /mcp refresh ")).toEqual({ type: "command", command: { type: "mcp_refresh", payload: {} } });
    expect(parseMcpRefreshCommand("/mcp refresh github")).toEqual({ type: "command", command: { type: "mcp_refresh", payload: { server: "github" } } });
    expect(parseMcpRefreshCommand("请执行 /mcp refresh")).toEqual({ type: "not_command" });
    expect(parseMcpRefreshCommand("/mcp")).toEqual({ type: "not_command" });
  });

  it.each(["/mcp wrong", "/mcp refresh x extra", "/mcp refresh GitHub", "/mcp refresh ../secret", "/mcp\nrefresh", "/mcp refresh; pwd"])("rejects reserved command misuse: %s", (text) => {
    expect(parseMcpRefreshCommand(text).type).toBe("invalid");
  });
});
