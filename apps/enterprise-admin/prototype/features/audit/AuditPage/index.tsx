import { useState } from 'react';
import { Badge, Button, Card, Descriptions, Empty, Input, Result, Select, Space, Table } from 'antd';
import { ArrowLeftOutlined, SearchOutlined } from '@ant-design/icons';
import type { TableProps } from 'antd';
import { useElementHeight } from '../../../components/useElementHeight';
import type { DemoAudit, PreviewState } from '../../../model';
import styles from './index.module.scss';

type AuditPageProps = Readonly<{ records: readonly DemoAudit[]; preview: PreviewState; onRecover: () => void }>;

export function AuditPage(props: AuditPageProps) {
  const [draft, setDraft] = useState('');
  const [query, setQuery] = useState('');
  const [action, setAction] = useState('all');
  const [result, setResult] = useState('all');
  const [page, setPage] = useState(1);
  const [selected, setSelected] = useState<DemoAudit>();
  const clear = () => { setDraft(''); setQuery(''); setAction('all'); setResult('all'); setPage(1); };
  // 列表区高度 = 卡片剩余空间；表格内部滚动，表头与分页保持固定。
  const { ref: listRef, height: listHeight } = useElementHeight<HTMLDivElement>();
  const rows = props.records.filter(record => `${record.actor} ${record.target}`.toLowerCase().includes(query.toLowerCase())
    && (action === 'all' || record.action === action) && (result === 'all' || String(record.success) === result));
  const columns: TableProps<DemoAudit>['columns'] = [
    { title: '发生时间', dataIndex: 'time', width: 190 },
    { title: '操作者', dataIndex: 'actor', width: 140 },
    { title: '操作', dataIndex: 'action', width: 170 },
    { title: '目标账号', dataIndex: 'target', width: 155 },
    { title: '结果', key: 'result', width: 100, render: (_, record) => <Badge status={record.success ? 'success' : 'error'} text={record.success ? '成功' : '失败'} /> },
    { title: '', key: 'detail', width: 84, align: 'center', render: (_, record) => <Button type="link" size="small" onClick={() => setSelected(record)}>详情</Button> },
  ];

  if (selected) return <section className={styles.page}>
    <Card variant="borderless" className={styles.detail}      title={<Space size={8}><Button type="text" size="small" icon={<ArrowLeftOutlined />} aria-label="返回列表" onClick={() => setSelected(undefined)} />审计详情</Space>}
      extra={<Badge status={selected.success ? 'success' : 'error'} text={selected.success ? '操作成功' : '操作失败'} />}>
      <Descriptions column={1} items={[
        { key: 'action', label: '操作', children: selected.action },
        { key: 'time', label: '发生时间', children: `${selected.time}（演示时间）` },
        { key: 'actor', label: '操作者', children: selected.actor },
        { key: 'target', label: '目标账号', children: selected.target },
        { key: 'result', label: '结果', children: selected.success ? '成功' : '失败' },
        { key: 'id', label: '请求关联 ID', children: `req-${selected.id}` },
        { key: 'detail', label: '操作说明', children: selected.detail },
      ]} />
    </Card>
  </section>;

  return <section className={styles.page}>
    <Card variant="borderless" className={styles.list_card} title={<>操作记录<span className={styles.count}>{rows.length} 条</span></>}>
      {/* 筛选行：label 与输入控件同行排列。 */}
      <div className={styles.filters}>
        <Input className={styles.search} prefix={<SearchOutlined />} aria-label="检索审计" placeholder="搜索操作者或目标账号" value={draft} onChange={event => setDraft(event.target.value)} onPressEnter={() => { setQuery(draft.trim()); setPage(1); }} allowClear />
        <Space size={8}><span className={styles.filter_label}>操作</span><Select className={styles.select} aria-label="操作类型" value={action} onChange={value => { setAction(value); setPage(1); }} options={[
          { value: 'all', label: '全部' }, ...Array.from(new Set(props.records.map(record => record.action))).map(value => ({ value, label: value })),
        ]} /></Space>
        <Space size={8}><span className={styles.filter_label}>结果</span><Select className={styles.select_small} aria-label="操作结果" value={result} onChange={value => { setResult(value); setPage(1); }} options={[{ value: 'all', label: '全部' }, { value: 'true', label: '成功' }, { value: 'false', label: '失败' }]} /></Space>
        <Button type="primary" onClick={() => { setQuery(draft.trim()); setPage(1); }}>查询</Button>
        {/* 重置常驻展示，与查询成对出现。 */}
        <Button onClick={clear}>重置</Button>
      </div>
      {props.preview === 'failure' ? <Result status="warning" title="暂时无法加载审计记录" extra={<Button onClick={props.onRecover}>重试</Button>} />
        : <div ref={listRef} className={styles.list_body}>
          <Table<DemoAudit> rowKey="id" columns={columns} dataSource={props.preview === 'empty' ? [] : rows}
            scroll={{ x: 850, y: Math.max(listHeight - 100, 160) }}
            locale={{ emptyText: <Empty image={Empty.PRESENTED_IMAGE_SIMPLE} description="没有符合条件的记录" /> }}
            pagination={{ current: page, pageSize: 8, onChange: setPage, showSizeChanger: false, showTotal: total => `共 ${total} 条` }} />
        </div>}
    </Card>
  </section>;
}
