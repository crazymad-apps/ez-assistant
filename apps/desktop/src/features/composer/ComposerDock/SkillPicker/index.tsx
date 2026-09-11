import { observer } from "mobx-react-lite";
import { useEffect, useState } from "react";
import type { SkillSummarySnapshot, WorkspaceId } from "@ez-assistant/protocol";
import { useRootStore } from "../../../../stores/RootStoreContext";
import { SkillListStore } from "../../../skills";
import { InputContextPicker } from "../InputContextPicker";


/** 每次呼出都从当前 Host 扫描；父级以会话/草稿身份重建，响应不跨 owner。 */
export const SkillPicker = observer(function SkillPicker(props: Readonly<{
  workspace_id?: WorkspaceId | null;
  on_close: () => void;
  on_select: (skill: SkillSummarySnapshot) => void;
}>) {
  const root = useRootStore();
  const [store] = useState(() => new SkillListStore());
  const load = () => store.load(() => root.listSkills(props.workspace_id));
  useEffect(() => {
    void load();
    return () => store.dispose();
  }, [store, props.workspace_id]);
  return <InputContextPicker
    error={store.error}
    label="技能"
    loading={store.loading}
    on_close={props.on_close}
    on_retry={() => void load()}
    on_select={props.on_select}
    open
    options={availableUserSkills(store.snapshot?.skills ?? [])}
  />;
});

function availableUserSkills(skills: readonly SkillSummarySnapshot[]): SkillSummarySnapshot[] {
  return skills.filter((skill) => skill.enabled && skill.user_invocable && skill.health === "ready");
}
