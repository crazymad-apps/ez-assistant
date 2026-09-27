import { useState } from 'react';
import { Alert, Button, Cascader, Input, Radio, Space, Tag, Typography } from 'antd';
import type { AgentCapabilityReference, AgentDependencyDiagnostic, PluginRecord } from '../../../request/openapi';
import { capabilityChoices, capabilityNameValid, dependencyLabel } from '../model';
import styles from './index.module.scss';

type Props = Readonly<{
  kind: 'skills' | 'mcp';
  value: AgentCapabilityReference[];
  onChange: (next: AgentCapabilityReference[]) => void;
  plugins: PluginRecord[];
  diagnostics: AgentDependencyDiagnostic[];
  disabled: boolean;
}>;

/** 来源只写入名称引用；候选来自中心当前包，已失效的旧引用仍保留在草稿中。 */
export function AgentCapabilities(props: Props) {
  const { kind, value, onChange, plugins, diagnostics, disabled } = props;
  const [open, setOpen] = useState(false);
  const [source, setSource] = useState<'plugin' | 'host'>('plugin');
  const [plugin, setPlugin] = useState<string>();
  const [name, setName] = useState('');
  const [error, setError] = useState('');
  const label = kind === 'skills' ? 'Skill' : 'MCP';
  const choices = capabilityChoices(plugins, plugin ?? '', kind);
  const pluginOptions = plugins
    .map((item) => ({
      value: item.name,
      label: `${item.display_name} (${item.name}) · ${item.publication === 'listed' ? '在架' : item.publication === 'deprecated' ? '已下架' : '未上架'}`,
      children: capabilityChoices(plugins, item.name, kind).map((capability) => ({
        value: capability,
        label: capability,
      })),
    }))
    .filter((item) => item.children.length > 0);

  function add() {
    const candidate = source === 'plugin' ? { name, plugin } : { name };
    if (!capabilityNameValid(kind, name) || (source === 'plugin' && (!plugin || !choices.includes(name)))) {
      setError(`请选择或输入有效的 ${label} 名称。`);
      return;
    }
    const previous = value.find((item) => item.name === name);
    if (previous) {
      setError(
        previous.plugin === candidate.plugin
          ? '该能力已添加。'
          : `已从「${previous.plugin ?? 'Host 已有能力'}」选择同名能力，请先移除后再选择其他来源。`,
      );
      return;
    }
    onChange([...value, candidate]);
    setName('');
    setError('');
    setOpen(false);
  }

  return (
    <div className={styles.group}>
      <Space orientation="vertical" className={styles.full}>
        <Typography.Text strong>默认启用的 {label}</Typography.Text>
        {value.length === 0 && <Typography.Text type="secondary">未设置</Typography.Text>}
        {value.map((item) => {
          const diagnostic = diagnostics.find(
            (entry) =>
              entry.kind === (kind === 'skills' ? 'skill' : 'mcp') &&
              entry.name === item.name &&
              entry.plugin === item.plugin,
          );
          return (
            <div className={styles.reference} key={item.name}>
              <Space wrap>
                <Tag>{item.name}</Tag>
                <Typography.Text type="secondary">{item.plugin ?? 'Host 已有能力'}</Typography.Text>
                <Button
                  size="small"
                  disabled={disabled}
                  onClick={() => onChange(value.filter((entry) => entry.name !== item.name))}
                >
                  移除
                </Button>
              </Space>
              {diagnostic && diagnostic.status !== 'available' && (
                <Typography.Text type="warning">{dependencyLabel(diagnostic)}</Typography.Text>
              )}
            </div>
          );
        })}
        {!open && (
          <Button disabled={disabled || value.length >= 256} onClick={() => setOpen(true)}>
            添加 {label}
          </Button>
        )}
        {open && (
          <div className={styles.selector}>
            <Radio.Group
              value={source}
              onChange={(event) => {
                setSource(event.target.value);
                setName('');
                setError('');
              }}
              options={[
                { value: 'plugin', label: '从插件选择' },
                { value: 'host', label: '引用 Host 已有能力' },
              ]}
            />
            {source === 'plugin' ? (
              <Cascader
                showSearch
                aria-label={`选择插件及 ${label}`}
                placeholder={`选择插件 / ${label}`}
                value={plugin && name ? [plugin, name] : undefined}
                onChange={(selected) => {
                  const [selectedPlugin, selectedName] = selected;
                  setPlugin(typeof selectedPlugin === 'string' ? selectedPlugin : undefined);
                  setName(typeof selectedName === 'string' ? selectedName : '');
                  setError('');
                }}
                options={pluginOptions}
                disabled={disabled}
              />
            ) : (
              <Input
                aria-label={`Host 已有 ${label} 名称`}
                placeholder={`输入 Host 已有 ${label} 名称`}
                value={name}
                onChange={(event) => setName(event.target.value)}
                disabled={disabled}
              />
            )}
            {error && <Alert type="error" title={error} />}
            <Space>
              <Button type="primary" disabled={disabled} onClick={add}>
                添加
              </Button>
              <Button
                onClick={() => {
                  setOpen(false);
                  setError('');
                }}
              >
                取消
              </Button>
            </Space>
          </div>
        )}
      </Space>
    </div>
  );
}
