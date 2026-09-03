import { compile, Logger } from "sass";
import { describe, expect, it } from "vitest";

describe("Desktop control height contract", () => {
  it("keeps input focus free of page-specific shadow rings", () => {
    const paths = Object.keys(import.meta.glob("../../src/**/*.module.scss"));
    const violations: string[] = [];
    for (const path of paths) {
      const css = compile(path.replace("../../", ""), { logger: Logger.silent }).css;
      for (const [, selector, body] of css.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
        if (!selector.includes(":focus")) continue;
        const is_field = /\b(input|textarea)\b/.test(selector)
          || (path.includes("SelectionPopover/") && selector.includes(".trigger"));
        const shadow = body.match(/box-shadow:\s*([^;]+)/)?.[1].trim();
        if (is_field && shadow && shadow !== "none") violations.push(`${path}: ${selector.trim()}`);
      }
    }
    expect(violations, "字段焦点共用 fields.focus-border，不得重新叠加阴影光圈").toEqual([]);
  });

  it("keeps the three border-box heights in the shared token source", () => {
    const tokens = compile("src/styles/tokens.scss", { logger: Logger.silent }).css;
    expect(tokens).toMatch(/--ez-control-height-small:\s*24px/);
    expect(tokens).toMatch(/--ez-control-height-default:\s*32px/);
    expect(tokens).toMatch(/--ez-control-height-large:\s*40px/);
    const reset = compile("src/styles/reset.scss", { logger: Logger.silent }).css;
    expect(reset).toMatch(/\*,\s*\*::before,\s*\*::after\s*\{\s*box-sizing: border-box/);
  });

  it("prevents literal heights from returning to buttons and single-line fields", () => {
    // Vitest 会 stub 样式导入；只用 glob 枚举路径，再让 Sass 编译实际源码。
    const paths = Object.keys(import.meta.glob("../../src/**/*.module.scss"));
    const violations: string[] = [];
    expect(paths.length).toBeGreaterThan(0);
    for (const path of paths) {
      const css = compile(path.replace("../../", ""), { logger: Logger.silent }).css;
      // Sass 已展开嵌套；此门禁仅检查控件声明，不误把图标、容器和多行列表当作单行字段。
      for (const [, selector, body] of css.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
        const control = selector.split(",").some((part) => /(?:^|[ >])(?:button|input|select)(?::[^ ]+)?$/.test(part.trim())
          || /\.[\w]*_(?:button|input|selector|trigger)$/.test(part.trim()));
        if (!control) continue;
        // 图片缩略图和代理开关有独立的内容/轨道尺寸，不是普通单行按钮。
        if (path.endsWith("ConversationView/index.module.scss") && selector.trim() === ".user_images button") continue;
        if (path.endsWith("AppShell/index.module.scss") && selector.trim() === ".proxy_control button") continue;
        if (/(?:^|;)\s*(?:min-)?height:\s*\d+(?:\.\d+)?px\s*;/.test(body)) violations.push(`${path}: ${selector.trim()}`);
      }
    }
    expect(violations, "单行控件必须使用统一高度 token；内容型例外需明确说明").toEqual([]);
  });
});
