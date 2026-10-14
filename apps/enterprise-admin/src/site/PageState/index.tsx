import { Button, Result, Spin } from 'antd';
import { Link } from 'react-router';
import styles from './index.module.scss';

export function PageState({
  loading,
  error,
  title = '内容尚未发布',
  onRetry,
}: {
  readonly loading?: boolean;
  readonly error?: string;
  readonly title?: string;
  readonly onRetry?: () => void;
}) {
  return (
    <main id="main" tabIndex={-1} className={styles.page} aria-busy={loading}>
      {loading ? (
        <Spin tip="正在读取内容…">
          <div className={styles.loading} />
        </Spin>
      ) : (
        <Result
          status={error ? 'warning' : 'info'}
          title={error ? '暂时无法读取' : title}
          subTitle={error}
          extra={
            <>
              {onRetry && <Button onClick={onRetry}>重新读取</Button>} <Link to="/">返回首页</Link>
            </>
          }
        />
      )}
    </main>
  );
}
