import { useState } from 'react';
import { Button } from '../../../../components/Button';
import styles from './index.module.scss';

export function PluginSettingField(
  props: Readonly<{
    label: string;
    is_set: boolean;
    secret?: boolean;
    busy: boolean;
    onSave: (value: string | null) => Promise<boolean>;
    onDirtyChange?: (dirty: boolean) => void;
  }>,
) {
  const [value, setValue] = useState('');

  async function save(next: string | null) {
    if (await props.onSave(next)) {
      setValue('');
      props.onDirtyChange?.(false);
    }
  }

  return (
    <div className={styles.setting_field}>
      <label>
        {props.label} <small>{props.is_set ? '已设置' : '未设置'}</small>
        <input
          aria-label={props.label}
          autoComplete="off"
          disabled={props.busy}
          onChange={(event) => {
            setValue(event.target.value);
            props.onDirtyChange?.(event.target.value.length > 0);
          }}
          placeholder={props.secret && props.is_set ? '留空保持原值' : '输入新值'}
          type={props.secret ? 'password' : 'text'}
          value={value}
        />
      </label>
      <Button disabled={props.busy || !value.trim()} onClick={() => void save(value)}>
        保存
      </Button>
      {props.is_set && (
        <Button disabled={props.busy} onClick={() => void save(null)}>
          清除
        </Button>
      )}
    </div>
  );
}
