import { Breadcrumb } from 'antd';
import { useMatches } from 'react-router';
import type { RouteHeading } from '../../routes';
import styles from './index.module.scss';

export function PageBreadcrumb() {
  const heading = useMatches().at(-1)?.handle as RouteHeading | undefined;
  return <Breadcrumb className={styles.breadcrumb} items={[{ title: '企业中心' }, { title: heading?.title }]} />;
}
