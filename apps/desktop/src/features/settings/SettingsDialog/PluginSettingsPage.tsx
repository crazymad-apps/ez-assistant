import { observer } from "mobx-react-lite";
import { useEffect, useRef, useState } from "react";
import type { InstalledPluginSnapshot, PluginMarketEntry, PluginOperationSnapshot, PluginSettingsMutation } from "@ez-assistant/protocol";
import { Button } from "../../../components/Button";
import { useAppMessage } from "../../../components/AppMessage";
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from "../../../components/DropdownMenu";
import { Icon } from "../../../components/Icon";
import { useRootStore } from "../../../stores/RootStoreContext";
import { SettingsPageContainer } from "./SettingsPageContainer";
import { PluginOperationDialog, type PluginOperationIntent } from "./PluginOperationDialog";
import styles from "./PluginSettingsPage.module.scss";

type OperationView = Readonly<{ intent: PluginOperationIntent | null; snapshot: PluginOperationSnapshot | null; auto_begin?: boolean }>;

export const PluginSettingsPage = observer(function PluginSettingsPage(props: Readonly<{ onDirtyChange: (dirty: boolean) => void }>) {
  const root = useRootStore();
  const show_message = useAppMessage();
  const client = root.runtime_client;
  const owner = useRef(client);
  owner.current = client;
  const file_ref = useRef<HTMLInputElement>(null);
  const config_file_ref = useRef<HTMLInputElement>(null);
  const file_request = useRef(0);
  const checked_updates = useRef(new Set<string>());
  const [plugins, setPlugins] = useState<readonly InstalledPluginSnapshot[]>([]);
  const [operations, setOperations] = useState<readonly PluginOperationSnapshot[]>([]);
  const [cleanup_blocked, setCleanupBlocked] = useState(false);
  const [market, setMarket] = useState<readonly PluginMarketEntry[] | null>(null);
  const [market_error, setMarketError] = useState(false);
  const [search, setSearch] = useState("");
  const [selected_name, setSelectedName] = useState<string | null>(null);
  const [operation, setOperation] = useState<OperationView | null>(null);
  const [uploading, setUploading] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [clear_data, setClearData] = useState(false);
  const [files, setFiles] = useState<readonly { path: string; revision: string; size_bytes: number }[]>([]);
  const [file_path, setFilePath] = useState("");
  const [file_value, setFileValue] = useState<File | null>(null);
  const selected = plugins.find((item) => item.name === selected_name) ?? null;
  const selected_identity = selected ? `${selected.name}:${selected.sha256 ?? ""}` : null;
  const selected_identity_ref = useRef(selected_identity);
  selected_identity_ref.current = selected_identity;
  const center_available = root.connection.capabilities?.mode === "enterprise";
  const query = search.trim().toLocaleLowerCase();
  const market_plugins = market ?? [];
  const selected_market = market_plugins.find((item) => item.name === selected_name) ?? null;
  const catalog_entries = [
    ...plugins.map((plugin) => ({ name: plugin.name, display_name: plugin.display_name, description: plugin.description, installed: plugin, market: market_plugins.find((item) => item.name === plugin.name) ?? null })),
    ...market_plugins.filter((item) => !plugins.some((plugin) => plugin.name === item.name)).map((item) => ({ name: item.name, display_name: item.display_name, description: item.description, installed: null, market: item })),
  ];
  const visible_entries = catalog_entries.filter((item) => `${item.display_name} ${item.name} ${item.description}`.toLocaleLowerCase().includes(query));

  async function reload(target = client) {
    if (!target) return;
    try {
      const result = await target.command({ type: "list_plugins", payload: {} });
      if (owner.current !== target) return;
      setPlugins(result.payload.plugins);
      setOperations(result.payload.operations);
      setCleanupBlocked(result.payload.cleanup_blocked);
      return result.payload.plugins;
    } catch (cause: unknown) {
      if (owner.current === target) setError(message(cause));
    }
  }

  async function loadMarket(target = client) {
    if (!target) return;
    try {
      const result = await target.command({ type: "list_plugin_market", payload: {} });
      if (owner.current === target) { setMarket(result.payload.plugins); setMarketError(false); }
    } catch {
      if (owner.current === target) setMarketError(true);
    }
  }

  async function loadFiles(plugin: InstalledPluginSnapshot, target = client) {
    const request = ++file_request.current;
    const identity = `${plugin.name}:${plugin.sha256 ?? ""}`;
    if (!target || !plugin.sha256) return;
    try {
      const result = await target.listPluginConfigFiles(plugin.name, plugin.sha256);
      if (owner.current === target && request === file_request.current && selected_identity_ref.current === identity) setFiles(result.files);
    } catch (cause: unknown) {
      if (owner.current === target && request === file_request.current && selected_identity_ref.current === identity) setError(message(cause));
    }
  }

  useEffect(() => {
    ++file_request.current;
    checked_updates.current.clear();
    setPlugins([]); setOperations([]); setCleanupBlocked(false); setMarket(null); setMarketError(false); setSearch(""); setSelectedName(null); setOperation(null); setFiles([]);
    void reload(client);
    if (center_available) void loadMarket(client);
  }, [client, center_available]);

  useEffect(() => {
    ++file_request.current;
    setFiles([]);
    if (selected) void loadFiles(selected);
  }, [selected?.name, selected?.sha256, client]);

  useEffect(() => {
    if (!client || !market || operation || busy || uploading) return;
    const candidate = plugins.find((plugin) => {
      const latest = market.find((item) => item.name === plugin.name)?.versions[0];
      return plugin.origin.type === "center" && plugin.sha256 && plugin.version && latest && latest.sha256 !== plugin.sha256
        && isNewerVersion(latest.version, plugin.version)
        && !checked_updates.current.has(`${plugin.name}:${plugin.sha256}:${latest.sha256}`);
    });
    if (!candidate?.sha256) return;
    const latest = market.find((item) => item.name === candidate.name)?.versions[0];
    if (!latest) return;
    checked_updates.current.add(`${candidate.name}:${candidate.sha256}:${latest.sha256}`);
    void prepareMarket(candidate.name, candidate.sha256);
  }, [client, market, plugins, operation, busy, uploading]);

  async function inspect(file: File, active_sha256?: string | null) {
    if (!client) return;
    const format = file.name.toLowerCase().endsWith(".zip") ? "zip" : file.name.toLowerCase().endsWith(".tar.gz") || file.name.toLowerCase().endsWith(".tgz") ? "tar_gzip" : null;
    if (!format) { setError("请选择 .zip、.tar.gz 或 .tgz 插件包。"); return; }
    setUploading(true); setError(null);
    try {
      const selection = await client.inspectPluginPackage(file, format);
      if (owner.current === client) {
        const active = plugins.find((item) => item.name === selection.name);
        setOperation({ intent: { type: "install", packages: [{ ...selection, expected_active_sha256: active_sha256 === undefined ? active?.sha256 ?? null : active_sha256 }], file }, snapshot: null });
      }
    } catch (cause: unknown) {
      if (owner.current === client) setError(message(cause));
    } finally {
      if (owner.current === client) setUploading(false);
      if (file_ref.current) file_ref.current.value = "";
    }
  }

  async function prepareMarket(name: string, expected_active_sha256: string | null) {
    if (!client) return;
    setBusy(true); setError(null);
    try {
      const result = await client.command({ type: "resolve_plugin_package", payload: { name, expected_active_sha256 } });
      if (owner.current === client) {
        const current_version = plugins.find((item) => item.name === name && item.sha256 === expected_active_sha256)?.version;
        if (expected_active_sha256 === result.payload.sha256
          || expected_active_sha256 && current_version && !isNewerVersion(result.payload.version, current_version)) {
          show_message("插件已是最新版本。");
        } else {
          setOperation({ intent: { type: "install", packages: [result.payload] }, snapshot: null, auto_begin: expected_active_sha256 !== null });
        }
      }
    } catch (cause: unknown) {
      if (owner.current === client) setError(message(cause));
    } finally { if (owner.current === client) setBusy(false); }
  }

  async function mutate(plugin: InstalledPluginSnapshot, mutation: PluginSettingsMutation): Promise<boolean> {
    if (!client || !plugin.sha256) return false;
    setBusy(true); setError(null);
    try {
      const result = await client.command({ type: "mutate_plugin_settings", payload: {
        name: plugin.name, expected_active_sha256: plugin.sha256, expected_revision: plugin.settings_revision, mutation,
      } });
      if (owner.current !== client) return false;
      setPlugins((current) => current.map((item) => item.name === plugin.name ? result.payload.plugin : item));
      show_message("插件设置已保存。");
      await reload(client);
      return true;
    } catch (cause: unknown) { if (owner.current === client) setError(message(cause)); return false; }
    finally { if (owner.current === client) setBusy(false); }
  }

  async function uploadConfig() {
    if (!client || !selected?.sha256 || !file_value || !file_path.trim()) return;
    setBusy(true); setError(null);
    try {
      const current = files.find((item) => item.path === file_path.trim());
      await client.uploadPluginConfigFile(selected.name, selected.sha256, file_path.trim(), current?.revision ?? "absent", file_value);
      if (owner.current !== client) return;
      setFileValue(null); setFilePath(""); show_message("原生配置文件已保存。");
      if (config_file_ref.current) config_file_ref.current.value = "";
      props.onDirtyChange(false);
      await loadFiles(selected, client);
      await reload(client);
    } catch (cause: unknown) { if (owner.current === client) setError(message(cause)); }
    finally { if (owner.current === client) setBusy(false); }
  }

  async function retryOperation(view: OperationView) {
    if (!client) return;
    setOperation(null);
    const latest = await reload(client);
    if (!latest || owner.current !== client || !view.intent) return;
    if (view.intent.type === "uninstall") {
      const name = view.intent.name;
      const current = latest.find((item) => item.name === name);
      if (current?.sha256) setOperation({ intent: { ...view.intent, sha256: current.sha256 }, snapshot: null });
      else show_message("插件已卸载，请刷新列表查看。");
      return;
    }
    const first = view.intent.packages[0];
    if (!first) return;
    const current_sha = latest.find((item) => item.name === first.name)?.sha256 ?? null;
    if (view.intent.file) {
      await inspect(view.intent.file, current_sha);
    } else {
      await prepareMarket(first.name, current_sha);
    }
  }

  return <>
    <SettingsPageContainer title={selected?.display_name ?? selected_market?.display_name ?? "插件"} on_back={selected || selected_market ? () => { setSelectedName(null); setFiles([]); props.onDirtyChange(false); } : undefined}
      actions={<><Button disabled={!client || uploading || busy} onClick={() => void reload()}><Icon name="refresh" size={15} />刷新</Button><Button disabled={!client || uploading || busy} onClick={() => file_ref.current?.click()}><Icon name="plus" size={15} />{uploading ? "检查插件包…" : "上传插件"}</Button><input accept=".zip,.gz,.tgz" aria-label="选择插件包" hidden onChange={(event) => { const file = event.target.files?.[0]; if (file) void inspect(file); }} ref={file_ref} type="file" /></>}>
      <div className={styles.page}>
        {error && <p className={styles.error} role="alert">{error}</p>}
        {cleanup_blocked && <p className={styles.error} role="status">插件目录存在待清理记录，应用会自动重试，不影响其他插件操作。</p>}
        {selected ? <>
          <div className={styles.meta}><span>{selected.origin.type === "center" ? "企业中心" : "个人上传"}</span><span>{selected.version ?? "无当前版本"}</span><span>{!selected.sha256 ? "已卸载，保留数据" : selected.package_state === "available" ? selected.enabled ? "已启用" : "已停用" : "包异常"}</span></div>
          <p>{pluginFaultSummary(selected)}</p>
          <p>{selected.description || "暂无说明"}</p>
          {!selected.sha256 && <p className={styles.notice}>当前未安装插件包；原有配置与运行数据保留在用户域。重新上传同名包或从企业市场安装后即可继续使用。</p>}
          {selected.sha256 && <>
          <label className={styles.toggle}><input checked={selected.enabled} disabled={busy} onChange={(event) => void mutate(selected, { type: "set_enabled", enabled: event.target.checked })} type="checkbox" />整体启用</label>
          <section><h4>包内能力</h4><p>技能：{selected.skills.join("、") || "无"}</p><p>MCP：{selected.mcp.join("、") || "无"}</p></section>
          <section><h4>配置参数</h4>{selected.configuration.length === 0 && <p>无需参数</p>}{selected.configuration.map((field) => <SettingField key={field.name} label={field.label} is_set={field.is_set} secret={field.secret} busy={busy} onSave={(value) => mutate(selected, { type: "set_parameter", name: field.name, value })} />)}</section>
          <section><h4>外部软件路径</h4>{selected.external_software.length === 0 && <p>无需外部软件</p>}{selected.external_software.map((field) => <SettingField key={field.name} label={field.display_name} is_set={field.is_set} busy={busy} onSave={(value) => mutate(selected, { type: "set_software_path", name: field.name, value })} />)}</section>
          <section><h4>原生配置文件</h4><p>按插件要求填写相对路径并上传原始文件，保留文件本身格式。</p><ul className={styles.files}>{files.map((file) => <li key={file.path}>{file.path} · {formatBytes(file.size_bytes)}</li>)}</ul><div className={styles.file_form}><input aria-label="配置文件相对路径" onChange={(event) => { setFilePath(event.target.value); props.onDirtyChange(true); }} placeholder="例如 settings/config.yaml" value={file_path} /><div className={styles.file_picker}><Button disabled={busy} onClick={() => config_file_ref.current?.click()}>选择文件</Button><span title={file_value?.name}>{file_value?.name ?? "未选择文件"}</span><input aria-label="选择原生配置文件" hidden onChange={(event) => { setFileValue(event.target.files?.[0] ?? null); props.onDirtyChange(true); }} ref={config_file_ref} type="file" /></div><Button disabled={busy || !file_path.trim() || !file_value} onClick={() => void uploadConfig()}>保存文件</Button></div></section>
          <section className={styles.danger}><h4>更新与卸载</h4>{market_plugins.some((item) => item.name === selected.name) && <Button disabled={busy} onClick={() => void prepareMarket(selected.name, selected.sha256)}>检查中心更新</Button>}<p>卸载默认保留插件配置与运行数据。项目中的产物始终保留。</p><label><input checked={clear_data} onChange={(event) => setClearData(event.target.checked)} type="checkbox" />同时清除插件配置与运行数据</label><Button disabled={busy} onClick={() => selected.sha256 && setOperation({ intent: { type: "uninstall", name: selected.name, sha256: selected.sha256, clear_data }, snapshot: null })} variant="danger">卸载插件</Button></section>
          </>}
          {!selected.sha256 && selected.origin.type === "center" && <Button disabled={busy} onClick={() => void prepareMarket(selected.name, null)}>从中心重新安装</Button>}
        </> : selected_market ? <>
          <div className={styles.meta}><span>企业中心</span><span>未安装</span></div>
          <p>{selected_market.description || "暂无说明"}</p>
          <section><h4>可用版本</h4><p>{selected_market.versions.map((version) => version.version).join("、") || "暂无可用版本"}</p></section>
          <Button disabled={busy || selected_market.versions.length === 0} onClick={() => void prepareMarket(selected_market.name, null)} variant="primary">安装插件</Button>
        </> : <div className={styles.catalog}>
          <label className={styles.search}><Icon name="search" size={17} /><input aria-label="搜索插件" onChange={(event) => setSearch(event.target.value)} placeholder="搜索插件" type="search" value={search} /></label>
          <section className={styles.catalog_section}>
            <header className={styles.section_heading}><h4>全部插件</h4><span>{catalog_entries.length}</span></header>
            {center_available && market_error && <div className={styles.inline_error} role="alert"><span>企业插件暂不可读取，以下仅显示已知插件。</span><Button onClick={() => void loadMarket()} size="small">重试</Button></div>}
            {center_available && !market_error && market === null && <p className={styles.loading} role="status">正在读取企业插件…</p>}
            {visible_entries.length > 0 ? <div className={styles.plugin_grid}>
              {visible_entries.map((item) => {
                const summary = item.installed ? pluginCatalogStatus(item.installed) : item.description || item.name;
                const copy = <><strong>{item.display_name}</strong><small>{summary}</small></>;
                const installed = item.installed;
                const active_sha = installed?.sha256 ?? null;
                const openDetail = () => { setSelectedName(item.name); setClearData(false); setFiles([]); };
                return <article className={styles.plugin_item} key={item.name}>
                  <PluginGlyph name={item.display_name} tone={pluginTone(item.name)} />
                  <span className={styles.plugin_copy}>{copy}</span>
                  <button aria-label={`查看 ${item.display_name} 详情`} className={styles.card_hit_area} onClick={openDetail} type="button" />
                  {active_sha && installed
                    ? <DropdownMenu className={styles.item_action}>
                      <DropdownMenuTrigger aria-label={`操作 ${item.display_name}`} iconOnly variant="text"><Icon name="more" size={18} /></DropdownMenuTrigger>
                      <DropdownMenuContent align="end" aria-label={`${item.display_name} 操作`}>
                        <DropdownMenuItem disabled={busy} onSelect={() => void mutate(installed, { type: "set_enabled", enabled: !installed.enabled })}><span className={styles.menu_icon}><Icon name={installed.enabled ? "stop" : "check"} size={installed.enabled ? 28 : 20} /></span>{installed.enabled ? "停用插件" : "启用插件"}</DropdownMenuItem>
                        {item.market
                          ? <DropdownMenuItem disabled={busy} onSelect={() => void prepareMarket(item.name, active_sha)}><span className={styles.menu_icon}><Icon name="refresh" size={16} /></span>检查更新</DropdownMenuItem>
                          : <DropdownMenuItem disabled={busy || uploading} onSelect={() => file_ref.current?.click()}><span className={styles.menu_icon}><Icon name="plus" size={20} /></span>上传新版本</DropdownMenuItem>}
                        <DropdownMenuItem className={styles.menu_danger} disabled={busy} onSelect={() => { setClearData(false); setOperation({ intent: { type: "uninstall", name: item.name, sha256: active_sha, clear_data: false }, snapshot: null }); }}><span className={styles.menu_icon}><Icon name="trash" size={19} /></span>卸载插件…</DropdownMenuItem>
                      </DropdownMenuContent>
                    </DropdownMenu>
                    : <Button aria-label={`${item.market ? "安装" : "上传"} ${item.display_name}`} className={styles.item_action} disabled={busy || uploading} iconOnly onClick={() => item.market ? void prepareMarket(item.name, null) : file_ref.current?.click()} title={item.market ? "安装插件" : "上传同名插件包"} variant="text"><Icon name="plus" size={18} /></Button>}
                </article>;
              })}
            </div> : <p className={styles.empty}>{query ? "没有匹配的插件。" : "暂无插件，可上传本地插件包。"}</p>}
          </section>
          {operations.length > 0 && <section className={`${styles.catalog_section} ${styles.operations_section}`}><header className={styles.section_heading}><h4>近期操作</h4><span>{operations.length}</span></header><div className={styles.operation_list}>{operations.map((item) => <div className={styles.operation_row} key={item.key}><span>{item.kind === "uninstall" ? "卸载" : item.kind === "update" ? "更新" : "安装"} · {item.phase} · {item.key}</span><Button onClick={() => setOperation({ intent: null, snapshot: item })} size="small">查看</Button></div>)}</div></section>}
        </div>}
      </div>
    </SettingsPageContainer>
    {client && operation && <PluginOperationDialog key={operation.snapshot?.key ?? (operation.intent?.type === "install" ? operation.intent.packages.map((item) => item.sha256).join(":") : operation.intent?.name ?? "operation")} client={client} intent={operation.intent} initial_snapshot={operation.snapshot} auto_begin={operation.auto_begin} on_close={() => { setOperation(null); void reload(client); }} on_complete={() => { show_message("插件操作已完成。"); void reload(client); }} on_retry={operation.intent ? () => void retryOperation(operation) : undefined} />}
  </>;
});

function SettingField(props: Readonly<{ label: string; is_set: boolean; secret?: boolean; busy: boolean; onSave: (value: string | null) => Promise<boolean> }>) {
  const [value, setValue] = useState("");
  return <div className={styles.setting_field}><label>{props.label} <small>{props.is_set ? "已设置" : "未设置"}</small><input aria-label={props.label} onChange={(event) => setValue(event.target.value)} placeholder={props.secret && props.is_set ? "留空保持原值" : "输入新值"} type={props.secret ? "password" : "text"} value={value} /></label><Button disabled={props.busy || !value.trim()} onClick={() => void props.onSave(value).then((saved) => { if (saved) setValue(""); })}>保存</Button>{props.is_set && <Button disabled={props.busy} onClick={() => void props.onSave(null)}>清除</Button>}</div>;
}

function formatBytes(value: number): string { return value >= 1024 ** 2 ? `${(value / 1024 ** 2).toFixed(1)} MiB` : `${Math.round(value / 1024)} KiB`; }
function isNewerVersion(candidate: string, installed: string): boolean {
  const next = candidate.split(".").map(Number);
  const current = installed.split(".").map(Number);
  if (next.length !== 3 || current.length !== 3 || [...next, ...current].some((part) => !Number.isSafeInteger(part))) return false;
  for (let index = 0; index < 3; index++) {
    if (next[index] !== current[index]) return next[index]! > current[index]!;
  }
  return false;
}
function message(error: unknown): string { return error instanceof Error ? error.message : "插件操作失败，请重试。"; }
function pluginTone(name: string): number { return Array.from(name).reduce((value, character) => value + (character.codePointAt(0) ?? 0), 0) % 4; }
function PluginGlyph(props: Readonly<{ name: string; tone: number }>) {
  return <span aria-hidden="true" className={styles.plugin_glyph} data-tone={props.tone}>{Array.from(props.name.trim())[0]?.toLocaleUpperCase() ?? <Icon name="plugin" size={19} />}</span>;
}
function pluginCatalogStatus(plugin: InstalledPluginSnapshot): string {
  if (!plugin.sha256) return "已卸载，保留数据";
  if (plugin.package_state !== "available") return "包异常";
  if (!plugin.enabled) return "已停用";
  const fault = pluginFaultSummary(plugin);
  return fault.startsWith("能力待处理") ? fault : "已启用";
}
function pluginFaultSummary(plugin: InstalledPluginSnapshot): string {
  if (!plugin.sha256) return "能力不可用：插件包未安装";
  if (plugin.package_state !== "available") return "能力不可用：插件包异常";
  if (!plugin.enabled) return "能力不可用：插件已停用";
  const missing_parameters = plugin.configuration.filter((field) => field.required && !field.is_set).length;
  const missing_software = plugin.external_software.filter((field) => !field.is_set).length;
  const faults = [missing_parameters && `缺少 ${missing_parameters} 项必要参数`, missing_software && `缺少 ${missing_software} 项外部软件路径`].filter(Boolean);
  return faults.length ? `能力待处理：${faults.join("、")}` : "已安装；能力可用性在会话中核对";
}
