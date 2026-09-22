import { useEffect, useState } from 'react';
import dayjs from 'dayjs';
import { Badge, Button, Card, DatePicker, Empty, Result, Select, Space, Table } from 'antd';
import type { TableProps } from 'antd';
import { useElementHeight } from '../../../components/useElementHeight';
import type { AuditView, LoadState } from '../../../model';
import { actionLabels } from '../../../model';
import type { AuditQuery } from '../../../request/api';
import styles from './index.module.scss';

type AuditPageProps = Readonly<{
  records: readonly AuditView[];
  preview: LoadState;
  error: string;
  total: number;
  query: AuditQuery;
  onQuery: (query: AuditQuery) => void;
  onRecover: () => void;
  onDetail: (record: AuditView) => void;
  findActors: (search: string, signal: AbortSignal) => Promise<{ value: number; label: string }[]>;
}>;

export function AuditPage(props: AuditPageProps) {
  const [draft, setDraft] = useState('');
  const [actors, setActors] = useState<{ value: number; label: string }[]>([]);
  const [actorError, setActorError] = useState(false);
  const action = props.query.action ?? 'all',
    result = props.query.success === undefined ? 'all' : String(props.query.success);
  const page = (props.query.offset ?? 0) / 20 + 1;
  const clear = () => {
    setDraft('');
    props.onQuery({ limit: 20, offset: 0 });
  };
  useEffect(() => {
    const request = new AbortController();
    const timer = setTimeout(() => {
      void props
        .findActors(draft, request.signal)
        .then((items) => {
          if (!request.signal.aborted) {
            setActors(items);
            setActorError(false);
          }
        })
        .catch(() => {
          if (!request.signal.aborted) {
            setActors([]);
            setActorError(true);
          }
        });
    }, 250);
    return () => {
      clearTimeout(timer);
      request.abort();
    };
  }, [draft, props.findActors]);
  // 列表区高度 = 卡片剩余空间；表格内部滚动，表头与分页保持固定。
  const { ref: listRef, height: listHeight } = useElementHeight<HTMLDivElement>();
  const columns: TableProps<AuditView>['columns'] = [
    { title: '发生时间', dataIndex: 'time', width: 190 },
    { title: '操作者用户名', dataIndex: 'actor', width: 140, ellipsis: true },
    { title: '操作', dataIndex: 'action', width: 170 },
    { title: '目标用户名', dataIndex: 'target', width: 155, ellipsis: true },
    {
      title: '结果',
      key: 'result',
      width: 100,
      render: (_, record) => (
        <Badge status={record.success ? 'success' : 'error'} text={record.success ? '成功' : '失败'} />
      ),
    },
    {
      title: '',
      key: 'detail',
      width: 84,
      fixed: 'right',
      align: 'center',
      render: (_, record) => (
        <Button type="link" size="small" onClick={() => props.onDetail(record)}>
          详情
        </Button>
      ),
    },
  ];

  return (
    <section className={styles.page}>
      <Card
        variant="borderless"
        className={styles.list_card}
        title={
          <>
            操作记录<span className={styles.count}>{props.total} 条</span>
          </>
        }
      >
        {/* 筛选行：label 与输入控件同行排列。 */}
        <div className={styles.filters}>
          <Select
            className={styles.search}
            showSearch
            filterOption={false}
            allowClear
            aria-label="审计操作者"
            placeholder="搜索并选择操作者"
            value={props.query.actor_user_id}
            onSearch={setDraft}
            options={actors}
            notFoundContent={actorError ? '查询失败，请重新输入重试' : '没有匹配用户'}
            onChange={(value) => props.onQuery({ ...props.query, actor_user_id: value, offset: 0 })}
          />
          <Space size={8}>
            <span className={styles.filter_label}>操作</span>
            <Select
              className={styles.select}
              aria-label="操作类型"
              value={action}
              onChange={(value) =>
                props.onQuery({ ...props.query, action: value === 'all' ? undefined : value, offset: 0 })
              }
              options={[
                { value: 'all', label: '全部' },
                ...Object.entries(actionLabels).map(([value, label]) => ({ value, label })),
              ]}
            />
          </Space>
          <Space size={8}>
            <span className={styles.filter_label}>结果</span>
            <Select
              className={styles.select_small}
              aria-label="操作结果"
              value={result}
              onChange={(value) =>
                props.onQuery({ ...props.query, success: value === 'all' ? undefined : value === 'true', offset: 0 })
              }
              options={[
                { value: 'all', label: '全部' },
                { value: 'true', label: '成功' },
                { value: 'false', label: '失败' },
              ]}
            />
          </Space>
          <DatePicker.RangePicker
            showTime
            aria-label="审计时间范围"
            placeholder={['开始时间（含）', '结束时间（不含）']}
            value={props.query.from && props.query.to ? [dayjs(props.query.from), dayjs(props.query.to)] : null}
            onChange={(values) =>
              props.onQuery({
                ...props.query,
                from: values?.[0]?.toISOString(),
                to: values?.[1]?.toISOString(),
                offset: 0,
              })
            }
          />
          <Button type="primary" onClick={props.onRecover}>
            查询
          </Button>
          {/* 操作按钮统一放在全部筛选条件之后。 */}
          <Button onClick={clear}>重置</Button>
        </div>
        {props.preview === 'failure' ? (
          <Result
            status="warning"
            title="暂时无法加载审计记录"
            subTitle={props.error}
            extra={<Button onClick={props.onRecover}>重试</Button>}
          />
        ) : (
          <div ref={listRef} className={styles.list_body}>
            <Table<AuditView>
              rowKey="id"
              columns={columns}
              dataSource={[...props.records]}
              loading={props.preview === 'loading'}
              scroll={{ x: 850, y: Math.max(listHeight - 100, 160) }}
              locale={{ emptyText: <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="没有符合条件的记录" /> }}
              pagination={{
                current: page,
                pageSize: 20,
                total: props.total,
                onChange: (value) => props.onQuery({ ...props.query, offset: (value - 1) * 20 }),
                showSizeChanger: false,
                showTotal: (total) => `共 ${total} 条`,
              }}
            />
          </div>
        )}
      </Card>
    </section>
  );
}
