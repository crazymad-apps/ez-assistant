import { Breadcrumb } from 'antd';
import { Link, useMatches, useParams } from 'react-router';
import type { RouteHeading } from '../../routes';
import styles from './index.module.scss';

export function PageBreadcrumb() {
  const heading = useMatches().at(-1)?.handle as RouteHeading | undefined;
  const { id } = useParams();
  const items = [{ title: <>企业中心</> }];
  if (heading?.menu === '/models' && heading.title !== '模型管理') {
    items.push({ title: <Link to="/models">模型管理</Link> });
    if (id && heading.title !== '服务商详情')
      items.push({ title: <Link to={`/models/providers/${id}`}>服务商详情</Link> });
  }
  items.push({ title: <>{heading?.title}</> });
  return <Breadcrumb className={styles.breadcrumb} items={items} />;
}
