import { array, object, string, structured, emptyProjection, parseError, yieldReading, type Block } from './projection';

/** 多模态和加密字段只给类型提示；不解码、不请求远端资源，实际值留在原文。 */
export function messageText(value: unknown): string {
  if (typeof value === 'string') return value;
  return (
    array(value)
      .slice(0, 20_000)
      .map((entry) => {
        const part = object(entry),
          type = string(part.type);
        if (typeof part.text === 'string') return part.text;
        if (type.includes('image')) return '[图片引用，见原文；不自动加载]';
        if (type.includes('audio')) return '[音频引用，见原文；不自动加载]';
        if (type.includes('file')) return `[文件引用 ${string(part.filename) || string(part.file_id)}，见原文]`;
        if (type.includes('encrypted') || type === 'compaction') return '[加密或压缩内容，见原文]';
        return `[${type || '未识别内容'}，见原文]`;
      })
      .join('\n') + (array(value).length > 20_000 ? '\n[展示条目达到上限，剩余内容请查看原文]' : '')
  );
}

export async function requestProjection(text: string, signal: AbortSignal) {
  const result = emptyProjection();
  await yieldReading(signal);
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch (error) {
    result.notices.push(`请求正文无法完整解析。${parseError(error)}；请查看原文。`);
    return result;
  }
  const request = object(value);
  if (!Object.keys(request).length) {
    result.notices.push('请求不是可识别的对象，请查看原文。');
    return result;
  }
  const source = { start: 0, end: text.length, number: 1, type: '请求 JSON' };
  if (typeof request.instructions === 'string')
    result.blocks.push({ title: '系统提示词', text: request.instructions, sources: [source] });
  if (request.previous_response_id)
    result.notices.push(`本次含历史引用，仅展示实际发送的增量。引用：${string(request.previous_response_id)}`);
  const inputs =
    typeof request.input === 'string'
      ? [{ role: 'user', content: request.input }]
      : array(request.messages ?? request.input);
  const callIds = new Set<string>(),
    resultIds = new Set<string>();
  const associationLimited = inputs.length > 20_000;
  for (let i = 0; i < Math.min(inputs.length, 20_000); i++) {
    if (i % 100 === 0) await yieldReading(signal);
    const item = object(inputs[i]);
    if (item.type === 'function_call') callIds.add(string(item.call_id));
    if (item.type === 'function_call_output' || item.role === 'tool')
      resultIds.add(string(item.call_id ?? item.tool_call_id));
    for (const call of array(item.tool_calls).slice(0, 20_000)) callIds.add(string(object(call).id));
  }
  for (let i = 0; i < inputs.length && result.blocks.length < 20_000; i++) {
    if (i % 100 === 0) await yieldReading(signal);
    const item = object(inputs[i]),
      type = string(item.type),
      role = string(item.role);
    const block: Block = { title: role || type || '未识别请求项', sources: [source] };
    if (type === 'function_call') {
      const id = string(item.call_id);
      block.title = '工具调用请求';
      block.text = string(item.arguments);
      block.detail = `${string(item.name)} · call ID：${id || '未提供'}`;
      if (!resultIds.has(id))
        block.status = associationLimited ? '关联结果未在已解析范围内' : '本次请求中未包含关联结果';
    } else if (type === 'function_call_output' || role === 'tool') {
      const id = string(item.call_id ?? item.tool_call_id);
      block.title = '工具结果';
      block.text = typeof item.output === 'string' ? item.output : messageText(item.output ?? item.content);
      block.detail = `关联 ID：${id || '未提供'}`;
      if (!callIds.has(id)) block.status = associationLimited ? '关联调用未在已解析范围内' : '本次请求中未包含关联调用';
    } else if (type === 'compaction' || item.encrypted_content) {
      block.text = '加密或压缩内容，仅在原文中查看。';
    } else if (type === 'reasoning') {
      block.title = '已返回的推理摘要';
      block.text = messageText(item.summary);
    } else {
      block.text = messageText(item.content);
      if (!block.text && !array(item.tool_calls).length) block.detail = '此项无可直接阅读的文本，请查看原文。';
    }
    result.blocks.push(block);
    for (const tool of array(item.tool_calls).slice(0, 20_000 - result.blocks.length)) {
      const call = object(tool),
        fn = object(call.function),
        id = string(call.id);
      result.blocks.push({
        title: '工具调用请求',
        text: string(fn.arguments),
        detail: `${string(fn.name)} · call ID：${id || '未提供'}`,
        status: resultIds.has(id)
          ? undefined
          : associationLimited
            ? '关联结果未在已解析范围内'
            : '本次请求中未包含关联结果',
        sources: [source],
      });
    }
  }
  if (result.blocks.length >= 20_000) result.notices.push('展示解析已到条目上限，剩余内容可查看原文。');
  const { messages: _messages, input: _input, instructions: _instructions, ...parameters } = request;
  result.metadata = structured(parameters);
  if (!result.blocks.length) result.notices.push('本次请求未包含可展示的消息，请查看模型参数或原文。');
  return result;
}
