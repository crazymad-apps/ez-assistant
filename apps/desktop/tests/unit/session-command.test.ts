import { describe, expect, it } from "vitest";
import { parseSessionCommand } from "../../src/features/composer/ComposerDock/sessionCommand";

describe("Session refresh commands", () => {
  it("converts only exact commands to structured requests", () => {
    expect(parseSessionCommand(" /mcp refresh ")).toEqual({ type: "command", command: { type: "mcp_refresh", payload: {} } });
    expect(parseSessionCommand("/mcp refresh github")).toEqual({ type: "command", command: { type: "mcp_refresh", payload: { server: "github" } } });
    expect(parseSessionCommand("请执行 /mcp refresh")).toEqual({ type: "not_command" });
    expect(parseSessionCommand("/mcp")).toEqual({ type: "not_command" });
  });

  it("parses skill refresh without treating the skill picker as a command", () => {
    expect(parseSessionCommand(" /skill refresh ")).toEqual({ type: "command", command: { type: "skill_refresh" } });
    expect(parseSessionCommand("/skill")).toEqual({ type: "not_command" });
    expect(parseSessionCommand("/skill refresh extra").type).toBe("invalid");
    expect(parseSessionCommand("/skill reload").type).toBe("invalid");
    expect(parseSessionCommand("/mcp reload").type).toBe("invalid");
  });

  it.each(["/mcp wrong", "/mcp refresh x extra", "/mcp refresh GitHub", "/mcp refresh ../secret", "/mcp\nrefresh", "/mcp refresh; pwd"])("rejects reserved command misuse: %s", (text) => {
    expect(parseSessionCommand(text).type).toBe("invalid");
  });
});
