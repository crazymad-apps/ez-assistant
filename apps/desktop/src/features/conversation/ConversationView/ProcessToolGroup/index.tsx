import { useId, useState, type ReactNode } from 'react';
import { Collapse } from '../../../../components/Collapse';
import { Icon } from '../../../../components/Icon';
import { humanizeToolName } from '../conversationRows';
import { toolPresentation } from '../toolPresentation';
import styles from './index.module.scss';

export function ProcessToolGroup(
  props: Readonly<{ tools: readonly { tool_name: string; is_mcp?: boolean }[]; children: ReactNode }>,
) {
  const [open, setOpen] = useState(false);
  const id = useId();
  const current = props.tools.at(-1);
  const presentation = current?.is_mcp ? { label: humanizeToolName(current.tool_name), icon: 'puzzle' as const } : current ? toolPresentation(current.tool_name) : { label: '工具调用', icon: 'puzzle' as const };
  return (
    <section className={styles.group}>
      <button
        className={styles.header}
        aria-label={`工具调用：${presentation.label}`}
        aria-expanded={open}
        aria-controls={id}
        onClick={() => setOpen(!open)}
        type="button"
      >
        <Icon name={presentation.icon} size={16} />
        <span className={styles.current}>{presentation.label}</span>
      </button>
      <Collapse id={id} open={open}>
        <div className={styles.content}>{props.children}</div>
      </Collapse>
    </section>
  );
}
