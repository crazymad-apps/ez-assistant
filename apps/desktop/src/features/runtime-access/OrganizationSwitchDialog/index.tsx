import { useId } from "react";
import { observer } from "mobx-react-lite";
import { Button } from "../../../components/Button";
import { Dialog } from "../../../components/Dialog";
import type { ApplicationConnectionStore } from "../ApplicationConnectionStore";
import styles from "../AccountDialog/index.module.scss";

export const OrganizationSwitchDialog = observer(function OrganizationSwitchDialog({
  connection,
}: Readonly<{ connection: ApplicationConnectionStore }>) {
  const state = connection.organization_switch;
  const title_id = useId();
  const preview = state && "preview" in state ? state.preview : null;
  const has_work = Boolean(preview && (preview.tasks || preview.approvals || preview.queued));
  const phase = state?.phase;
  const waiting = connection.pending || phase === "stopping" || phase === "exchanging";
  const failure_without_workspace = (phase === "stop_failed" || phase === "exchange_failed") && connection.phase !== "workspace";
  const dismissible = !waiting && phase !== "verify" && !failure_without_workspace;
  const close = () => connection.closeOrganizationSwitch();

  return <Dialog
    open={Boolean(state) && connection.organization_switch_visible} aria_labelledby={title_id}
    backdrop_class_name={styles.backdrop} dialog_class_name={styles.dialog}
    on_close={close} dismissible={dismissible}
  >
    <header><h3 id={title_id}>{preview ? `切换到「${preview.organization.name}」` : "切换组织"}</h3></header>
    <div className={styles.body}>
      {phase === "checking" && <p role="status">正在检查任务…</p>}
      {state?.phase === "selection_error" && <p role="alert">{state.message}</p>}
      {phase === "confirm" && <p>未保存的修改、未发送的输入和附件选择会被舍弃。</p>}
      {phase === "confirm" && preview && has_work && <div className={styles.impact}>
        <p>需要先终止当前组织中的任务。未执行的排队内容会保留，返回后需手动恢复。已经发生的操作无法回滚。</p>
        <dl>
          {preview.tasks > 0 && <div><dt>活动工作</dt><dd>{preview.tasks}</dd></div>}
          {preview.approvals > 0 && <div><dt>等待审批</dt><dd>{preview.approvals}</dd></div>}
          {preview.queued > 0 && <div><dt>待执行队列</dt><dd>{preview.queued}</dd></div>}
        </dl>
      </div>}
      {phase === "stopping" && <p role="status">正在终止任务，等待工具与存储收尾…</p>}
      {phase === "exchanging" && <p role="status">正在切换组织…</p>}
      {phase === "verify" && <p role="alert">连接中断，切换结果尚未确认。</p>}
      {phase === "stop_failed" && <p role="alert">尚未确认全部任务和资源已完成收尾，当前组织未切换。</p>}
      {phase === "exchange_failed" && <p role="alert">当前组织已完成收尾，目标组织登录未换发。可以重新进入当前组织。</p>}
    </div>
    <footer>
      {dismissible && <Button onClick={close}>{phase === "stop_failed" || phase === "exchange_failed" || phase === "selection_error" ? "关闭" : "取消"}</Button>}
      {phase === "stop_failed" && failure_without_workspace && <Button disabled={waiting} onClick={() => connection.leaveFailedOrganizationSwitch()}>返回登录</Button>}
      {phase === "confirm" && <Button variant={has_work ? "danger" : "primary"} onClick={() => void connection.confirmOrganizationSwitch()}>{has_work ? "终止并切换" : "切换组织"}</Button>}
      {(phase === "verify" || phase === "stop_failed") && <Button variant="primary" disabled={waiting} onClick={() => void connection.checkOrganizationSwitch()}>重新检查状态</Button>}
      {phase === "exchange_failed" && <Button variant="primary" disabled={waiting} onClick={() => void connection.resumeOrganizationAfterSwitch()}>重新进入当前组织</Button>}
    </footer>
  </Dialog>;
});
