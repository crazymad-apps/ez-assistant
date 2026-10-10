import type { IconName } from '../../../components/Icon';
import { humanizeToolName } from './conversationRows';

const tools: Record<string, { label: string; icon: IconName }> = {
  read_file: { label: '读取文件', icon: 'file' },
  write_file: { label: '写入文件', icon: 'edit' },
  edit_file: { label: '编辑文件', icon: 'edit' },
  delete_file: { label: '删除文件', icon: 'trash' },
  list_directory: { label: '查看目录', icon: 'folder' },
  find_files: { label: '查找文件', icon: 'search' },
  search_content: { label: '搜索内容', icon: 'search' },
  shell: { label: '运行命令', icon: 'terminal' },
  read_image: { label: '查看图片', icon: 'image' },
  inspect_images: { label: '分析图片', icon: 'eye' },
  load_skill: { label: '加载技能', icon: 'plugin' },
  discover_mcp_tools: { label: '查找外部工具', icon: 'puzzle' },
  request_user_input: { label: '询问用户', icon: 'message' },
  ask: { label: '询问用户', icon: 'message' },
  submit_feedback: { label: '提交反馈', icon: 'message' },
  delegate_task: { label: '委派子任务', icon: 'bot' },
  recall_memory: { label: '检索记忆', icon: 'search' },
  pin_memory: { label: '保存记忆', icon: 'pin' },
  update_pinned_memory: { label: '更新记忆', icon: 'edit' },
  unpin_memory: { label: '移除记忆', icon: 'pin-outline' },
};

/** 只映射工具动作，不从执行状态生成折叠标题；未知工具仍保留实际名称。 */
export function toolPresentation(name: string): { label: string; icon: IconName } {
  return tools[name] ?? { label: humanizeToolName(name), icon: 'puzzle' };
}
