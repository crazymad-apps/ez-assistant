import { observer } from "mobx-react-lite";
import { useCallback, useEffect, useState } from "react";
import { SelectionPopover, type SelectionOption } from "../../../../components/SelectionPopover";
import { useRootStore } from "../../../../stores/RootStoreContext";
import { normalizeConsolePath, parseMacEntries, type MacWhitelistEntry } from "./api";
import styles from "./index.module.scss";

type MacWhitelistTabProps = Readonly<{
  device_id: string;
  api_prefix: string;
}>;

const ENABLED_OPTIONS: ReadonlyArray<SelectionOption<string>> = [
  { value: "true", label: "启用" },
  { value: "false", label: "禁用" },
];

export const MacWhitelistTab = observer(function MacWhitelistTab(props: MacWhitelistTabProps) {
  const { device_gateway: gateway } = useRootStore();
  const [entries, setEntries] = useState<MacWhitelistEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [mac, setMac] = useState("");
  const [groupId, setGroupId] = useState("home-default");
  const [zone, setZone] = useState("living_room");
  const [label, setLabel] = useState("");
  const [enabled, setEnabled] = useState("true");
  const [enabledOpen, setEnabledOpen] = useState(false);
  const [saving, setSaving] = useState(false);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const payload = await gateway.consoleJson<unknown>(
        props.device_id,
        normalizeConsolePath(props.api_prefix, "macs"),
      );
      setEntries(parseMacEntries(payload));
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "加载 MAC 白名单失败。");
    } finally {
      setLoading(false);
    }
  }, [gateway, props.api_prefix, props.device_id]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  async function saveEntry(event: React.FormEvent) {
    event.preventDefault();
    const cleanedMac = mac.trim();
    if (!cleanedMac) {
      setError("请填写终端 MAC 地址。");
      return;
    }
    setSaving(true);
    setError(null);
    try {
      await gateway.consoleJson(
        props.device_id,
        normalizeConsolePath(props.api_prefix, `macs/${encodeURIComponent(cleanedMac)}`),
        {
          method: "PUT",
          body: {
            group_id: groupId.trim() || "home-default",
            zone: zone.trim(),
            label: label.trim(),
            enabled: enabled === "true",
          },
        },
      );
      setMac("");
      setLabel("");
      await refresh();
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "保存 MAC 白名单失败。");
    } finally {
      setSaving(false);
    }
  }

  async function deleteEntry(targetMac: string) {
    setError(null);
    try {
      await gateway.consoleJson(
        props.device_id,
        normalizeConsolePath(props.api_prefix, `macs/${encodeURIComponent(targetMac)}`),
        { method: "DELETE" },
      );
      await refresh();
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "删除 MAC 失败。");
    }
  }

  return (
    <div className={styles.section_grid}>
      <section className={styles.panel}>
        <header className={styles.panel_header}>
          <h4>添加或更新 MAC 白名单</h4>
        </header>
        <form className={styles.panel_body} onSubmit={(event) => void saveEntry(event)}>
          {error ? <p className={styles.error_banner} role="alert">{error}</p> : null}
          <div className={styles.form_grid}>
            <label className={styles.field}>
              <span>MAC 地址</span>
              <input
                aria-label="白名单 MAC 地址"
                onChange={(event) => setMac(event.currentTarget.value)}
                placeholder="30:ed:a0:f3:dc:70"
                value={mac}
              />
            </label>

            <label className={styles.field}>
              <span>家庭/组 ID (Group)</span>
              <input
                aria-label="白名单组 ID"
                onChange={(event) => setGroupId(event.currentTarget.value)}
                value={groupId}
              />
            </label>

            <label className={styles.field}>
              <span>房间区域 (Zone)</span>
              <input
                aria-label="房间区域"
                onChange={(event) => setZone(event.currentTarget.value)}
                value={zone}
              />
            </label>

            <label className={styles.field}>
              <span>设备备注</span>
              <input
                aria-label="设备备注"
                onChange={(event) => setLabel(event.currentTarget.value)}
                placeholder="客厅音箱"
                value={label}
              />
            </label>

            <div className={styles.field}>
              <span>状态</span>
              <SelectionPopover
                aria_label="白名单启用状态"
                content_width="content"
                on_open_change={setEnabledOpen}
                on_select={(val) => {
                  setEnabled(val);
                  setEnabledOpen(false);
                }}
                open={enabledOpen}
                options={ENABLED_OPTIONS}
                selected={enabled}
                trigger_variant="field"
              />
            </div>
          </div>

          <div className={styles.action_row}>
            <button className={styles.primary_btn} disabled={saving} type="submit">
              {saving ? "保存中…" : "保存白名单"}
            </button>
          </div>
        </form>
      </section>

      <section className={styles.panel}>
        <header className={styles.panel_header}>
          <h4>已授权 MAC 列表 ({entries.length})</h4>
          <button disabled={loading} onClick={() => void refresh()} type="button">
            {loading ? "加载中…" : "刷新"}
          </button>
        </header>
        <div className={styles.panel_body}>
          {entries.length > 0 ? (
            <div className={styles.table_wrap}>
              <table>
                <thead>
                  <tr>
                    <th>MAC</th>
                    <th>Group</th>
                    <th>Zone</th>
                    <th>备注</th>
                    <th>状态</th>
                    <th>操作</th>
                  </tr>
                </thead>
                <tbody>
                  {entries.map((entry) => (
                    <tr key={entry.mac}>
                      <td><code>{entry.mac}</code></td>
                      <td>{entry.group_id}</td>
                      <td>{entry.zone}</td>
                      <td>{entry.label || "-"}</td>
                      <td>
                        <span className={styles.badge} data-tone={entry.enabled ? "online" : "offline"}>
                          {entry.enabled ? "启用" : "禁用"}
                        </span>
                      </td>
                      <td>
                        <button
                          className={styles.danger_btn}
                          onClick={() => void deleteEntry(entry.mac)}
                          type="button"
                        >
                          删除
                        </button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          ) : (
            <p className={styles.empty_note}>暂无 MAC 白名单条目。</p>
          )}
        </div>
      </section>
    </div>
  );
});
