import { useState } from "react";
import { createRoot } from "react-dom/client";
import { SelectionPopover } from "../../../src/components/SelectionPopover";
import { Icon } from "../../../src/components/Icon";
import mcp from "../../../src/features/settings/SettingsDialog/McpSettingsPage/index.module.scss";
import settings from "../../../src/features/settings/SettingsDialog/index.module.scss";
import skills from "../../../src/features/settings/SettingsDialog/SkillSettingsPage.module.scss";
import composer from "../../../src/features/composer/ComposerDock/index.module.scss";
import workspace from "../../../src/features/workspaces/WorkspaceEditorDialog/index.module.scss";
import "../../../src/styles/index.scss";
import styles from "./index.module.scss";

// 只使用生产控件与样式，不装配 Runtime、不读取用户配置，供浏览器与 E2E 复用。
function ControlSizingFixture() {
  const [open, setOpen] = useState("");
  const [value, setValue] = useState("保持原值");
  const [scale, setScale] = useState(1);
  return <main className={styles.fixture} style={{ zoom: scale }}>
    <h1>控件高度回归</h1>
    <nav aria-label="缩放">
      {[1, 1.25, 1.5].map((factor) => <button key={factor} onClick={() => setScale(factor)}>{factor * 100}%</button>)}
    </nav>
    {(["small", "default", "large"] as const).map((size) => <section className={mcp.editor} data-testid={`size-${size}`} key={size}>
      <h2>{size}</h2>
      <div className={styles.row}>
        <button data-measure="button" data-size={size}>按钮</button>
        <input aria-label={`${size}输入`} data-measure="input" data-size={size} defaultValue="输入内容" />
        <SelectionPopover aria_label={`${size}选择`} size={size} trigger_variant="field" open={open === size}
          on_open_change={(next) => setOpen(next ? size : "")} selected="keep" on_select={() => {}}
          options={[{ value: "keep", label: size === "default" ? "很长的服务名称与业务范围说明".repeat(5) : "保持原值" }, { value: "replace", label: "替换", description: "此项有说明，列表保持内容自适应" }]} />
        <SelectionPopover aria_label={`${size}可编辑选择`} size={size} editable trigger_variant="field" open={open === `${size}-edit`}
          on_open_change={(next) => setOpen(next ? `${size}-edit` : "")} selected={value} on_select={setValue}
          options={[{ value: "保持原值", label: "保持原值" }]} />
      </div>
      <div className={styles.row}>
        <button data-measure="disabled" data-size={size} disabled>禁用</button>
        <button data-measure="loading" data-size={size} aria-busy="true"><Icon name="refresh" size={16} /> 加载中</button>
        <input aria-label={`${size}错误输入`} data-measure="invalid" data-size={size} aria-invalid="true" defaultValue="错误状态" />
        <SelectionPopover aria_label={`${size}紧凑选择`} size={size} trigger_variant="compact" open={false}
          on_open_change={() => {}} selected="keep" on_select={() => {}} options={[{ value: "keep", label: "保持原值" }]} />
      </div>
    </section>)}
    <section className={settings.model_form}>
      <h2>模型搜索选择</h2>
      <label>模型显示名称<input aria-label="模型显示名称" defaultValue="DeepSeek" /></label>
      <label>模型密钥<input aria-label="模型密钥" type="password" defaultValue="fixture-only" /></label>
      <label>禁用模型字段<input aria-label="禁用模型字段" disabled defaultValue="fixture" /></label>
      <SelectionPopover aria_label="模型搜索选择" editable trigger_variant="field" open={open === "model"}
        on_open_change={(next) => setOpen(next ? "model" : "")} selected={value} on_select={setValue}
        options={[{ value: "保持原值", label: "保持原值" }]} />
    </section>
    <section>
      <h2>各页面输入框焦点</h2>
      <div className={settings.permission_form}><label>权限路径<input aria-label="权限路径" /></label></div>
      <div className={settings.pending_device_list}><article><Icon name="check" /><div>待配对设备</div><span>待确认</span><label>设备配对码<input aria-label="设备配对码" /></label></article></div>
      <label className={workspace.label_field}>工作区名称<input aria-label="工作区名称" /></label>
      <div className={settings.memory_persona}><textarea aria-label="Persona 内容" /></div>
      <div className={settings.memory_editor}><input aria-label="记忆分类" /><textarea aria-label="记忆正文" /></div>
      <div className={mcp.editor}><label><input type="checkbox" aria-label="焦点勾选项" />保留键盘提示</label></div>
    </section>
    <section>
      <h2>设置列表对齐</h2>
      {(["mcp", "skills"] as const).map((kind) => <div className={kind === "mcp" ? mcp.page : undefined} key={kind}>
        <h3>{kind === "mcp" ? "MCP" : "技能"}</h3>
        <div className={kind === "mcp" ? mcp.server_list : skills.list} data-testid={`${kind}-list`}>
          {(["ready", "disabled", "conflict", "unavailable"] as const).map((health) => <article key={health}>
            <button className={kind === "mcp" ? mcp.server_body : skills.skill_body} type="button">
              <i data-health={health} data-state={health === "ready" ? "connected" : "disabled"} />
              <span><strong>{health === "ready" ? "review" : `${health}-${"long-name-".repeat(12)}`}</strong><em>{"检查实现、测试和设计规范，保留较长说明的省略展示。".repeat(8)}</em><small>来源 · 当前状态</small></span>
              <Icon name="chevron-right" size={15} />
            </button>
            <label className={settings.switch}><input aria-label={`${kind} ${health}`} defaultChecked={health === "ready"} disabled={health === "unavailable"} type="checkbox" /><span /><b>{health === "ready" ? "已启用" : "已禁用"}</b></label>
          </article>)}
        </div>
      </div>)}
    </section>
    <section className={mcp.editor}>
      <h2>业务组合</h2>
      <div className={mcp.search} data-testid="mcp-search"><input aria-label="服务搜索" placeholder="搜索服务" /><button aria-label="搜索操作"><Icon name="refresh" /></button></div>
      <div className={mcp.secret_row} data-testid="mcp-env"><input aria-label="环境变量名称" /><input aria-label="环境变量值" /><button>移除</button></div>
      <div className={settings.permission_scope_tabs} data-testid="tabs"><button>全局</button><button>工作区</button></div>
      <label className={settings.memory_search} data-testid="memory-search"><Icon name="search" /><input aria-label="记忆搜索" /></label>
      <div className={composer.composer_actions} data-testid="composer-actions">
        <button className={composer.icon_button} aria-label="添加"><Icon name="plus" /></button>
        <button className={composer.execution_selector}>执行设置</button>
        <button className={composer.model_selector}>模型设置</button>
        <button className={composer.send_button} aria-label="发送"><Icon name="arrow-down" /></button>
      </div>
      <textarea aria-label="多行说明" rows={3} defaultValue={"多行文本框保持可变高度\n不压缩成单行"} />
      <div className={mcp.server_list}><button className={mcp.server_body} data-testid="server-row"><span><strong>Pencil</strong><em>多行服务说明</em><small>已连接 · 14 个工具</small></span></button></div>
    </section>
  </main>;
}

createRoot(document.getElementById("app")!).render(<ControlSizingFixture />);
