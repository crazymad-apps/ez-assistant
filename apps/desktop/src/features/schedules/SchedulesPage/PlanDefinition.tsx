import type { ReactNode } from "react";
import type { ScheduleSnapshot } from "@ez-assistant/protocol";
import { MarkdownContent } from "../../../components/MarkdownContent";
import { useAppearance } from "../../../stores/AppearanceContext";
import { planStatus, schedule_frequencies, scheduledTime, weekdays } from "./display";
import styles from "./index.module.scss";

export function PlanDefinition({ plan, workspace }: Readonly<{ plan: ScheduleSnapshot; workspace?: string }>) {
  const theme = useAppearance().preset;
  const { action, rule, workspace_id, report_to_controller } = plan.settings;
  const status = planStatus(plan);
  return <section className={styles.definition} aria-label="计划配置">
    <dl className={styles.fields_row}>
      <ReadField label="任务类型">{action.type === "script" ? "脚本任务" : "Agent 任务"}</ReadField>
      <ReadField label="关联项目">{workspace ?? (workspace_id ? "已关联项目（当前不可用）" : "不关联项目")}</ReadField>
    </dl>
    <div className={styles.schedule_fields}>
      <dl className={styles.fields_row}>
        <ReadField label="执行频率">{schedule_frequencies[rule.type]}</ReadField>
        {rule.type === "once" && <ReadField label="日期">{rule.date}</ReadField>}
        <ReadField label="时间（设备当地时间）">{rule.time}</ReadField>
      </dl>
      {rule.type === "weekly" && <dl>
        <ReadField label="执行星期">{weekdays.filter(([day]) => rule.weekdays.includes(day)).map(([, label]) => label).join("、")}</ReadField>
      </dl>}
      <div className={styles.schedule_status}>
        <span className={styles.preview}>下次触发 {scheduledTime(plan)}</span>
        <span className={styles.plan_status} data-enabled={status === "已启用"}>{status}</span>
      </div>
    </div>
    <dl>
      <div className={styles.field}>
        <dt>{action.type === "script" ? "Shell 脚本" : "任务指令"}</dt>
        <dd className={styles.read_body} tabIndex={0}>
          {action.type === "script" ? <pre>{action.command}</pre> : <MarkdownContent theme={theme} text={action.instructions} />}
        </dd>
      </div>
    </dl>
    {action.type === "agent" && <dl className={styles.fields_row}>
      <ReadField label="智能体">{action.custom_agent?.name ?? "默认智能体"}</ReadField>
      <ReadField label="模型">{action.model_selection?.model_id ?? "默认模型"}</ReadField>
      <ReadField label="工具权限模式">{action.approval_mode === "auto" ? "按规则自动决定" : "需要授权时询问"}</ReadField>
    </dl>}
    <dl>
      <ReadField label="结果去向">{report_to_controller ? "返回主控会话" : "仅保存在执行记录"}</ReadField>
    </dl>
  </section>;
}

function ReadField({ label, children }: Readonly<{ label: string; children: ReactNode }>) {
  return <div className={styles.field}>
    <dt>{label}</dt>
    <dd className={styles.read_value}>{children}</dd>
  </div>;
}
