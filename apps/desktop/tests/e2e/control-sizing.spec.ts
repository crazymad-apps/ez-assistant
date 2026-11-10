import { expect, test } from "@playwright/test";

test("skill rows match MCP spacing, typography and hover at narrow widths and zoom levels", async ({ page }) => {
  await page.goto("/tests/support/control-sizing/index.html");
  for (const width of [1280, 720]) {
    await page.setViewportSize({ width, height: 1000 });
    for (const scale of [1, 1.25, 1.5]) {
      await page.getByRole("button", { name: `${scale * 100}%`, exact: true }).click();
      const measurements = [];
      const hover_colors = [];
      for (const kind of ["mcp", "skills"]) {
        const list = page.getByTestId(`${kind}-list`);
        measurements.push(await list.locator("article").evaluateAll((rows) => rows.map((row) => {
          const body = row.querySelector("button")!;
          const text = body.querySelector("span")!;
          const description = body.querySelector("em")!;
          return {
            height: row.getBoundingClientRect().height,
            gap: getComputedStyle(row).gap,
            padding: getComputedStyle(row).padding,
            body_padding: getComputedStyle(body).padding,
            color: getComputedStyle(body).color,
            radius: getComputedStyle(body).borderRadius,
            text_gap: getComputedStyle(text).gap,
            description_font: getComputedStyle(description).fontSize,
            description_overflow: getComputedStyle(description).textOverflow,
            meta_font: getComputedStyle(body.querySelector("small")!).fontSize,
            fits: row.scrollWidth <= row.clientWidth,
          };
        })));
        const body = list.getByRole("button").first();
        await body.hover();
        hover_colors.push(await body.evaluate((element) => getComputedStyle(element).backgroundColor));
      }
      expect(measurements[1]).toEqual(measurements[0]);
      expect(hover_colors[1]).toBe(hover_colors[0]);
      expect(hover_colors[1]).not.toBe("rgba(0, 0, 0, 0)");
      for (const row of measurements[1]) {
        expect(row.fits).toBe(true);
        expect(row.description_font).toBe("12px");
        expect(row.description_overflow).toBe("ellipsis");
        expect(row.height / scale).toBeGreaterThan(40);
      }
      expect(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth)).toBe(false);
    }
  }
});

test("select options fill the rounded panel without inset or individual rounded highlights", async ({ page }) => {
  await page.goto("/tests/support/control-sizing/index.html");
  for (const [role, name] of [["button", "default选择"], ["combobox", "模型搜索选择"]] as const) {
    const trigger = page.getByRole(role, { name, exact: true });
    await trigger.click();
    const panel = page.getByRole("listbox");
    await expect(panel).toBeVisible();
    await expect(panel).toHaveCSS("padding", "0px");
    await expect(panel).toHaveCSS("overflow", "hidden");
    const geometry = await panel.evaluate((element) => {
      const style = getComputedStyle(element);
      const box = element.getBoundingClientRect();
      return { width: box.width - parseFloat(style.borderLeftWidth) - parseFloat(style.borderRightWidth), radius: parseFloat(style.borderTopLeftRadius) };
    });
    expect(geometry.radius).toBeGreaterThan(0);
    const options = panel.getByRole("option");
    for (const option of await options.all()) {
      await expect(option).toHaveCSS("border-radius", "0px");
      expect((await option.boundingBox())!.width).toBeCloseTo(geometry.width, 1);
    }
    await options.last().hover();
    await expect(options.last()).not.toHaveCSS("background-color", "rgba(0, 0, 0, 0)");
    await page.keyboard.press("Escape");
    await expect(panel).toBeHidden();
    await expect(trigger).toBeFocused();
  }
});

test("text fields use border focus and editable selects highlight only the outer field", async ({ page }) => {
  await page.goto("/tests/support/control-sizing/index.html");
  const primary_hex = await page.locator("html").evaluate((element) => getComputedStyle(element).getPropertyValue("--ez-primary").trim());
  const primary = `rgb(${primary_hex.slice(1).match(/../g)!.map((channel) => parseInt(channel, 16)).join(", ")})`;
  for (const name of ["default输入", "多行说明", "模型显示名称", "模型密钥", "权限路径", "设备配对码", "工作区名称", "Persona 内容", "记忆分类", "记忆正文"]) {
    const input = page.getByLabel(name, { exact: true });
    const before = await input.boundingBox();
    const border_before = await input.evaluate((element) => getComputedStyle(element).borderTopWidth);
    await input.click();
    await expect(input).toBeFocused();
    await expect(input).toHaveCSS("border-top-color", primary);
    await expect(input).toHaveCSS("outline-style", "none");
    await expect(input).toHaveCSS("box-shadow", "none");
    expect((await input.boundingBox())!.height).toBe(before!.height);
    await expect(input).toHaveCSS("border-top-width", border_before);
    await input.press("Tab");
    await expect(input).not.toBeFocused();
    await page.keyboard.press("Shift+Tab");
    await expect(input).toBeFocused();
    await expect(input).toHaveCSS("outline-style", "none");
    await expect(input).toHaveCSS("box-shadow", "none");
  }
  for (const name of ["default可编辑选择", "模型搜索选择"]) {
    const input = page.getByRole("combobox", { name, exact: true });
    await input.click();
    await expect(input).toHaveCSS("outline-style", "none");
    await expect(input).toHaveCSS("box-shadow", "none");
    await expect(input).toHaveCSS("border-top-width", "0px");
    const field = page.locator('[data-control="field"]').filter({ has: input });
    await expect(field).toHaveCSS("border-top-color", primary);
    await expect(field).toHaveCSS("box-shadow", "none");
    await input.press("Escape");
    await input.press("Tab");
    await expect(input).not.toBeFocused();
    await page.keyboard.press("Shift+Tab");
    await expect(input).toBeFocused();
    await expect(input).toHaveCSS("box-shadow", "none");
  }
  await page.getByRole("textbox", { name: "记忆搜索", exact: true }).click();
  await expect(page.getByTestId("memory-search")).toHaveCSS("border-top-color", primary);
  await expect(page.getByTestId("memory-search").getByRole("textbox")).toHaveCSS("border-top-width", "0px");
  const invalid = page.getByRole("textbox", { name: "default错误输入", exact: true });
  const error_color = await invalid.evaluate((element) => getComputedStyle(element).borderTopColor);
  await invalid.click();
  await expect(invalid).toHaveCSS("border-top-color", error_color);
  await expect(page.getByRole("textbox", { name: "禁用模型字段", exact: true })).toBeDisabled();
  const checkbox = page.getByRole("checkbox", { name: "焦点勾选项" });
  await checkbox.press("Shift+Tab");
  await page.keyboard.press("Tab");
  await expect(checkbox).toBeFocused();
  await expect(checkbox).toHaveCSS("outline-style", "solid");
});

test("single-line controls share border-box height tokens at all sizes and scales", async ({ page }) => {
  await page.goto("/tests/support/control-sizing/index.html");
  await expect(page.getByRole("heading", { name: "控件高度回归" })).toBeVisible();
  for (const width of [1280, 720]) {
    await page.setViewportSize({ width, height: 1000 });
    for (const scale of [1, 1.25, 1.5]) {
      await page.getByRole("button", { name: `${scale * 100}%`, exact: true }).click();
      for (const [size, height] of [["small", 24], ["default", 32], ["large", 40]] as const) {
        const section = page.getByTestId(`size-${size}`);
        const controls = section.locator('[data-measure], button[aria-haspopup="listbox"], [data-control="field"]');
        const boxes = await controls.evaluateAll((elements) => elements.map((element) => ({
          height: element.getBoundingClientRect().height, box: getComputedStyle(element).boxSizing,
        })));
        expect(boxes).toHaveLength(8);
        for (const box of boxes) {
          expect(box.height / scale).toBeCloseTo(height, 1);
          expect(box.box).toBe("border-box");
        }
        const editable = section.locator('[data-control="field"]');
        // 非整数缩放会把边框吸附到设备像素，按实际边框校验内容区，外框仍必须严格等于 token。
        const border = await editable.evaluate((element) => {
          const style = getComputedStyle(element);
          return parseFloat(style.borderTopWidth) + parseFloat(style.borderBottomWidth);
        });
        const inner = await editable.locator("input,button").evaluateAll((elements) => elements.map((element) => element.getBoundingClientRect().height));
        for (const actual of inner) expect(actual / scale).toBeCloseTo(height - border, 1);
        const button = section.getByRole("button", { name: "按钮", exact: true });
        await button.hover();
        await button.focus();
        expect((await button.boundingBox())!.height / scale).toBeCloseTo(height, 1);
      }
      for (const id of ["mcp-search", "mcp-env", "composer-actions"]) {
        const heights = await page.getByTestId(id).locator("input,button").evaluateAll((elements) => elements.map((element) => element.getBoundingClientRect().height));
        for (const height of heights) expect(height / scale).toBeCloseTo(32, 1);
      }
      expect((await page.getByTestId("tabs").boundingBox())!.height / scale).toBeCloseTo(32, 1);
      expect((await page.getByTestId("tabs").getByRole("button").first().boundingBox())!.height / scale).toBeCloseTo(28, 1);
      expect((await page.getByTestId("memory-search").boundingBox())!.height / scale).toBeCloseTo(32, 1);
      expect((await page.getByTestId("server-row").boundingBox())!.height / scale).toBeGreaterThan(40);
      expect((await page.getByRole("textbox", { name: "多行说明" }).boundingBox())!.height / scale).toBeGreaterThan(40);
      const overflow = await page.evaluate(() => document.documentElement.scrollWidth > innerWidth);
      expect(overflow).toBe(false);
      const long_trigger = page.getByRole("button", { name: "default选择", exact: true });
      const trigger_box = (await long_trigger.boundingBox())!;
      const arrow_box = (await long_trigger.locator("svg").boundingBox())!;
      expect(arrow_box.x + arrow_box.width).toBeLessThanOrEqual(trigger_box.x + trigger_box.width);
    }
  }
  await page.getByRole("button", { name: "100%", exact: true }).click();
  await page.getByRole("button", { name: "default选择", exact: true }).click();
  await expect(page.getByRole("option", { name: /替换/ })).toBeVisible();
  expect((await page.getByRole("option", { name: /替换/ }).boundingBox())!.height).toBeGreaterThan(32);
});

test("cascade options fit their content within shared bounds and wrap at the available edge", async ({ page }) => {
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.goto("/tests/support/control-sizing/index.html");
  const trigger = page.getByRole("button", { name: "自适应选项", exact: true });
  const widths: number[] = [];
  for (const label of ["短选项", "中等选项", "超长选项"]) {
    await trigger.click();
    await page.getByRole("menuitem", { name: label, exact: true }).click();
    const panel = page.getByRole("menu", { name: label, exact: true });
    await expect(panel).toBeVisible();
    await expect.poll(async () => (await panel.boundingBox())!.y
      - await page.getByRole("menu", { name: "自适应选项", exact: true })
        .evaluate(element => element.parentElement!.getBoundingClientRect().top)).toBeCloseTo(0, 1);
    const box = (await panel.boundingBox())!;
    expect(box.width).toBeGreaterThanOrEqual(160);
    expect(box.width).toBeLessThanOrEqual(340);
    expect(box.x).toBeGreaterThanOrEqual(8);
    expect(box.x + box.width).toBeLessThanOrEqual(1272);
    widths.push(box.width);
    if (label === "超长选项") {
      const text = await panel.locator("b").evaluate(element => {
        const first_character = document.createRange();
        first_character.setStart(element.firstChild!, 0);
        first_character.setEnd(element.firstChild!, 1);
        return {
          height: element.getBoundingClientRect().height,
          line_height: first_character.getBoundingClientRect().height,
          scroll_width: element.scrollWidth,
          client_width: element.clientWidth,
        };
      });
      expect(text.height).toBeGreaterThan(text.line_height);
      expect(text.scroll_width).toBeLessThanOrEqual(text.client_width);
    }
    await trigger.click();
    await expect(trigger).toHaveAttribute("aria-expanded", "false");
  }
  expect(widths[0]).toBe(160);
  expect(widths[1]).toBeGreaterThan(widths[0]);
  expect(widths[2]).toBeGreaterThanOrEqual(widths[1]);

  // 中间宽度仍保留侧向面板，但其最大宽度须跟随实际可用空间。
  await page.setViewportSize({ width: 470, height: 800 });
  await trigger.click();
  await page.getByRole("menuitem", { name: "超长选项", exact: true }).click();
  const panel = page.getByRole("menu", { name: "超长选项", exact: true });
  await expect(panel).toHaveAttribute("data-side", "right");
  await expect.poll(async () => ((await panel.boundingBox())!.x + (await panel.boundingBox())!.width)).toBeLessThanOrEqual(462);

  await page.setViewportSize({ width: 360, height: 800 });
  await expect(panel).toHaveAttribute("data-side", "inline");
  await expect(panel.getByRole("button", { name: "返回一级" })).toBeVisible();
  const narrow = (await panel.boundingBox())!;
  expect(narrow.x).toBeGreaterThanOrEqual(8);
  expect(narrow.x + narrow.width).toBeLessThanOrEqual(352);
  await panel.getByRole("button", { name: "返回一级" }).click();
  await expect(page.getByRole("menuitem", { name: "超长选项", exact: true })).toBeFocused();
  await trigger.click();

  // 左侧展开仍按容器顶部对齐；靠近底部时，视口约束优先于对齐。
  await page.setViewportSize({ width: 1280, height: 500 });
  await trigger.evaluate(element => Object.assign(element.parentElement!.style, { position: "fixed", right: "20px", bottom: "20px" }));
  await trigger.click();
  await page.getByRole("menuitem", { name: "超长选项", exact: true }).click();
  await expect(panel).toHaveAttribute("data-side", "left");
  await expect.poll(async () => (await panel.boundingBox())!.y).toBeGreaterThanOrEqual(8);
  await expect.poll(async () => {
    const box = (await panel.boundingBox())!;
    return box.y + box.height;
  }).toBeLessThanOrEqual(492);
});

test("single-line cascade, selection and organization menu rows share the default height", async ({ page }) => {
  await page.goto("/tests/support/control-sizing/index.html");
  for (const { width, row_height } of [{ width: 1280, row_height: 32 }, { width: 720, row_height: 40 }]) {
    await page.setViewportSize({ width, height: 1000 });
    if (row_height !== 32) {
      await page.locator("html").evaluate((element, height) => element.style.setProperty("--ez-menu-row-height", `${height}px`), row_height);
    }
    const trigger = page.getByRole("button", { name: "自适应选项", exact: true });
    await trigger.click();
    await page.getByRole("menuitem", { name: "短选项", exact: true }).click();
    for (const row of [
      page.getByRole("menuitemradio", { name: "跟随组织默认", exact: true }),
      page.getByRole("menuitem", { name: "短选项", exact: true }),
      page.getByRole("menuitemradio", { name: "k3", exact: true }),
      page.getByRole("menuitem", { name: "菜单操作", exact: true }),
    ]) {
      expect((await row.boundingBox())!.height).toBe(row_height);
      await row.hover();
      expect((await row.boundingBox())!.height).toBe(row_height);
    }
    const default_row = (await page.getByRole("menuitemradio", { name: "跟随组织默认", exact: true }).boundingBox())!;
    const category_row = (await page.getByRole("menuitem", { name: "短选项", exact: true }).boundingBox())!;
    expect(category_row.y - default_row.y - default_row.height).toBeCloseTo(0, 1);
    await expect(page.getByRole("menu", { name: "自适应选项", exact: true }).getByRole("separator")).toBeVisible();
    const first_model = (await page.getByRole("menuitemradio", { name: "k3", exact: true }).boundingBox())!;
    expect((await page.getByRole("menuitemradio", { name: "k3.1-pro", exact: true }).boundingBox())!.y
      - first_model.y - first_model.height).toBeCloseTo(0, 1);
    await trigger.click();

    await page.getByRole("button", { name: "default选择", exact: true }).click();
    const options = page.getByRole("listbox").getByRole("option");
    expect((await options.first().boundingBox())!.height).toBe(row_height);
    expect((await options.last().boundingBox())!.height).toBeGreaterThan(row_height);
    const first_option = (await options.first().boundingBox())!;
    expect((await options.last().boundingBox())!.y - first_option.y - first_option.height).toBeCloseTo(0, 1);
    await page.keyboard.press("Escape");

    await page.getByRole("button", { name: "组织菜单", exact: true }).click();
    const menu = page.getByRole("menu", { name: "组织菜单", exact: true });
    const header = menu.getByRole("menuitem", { name: "切换组织，当前组织：默认组织", exact: true });
    await header.click();
    const option = menu.getByRole("menuitem", { name: "开发验收组织", exact: true });
    await expect(option).toBeVisible();
    // 折叠容器不能在末个选项下附加一截背景，造成子选项看起来更高。
    await expect(menu.getByRole("group", { name: "可切换组织" })).toHaveCSS("padding-bottom", "0px");
    for (const row of [header, option, menu.getByRole("menuitem", { name: "设置", exact: true }), menu.getByRole("menuitem", { name: "禁用操作", exact: true })]) {
      expect((await row.boundingBox())!.height).toBe(row_height);
    }
    const header_box = (await header.boundingBox())!;
    expect((await option.boundingBox())!.y - header_box.y - header_box.height).toBeCloseTo(0, 1);
    await page.keyboard.press("Escape");
  }
});

test("repeated pointer clicks expand the category while keyboard entry focuses an option", async ({ page }) => {
  await page.goto("/tests/support/control-sizing/index.html");
  const trigger = page.getByRole("button", { name: "自适应选项", exact: true });
  await trigger.click();
  const category = page.getByRole("menuitem", { name: "短选项", exact: true });
  await category.dblclick();
  const selected = page.getByRole("menuitemradio", { name: "k3", exact: true });
  await expect(category).toBeFocused();
  await expect(selected).toHaveAttribute("data-active", "false");
  await expect(selected).toHaveAttribute("aria-checked", "true");
  await expect(trigger).toHaveAttribute("aria-expanded", "true");

  await category.press("ArrowRight");
  await expect(selected).toBeFocused();
  await category.click();
  await expect(category).toBeFocused();
  await expect(selected).toHaveAttribute("data-active", "false");

  await category.press("ArrowRight");
  await selected.press("ArrowDown");
  const other = page.getByRole("menuitemradio", { name: "k3.1-pro", exact: true });
  await expect(other).toBeFocused();
  await other.click();
  await expect(trigger).toHaveAttribute("aria-expanded", "false");
});
