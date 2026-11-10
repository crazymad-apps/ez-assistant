import { describe, expect, it, vi } from 'vitest';
import { AppWindowTransport } from '../../src/features/mcp-apps/windowTransport';

describe('MCP App 实例消息边界', () => {
  it('只接受指定 Window 与实际来源，其他 Window 的 null 消息被忽略', async () => {
    const target = document.createElement('iframe');
    document.body.append(target);
    const retire = vi.fn();
    const receive = vi.fn();
    const transport = new AppWindowTransport(target.contentWindow!, 'null', retire);
    transport.onmessage = receive;
    await transport.start();
    const message = { jsonrpc: '2.0', method: 'ui/notifications/initialized' };
    window.dispatchEvent(new MessageEvent('message', { origin: 'null', source: window, data: message }));
    expect(receive).not.toHaveBeenCalled();
    window.dispatchEvent(new MessageEvent('message', { origin: 'null', source: target.contentWindow, data: message }));
    expect(receive).toHaveBeenCalledOnce();
    window.dispatchEvent(
      new MessageEvent('message', { origin: 'https://changed.example', source: target.contentWindow, data: message }),
    );
    expect(retire).toHaveBeenCalledOnce();
    await transport.close();
    target.remove();
  });

  it('关闭后迟到消息和重复初始化不能复用旧连接', async () => {
    const target = document.createElement('iframe');
    document.body.append(target);
    const retire = vi.fn();
    const receive = vi.fn();
    const transport = new AppWindowTransport(target.contentWindow!, 'null', retire);
    transport.onmessage = receive;
    await transport.start();
    const send = () =>
      window.dispatchEvent(
        new MessageEvent('message', {
          origin: 'null',
          source: target.contentWindow,
          data: { jsonrpc: '2.0', id: 1, method: 'ui/initialize', params: {} },
        }),
      );
    send();
    send();
    expect(receive).toHaveBeenCalledOnce();
    expect(retire).toHaveBeenCalledOnce();
    await transport.close();
    send();
    expect(receive).toHaveBeenCalledOnce();
    target.remove();
  });

  it('工具手势仅接受当前代理的私有标记，且只能消费一次', async () => {
    const target = document.createElement('iframe');
    document.body.append(target);
    const post = vi.spyOn(target.contentWindow!, 'postMessage');
    const transport = new AppWindowTransport(target.contentWindow!, 'null', vi.fn(), true);
    await transport.start();
    await transport.send({
      jsonrpc: '2.0',
      method: 'ui/notifications/sandbox-resource-ready',
      params: { html: '' },
    });
    const channel = post.mock.calls[0][0].params.ez_assistant_gesture_channel;
    expect(channel).toEqual(expect.any(String));
    const send = (id: number, source: Window, marker: string) =>
      window.dispatchEvent(new MessageEvent('message', {
        origin: 'null',
        source,
        data: { jsonrpc: '2.0', id, method: 'tools/call', params: { name: 'app_only' }, ez_assistant_gesture: marker },
      }));
    send(1, window, channel);
    send(2, target.contentWindow!, 'page-supplied');
    send(3, target.contentWindow!, channel);
    expect(transport.consumeToolGesture(1)).toBe(false);
    expect(transport.consumeToolGesture(2)).toBe(false);
    expect(transport.consumeToolGesture(3)).toBe(true);
    expect(transport.consumeToolGesture(3)).toBe(false);
    await transport.close();
    target.remove();
  });

  it('限制未答复请求和载荷容量，不记录私有正文', async () => {
    const target = document.createElement('iframe');
    document.body.append(target);
    const retire = vi.fn();
    const receive = vi.fn();
    const transport = new AppWindowTransport(target.contentWindow!, 'null', retire);
    transport.onmessage = receive;
    await transport.start();
    for (let id = 1; id <= 9; id++)
      window.dispatchEvent(
        new MessageEvent('message', {
          origin: 'null',
          source: target.contentWindow,
          data: { jsonrpc: '2.0', id, method: 'unsupported' },
        }),
      );
    expect(receive).toHaveBeenCalledTimes(8);
    expect(retire).toHaveBeenCalledOnce();
    await transport.close();
    target.remove();
  });
});
