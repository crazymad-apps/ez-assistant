import { scanFrames } from './frames';
import { messageText } from './request';
import {
  array,
  object,
  string,
  index,
  structured,
  emptyProjection,
  parseError,
  yieldReading,
  type Block,
  type Source,
} from './projection';

const lifecycle: Record<string, string> = {
  'response.created': '响应已建立',
  'response.in_progress': '响应生成中',
  'response.output_item.added': '输出项建立',
  'response.output_item.done': '输出项结束',
  'response.content_part.added': '内容项建立',
  'response.content_part.done': '内容项结束',
  'response.completed': '响应结束',
  'response.failed': '模型响应失败',
  'response.incomplete': '模型未完整结束',
  'response.cancelled': '上游报告响应取消',
  error: '上游返回错误',
};
const terminals = new Set(['response.completed', 'response.failed', 'response.incomplete', 'response.cancelled']);

/** 只合并相邻增量；全局索引只服务完成快照校准，不移动交错的输出片段。 */
export async function responseProjection(text: string, contentType: string | null, signal: AbortSignal) {
  const result = emptyProjection();
  await yieldReading(signal);
  result.stream = contentType?.includes('text/event-stream') === true || /^(?:\uFEFF)?\s*(?:data:|event:|:)/.test(text);
  const streams = new Map<string, Block[]>();
  const conflicts = new Map<string, Block>();
  const items = new Map<string, string>();
  const names = new Map<string, string>();
  const toolIds = new Map<string, string>();
  const choices = new Set<string>(),
    finished = new Map<string, string>();
  let responseId = '',
    terminal = '',
    heartbeat = 0,
    boundary = true,
    limited = false,
    chat = false;
  const add = (block: Block) => {
    boundary = true;
    if (result.blocks.length < 20_000) result.blocks.push(block);
    else limited = true;
  };
  const node = (title: string, source: Source, warning?: string, value?: unknown) =>
    add({ title, sources: [source], warning, ...(value === undefined ? {} : { text: structured(value) }) });
  const delta = (key: string, title: string, value: string, source: Source, detail?: string) => {
    if (!key) {
      node(title, source, '归属字段缺失，未与其他增量合并', value);
      return;
    }
    if (terminal) {
      node(title, source, '结束信号后仍有输出', value);
      return;
    }
    const prior = streams.get(key) ?? [],
      last = result.blocks.at(-1);
    if (!boundary && last?.key === key && !last.status && !last.warning) {
      last.text = (last.text ?? '') + value;
      last.deltaCount = (last.deltaCount ?? 0) + 1;
      if (last.sources.at(-1) !== source) last.sources.push(source);
    } else {
      const block: Block = {
        key,
        title,
        text: value,
        detail,
        deltaCount: 1,
        continuation: prior.length > 0,
        sources: [source],
      };
      add(block);
      prior.push(block);
      streams.set(key, prior);
    }
    boundary = false;
  };
  const complete = (key: string, title: string, value: string, source: Source, detail?: string) => {
    boundary = true;
    if (!key) {
      node(title, source, '结束内容归属不明，保留独立内容', value);
      return;
    }
    const prior = streams.get(key) ?? [],
      accumulated = prior.map((block) => block.text ?? '').join('');
    if (!prior.length) {
      const block: Block = { key, title, text: value, detail, status: '结束事件内容', sources: [source] };
      add(block);
      streams.set(key, [block]);
      return;
    }
    const last = prior.at(-1)!;
    if (last.sources.at(-1) !== source) last.sources.push(source);
    if (value === accumulated) {
      for (const block of prior) block.status = '内容已结束';
      return;
    }
    if (value.startsWith(accumulated) && !prior.some((block) => block.conflict !== undefined)) {
      // 补足放在结束事件的位置，避免把 A→B→A 的末段塞到 B 之前。
      for (const block of prior) block.status = '内容已结束';
      const block: Block = {
        key,
        title,
        text: value.slice(accumulated.length),
        status: '由结束事件补全',
        continuation: true,
        sources: [source],
      };
      add(block);
      prior.push(block);
      return;
    }
    const previousConflict = conflicts.get(key);
    if (previousConflict?.text === accumulated && previousConflict.conflict === value) {
      if (previousConflict.sources.at(-1) !== source) previousConflict.sources.push(source);
      return;
    }
    node('结束内容冲突', source, '增量内容与结束事件内容不一致；两份证据均保留。');
    const conflict = result.blocks.at(-1)!;
    conflict.text = accumulated;
    conflict.conflict = value;
    conflicts.set(key, conflict);
    for (const block of prior) block.status = '结束内容冲突';
  };
  const outputKey = (value: Record<string, unknown>) => {
    const position = index(value.output_index),
      item = string(value.item_id);
    if (!position && !item) return '';
    const atIndex = position ? items.get(`index:${position}`) : undefined;
    const atItem = item ? items.get(`item:${item}`) : undefined;
    const knownItem = position ? items.get(`identity:${position}`) : undefined;
    // 同时出现的 index / ID 可以建立别名，但不能合并已经互相矛盾的两份归属。
    if ((knownItem && item && knownItem !== item) || (atIndex && atItem && atIndex !== atItem)) return '';
    const key = atIndex ?? atItem ?? (position ? `output:${position}` : `item:${item}`);
    if (position) items.set(`index:${position}`, key);
    if (item) items.set(`item:${item}`, key);
    if (position && item) items.set(`identity:${position}`, item);
    return key;
  };
  const responseContent = (output: unknown, source: Source) => {
    for (let i = 0; i < array(output).length && i < 20_000; i++) {
      const item = object(array(output)[i]);
      const key = outputKey({ output_index: i, item_id: item.id });
      if (item.type === 'function_call')
        complete(
          key && `${key}:tool`,
          '工具调用请求',
          string(item.arguments),
          source,
          `${string(item.name)} · call ID：${string(item.call_id) || '未提供'}`,
        );
      else {
        const content = array(item.type === 'reasoning' ? item.summary : item.content);
        for (let j = 0; j < content.length && j < 20_000; j++) {
          const part = object(content[j]),
            reasoning = item.type === 'reasoning';
          if (typeof (part.text ?? part.refusal) === 'string')
            complete(
              key && `${key}:${reasoning ? 'summary' : part.type === 'refusal' ? 'refusal' : 'text'}:${j}`,
              reasoning ? '推理摘要' : part.type === 'refusal' ? '拒绝内容' : '输出文本',
              string(part.text ?? part.refusal),
              source,
            );
          else node(string(part.type) || '输出内容', source, undefined, messageText([part]));
        }
        if (!content.length && item.type !== 'message')
          node(
            string(item.type) || '输出项',
            source,
            undefined,
            item.type === 'compaction' || item.encrypted_content ? '加密或压缩内容，见原文' : item,
          );
      }
    }
  };
  const consume = (source: Source, data: string | null, problem?: string) => {
    if (limited) return;
    if (problem) {
      node('事件无法完整解析', source, problem);
      return;
    }
    if (data === null) {
      heartbeat++;
      boundary = true;
      return;
    }
    if (data === '[DONE]') {
      const unclosed = [...choices].some((choice) => !finished.has(choice));
      node('流结束标记', source, unclosed ? '部分 choice 缺少 finish_reason' : undefined);
      terminal ||= 'done';
      return;
    }
    let value: unknown;
    try {
      value = JSON.parse(data);
    } catch (error) {
      node('事件格式损坏', source, `事件 data：${parseError(error)}`);
      return;
    }
    const event = object(value),
      type = string(event.type) || source.type;
    source.type = type;
    const incomingId =
      string(event.response_id) ||
      string(object(event.response).id) ||
      (Array.isArray(event.choices) ? string(event.id) : '');
    if (incomingId && responseId && incomingId !== responseId) {
      node('响应身份冲突', source, '响应 ID 与前文不同，未合并内容');
      return;
    }
    if (incomingId) responseId = incomingId;
    if (Array.isArray(event.choices)) {
      chat = true;
      for (const entry of event.choices.slice(0, 1024)) {
        const choice = object(entry),
          choiceIndex = index(choice.index),
          change = object(choice.delta ?? choice.message);
        if (!choiceIndex) {
          node('Choice 归属不明', source, '缺少有效 choice index');
          continue;
        }
        choices.add(choiceIndex);
        const key = `choice:${choiceIndex}`;
        if (finished.has(choiceIndex) && Object.keys(change).length) {
          node('结束后仍有内容', source, `Choice ${choiceIndex} 已结束`, change);
          continue;
        }
        const isFinal = choice.message !== undefined;
        for (const [field, title] of [
          ['content', '输出文本'],
          ['reasoning_content', '已返回的推理'],
          ['reasoning', '已返回的推理'],
        ] as const) {
          if (typeof change[field] === 'string')
            (isFinal ? complete : delta)(`${key}:${field}`, title, change[field], source, `Choice ${choiceIndex}`);
        }
        for (const raw of array(change.tool_calls).slice(0, 1024)) {
          const tool = object(raw),
            toolIndex = index(tool.index),
            fn = object(tool.function);
          const toolKey = toolIndex ? `${key}:tool:${toolIndex}` : '';
          const id = string(tool.id),
            name = string(fn.name);
          if (toolKey && id && toolIds.has(toolKey) && toolIds.get(toolKey) !== id) {
            node('工具身份冲突', source, '同一 tool index 出现不同 ID');
            continue;
          }
          if (toolKey && id) toolIds.set(toolKey, id);
          if (toolKey && name) names.set(toolKey, name);
          (isFinal ? complete : delta)(
            toolKey,
            '工具调用请求',
            string(fn.arguments),
            source,
            `${names.get(toolKey) || '工具名未提供'} · Choice ${choiceIndex} · call ID：${toolIds.get(toolKey) || '未提供'}`,
          );
        }
        if (change.function_call) {
          const fn = object(change.function_call),
            toolKey = `${key}:function`;
          if (fn.name) names.set(toolKey, string(fn.name));
          (isFinal ? complete : delta)(toolKey, '工具调用请求', string(fn.arguments), source, names.get(toolKey));
        }
        if (choice.finish_reason != null) {
          if (finished.has(choiceIndex) && finished.get(choiceIndex) !== string(choice.finish_reason))
            node('Choice 终态冲突', source, '同一 choice 的 finish_reason 不一致');
          finished.set(choiceIndex, string(choice.finish_reason));
          boundary = true;
          for (const [streamKey, blocks] of streams)
            if (streamKey.startsWith(`${key}:`))
              for (const block of blocks) {
                block.status = `生成结束：${string(choice.finish_reason)}`;
                if (block.sources.at(-1) !== source) block.sources.push(source);
              }
          node(
            `Choice ${choiceIndex} 生成结束`,
            source,
            ['length', 'content_filter'].includes(string(choice.finish_reason))
              ? `输出受限：${string(choice.finish_reason)}`
              : undefined,
          );
        }
      }
      if (event.choices.length > 1024) node('Choice 解析受限', source, '展示解析达到单事件条目上限');
      if (!event.choices.length && event.usage) node('用量信息', source, undefined, event.usage);
      return;
    }
    if (type === 'response.output_item.added') {
      const item = object(event.item),
        key = outputKey({ output_index: event.output_index, item_id: item.id });
      if (key) names.set(key, `${string(item.name)}${item.call_id ? ` · call ID：${string(item.call_id)}` : ''}`);
    }
    if (type === 'response.content_part.done' || type === 'response.reasoning_summary_part.done') {
      const part = object(event.part),
        base = outputKey(event),
        summary = type.includes('summary');
      const position = index(summary ? event.summary_index : event.content_index);
      if (typeof (part.text ?? part.refusal) === 'string') {
        complete(
          base && position
            ? `${base}:${summary ? 'summary' : part.type === 'refusal' ? 'refusal' : 'text'}:${position}`
            : '',
          summary ? '推理摘要' : part.type === 'refusal' ? '拒绝内容' : '输出文本',
          string(part.text ?? part.refusal),
          source,
        );
      } else node('内容项结束', source);
      return;
    }
    const isReasoning = type.includes('reasoning'),
      isTool = type.includes('function_call_arguments');
    const isText = type.includes('output_text') || type.includes('refusal');
    if ((isReasoning || isTool || isText) && (type.endsWith('.delta') || type.endsWith('.done'))) {
      const base = outputKey(event),
        part = index(isReasoning ? (event.summary_index ?? event.content_index) : event.content_index);
      const category = isReasoning
        ? type.includes('summary')
          ? 'summary'
          : 'reasoning'
        : type.includes('refusal')
          ? 'refusal'
          : 'text';
      const key = base && (isTool ? `${base}:tool` : part ? `${base}:${category}:${part}` : '');
      const title = isTool
        ? '工具调用请求'
        : isReasoning
          ? '推理摘要'
          : type.includes('refusal')
            ? '拒绝内容'
            : '输出文本';
      const content = type.endsWith('.delta') ? event.delta : (event.text ?? event.arguments ?? event.refusal);
      if (typeof content !== 'string') {
        node(title, source, '文本字段格式异常');
        return;
      }
      (type.endsWith('.delta') ? delta : complete)(key, title, content, source, base ? names.get(base) : undefined);
      return;
    }
    if (type === 'response.output_item.done') {
      const item = object(event.item),
        position = index(event.output_index),
        key = outputKey({ output_index: event.output_index, item_id: item.id });
      if (item.type === 'function_call')
        complete(
          key && `${key}:tool`,
          '工具调用请求',
          string(item.arguments),
          source,
          `${string(item.name)} · call ID：${string(item.call_id)}`,
        );
      else if (position) {
        const content = array(item.type === 'reasoning' ? item.summary : item.content);
        content.slice(0, 20_000).forEach((part, j) => {
          if (typeof (object(part).text ?? object(part).refusal) === 'string')
            complete(
              key &&
                `${key}:${item.type === 'reasoning' ? 'summary' : object(part).type === 'refusal' ? 'refusal' : 'text'}:${j}`,
              item.type === 'reasoning' ? '推理摘要' : '输出文本',
              string(object(part).text ?? object(part).refusal),
              source,
            );
        });
      }
      node(lifecycle[type]!, source);
      return;
    }
    if (terminals.has(type)) {
      const response = object(event.response);
      if (terminal && terminal !== type) node('响应终态冲突', source, '出现不一致的结束事件');
      if (
        !string(response.id) ||
        !Array.isArray(response.output) ||
        response.status !== type.slice('response.'.length)
      ) {
        node('终态结构未确认', source, '结束事件缺少或冲突的响应身份、状态或输出字段');
      }
      responseContent(response.output, source);
      terminal = type;
      node(
        lifecycle[type]!,
        source,
        type === 'response.completed'
          ? undefined
          : string(object(response.error).message) ||
              string(object(response.incomplete_details).reason) ||
              lifecycle[type],
        response.error,
      );
      return;
    }
    if (type === 'error' || event.error) {
      node('上游返回错误', source, string(object(event.error).message) || '存在错误对象', event.error ?? event);
      return;
    }
    node(lifecycle[type] || type || '未识别事件', source);
  };
  if (result.stream) {
    const scan = await scanFrames(text, signal, consume);
    result.frames = scan.count;
    if (scan.limited || limited) result.notices.push('展示解析已到上限；后续内容请查看原文，不能据此判断响应结束。');
    if (heartbeat) result.notices.push(`来源中包含 ${heartbeat} 个注释或无数据帧，已收拢。`);
    if (!terminal) result.notices.push('阅读解析：未找到完整结束信号；这不等同于已确认上游断流。');
    if (chat && [...choices].some((choice) => !finished.has(choice)))
      result.notices.push('阅读解析：部分 choice 未完整结束。');
    for (const blocks of streams.values()) for (const block of blocks) if (!block.status) block.status = '未确认完成';
  } else {
    const source = { start: 0, end: text.length, number: 1, type: 'JSON' };
    if (!text.length) result.notices.push('正文为零字节；没有可解析的响应对象。');
    else {
      let value: unknown;
      try {
        value = JSON.parse(text);
      } catch (error) {
        result.notices.push(`响应正文无法完整解析。${parseError(error)}；请查看原文。`);
        return result;
      }
      const response = object(value);
      if (response.error)
        node('错误对象', source, string(object(response.error).message) || '上游返回错误', response.error);
      else if (response.input_tokens !== undefined) node('输入计量对象', source, undefined, value);
      else {
        node(
          response.object === 'response.compaction' ? '压缩响应对象' : '响应对象',
          source,
          ['failed', 'incomplete', 'cancelled'].includes(string(response.status))
            ? `响应状态：${string(response.status)}`
            : undefined,
        );
        if (Array.isArray(response.output)) responseContent(response.output, source);
        else if (Array.isArray(response.choices)) {
          for (const entry of response.choices.slice(0, 1024)) {
            const choice = object(entry),
              message = object(choice.message);
            node(
              `Choice ${index(choice.index) || '未提供'} · ${string(message.role) || '输出'}`,
              source,
              ['length', 'content_filter'].includes(string(choice.finish_reason))
                ? `输出受限：${string(choice.finish_reason)}`
                : undefined,
              message,
            );
          }
        } else result.metadata = structured(value);
        for (const block of result.blocks) if (block.status === '结束事件内容') block.status = '响应对象内容';
      }
    }
  }
  signal.throwIfAborted();
  return result;
}
