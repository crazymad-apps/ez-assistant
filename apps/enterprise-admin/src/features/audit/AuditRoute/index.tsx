import { observer } from 'mobx-react-lite';
import { useNavigate } from 'react-router';
import { AuditPage } from '../AuditPage';
import { useAuditData } from './useAuditData';

/** 管理审计路由页：数据请求由页面 hook 承担；详情跳转为路由导航。 */
export const AuditRoute = observer(function AuditRoute() {
  const data = useAuditData();
  const navigate = useNavigate();
  return <AuditPage {...data} onDetail={(record) => void navigate(`/audit/${record.id}`)} />;
});
