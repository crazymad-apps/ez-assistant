import { observer } from "mobx-react-lite";
import { useCallback, useEffect, useState } from "react";
import { SelectionPopover, type SelectionOption } from "../../../../components/SelectionPopover";
import { useRootStore } from "../../../../stores/RootStoreContext";
import { normalizeConsolePath, parseAclRules, roleLabel, type AclRuleEntry } from "./api";
import styles from "./index.module.scss";

type AclMatrixTabProps = Readonly<{
  device_id: string;
  api_prefix: string;
}>;

const ROLE_OPTIONS: ReadonlyArray<SelectionOption<string>> = [
  { value: "1", label: "Owner (1)" },
  { value: "2", label: "Family (2)" },
  { value: "3", label: "Guest (3)" },
];

const ALLOW_OPTIONS: ReadonlyArray<SelectionOption<string>> = [
  { value: "true", label: "Allow (允许)" },
  { value: "false", label: "Deny (拒绝)" },
];

export const AclMatrixTab = observer(function AclMatrixTab(props: AclMatrixTabProps) {
  const { device_gateway: gateway } = useRootStore();
  const [rules, setRules] = useState<AclRuleEntry[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const [ruleId, setRuleId] = useState("");
  const [agentId, setAgentId] = useState("ez-assistant");
  const [groupId, setGroupId] = useState("*");
  const [zonePattern, setZonePattern] = useState("*");
  const [targetDeviceId, setTargetDeviceId] = useState("*");
  const [capabilityPattern, setCapabilityPattern] = useState("*");
  const [minRole, setMinRole] = useState("1");
  const [allow, setAllow] = useState("true");
  const [roleOpen, setRoleOpen] = useState(false);
  const [allowOpen, setAllowOpen] = useState(false);
  const [saving, setSaving] = useState(false);

  const refresh = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const payload = await gateway.consoleJson<unknown>(
        props.device_id,
        normalizeConsolePath(props.api_prefix, "acls"),
      );
      setRules(parseAclRules(payload));
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "加载 ACL 规则失败。");
    } finally {
      setLoading(false);
    }
  }, [gateway, props.api_prefix, props.device_id]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  async function saveRule(event: React.FormEvent) {
    event.preventDefault();
    const cleanedId = ruleId.trim();
    if (!cleanedId) {
      setError("请填写规则 ID。");
      return;
    }
    setSaving(true);
    setError(null);
    try {
      await gateway.consoleJson(
        props.device_id,
        normalizeConsolePath(props.api_prefix, `acls/${encodeURIComponent(cleanedId)}`),
        {
          method: "PUT",
          body: {
            agent_id: agentId.trim() || "*",
            group_id: groupId.trim() || "*",
            zone_pattern: zonePattern.trim() || "*",
            device_id: targetDeviceId.trim() || "*",
            capability_pattern: capabilityPattern.trim() || "*",
            min_speaker_role: Number(minRole),
            allow: allow === "true",
          },
        },
      );
      setRuleId("");
      await refresh();
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "保存 ACL 规则失败。");
    } finally {
      setSaving(false);
    }
  }

  async function deleteRule(id: string) {
    setError(null);
    try {
      await gateway.consoleJson(
        props.device_id,
        normalizeConsolePath(props.api_prefix, `acls/${encodeURIComponent(id)}`),
        { method: "DELETE" },
      );
      await refresh();
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : "删除 ACL 规则失败。");
    }
  }

  return (
    <div className={styles.section_grid}>
      <section className={styles.panel}>
        <header className={styles.panel_header}>
          <h4>添加或更新 Agent → Device ACL 规则</h4>
        </header>
        <form className={styles.panel_body} onSubmit={(event) => void saveRule(event)}>
          {error ? <p className={styles.error_banner} role="alert">{error}</p> : null}
          <div className={styles.form_grid}>
            <label className={styles.field}>
              <span>规则 ID</span>
              <input
                aria-label="ACL 规则 ID"
                onChange={(event) => setRuleId(event.currentTarget.value)}
                placeholder="acl-living-camera"
                value={ruleId}
              />
            </label>

            <label className={styles.field}>
              <span>Agent ID</span>
              <input
                aria-label="ACL Agent ID"
                onChange={(event) => setAgentId(event.currentTarget.value)}
                value={agentId}
              />
            </label>

            <label className={styles.field}>
              <span>Group 模式</span>
              <input
                aria-label="ACL Group 模式"
                onChange={(event) => setGroupId(event.currentTarget.value)}
                value={groupId}
              />
            </label>

            <label className={styles.field}>
              <span>Zone 模式</span>
              <input
                aria-label="ACL Zone 模式"
                onChange={(event) => setZonePattern(event.currentTarget.value)}
                value={zonePattern}
              />
            </label>

            <label className={styles.field}>
              <span>目标设备 ID</span>
              <input
                aria-label="ACL 目标设备 ID"
                onChange={(event) => setTargetDeviceId(event.currentTarget.value)}
                value={targetDeviceId}
              />
            </label>

            <label className={styles.field}>
              <span>能力通配 (Capability)</span>
              <input
                aria-label="ACL 能力通配"
                onChange={(event) => setCapabilityPattern(event.currentTarget.value)}
                value={capabilityPattern}
              />
            </label>

            <div className={styles.field}>
              <span>最低声纹角色</span>
              <SelectionPopover
                aria_label="最低声纹角色"
                content_width="content"
                on_open_change={setRoleOpen}
                on_select={(val) => {
                  setMinRole(val);
                  setRoleOpen(false);
                }}
                open={roleOpen}
                options={ROLE_OPTIONS}
                selected={minRole}
                trigger_variant="field"
              />
            </div>

            <div className={styles.field}>
              <span>动作</span>
              <SelectionPopover
                aria_label="ACL 动作"
                content_width="content"
                on_open_change={setAllowOpen}
                on_select={(val) => {
                  setAllow(val);
                  setAllowOpen(false);
                }}
                open={allowOpen}
                options={ALLOW_OPTIONS}
                selected={allow}
                trigger_variant="field"
              />
            </div>
          </div>

          <div className={styles.action_row}>
            <button className={styles.primary_btn} disabled={saving} type="submit">
              {saving ? "保存中…" : "保存 ACL 规则"}
            </button>
          </div>
        </form>
      </section>

      <section className={styles.panel}>
        <header className={styles.panel_header}>
          <h4>现有 ACL 权限规则 ({rules.length})</h4>
          <button disabled={loading} onClick={() => void refresh()} type="button">
            {loading ? "加载中…" : "刷新"}
          </button>
        </header>
        <div className={styles.panel_body}>
          {rules.length > 0 ? (
            <div className={styles.table_wrap}>
              <table>
                <thead>
                  <tr>
                    <th>规则 ID</th>
                    <th>Agent</th>
                    <th>Group / Zone</th>
                    <th>Capability</th>
                    <th>最低角色</th>
                    <th>结果</th>
                    <th>操作</th>
                  </tr>
                </thead>
                <tbody>
                  {rules.map((rule) => (
                    <tr key={rule.rule_id}>
                      <td><code>{rule.rule_id}</code></td>
                      <td>{rule.agent_id}</td>
                      <td>{rule.group_id} / {rule.zone_pattern}</td>
                      <td><code>{rule.capability_pattern}</code></td>
                      <td>{roleLabel(rule.min_speaker_role)}</td>
                      <td>
                        <span className={styles.badge} data-tone={rule.allow ? "online" : "warn"}>
                          {rule.allow ? "Allow" : "Deny"}
                        </span>
                      </td>
                      <td>
                        <button
                          className={styles.danger_btn}
                          onClick={() => void deleteRule(rule.rule_id)}
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
            <p className={styles.empty_note}>暂无 ACL 规则。</p>
          )}
        </div>
      </section>
    </div>
  );
});
