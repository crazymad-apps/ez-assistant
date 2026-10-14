import { useCallback } from 'react';
import { Alert, Button, Spin, Typography } from 'antd';
import { observer } from 'mobx-react-lite';
import { desktopReleaseApi } from '../../../request/desktop-releases';
import { useResourceQuery } from '../../../request/useResourceQuery';
import { SiteDocument } from '../../../site/SiteDocument';

/** 切换编辑／预览即取消旧读取，结果沿用 PagedList 的身份与代次守卫。 */
export const ReleaseNotesPreview = observer(function ReleaseNotesPreview({ notes }: { notes: string }) {
  const read = useCallback(
    (token: string, signal: AbortSignal) => desktopReleaseApi.previewNotes(token, notes, signal),
    [notes],
  );
  const query = useResourceQuery(read);
  if (query.loading) return <Spin />;
  if (query.error)
    return <Alert type="error" title={query.error} action={<Button onClick={query.reload}>重试</Button>} />;
  if (!query.value?.length) return <Typography.Text type="secondary">暂无更新日志</Typography.Text>;
  return <SiteDocument body={query.value} />;
});
