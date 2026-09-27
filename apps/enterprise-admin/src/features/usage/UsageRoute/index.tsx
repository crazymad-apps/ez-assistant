import { useCallback, useEffect, useState } from 'react';
import { CalendarOutlined, FilterOutlined, ReloadOutlined } from '@ant-design/icons';
import { useNavigate } from 'react-router';
import { Alert, Button, Card, DatePicker, InputNumber, Pagination, Segmented, Select, Table } from 'antd';
import dayjs from 'dayjs';
import type { Dayjs } from 'dayjs';
import type { GetLlmUsageData, UsageBucket, UsageGroup, UsagePage, UsageTotals } from '../../../request/openapi';
import { callApi } from '../../../request/calls';
import { useResourceQuery } from '../../../request/useResourceQuery';
import adminStore from '../../../stores/AdminStore';
import { outcomeLabels } from '../../calls/model';
import { UsageTrend } from '../UsageTrend';
import type { TrendMetric } from '../UsageTrend';
import styles from './index.module.scss';

type Query = NonNullable<GetLlmUsageData['query']>;
type TokenMetric = Exclude<TrendMetric, 'all'>;
type Preset = 'today' | '7d' | '30d' | 'custom';
type UserMode = 'all' | 'me' | 'custom';

const initialQuery = (): Query => ({
  ...presetRange('today'),
  group_by: 'model',
  kind: 'proxy',
  limit: 20,
  offset: 0,
});
const number = (value: string | null) => (value === null ? '—' : BigInt(value).toLocaleString('zh-CN'));
const displayTime = (value: string) => new Date(value).toLocaleString('zh-CN', { timeZone: 'Asia/Shanghai' });
const known = (value: UsageTotals, metric: TokenMetric) =>
  `${number(value[metric])} · ${number(value[metric.replace('_tokens', '_samples') as keyof UsageTotals] as string)} / ${number(value.calls)} 次有数据`;

function coverage(available: string, calls: string) {
  if (calls === '0') return 0;
  return Number((BigInt(available) * 1000n) / BigInt(calls)) / 10;
}

function cacheHitRate(summary: UsageTotals): string {
  if (
    summary.input_tokens === null ||
    summary.cached_input_tokens === null ||
    summary.input_samples !== summary.cached_samples
  )
    return '—';
  const input = BigInt(summary.input_tokens);
  const cached = BigInt(summary.cached_input_tokens);
  if (input === 0n || cached > input) return '—';
  return `${Number((cached * 1000n + input / 2n) / input) / 10}%`;
}

function uncachedInput(summary: UsageTotals): string | null {
  if (
    summary.input_tokens === null ||
    summary.cached_input_tokens === null ||
    summary.input_samples !== summary.cached_samples
  )
    return null;
  const input = BigInt(summary.input_tokens);
  const cached = BigInt(summary.cached_input_tokens);
  return cached <= input ? (input - cached).toString() : null;
}

function presetRange(preset: Exclude<Preset, 'custom'>): Pick<Query, 'from' | 'to' | 'granularity'> {
  const now = dayjs();
  if (preset === 'today') {
    // 与中心固定的 Asia/Shanghai 小时桶使用同一天界，不依赖浏览器所在时区。
    const local = new Date(now.valueOf() + 8 * 3600000);
    local.setUTCHours(0, 0, 0, 0);
    return { from: new Date(local.getTime() - 8 * 3600000).toISOString(), to: now.toISOString(), granularity: 'hour' };
  }
  return {
    from: now.subtract(preset === '7d' ? 7 : 30, 'day').toISOString(),
    to: now.toISOString(),
    granularity: 'day',
  };
}

function bucketEnd(start: string, granularity: string): string {
  const date = new Date(start);
  if (granularity === 'hour') date.setUTCHours(date.getUTCHours() + 1);
  else if (granularity === 'day') date.setUTCDate(date.getUTCDate() + 1);
  else {
    date.setTime(date.getTime() + 8 * 3600000);
    date.setUTCMonth(date.getUTCMonth() + 1);
    date.setTime(date.getTime() - 8 * 3600000);
  }
  return date.toISOString();
}

export function UsageRoute() {
  const navigate = useNavigate();
  const [query, setQuery] = useState<Query>(initialQuery);
  const [search, setSearch] = useState('');
  const [preset, setPreset] = useState<Preset>('today');
  const [userMode, setUserMode] = useState<UserMode>('all');
  const [advanced, setAdvanced] = useState(false);
  const [showBuckets, setShowBuckets] = useState(false);
  const [metric, setMetric] = useState<TrendMetric>('all');
  const [lastResult, setLastResult] = useState<{ token: string; query: Query; data: UsagePage }>();
  const read = useCallback((token: string, signal: AbortSignal) => callApi.usage(token, query, signal), [query]);
  const modelsRead = useCallback(
    (token: string, signal: AbortSignal) => callApi.usageModels(token, search, signal),
    [search],
  );
  const result = useResourceQuery(read);
  const models = useResourceQuery(modelsRead);

  useEffect(() => {
    if (result.value) setLastResult({ token: adminStore.token, query, data: result.value });
  }, [result.value, query]);

  const previous = lastResult?.token === adminStore.token ? lastResult : undefined;
  const data = result.value ?? previous?.data;
  const newInput = data ? uncachedInput(data.summary) : null;
  const shownQuery = result.value ? query : (previous?.query ?? query);
  const currentUserId = adminStore.currentUser?.id;
  const range: [Dayjs, Dayjs] | undefined =
    query.from && query.to
      ? [dayjs(query.from), dayjs(query.to)]
      : data
        ? [dayjs(data.from), dayjs(data.to)]
        : undefined;

  function updateQuery(changes: Partial<Query>) {
    setQuery((current) => ({ ...current, ...changes, offset: 0 }));
  }

  function choosePreset(value: Exclude<Preset, 'custom'>) {
    setPreset(value);
    updateQuery(presetRange(value));
  }

  function refresh() {
    if (preset === 'custom') result.reload();
    else updateQuery(presetRange(preset));
  }

  function reset() {
    setQuery(initialQuery());
    setPreset('today');
    setUserMode('all');
    setSearch('');
  }

  function drill(userId: number | null, providerId: string | null, modelId: string | null, from?: string, to?: string) {
    const params = new URLSearchParams();
    const values: Record<string, string | number | undefined> = {
      user_id: userId ?? shownQuery.user_id,
      provider_instance_id: providerId ?? shownQuery.provider_instance_id,
      model_id: modelId ?? shownQuery.model_id,
      from: from ?? data?.from,
      to: to ?? data?.to,
      kind: shownQuery.kind,
      outcome: shownQuery.outcome,
    };
    for (const [key, value] of Object.entries(values)) if (value !== undefined) params.set(key, String(value));
    navigate(`/calls?${params.toString()}`);
  }

  function drillBucket(bucket: UsageBucket) {
    if (!data) return;
    drill(
      null,
      null,
      null,
      new Date(Math.max(Date.parse(bucket.start), Date.parse(data.from))).toISOString(),
      new Date(Math.min(Date.parse(bucketEnd(bucket.start, data.granularity)), Date.parse(data.to))).toISOString(),
    );
  }

  const groupColumns = [
    {
      title: shownQuery.group_by === 'user' ? '用户' : '服务商 / 模型',
      key: 'name',
      render: (_: unknown, row: UsageGroup) =>
        shownQuery.group_by === 'user'
          ? `${row.username ?? '未知用户'} (${row.user_id ?? '—'})`
          : `${row.provider_name ?? '未知服务商'} / ${row.model_id ?? '未知模型'}`,
    },
    {
      title: '已知总 Token',
      key: 'tokens',
      className: styles.numericCell,
      render: (_: unknown, row: UsageGroup) => number(row.total_tokens),
    },
    {
      title: '调用次数',
      key: 'calls',
      className: styles.numericCell,
      render: (_: unknown, row: UsageGroup) => number(row.calls),
    },
    {
      title: '完整用量',
      key: 'available',
      render: (_: unknown, row: UsageGroup) => `${number(row.available)} / ${number(row.calls)}`,
    },
    {
      title: '操作',
      key: 'action',
      align: 'center' as const,
      fixed: 'right' as const,
      render: (_: unknown, row: UsageGroup) => (
        <Button type="link" size="small" onClick={() => drill(row.user_id, row.provider_instance_id, row.model_id)}>
          查看调用
        </Button>
      ),
    },
  ];

  return (
    <section className={styles.page}>
      <Card className={styles.filterCard}>
        <div className={styles.filterRow}>
          <Segmented
            value={preset === 'custom' ? undefined : preset}
            options={[
              { label: '今天', value: 'today' },
              { label: '近 7 天', value: '7d' },
              { label: '近 30 天', value: '30d' },
            ]}
            onChange={(value) => choosePreset(value as Exclude<Preset, 'custom'>)}
          />
          <DatePicker.RangePicker
            className={styles.dateRange}
            allowClear={false}
            showTime={{ format: 'HH:mm' }}
            format="YYYY/MM/DD HH:mm"
            value={range}
            suffixIcon={<CalendarOutlined />}
            onChange={(dates) => {
              if (!dates?.[0] || !dates[1]) return;
              setPreset('custom');
              updateQuery({ from: dates[0].toISOString(), to: dates[1].toISOString() });
            }}
          />
          <Select
            className={styles.userSelect}
            value={userMode}
            options={[
              { label: '全部用户', value: 'all' },
              ...(currentUserId ? [{ label: '仅本人', value: 'me' }] : []),
              { label: '指定用户 ID', value: 'custom' },
            ]}
            onChange={(value: UserMode) => {
              setUserMode(value);
              updateQuery({ user_id: value === 'me' ? currentUserId : undefined });
            }}
          />
          {userMode === 'custom' && (
            <InputNumber
              className={styles.userId}
              min={1}
              precision={0}
              placeholder="用户 ID"
              value={query.user_id}
              onChange={(value) => updateQuery({ user_id: value ?? undefined })}
            />
          )}
          <Select
            showSearch
            allowClear
            className={styles.modelSelect}
            placeholder="全部服务商 / 模型"
            filterOption={false}
            onSearch={setSearch}
            loading={models.loading}
            value={
              query.provider_instance_id && query.model_id
                ? `${query.provider_instance_id}\n${query.model_id}`
                : undefined
            }
            onChange={(value?: string) => {
              const [provider_instance_id, model_id] = value?.split('\n') ?? [];
              updateQuery({ provider_instance_id, model_id });
            }}
            options={(models.value ?? []).map((item) => ({
              value: `${item.provider_instance_id}\n${item.model_id}`,
              label: `${item.provider_name} / ${item.model_id} (${item.provider_instance_id.slice(0, 8)})`,
            }))}
          />
        </div>
        <div className={styles.filterFooter}>
          <Button
            type="text"
            icon={<FilterOutlined />}
            onClick={() => setAdvanced((value) => !value)}
            aria-expanded={advanced}
          >
            {advanced ? '收起筛选' : '更多筛选'}
          </Button>
          <div className={styles.filterActions}>
            <Button icon={<ReloadOutlined />} onClick={refresh} loading={result.loading}>
              刷新数据
            </Button>
            <Button type="link" onClick={reset}>
              重置
            </Button>
          </div>
        </div>
        {advanced && (
          <div className={styles.advancedFilters}>
            <label>
              时间粒度
              <Select
                value={query.granularity}
                onChange={(granularity) => updateQuery({ granularity })}
                options={[
                  { value: 'hour', label: '小时' },
                  { value: 'day', label: '天' },
                  { value: 'month', label: '月' },
                ]}
              />
            </label>
            <label>
              调用类型
              <Select
                value={query.kind}
                onChange={(kind) => updateQuery({ kind })}
                options={[
                  { value: 'proxy', label: '业务调用' },
                  { value: 'admin_test', label: '配置测试' },
                ]}
              />
            </label>
            <label>
              转发结果
              <Select
                allowClear
                placeholder="全部结果"
                value={query.outcome}
                onChange={(outcome) => updateQuery({ outcome })}
                options={Object.entries(outcomeLabels).map(([value, label]) => ({ value, label }))}
              />
            </label>
          </div>
        )}
      </Card>

      {result.error && (
        <Alert type="error" title={result.error} action={<Button onClick={result.reload}>重试</Button>} />
      )}
      {data && shownQuery !== query && <Alert type="info" title="正在显示上一次查询结果；新筛选的结果尚未加载完成。" />}
      {models.error && (
        <Alert type="warning" title="模型选项读取失败" action={<Button onClick={models.reload}>重试</Button>} />
      )}

      {data && (
        <>
          <Card className={styles.overviewCard}>
            <div className={styles.overviewTop}>
              <div>
                <span className={styles.eyebrow}>已知总 Token</span>
                <div className={styles.mainNumber}>{number(data.summary.total_tokens)}</div>
                <div className={styles.rangeCaption}>
                  {displayTime(data.from)} 至 {displayTime(data.to)}
                </div>
              </div>
              <div className={styles.headlineSide}>
                <div>
                  <span>调用次数</span>
                  <strong>{number(data.summary.calls)}</strong>
                </div>
                <div>
                  <span>完整用量</span>
                  <strong>{coverage(data.summary.available, data.summary.calls)}%</strong>
                </div>
                <div>
                  <span title="缓存命中输入 Token ÷ 输入 Token；仅在两项覆盖的调用数一致时计算">缓存命中率</span>
                  <strong>{cacheHitRate(data.summary)}</strong>
                </div>
              </div>
            </div>
            <div className={styles.metricGrid}>
              <div className={`${styles.metric} ${styles.inputMetric}`}>
                <span>总输入 Token</span>
                <strong>{number(data.summary.input_tokens)}</strong>
              </div>
              <div className={`${styles.metric} ${styles.cachedMetric}`}>
                <span>缓存命中 Token</span>
                <strong>{number(data.summary.cached_input_tokens)}</strong>
                <small>{number(data.summary.cached_samples)} 次有数据</small>
              </div>
              <div className={`${styles.metric} ${styles.inputMetric}`}>
                <span title="总输入 Token 减缓存命中 Token；仅在两项覆盖的调用数一致时计算">新增输入 Token</span>
                <strong>{number(newInput)}</strong>
                <small>
                  {newInput === null ? '输入与缓存覆盖不一致' : `${number(data.summary.cached_samples)} 次有数据`}
                </small>
              </div>
              <div className={`${styles.metric} ${styles.outputMetric}`}>
                <span>总输出 Token</span>
                <strong>{number(data.summary.output_tokens)}</strong>
              </div>
            </div>
            <div className={styles.coverageRow}>
              <div className={styles.coverageTrack}>
                <span style={{ width: `${coverage(data.summary.available, data.summary.calls)}%` }} />
              </div>
              <span>
                完整 {number(data.summary.available)} · 部分 {number(data.summary.partial)} · 缺失{' '}
                {number(data.summary.unavailable)} · 待结算 {number(data.summary.pending)}
              </span>
            </div>
            <div className={styles.outcomeRow}>
              完成 {number(data.summary.forwarded)}　上游错误 {number(data.summary.upstream_error)}　准入拒绝{' '}
              {number(data.summary.rejected)}　中断 {number(data.summary.interrupted)}　结果未知{' '}
              {number(data.summary.unknown)}　进行中 {number(data.summary.in_progress)}
              {data.summary.avg_duration_ms !== null && `　平均耗时 ${number(data.summary.avg_duration_ms)} 毫秒`}
            </div>
          </Card>

          {new Date(data.from) < new Date(data.available_from) && (
            <Alert
              type="warning"
              title={`所选范围早于完整历史起点 ${displayTime(data.available_from)}；总量只包含已知数据。`}
            />
          )}

          <Card
            className={styles.chartCard}
            title="用量趋势"
            extra={
              <Segmented
                size="small"
                value={metric}
                onChange={(value) => setMetric(value as TrendMetric)}
                options={[
                  { label: '总览', value: 'all' },
                  { label: '总 Token', value: 'total_tokens' },
                  { label: '输入', value: 'input_tokens' },
                  { label: '缓存命中', value: 'cached_input_tokens' },
                  { label: '输出', value: 'output_tokens' },
                ]}
              />
            }
          >
            <UsageTrend buckets={data.buckets} granularity={data.granularity} metric={metric} onSelect={drillBucket} />
            <div className={styles.tableToggle}>
              <Button type="link" onClick={() => setShowBuckets((value) => !value)} aria-expanded={showBuckets}>
                {showBuckets ? '收起逐时数据' : '查看逐时数据'}
              </Button>
            </div>
            {showBuckets && (
              <Table
                size="small"
                rowKey="start"
                pagination={{ pageSize: 10 }}
                dataSource={data.buckets}
                columns={[
                  { title: '时间', dataIndex: 'start', render: displayTime },
                  {
                    title: '已知总 Token / 覆盖',
                    key: 'value',
                    render: (_: unknown, row: UsageBucket) => known(row, 'total_tokens'),
                  },
                  {
                    title: '状态',
                    key: 'state',
                    render: (_: unknown, row: UsageBucket) =>
                      row.incomplete
                        ? '时间范围不完整'
                        : row.partial !== '0' || row.unavailable !== '0' || row.pending !== '0'
                          ? '用量不完整'
                          : '完整',
                  },
                  {
                    title: '操作',
                    key: 'action',
                    render: (_: unknown, row: UsageBucket) => (
                      <Button type="link" size="small" onClick={() => drillBucket(row)}>
                        查看调用
                      </Button>
                    ),
                  },
                ]}
              />
            )}
          </Card>

          <Card
            className={styles.groupCard}
            title="用量分布"
            extra={
              <Select
                value={shownQuery.group_by}
                onChange={(group_by) => updateQuery({ group_by })}
                options={[
                  { value: 'model', label: '按服务商 / 模型' },
                  { value: 'user', label: '按用户' },
                ]}
              />
            }
          >
            <Table
              size="small"
              rowKey={(row) => `${row.user_id ?? ''}:${row.provider_instance_id ?? ''}:${row.model_id ?? ''}`}
              columns={groupColumns}
              dataSource={data.groups}
              pagination={false}
              scroll={{ x: 800 }}
            />
            <Pagination
              className={styles.pagination}
              current={Math.floor((shownQuery.offset ?? 0) / (shownQuery.limit ?? 20)) + 1}
              pageSize={shownQuery.limit ?? 20}
              total={data.group_total}
              showSizeChanger={false}
              onChange={(page) => setQuery({ ...shownQuery, offset: (page - 1) * (shownQuery.limit ?? 20) })}
            />
          </Card>
        </>
      )}
      {!data && !result.error && <Card loading />}
    </section>
  );
}
