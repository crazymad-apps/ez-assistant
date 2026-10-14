import { describe, expect, it, vi } from "vitest";
import type { CodeHighlighterPlugin, HighlightResult } from "@streamdown/code";
import { scopedCodePlugin } from "../../src/components/MarkdownContent/codeTheme";
import { renderMermaid } from "../../src/components/MarkdownContent/MarkdownCode";
import { THEME_PRESETS } from "../../src/styles/themePresets";

const diagram = vi.hoisted(() => ({ getMermaid: vi.fn(), render: vi.fn() }));
vi.mock("@streamdown/mermaid", () => ({ mermaid: { getMermaid: diagram.getMermaid } }));
function tokens(text: string): HighlightResult { return { tokens: [[{ content: text, offset: 0 }]] }; }
function deferred<T>() { let resolve!: (v: T) => void; let reject!: (e: Error) => void; const promise = new Promise<T>((a, b) => { resolve = a; reject = b; }); return { resolve, reject, promise }; }

describe("Markdown theme rendering scopes", () => {
  it("drops a completed Mermaid SVG after its content or theme scope has been replaced", async () => {
    diagram.render.mockReset(); diagram.getMermaid.mockReset();
    const pending = deferred<{ svg: string }>();
    diagram.getMermaid.mockReturnValue({ render: diagram.render });
    diagram.render.mockReturnValue(pending.promise);
    let active = true;
    const result = renderMermaid("old", THEME_PRESETS.monokai, "stale", () => active);
    await vi.waitFor(() => expect(diagram.render).toHaveBeenCalledOnce());
    active = false; pending.resolve({ svg: "old svg" });
    expect(await result).toBeNull();
    diagram.render.mockReset(); diagram.getMermaid.mockReset();
  });

  it("uses both slots of the chosen theme, rejects stale callbacks and cache collisions, and returns current plain text while loading", () => {
    let callback!: (result: HighlightResult) => void;
    const highlight = vi.fn((_options, next) => { callback = next; return null; });
    const base: CodeHighlighterPlugin = { name: "shiki", type: "code-highlighter", getThemes: () => ["github-light", "github-dark"], supportsLanguage: () => true, getSupportedLanguages: () => [], highlight };
    let active = true;
    const plugin = scopedCodePlugin(base, THEME_PRESETS.monokai, () => active);
    const receive = vi.fn();
    const options = { code: "current text", language: "typescript" as const, themes: plugin.getThemes() };
    expect(plugin.highlight(options, receive)?.tokens[0][0].content).toBe("current text");
    expect(highlight.mock.calls[0][0].themes).toEqual(["monokai", "monokai"]);
    callback(tokens("wrong cached text")); expect(receive).not.toHaveBeenCalled();
    callback(tokens("current text")); expect(receive).toHaveBeenCalledOnce();
    active = false; callback(tokens("current text")); expect(receive).toHaveBeenCalledOnce();
    highlight.mockReturnValueOnce(tokens("wrong cached text") as never);
    expect(plugin.highlight(options)?.tokens[0][0].content).toBe("current text");
  });

  it("serializes Mermaid configuration and render, skips obsolete work, and recovers after a failed diagram", async () => {
    const first = deferred<{ svg: string }>();
    diagram.getMermaid.mockImplementation(() => ({ render: diagram.render }));
    diagram.render.mockReturnValueOnce(first.promise).mockResolvedValueOnce({ svg: "latest" });
    let active = true;
    const old = renderMermaid("old", THEME_PRESETS.default_light, "first", () => active);
    await vi.waitFor(() => expect(diagram.render).toHaveBeenCalledOnce());
    const skip = renderMermaid("obsolete", THEME_PRESETS.monokai, "skip", () => false);
    const latest = renderMermaid("latest", THEME_PRESETS.solarized_dark, "last", () => true);
    expect(diagram.getMermaid).toHaveBeenCalledOnce();
    active = false; first.reject(new Error("parse failed"));
    await expect(old).rejects.toThrow("parse failed");
    expect(await skip).toBeNull(); expect(await latest).toBe("latest");
    expect(diagram.render.mock.calls.map(([id]) => id)).toEqual(["first", "last"]);
    expect(diagram.getMermaid.mock.lastCall?.[0]).toMatchObject({ securityLevel: "strict", startOnLoad: false, theme: "base", themeVariables: { darkMode: true, primaryTextColor: THEME_PRESETS.solarized_dark.colors["--ez-text"] } });
  });
});
