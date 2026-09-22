import { Card, Form, Input, InputNumber, Select, Space } from 'antd';
import type { ModelParameters, TokenLimit } from '../../../request/openapi';
import { effortKeys, supports, tokenFields } from '../model';
import styles from './index.module.scss';

type Props = {
  readonly value: ModelParameters;
  readonly sources: Record<string, string>;
  readonly disabled: boolean;
  readonly onChange: (parameters: ModelParameters) => void;
};
const sourceLabels: Record<string, string> = {
  template: '模板',
  online: '在线',
  fixed: '固定',
  unconfigured: '未配置',
};

/** 标准参数编辑器只提交草稿；未知、非法与明确不支持不合并为布尔值或零。 */
export function ParameterFields(props: Props) {
  const set = (key: keyof ModelParameters, value: unknown) => props.onChange({ ...props.value, [key]: value });

  const label = (name: string, field: string) => {
    const source = props.sources[field] ?? props.sources[field.split('.')[0]!];
    return source ? `${name} · ${sourceLabels[source] ?? source}` : name;
  };

  return (
    <div className={styles.sections}>
      <Card title="上下文与输出限制" size="small" type="inner">
        <div className={styles.fields}>
          {tokenFields.map(([field, name]) => (
            <Form.Item key={field} label={label(name, field)}>
              <Space.Compact block className={styles.token_limit}>
                <Select
                  className={props.value[field].state === 'known' ? styles.limit_state : styles.limit_value}
                  aria-label={`${name}状态`}
                  disabled={props.disabled}
                  value={props.value[field].state}
                  options={[
                    { value: 'unknown', label: '未知' },
                    { value: 'known', label: '已知' },
                    { value: 'invalid', label: '非法' },
                  ]}
                  onChange={(state) => set(field, state === 'known' ? { state, value: 1 } : { state })}
                />
                {props.value[field].state === 'known' && (
                  <InputNumber
                    className={styles.limit_value}
                    aria-label={name}
                    placeholder="Token 数量"
                    min={1}
                    max={Number.MAX_SAFE_INTEGER}
                    precision={0}
                    disabled={props.disabled}
                    value={props.value[field].value}
                    onChange={(value) => set(field, { state: 'known', value: value ?? 1 } satisfies TokenLimit)}
                  />
                )}
              </Space.Compact>
            </Form.Item>
          ))}
        </div>
      </Card>
      <Card title="模型能力" size="small" type="inner">
        <div className={styles.fields}>
          {(
            [
              ['streaming', '流式输出'],
              ['image_input', '图片输入'],
              ['tool_calls', '工具调用'],
              ['reasoning', '思考能力'],
            ] as const
          ).map(([field, name]) => (
            <Form.Item key={field} label={label(name, field)}>
              <Select
                aria-label={name}
                disabled={props.disabled}
                value={props.value[field]}
                options={supports}
                onChange={(value) => set(field, value)}
              />
            </Form.Item>
          ))}
          <Form.Item label={label('思考模式', 'reasoning_mode')}>
            <Select
              aria-label="思考模式"
              disabled={props.disabled}
              value={props.value.reasoning_mode}
              options={[
                { value: 'unknown', label: '未知' },
                { value: 'unsupported', label: '不支持' },
                { value: 'optional', label: '可关闭' },
                { value: 'always', label: '始终开启' },
              ]}
              onChange={(value) => set('reasoning_mode', value)}
            />
          </Form.Item>
        </div>
      </Card>
      <Card title="工具调用" size="small" type="inner">
        <div className={styles.fields}>
          {(['auto', 'none', 'required', 'named'] as const).map((key) => (
            <Form.Item key={key} label={label(`工具选择 ${key}`, `tool_choice.${key}`)}>
              <Select
                aria-label={`工具选择 ${key}`}
                disabled={props.disabled}
                value={props.value.tool_choice[key]}
                options={supports}
                onChange={(value) => set('tool_choice', { ...props.value.tool_choice, [key]: value })}
              />
            </Form.Item>
          ))}
          <Form.Item label={label('工具图片投影', 'tool_image_projection')}>
            <Select
              aria-label="工具图片投影"
              disabled={props.disabled}
              value={props.value.tool_image_projection}
              options={[
                { value: 'unknown', label: '未知' },
                { value: 'unsupported', label: '不支持' },
                { value: 'native_tool_result', label: '工具结果原生图片' },
                { value: 'follow_up_user_message', label: '后续用户图片消息' },
              ]}
              onChange={(value) => set('tool_image_projection', value)}
            />
          </Form.Item>
        </div>
      </Card>
      <Card title="思考强度" size="small" type="inner">
        <div className={styles.efforts}>
          <div className={styles.fields}>
            <Form.Item label={label('思考强度映射', 'reasoning_efforts')}>
              <Select
                aria-label="思考强度映射状态"
                value={props.value.reasoning_efforts === null ? 'unknown' : 'known'}
                disabled={props.disabled}
                options={[
                  { value: 'unknown', label: '未提供' },
                  { value: 'known', label: '明确指定（允许空集合）' },
                ]}
                onChange={(value) =>
                  props.onChange({
                    ...props.value,
                    reasoning_efforts: value === 'unknown' ? null : {},
                    default_reasoning_effort: null,
                  })
                }
              />
            </Form.Item>
          </div>
          {props.value.reasoning_efforts !== null && (
            <div className={styles.fields}>
              {effortKeys.map((key) => (
                <Form.Item key={key} label={key}>
                  <Input
                    aria-label={`${key} 线上值`}
                    placeholder="服务商线上值，留空不启用此档位"
                    value={props.value.reasoning_efforts?.[key] ?? ''}
                    disabled={props.disabled}
                    onChange={(event) => {
                      const map = { ...props.value.reasoning_efforts };
                      if (event.target.value) map[key] = event.target.value;
                      else delete map[key];
                      props.onChange({
                        ...props.value,
                        reasoning_efforts: map,
                        default_reasoning_effort:
                          props.value.default_reasoning_effort === key && !map[key]
                            ? null
                            : props.value.default_reasoning_effort,
                      });
                    }}
                  />
                </Form.Item>
              ))}
            </div>
          )}
          <div className={styles.fields}>
            <Form.Item label="默认思考档位">
              <Select
                aria-label="默认思考档位"
                placeholder="选择默认思考档位"
                allowClear
                value={props.value.default_reasoning_effort ?? undefined}
                disabled={props.disabled}
                options={Object.keys(props.value.reasoning_efforts ?? {}).map((key) => ({ value: key, label: key }))}
                onChange={(value) => set('default_reasoning_effort', value ?? null)}
              />
            </Form.Item>
          </div>
        </div>
      </Card>
    </div>
  );
}
