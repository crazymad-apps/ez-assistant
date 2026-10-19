import { useCallback, useEffect, useState } from 'react';
import { Alert, Button, Input, Modal, Table, Tag } from 'antd';
import type { IdentityUser } from '../../../../request/openapi';
import { api } from '../../../../request/api';
import { useResourceQuery } from '../../../../request/useResourceQuery';

/** 候选按需读取；添加只修改父表单草稿，不单独保存授权。 */
export function AddUserDialog(props: {
  selected: number[];
  disabled: boolean;
  onAdd: (user: IdentityUser) => void;
  onClose: () => void;
}) {
  const [search, setSearch] = useState('');
  const [page, setPage] = useState(1);
  const read = useCallback(
    (token: string, signal: AbortSignal) => api.users(token, { search, limit: 20, offset: (page - 1) * 20 }, signal),
    [search, page],
  );
  const result = useResourceQuery(read);
  useEffect(() => {
    if (!result.value || result.loading || result.error) return;
    const lastPage = Math.max(1, Math.ceil(result.value.total / 20));
    if (page > lastPage) setPage(lastPage);
  }, [result.value, result.loading, result.error, page]);
  return (
    <Modal
      open
      title="添加用户"
      width={640}
      onCancel={props.onClose}
      footer={<Button onClick={props.onClose}>完成</Button>}
    >
      <Input.Search
        aria-label="搜索可用用户"
        placeholder="账号或显示名称"
        allowClear
        onSearch={(value) => {
          setSearch(value);
          setPage(1);
        }}
      />
      {result.error && (
        <Alert type="error" title={result.error} action={<Button onClick={result.reload}>重试</Button>} />
      )}
      <Table<IdentityUser>
        size="small"
        rowKey="id"
        dataSource={result.value?.items ?? []}
        loading={result.loading}
        scroll={{ y: 320, x: 460 }}
        pagination={{
          current: page,
          total: result.value?.total ?? 0,
          pageSize: 20,
          hideOnSinglePage: true,
          showSizeChanger: false,
          onChange: setPage,
        }}
        columns={[
          { title: '用户', dataIndex: 'display_name' },
          { title: '账号', dataIndex: 'username' },
          {
            title: '状态',
            width: 88,
            render: (_, user) => (
              <Tag color={user.enabled ? 'success' : 'default'}>{user.enabled ? '启用' : '已停用'}</Tag>
            ),
          },
          {
            title: '操作',
            width: 90,
            fixed: 'right',
            align: 'center',
            render: (_, user) => {
              const selected = props.selected.includes(user.id);
              return (
                <Button
                  size="small"
                  disabled={
                    props.disabled ||
                    result.loading ||
                    !!result.error ||
                    selected ||
                    !user.enabled ||
                    props.selected.length >= 100
                  }
                  onClick={() => props.onAdd(user)}
                >
                  {selected ? '已添加' : '添加'}
                </Button>
              );
            },
          },
        ]}
      />
    </Modal>
  );
}
