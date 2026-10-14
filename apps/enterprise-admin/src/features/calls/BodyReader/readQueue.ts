/** 队列仅属于当前详情页，初次两侧顺序读取；失败和取消都释放后续读取，不跨页面缓存正文。 */
export function createReadQueue() {
  let tail: Promise<void> = Promise.resolve();
  return async <T>(read: () => Promise<T>, signal: AbortSignal): Promise<T> => {
    const task = tail
      .catch(() => undefined)
      .then(() => {
        signal.throwIfAborted();
        return read();
      });
    // tail 只保留完成信号，不能持有正文结果，否则收起后仍会滞留最后一个快照。
    tail = task.then(
      () => undefined,
      () => undefined,
    );
    return task;
  };
}
