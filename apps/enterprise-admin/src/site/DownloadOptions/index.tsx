import { useCallback, useState } from 'react';
import { Link } from 'react-router';
import { Alert, Button, Segmented, Spin, Typography } from 'antd';
import { DownloadOutlined, ArrowRightOutlined } from '@ant-design/icons';
import { useNativeDownload } from './useNativeDownload';
import { readDownloads, readContent, previewMode } from '../request';
import { useSiteQuery } from '../useSiteQuery';
import { platforms } from './platforms';
import { useDownloadPlatform } from './useDownloadPlatform';
import styles from './index.module.scss';

export function DownloadOptions({
  version,
  onNavigate,
}: {
  readonly version?: string;
  readonly onNavigate?: () => void;
}) {
  const device = useDownloadPlatform();
  const [manualPlatform, setPlatform] = useState<string>();
  // 异步识别仅提供默认项，不覆盖用户已经做出的选择。
  const platform = manualPlatform ?? device.target;
  const read = useCallback((signal: AbortSignal) => readDownloads(version, signal), [version]);
  const query = useSiteQuery(read);
  const selected = platforms.find((item) => item.id === platform);
  const release = query.value?.find((item) => item.target === platform);
  const native = useNativeDownload(release);
  const content = useSiteQuery(readContent);
  const manuals = content.value?.manual.flatMap((chapter) => chapter.articles);
  const installation = manuals?.find((article) => article.id === 'installation') ?? manuals?.[0];
  let downloadLabel = selected?.button ?? '选择安装平台';
  if (selected && !query.loading && !query.error && !release) downloadLabel = '暂无可用安装包';
  return (
    <div className={styles.body}>
      {!manualPlatform && (
        <p className={styles.detection} role="status">
          {device.hint}
        </p>
      )}
      <Segmented
        block
        className={styles.platforms}
        aria-label="安装平台与芯片类型"
        value={platform}
        onChange={setPlatform}
        options={platforms.map((item) => ({
          value: item.id,
          label: (
            <span className={styles.platform_content}>
              <span className={styles.platform_name}>
                <item.icon />
                <strong>{item.name}</strong>
              </span>
              <small>{item.chip}</small>
            </span>
          ),
        }))}
      />
      {selected ? (
        <div className={styles.package}>
          <div className={styles.package_heading}>
            <strong>
              {selected.name} · {selected.chip}
            </strong>
            <span>{selected.format} 安装包</span>
          </div>
          <p>{selected.hint}</p>
          {release && !query.loading && !query.error && (
            <p>
              v{release.version} ·{' '}
              {(release.size / (release.size < 1024 * 1024 ? 1024 : 1024 * 1024)).toLocaleString('zh-CN', {
                maximumFractionDigits: 1,
                minimumFractionDigits: 1,
              })}{' '}
              {release.size < 1024 * 1024 ? 'KB' : 'MB'} · {new Date(release.published_at).toLocaleDateString('zh-CN')}
            </p>
          )}
          {installation ? (
            <Link className={styles.help} to={`/manual/${installation.id}`} onClick={onNavigate}>
              查看安装说明 <ArrowRightOutlined />
            </Link>
          ) : (
            <p>安装说明暂不可用，可稍后到操作手册查看。</p>
          )}
        </div>
      ) : null}
      {query.loading && <Spin aria-label="正在查询安装包" />}
      {query.error && (
        <Alert
          type="warning"
          title="安装包查询失败"
          description={query.error}
          action={
            <Button size="small" onClick={query.reload}>
              重试
            </Button>
          }
        />
      )}
      {native.error && <Alert type="warning" title={native.error} />}
      <Button
        type="primary"
        size="large"
        block
        icon={<DownloadOutlined />}
        loading={native.checking}
        disabled={!release || query.loading || Boolean(query.error) || previewMode}
        onClick={() => void native.download()}
      >
        {downloadLabel}
      </Button>
      {release && !query.loading && !query.error && (
        <details className={styles.details}>
          <summary>文件详情</summary>
          <p>{release.name}</p>
          <Typography.Paragraph copyable={{ text: release.sha256 }}>SHA-256: {release.sha256}</Typography.Paragraph>
        </details>
      )}
      {(previewMode || native.notice) && (
        <div className={styles.notice} role="status" aria-live="polite">
          {previewMode ? '本地内容预览，下载已禁用。' : native.notice}
        </div>
      )}
      {version && (
        <Link to="/download" onClick={onNavigate}>
          查看各平台最新版本 <ArrowRightOutlined />
        </Link>
      )}
    </div>
  );
}
