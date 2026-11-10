import type { HostUserIdentity } from "@ez-assistant/protocol";
import { observer } from "mobx-react-lite";
import { useId, useState } from "react";
import { Collapse } from "../../../../components/Collapse";
import { DropdownMenuItem } from "../../../../components/DropdownMenu";
import { Icon } from "../../../../components/Icon";
import type { ApplicationConnectionStore } from "../../ApplicationConnectionStore";
import styles from "./index.module.scss";

export const OrganizationMenu = observer(function OrganizationMenu({ connection, identity }: Readonly<{
  connection: ApplicationConnectionStore;
  identity: HostUserIdentity;
}>) {
  const [expanded, setExpanded] = useState(false);
  const list_id = useId();
  const state = connection.organization_switch;
  const recovering = state?.phase === "stop_failed" || state?.phase === "exchange_failed" || state?.phase === "verify";
  const options = state?.phase === "options" ? state : null;

  function toggleOptions() {
    setExpanded(!expanded && !recovering);
    if (!expanded || recovering) void connection.openOrganizationSwitch();
  }

  return (
    <>
      <div className={styles.divider} role="separator" />
      <div className={styles.organization}>
        <DropdownMenuItem
          aria-controls={expanded ? list_id : undefined}
          aria-expanded={expanded}
          aria-label={`切换组织，当前组织：${identity.organization_name}`}
          className={styles.organization_row}
          disabled={connection.pending}
          keep_open={!recovering}
          onSelect={toggleOptions}
        >
          <Icon name="organization" size={15} />
          <span title={identity.organization_name}>{identity.organization_name}</span>
          <Icon className={styles.disclosure} name="chevron-down" size={14} />
        </DropdownMenuItem>
        <Collapse id={list_id} open={expanded}>
          <div aria-label="可切换组织" className={styles.options} role="group">
            {options?.organizations.map(({ id, name }) => (
              <DropdownMenuItem
                className={styles.option}
                key={id}
                onSelect={() => {
                  setExpanded(false);
                  void connection.previewOrganizationSwitch(id);
                }}
              >
                <span title={name}>{name}</span>
              </DropdownMenuItem>
            ))}
            {state?.phase === "loading" && <span className={styles.status} role="status">正在读取组织…</span>}
            {options?.message && <span className={styles.status} role="status" title={options.message}>读取失败，请重新展开</span>}
            {options && !options.message && options.organizations.length === 0 && (
              <span className={styles.status} role="status">暂无其他可切换组织</span>
            )}
          </div>
        </Collapse>
      </div>
    </>
  );
});
