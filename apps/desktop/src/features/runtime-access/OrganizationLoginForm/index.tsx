import { observer } from "mobx-react-lite";
import { useState } from "react";
import { Button } from "../../../components/Button";
import { SelectionPopover } from "../../../components/SelectionPopover";
import type { ApplicationConnectionStore } from "../ApplicationConnectionStore";
import styles from "./index.module.scss";

type OrganizationLoginFormProps = Readonly<{
  connection: ApplicationConnectionStore;
  className: string;
}>;

export const OrganizationLoginForm = observer(function OrganizationLoginForm({
  connection,
  className,
}: OrganizationLoginFormProps) {
  const [open, setOpen] = useState(false);
  const [choice, setChoice] = useState<string>();
  const options = connection.organization_selection?.organizations ?? [];
  const selected = options.find((item) => item.id === choice) ?? options[0];

  return (
    <form className={className} onSubmit={(event) => {
      event.preventDefault();
      if (selected) void connection.selectOrganization(selected.id);
    }}>
      {selected ? (
        <>
          <strong>选择组织</strong>
          <SelectionPopover
            aria_label="选择组织"
            trigger_variant="field"
            trigger_class_name={styles.field}
            size="large"
            open={open}
            on_open_change={setOpen}
            selected={selected.id}
            options={options.map(({ id, name }) => ({ value: id, label: name }))}
            disabled={connection.pending}
            on_select={(id) => { setChoice(id); setOpen(false); }}
          />
        </>
      ) : <p role="status">当前账号没有可用的组织，请联系管理员。</p>}
      <div className={styles.actions}>
        {selected && (
          <Button disabled={connection.pending} size="large" type="submit" variant="primary">
            {connection.pending ? "正在进入…" : "进入组织"}
          </Button>
        )}
        <Button className={styles.back} type="button" size="small" variant="text" disabled={connection.pending} onClick={() => connection.cancelOrganizationSelection()}>
          返回登录
        </Button>
      </div>
    </form>
  );
});
