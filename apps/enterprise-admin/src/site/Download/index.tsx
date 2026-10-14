import { useSearchParams } from 'react-router';
import { DownloadOptions } from '../DownloadOptions';
import styles from './index.module.scss';

export function Download() {
  const [query] = useSearchParams();
  const version = query.get('version') ?? undefined;
  return (
    <main id="main" tabIndex={-1} className={styles.page}>
      <h1>下载 EZ Assistant</h1>
      <p>{version ? `安装版本 v${version}` : '各平台自动提供最新可用版本。'}</p>
      <DownloadOptions version={version} />
    </main>
  );
}
