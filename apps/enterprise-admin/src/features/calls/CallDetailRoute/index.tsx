import { callKindLabel } from '../callKindLabel';
import { useCallback } from 'react';
import { useNavigate, useParams, useSearchParams } from 'react-router';
import { Alert, Button, Card, Descriptions, Space } from 'antd';
import { observer } from 'mobx-react-lite';
import { callApi } from '../../../request/calls';
import { useResourceQuery } from '../../../request/useResourceQuery';
import { BodyReader } from '../BodyReader';
import {
  outcomeLabels,
  usageLabels,
  usageReasonLabels,
  observationLabels,
  evidenceReasonLabels,
  sourceLabels,
} from '../model';
import styles from './index.module.scss';

export const CallDetailRoute = observer(function CallDetailRoute() {
  const id = useParams().id!;
  const navigate = useNavigate();
  const [searchParams] = useSearchParams();
  const read = useCallback((token: string, signal: AbortSignal) => callApi.get(token, id, signal), [id]);
  const query = useResourceQuery(read);
  const row = query.value;

  return (
    <section className={styles.page}>
      <Card
        title={
          <div className={styles.header}>
            <span>调用详情</span>
            <Space wrap>
              <Button onClick={query.reload}>刷新</Button>
              <Button onClick={() => navigate(`/calls?${searchParams.toString()}`)}>返回列表</Button>
            </Space>
          </div>
        }
      >
        {query.error && <Alert type="error" title={query.error} />}
        {row && (
          <Descriptions
            size="small"
            column={{ xs: 1, sm: 1, md: 2 }}
            items={[
              { key: 'id', label: '调用 ID', children: row.id },
              { key: 'user', label: '调用用户', children: `${row.username ?? '—'} (${row.user_id ?? '—'})` },
              {
                key: 'model',
                label: '服务商 / 模型',
                children: `${row.provider_name ?? '—'} / ${row.model_id ?? '—'}`,
              },
              {
                key: 'kind',
                label: '类型 / 协议',
                children: `${callKindLabel[row.kind]} / ${row.protocol === 'open_ai_responses' ? 'Responses' : 'Chat Completions'}`,
              },
              { key: 'start', label: '开始时间', children: new Date(row.started_at).toLocaleString() },
              { key: 'end', label: '结束时间', children: row.ended_at ? new Date(row.ended_at).toLocaleString() : '—' },
              {
                key: 'result',
                label: '转发结果 / HTTP',
                children: `${outcomeLabels[row.outcome]} / ${row.http_status ?? '—'}`,
              },
              {
                key: 'observation',
                label: '响应观察',
                children: `${row.response_observation ? observationLabels[row.response_observation] : '历史未记录'}${row.response_observation_reason ? ` · ${evidenceReasonLabels[row.response_observation_reason] ?? row.response_observation_reason}` : ''}`,
              },
              {
                key: 'source',
                label: '响应来源',
                children: row.response_source ? sourceLabels[row.response_source] : '历史未记录',
              },
              {
                key: 'reason',
                label: '原因',
                children: row.reason ? (evidenceReasonLabels[row.reason] ?? row.reason) : '—',
              },
              {
                key: 'usage',
                label: 'Token 用量',
                children: `${row.input_tokens ?? '—'} 输入 / ${row.output_tokens ?? '—'} 输出 / ${row.total_tokens ?? '—'} 总量`,
              },
              {
                key: 'usage-state',
                label: '用量状态',
                children: `${usageLabels[row.usage_state]}${row.usage_reason ? ` · ${usageReasonLabels[row.usage_reason] ?? row.usage_reason}` : ''}`,
              },
              { key: 'cache', label: '缓存命中输入', children: row.cached_input_tokens ?? '—' },
              { key: 'reasoning', label: '推理输出', children: row.reasoning_tokens ?? '—' },
            ]}
          />
        )}
      </Card>
      <Alert type="info" title="转发完成仅表示 HTTP 2xx 正文完整转发，不代表模型业务或任务成功。" />
      {query.error && row && <Alert type="warning" title="刷新失败，当前为上次读取内容。" />}
      {row && <BodyReader key={id} row={row} />}
    </section>
  );
});
