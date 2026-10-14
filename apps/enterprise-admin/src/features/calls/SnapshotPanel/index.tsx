import { useCallback, useState } from 'react';
import { Alert, Button, Collapse, Descriptions, Space, Spin, Typography } from 'antd';
import { observer } from 'mobx-react-lite';
import { callApi } from '../../../request/calls';
import { useResourceQuery } from '../../../request/useResourceQuery';
import type { CallDetails, CallSnapshot } from '../../../request/openapi';
import { snapshotLabel, evidenceReasonLabels, sourceLabels } from '../model';
import { requestProjection } from '../BodyReader/request';
import { responseProjection } from '../BodyReader/response';
import { emptyProjection } from '../BodyReader/projection';
import { RawText } from '../RawText';
import { EvidenceBlocks } from '../EvidenceBlocks';
import type { createReadQueue } from '../BodyReader/readQueue';
import styles from './index.module.scss';

/** key 由父级快照 SHA/可用状态决定；原文、投影、分页随收起或失效一起卸载。 */
export const SnapshotPanel = observer(function SnapshotPanel(
  props: Readonly<{
    id: string;
    side: 'request' | 'response' | 'upstream_failure';
    snapshot: CallSnapshot;
    row: CallDetails;
    opened?: boolean;
    enqueue: ReturnType<typeof createReadQueue>;
  }>,
) {
  const [failureOpened, setFailureOpened] = useState(false);
  const isFailure = props.side === 'upstream_failure';
  const opened = isFailure ? failureOpened : props.opened;
  const canRead = ['complete', 'partial'].includes(props.snapshot.state);
  const { enqueue, id, side } = props;
  const contentType = props.row.response_content_type;
  const read = useCallback(
    async (token: string, signal: AbortSignal) => {
      if (!opened || !canRead) return null;
      return enqueue(async () => {
        const snapshot = await callApi.snapshot(token, id, side, signal);
        const projection =
          snapshot.text == null
            ? emptyProjection()
            : side === 'request'
              ? await requestProjection(snapshot.text, signal)
              : await responseProjection(
                  snapshot.text,
                  side === 'upstream_failure' ? 'application/json' : contentType,
                  signal,
                );
        return { snapshot, projection };
      }, signal);
    },
    [opened, canRead, enqueue, id, side, contentType],
  );
  const query = useResourceQuery(read);
  const snapshot = query.value?.snapshot ?? props.snapshot;
  const body = query.value?.snapshot.text ?? query.value?.snapshot.body_base64;
  return (
    <div className={styles.panel}>
      {isFailure && (
        <Space wrap>
          <Typography.Text strong>上游失败帧 · {snapshotLabel(snapshot)}</Typography.Text>
          <Button disabled={!canRead} onClick={() => setFailureOpened((value) => !value)}>
            {failureOpened ? '收起上游失败帧' : '查看上游失败帧'}
          </Button>
          <Typography.Text type="secondary">独立证据，不属于桥接响应事件流</Typography.Text>
        </Space>
      )}
      {query.value && snapshot.state !== props.snapshot.state && <Alert type="info" title={snapshotLabel(snapshot)} />}
      {snapshot.reason && (
        <Typography.Text type="secondary">{evidenceReasonLabels[snapshot.reason] ?? snapshot.reason}</Typography.Text>
      )}
      {snapshot.state === 'partial' && (
        <Alert
          type="warning"
          showIcon
          title={
            ['size_limit', 'memory_limit'].includes(snapshot.reason ?? '')
              ? '采集在此截断，后续未知；下方只展示已保存内容。'
              : '仅保存部分证据，不能据此判断上游完整输出。'
          }
        />
      )}
      {opened && query.loading && canRead && (
        <Spin tip="等待读取或解析正文">
          <div className={styles.loading} />
        </Spin>
      )}
      {query.error && (
        <Alert
          type="error"
          showIcon
          title={query.error}
          action={
            <Button size="small" onClick={query.reload}>
              重试读取
            </Button>
          }
        />
      )}
      {side === 'response' && props.row.reason && (
        <Alert
          type="warning"
          showIcon
          title={`转发诊断：${evidenceReasonLabels[props.row.reason] ?? props.row.reason}`}
        />
      )}
      {query.value && (
        <>
          {query.value.snapshot.body_base64 != null && (
            <Alert type="info" showIcon title="已保存字节证据，无法按文本解析；可在下方查看 Base64。" />
          )}
          {side !== 'request' && query.value.snapshot.text != null && (
            <Typography.Text type="secondary">阅读解析 · 基于已保存正文</Typography.Text>
          )}
          {query.value.snapshot.text != null && (
            <EvidenceBlocks
              projection={query.value.projection}
              text={query.value.snapshot.text}
              kind={side === 'request' ? 'request' : 'response'}
            />
          )}
          {query.value.projection.metadata && (
            <Collapse
              size="small"
              destroyOnHidden
              items={[
                {
                  key: 'parameters',
                  label: side === 'request' ? '模型参数与工具定义（结构化展示）' : '响应对象字段（结构化展示）',
                  children: <RawText text={query.value.projection.metadata} label="结构化字段" />,
                },
              ]}
            />
          )}
          {body != null && (
            <Collapse
              size="small"
              destroyOnHidden
              items={[
                {
                  key: 'raw',
                  label: query.value.snapshot.body_base64 != null ? '字节证据（Base64）' : '完整快照原文',
                  children: (
                    <RawText
                      text={body}
                      label={isFailure ? '上游失败帧原文' : side === 'request' ? '请求原文' : '响应原文'}
                    />
                  ),
                },
              ]}
            />
          )}
        </>
      )}
      <Collapse
        size="small"
        ghost
        items={[
          {
            key: 'snapshot',
            label: '快照信息',
            children: (
              <Descriptions
                size="small"
                column={1}
                items={[
                  { key: 'state', label: '保存状态', children: snapshotLabel(snapshot) },
                  { key: 'bytes', label: '字节数', children: snapshot.bytes ?? '—' },
                  {
                    key: 'sha',
                    label: 'SHA-256',
                    children: <span className={styles.digest}>{snapshot.sha256 ?? '—'}</span>,
                  },
                  {
                    key: 'source',
                    label: '来源',
                    children:
                      side === 'request'
                        ? 'Host 实际发送的请求'
                        : isFailure
                          ? '独立上游失败帧'
                          : props.row.response_source
                            ? sourceLabels[props.row.response_source]
                            : '历史未记录来源',
                  },
                  { key: 'type', label: '媒体类型', children: side === 'response' ? (contentType ?? '未记录') : '—' },
                  {
                    key: 'encoding',
                    label: '编码',
                    children: side === 'response' ? (props.row.response_content_encoding ?? '未记录') : '—',
                  },
                ]}
              />
            ),
          },
        ]}
      />
    </div>
  );
});
