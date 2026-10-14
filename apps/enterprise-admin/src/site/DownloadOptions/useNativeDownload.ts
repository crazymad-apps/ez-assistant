import { useEffect, useRef, useState } from 'react';
import type { SiteDownload } from '../../request/openapi';
import { readDownloads } from '../request';

/** 只复核小清单，实际文件交给浏览器；关闭/换平台中止尚未发起的下载意图。 */
export function useNativeDownload(release?: SiteDownload) {
  const request = useRef<AbortController | undefined>(undefined);
  const [status, setStatus] = useState({ checking: false, error: '', notice: '' });
  const identity = release ? `${release.target}/${release.version}/${release.sha256}` : '';
  useEffect(() => {
    setStatus({ checking: false, error: '', notice: '' });
    return () => request.current?.abort();
  }, [identity]);

  async function download() {
    if (!release) return;
    request.current?.abort();
    const owner = new AbortController();
    request.current = owner;
    setStatus({ checking: true, error: '', notice: '' });
    try {
      const rows = await readDownloads(release.version, owner.signal);
      if (owner.signal.aborted) return;
      const current = rows.find((row) => row.target === release.target && row.sha256 === release.sha256);
      if (!current) throw new Error('该安装包当前不可下载，请刷新列表后重试。');
      const link = document.createElement('a');
      link.href = `/api/desktop-updates/${encodeURIComponent(current.target)}/releases/${encodeURIComponent(current.version)}/files/installer`;
      link.download = current.name;
      link.rel = 'noopener noreferrer';
      document.body.append(link);
      link.click();
      link.remove();
      setStatus({ checking: false, error: '', notice: '已交给浏览器，请查看下载列表。若下载失败，请刷新后重试。' });
    } catch {
      if (!owner.signal.aborted)
        setStatus({ checking: false, error: '该安装包暂时无法下载，请刷新列表后重试。', notice: '' });
    }
  }
  return { ...status, download };
}
