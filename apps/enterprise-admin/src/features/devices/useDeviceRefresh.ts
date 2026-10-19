import { useEffect, useRef } from 'react';

/** 仅刷新页面投影；页面隐藏、写入或编辑期间暂停，同一读取未结束时不叠加请求。 */
export function useDeviceRefresh(reload: () => void, paused: boolean) {
  const latest = useRef({ reload, paused });
  latest.current = { reload, paused };
  useEffect(() => {
    const refresh = () => {
      if (!document.hidden && !latest.current.paused) latest.current.reload();
    };
    const timer = window.setInterval(refresh, 10000);
    document.addEventListener('visibilitychange', refresh);
    return () => {
      window.clearInterval(timer);
      document.removeEventListener('visibilitychange', refresh);
    };
  }, []);
}
