import { useEffect, useState } from 'react';
import { observer } from 'mobx-react-lite';
import { useRootStore } from '../../../../stores/RootStoreContext';
import { executionElapsedMs } from '../../../../stores/liveExecutionProjection';
import type { LiveRunProjection } from '../../../../stores/LiveExecutionStore';
import { formatElapsed, isRunActive } from './presentation';

/** 只有计时文本订阅秒级更新；折叠正文不重挂载，终态停止定时器。 */
export const ElapsedTime = observer(function ElapsedTime(
  props: Readonly<{ run: Pick<LiveRunProjection, 'created_at_ms' | 'finished_at_ms' | 'status'> }>,
) {
  const store = useRootStore();
  const [now, setNow] = useState(() => performance.now());
  const active = isRunActive(props.run.status);
  useEffect(() => {
    if (!active) return;
    const timer = window.setInterval(() => setNow(performance.now()), 1000);
    return () => window.clearInterval(timer);
  }, [active]);
  return (
    <span aria-label="处理耗时">
      {formatElapsed(
        executionElapsedMs(props.run, store.live_execution.clock_anchor, Math.max(now, performance.now())),
      )}
    </span>
  );
});
