import { useEffect, useRef } from 'react';
import { theme } from 'antd';
import * as echarts from 'echarts/core';
import { LineChart } from 'echarts/charts';
import { DataZoomComponent, GridComponent, LegendComponent, TooltipComponent } from 'echarts/components';
import { CanvasRenderer } from 'echarts/renderers';
import type { UsageBucket } from '../../../request/openapi';
import styles from './index.module.scss';

echarts.use([LineChart, DataZoomComponent, GridComponent, LegendComponent, TooltipComponent, CanvasRenderer]);

export type TrendMetric = 'all' | 'total_tokens' | 'input_tokens' | 'cached_input_tokens' | 'output_tokens';
type TokenMetric = Exclude<TrendMetric, 'all'>;
type ChartMetric = TokenMetric | 'uncached_input';

const labels: Record<ChartMetric, string> = {
  total_tokens: '总 Token',
  input_tokens: '输入',
  uncached_input: '新增输入',
  cached_input_tokens: '缓存命中',
  output_tokens: '输出',
};
const sampleKeys: Record<TokenMetric, keyof UsageBucket> = {
  total_tokens: 'total_samples',
  input_tokens: 'input_samples',
  cached_input_tokens: 'cached_samples',
  output_tokens: 'output_samples',
};

const number = (value: string | null) => (value === null ? '—' : BigInt(value).toLocaleString('zh-CN'));
const compact = (value: number) =>
  new Intl.NumberFormat('zh-CN', { notation: 'compact', maximumFractionDigits: 1 }).format(value);

function timeLabel(value: string, granularity: string) {
  const options: Intl.DateTimeFormatOptions = { timeZone: 'Asia/Shanghai', month: '2-digit', day: '2-digit' };
  if (granularity === 'hour') options.hour = '2-digit';
  if (granularity === 'month') options.year = 'numeric';
  if (granularity === 'month') delete options.day;
  return new Date(value).toLocaleString('zh-CN', options);
}

function chartValue(bucket: UsageBucket, metric: ChartMetric): number | null {
  if (metric === 'uncached_input') {
    if (bucket.input_tokens === null || bucket.cached_input_tokens === null) return null;
    const value = BigInt(bucket.input_tokens) - BigInt(bucket.cached_input_tokens);
    return value < 0n ? null : Number(value);
  }
  return bucket[metric] === null ? null : Number(bucket[metric]);
}

export function UsageTrend(
  props: Readonly<{
    buckets: UsageBucket[];
    granularity: string;
    metric: TrendMetric;
    onSelect: (bucket: UsageBucket) => void;
  }>,
) {
  const { token } = theme.useToken();
  const container = useRef<HTMLDivElement>(null);
  const chart = useRef<echarts.EChartsType | null>(null);
  const onSelect = useRef(props.onSelect);
  const buckets = useRef(props.buckets);
  onSelect.current = props.onSelect;
  buckets.current = props.buckets;
  const metricKeys: TokenMetric[] = props.metric === 'all' ? ['cached_input_tokens', 'output_tokens'] : [props.metric];
  const hasKnownUsage = props.buckets.some((bucket) =>
    metricKeys.some((key) => bucket[key] !== null && bucket[sampleKeys[key]] !== '0'),
  );
  const noCalls = props.buckets.length > 0 && props.buckets.every((bucket) => bucket.calls === '0');
  const showChart = hasKnownUsage || noCalls;

  useEffect(() => {
    const element = container.current;
    if (!element) return;
    const instance = echarts.init(element, undefined, { renderer: 'canvas' });
    chart.current = instance;
    const resize = new ResizeObserver(() => instance.resize());
    resize.observe(element);
    instance.on('click', (event) => {
      if (typeof event.dataIndex === 'number') {
        const bucket = buckets.current[event.dataIndex];
        if (bucket) onSelect.current(bucket);
      }
    });
    return () => {
      resize.disconnect();
      instance.dispose();
      chart.current = null;
    };
  }, []);

  useEffect(() => {
    const instance = chart.current;
    if (!instance) return;
    const colors: Record<ChartMetric, string> = {
      total_tokens: token.colorPrimary,
      input_tokens: '#35a7c7',
      uncached_input: '#35a7c7',
      cached_input_tokens: '#e0aa29',
      output_tokens: token.colorSuccess,
    };
    const visible: ChartMetric[] =
      props.metric === 'all' ? ['uncached_input', 'output_tokens', 'cached_input_tokens'] : [props.metric];
    const labelsByTime = props.buckets.map((bucket) => timeLabel(bucket.start, props.granularity));
    const scrollable = props.buckets.length > 40;
    instance.setOption(
      {
        animationDuration: window.matchMedia('(prefers-reduced-motion: reduce)').matches ? 0 : 280,
        color: visible.map((key) => colors[key]),
        grid: { left: 12, right: 16, top: 24, bottom: scrollable ? 104 : 70, containLabel: true },
        legend: {
          bottom: 2,
          itemWidth: 15,
          itemHeight: 4,
          icon: 'roundRect',
          textStyle: { color: token.colorTextSecondary, fontSize: 12 },
        },
        tooltip: {
          trigger: 'axis',
          axisPointer: { type: 'line', lineStyle: { color: token.colorBorder } },
          backgroundColor: token.colorBgContainer,
          borderColor: token.colorBorderSecondary,
          textStyle: { color: token.colorText, fontSize: 12 },
          formatter: (params: unknown) => {
            const items = Array.isArray(params)
              ? (params as { dataIndex: number; marker: string; seriesName: string; value: number | null }[])
              : [];
            const index = items[0]?.dataIndex;
            const bucket = index === undefined ? undefined : props.buckets[index];
            if (index === undefined || !bucket) return '';
            const rows = items.map(
              (item) =>
                `${item.marker}${item.seriesName}　<strong>${typeof item.value === 'number' ? compact(item.value) : '—'}</strong>`,
            );
            const inputNote =
              props.metric === 'all'
                ? `<br><span style="color:${token.colorTextSecondary}">输入总量 ${number(bucket.input_tokens)}；缓存命中包含在输入中</span>`
                : '';
            return `<div style="min-width:160px"><strong>${labelsByTime[index]}</strong><br>${rows.join('<br>')}${inputNote}<br><span style="color:${token.colorTextSecondary}">完整用量 ${number(bucket.available)} / ${number(bucket.calls)} 次</span></div>`;
          },
        },
        xAxis: {
          type: 'category',
          boundaryGap: false,
          data: labelsByTime,
          axisLine: { lineStyle: { color: token.colorBorderSecondary } },
          axisTick: { show: false },
          axisLabel: { color: token.colorTextTertiary, hideOverlap: true, margin: 14 },
        },
        yAxis: {
          type: 'value',
          min: 0,
          axisLine: { show: false },
          axisLabel: { color: token.colorTextTertiary, formatter: (value: number) => compact(value) },
          splitLine: { lineStyle: { color: token.colorSplit, type: 'dashed' } },
        },
        dataZoom: scrollable
          ? [
              { type: 'inside', xAxisIndex: 0, filterMode: 'none' },
              {
                type: 'slider',
                xAxisIndex: 0,
                bottom: 32,
                height: 16,
                borderColor: token.colorBorderSecondary,
                fillerColor: token.colorFillSecondary,
                filterMode: 'none',
              },
            ]
          : [],
        series: visible.map((key) => ({
          name: labels[key],
          type: 'line',
          data: props.buckets.map((bucket) => chartValue(bucket, key)),
          stack: props.metric === 'all' && key !== 'total_tokens' ? 'token-components' : undefined,
          connectNulls: true,
          smooth: true,
          showSymbol: props.buckets.length <= 40 && (props.metric !== 'all' || key === 'total_tokens'),
          symbolSize: 7,
          lineStyle: { width: key === 'total_tokens' ? 3 : 2 },
          areaStyle: props.metric === 'all' && key === 'total_tokens' ? undefined : { opacity: 0.16 },
          z: key === 'total_tokens' ? 5 : 2,
          emphasis: { focus: 'series' },
        })),
      },
      { notMerge: true },
    );
  }, [props.buckets, props.granularity, props.metric, token]);

  return (
    <div className={styles.wrapper}>
      <div
        ref={container}
        className={styles.chart}
        style={{ opacity: showChart ? 1 : 0 }}
        role="img"
        aria-label="用量时间趋势，图下方的明细表提供精确数值"
      />
      {!showChart && (
        <div className={styles.empty}>
          <strong>当前范围暂无已知用量</strong>
          <span>无调用的时间段按 0 绘制；历史调用缺失的用量不计为 0</span>
        </div>
      )}
      {showChart && (
        <span className={styles.hint}>
          {noCalls ? '当前范围没有调用，Token 为 0' : '悬停查看分项，点击时间点查看调用；历史缺失用量仅连接趋势'}
        </span>
      )}
    </div>
  );
}
