import { compatibilityHeaders, type McpConfigurationSnapshot } from "@ez-assistant/protocol";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { expect, type Page } from "@playwright/test";

/** 正式 Host 浏览器回证；只使用全局 setup 创建的临时会话和安全 MCP fixture。 */
export async function expectMcpAcceptance(page: Page): Promise<void> {
  const secret = process.env.EZ_ASSISTANT_E2E_MCP_SECRET;
  if (!secret) throw new Error("isolated MCP credential marker is missing");
  await page.getByRole("button", { name: "账号与 Runtime" }).click();
  await page.getByRole("menuitem", { name: "设置" }).click();
  const settings = page.getByRole("dialog", { name: "设置" });
  await settings.getByRole("button", { name: "MCP", exact: true }).click();
  await settings.getByRole("button", { name: /MCP fixture local_fixture/ }).click();
  await expect(settings.getByLabel("显示名称")).toHaveValue("MCP fixture");
  await expect(settings.getByRole("button", { name: "删除服务…" })).toBeVisible();
  await expectCleanPage(page, secret);
  await settings.getByRole("button", { name: "测试连接", exact: true }).click();
  await expect(settings.getByText(/连接测试成功.*2 个工具/)).toBeVisible();
  await expectCleanPage(page, secret);
  // 长于原 10 分钟上限的覆盖可保存并回显；清空后恢复继承，不写成零。
  await settings.getByText("高级设置", { exact: true }).click();
  await settings.getByLabel("工具超时（毫秒）", { exact: true }).fill("1800000");
  await settings.getByRole("button", { name: "保存", exact: true }).click();
  await settings.getByRole("button", { name: /MCP fixture local_fixture/ }).click();
  await settings.getByText("高级设置", { exact: true }).click();
  await expect(settings.getByLabel("工具超时（毫秒）", { exact: true })).toHaveValue("1800000");
  await settings.getByLabel("工具超时（毫秒）", { exact: true }).fill("");
  await settings.getByRole("button", { name: "保存", exact: true }).click();
  await settings.getByRole("button", { name: /MCP fixture local_fixture/ }).click();
  await settings.getByText("高级设置", { exact: true }).click();
  await expect(settings.getByLabel("工具超时（毫秒）", { exact: true })).toHaveValue("");
  await settings.getByRole("button", { name: "返回 MCP 列表" }).click();
  await settings.getByRole("button", { name: "关闭设置" }).click();

  // 单次允许不能扩大为服务级权限；下一轮调用必须再次出现审批。
  const composer = page.getByRole("textbox", { name: "输入消息" });
  for (const allow of [true, false]) {
    await composer.fill("/mcp");
    await composer.press("Enter");
    await page.getByRole("option", { name: /MCP fixture \(local_fixture\)/ }).click();
    await composer.fill("MCP_CASE");
    await composer.press("Enter");
    await expect(page.getByText("允许调用 local_fixture / first_tool？", { exact: true })).toBeVisible();
    await expectCleanPage(page, secret);
    if (allow) {
      await page.getByRole("radio", { name: /仅本次/ }).check();
      await page.getByRole("button", { name: "允许执行", exact: true }).click();
      await expect(page.getByText("MCP 调用已回传。", { exact: true })).toBeVisible();
      await page.getByRole("button", { name: /MCP fixture.*first_tool.*完成/ }).last().click();
      const detail = page.getByRole("dialog", { name: "MCP fixture (local_fixture) / first_tool" });
      await expect(detail).toContainText("called:first_tool");
      await expectCleanPage(page, secret);
      await detail.getByRole("button", { name: "关闭工具详情" }).click();
    } else {
      await page.getByRole("button", { name: "拒绝", exact: true }).click();
      await expect(page.getByText("MCP 调用已拒绝。", { exact: true })).toBeVisible();
    }
  }

  await composer.fill("/mcp refresh");
  await composer.press("Enter");
  const result = page.getByRole("article", { name: "MCP 刷新完成" });
  await expect(result).toBeVisible();
  await result.locator("summary").click();
  await expect(result).toContainText("已刷新 · 2 个工具");
  await expectCleanPage(page, secret);
  await page.reload();
  await page.getByRole("button", { name: "进入项目" }).click();
  await expect(page.getByRole("article", { name: "MCP 刷新完成" })).toBeVisible();
  await expectCleanPage(page, secret);
}

async function expectCleanPage(page: Page, secret: string): Promise<void> {
  // 不只扫描 outerHTML：输入值可能只存在 DOM property，浏览器存储也不能残留凭据。
  const exposed = await page.evaluate(() => JSON.stringify({
    html: document.documentElement.outerHTML,
    values: [...document.querySelectorAll("input,textarea")].map((element) => (element as HTMLInputElement).value),
    local: { ...localStorage }, session: { ...sessionStorage },
  }));
  expect(exposed).not.toContain(secret);
  expect(exposed).not.toContain("e2e-placeholder-not-a-real-secret");
}

/** F05 的两种客户端复用同一 UI；只连本地夹具，测试连接绝不调用搜索。 */
export async function expectBuiltinMcpAcceptance(page: Page): Promise<void> {
  await page.getByRole("button", { name: "账号与 Runtime" }).click();
  await page.getByRole("menuitem", { name: "设置" }).click();
  const settings=page.getByRole("dialog",{name:"设置"});
  await settings.getByRole("button",{name:"MCP",exact:true}).click();
  const open=async()=>settings.getByRole("button",{name:/DeepSeek 搜索 deepseek-search/}).click();
  await open();
  await expect(settings.getByRole("checkbox",{name:"启用此服务"})).not.toBeChecked();
  await settings.getByLabel("环境变量值 1",{exact:true}).fill("e2e-builtin-fake-key");
  await settings.getByLabel("环境变量值 2",{exact:true}).fill(process.env.EZ_ASSISTANT_E2E_SEARCH_BASE_URL!);
  await settings.getByRole("button",{name:"保存",exact:true}).click();
  await open();
  await expect(settings.getByRole("checkbox",{name:"启用此服务"})).not.toBeChecked();
  await expect(settings.getByLabel("环境变量值 1",{exact:true})).toHaveValue("");
  await settings.getByRole("button",{name:"测试连接",exact:true}).click();
  await expect(settings.getByText(/连接测试成功.*1 个工具/)).toBeVisible();
  await expect(settings.getByRole("checkbox",{name:"启用此服务"})).not.toBeChecked();
  // 另一客户端先提交；SSE 更新不能覆盖已打开的旧草稿，旧 revision 也不能覆盖文件。
  await settings.getByLabel("环境变量值 1",{exact:true}).fill("f05-unsaved-draft");
  const current=await builtinCommand("get_mcp_configuration",{});
  const server=current.snapshot.servers.find((entry)=>entry.source.type==="builtin")!;
  await builtinCommand("mutate_mcp_configuration",{
    expected_revision:server.managed!.revision,
    mutation:{type:"patch_managed",payload:{target:{source:server.source,server_key:server.server_key},expected_definition_digest:server.managed!.definition_digest,environment:{DEEPSEEK_API_KEY:{mode:"replace",value:"f05-other-client-key"}}}},
  });
  await settings.getByRole("button",{name:"保存",exact:true}).click();
  await expect(settings.getByText(/来源或配置已经改变/)).toBeVisible();
  await expect(settings.getByLabel("环境变量值 1",{exact:true})).toHaveValue("f05-unsaved-draft");
  const file=JSON.parse(await readFile(join(process.env.EZ_ASSISTANT_E2E_RUNTIME_HOME!,"users/_personal/mcp.json"),"utf8"));
  expect(file.managedServers.builtin["deepseek-search"].env.DEEPSEEK_API_KEY).toBe("f05-other-client-key");
  await settings.getByRole("button",{name:"重新载入",exact:true}).click();
  await open();
  await expect(settings.getByLabel("环境变量值 1",{exact:true})).toHaveValue("");
  await expectCleanPage(page,"f05-other-client-key");
  await settings.getByRole("checkbox",{name:"启用此服务"}).check();
  await settings.getByRole("button",{name:"保存",exact:true}).click();
  await open();
  await settings.getByRole("button",{name:"刷新此服务",exact:true}).click();
  await expect(settings.getByText(/本地 stdio.*已连接/)).toBeVisible();
  await settings.getByRole("checkbox",{name:"启用此服务"}).uncheck();
  await settings.getByRole("button",{name:"保存",exact:true}).click();
  await open();
  await settings.getByRole("button",{name:"刷新此服务",exact:true}).click();
  await expect(settings.getByText(/本地 stdio.*已禁用/)).toBeVisible();
  await settings.getByRole("button",{name:"返回 MCP 列表"}).click();
  await settings.getByRole("button",{name:"技能",exact:true}).click();
  await expect(settings.getByText("skill-creator",{exact:true})).toBeVisible();
  await expect(settings.getByText("内置 Skill",{exact:false}).first()).toBeVisible();
  await settings.getByRole("button",{name:"关闭设置"}).click();
}

async function builtinCommand(type:string,payload:object):Promise<{snapshot:McpConfigurationSnapshot}> {
  const host=JSON.parse(process.env.EZ_ASSISTANT_E2E_BOOTSTRAP!);
  const response=await fetch(`${host.base_url}/commands`,{method:"POST",headers:{...compatibilityHeaders(),Authorization:`Bearer ${host.access_token}`,"Content-Type":"application/json"},body:JSON.stringify({request_id:crypto.randomUUID(),command:{scope:"runtime",payload:{type,payload}}})});
  expect(response.ok).toBe(true);
  return (await response.json()).result.payload.payload;
}
