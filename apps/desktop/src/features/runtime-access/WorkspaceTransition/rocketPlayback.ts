const sheets = Object.entries(import.meta.glob("./assets/rocket-*.webp", {
  eager: true, query: "?url", import: "default",
})).sort(([left], [right]) => left.localeCompare(right)).map(([, url]) => url as string);

/** PNG 序列导出的透明图集，避免不同 WebView 对 VP9 alpha 的差异。只保留当前及下一张解码图。 */
export async function playRocket(canvas: HTMLCanvasElement, signal: AbortSignal, onFrame: (frame: number) => void): Promise<void> {
  const context = canvas.getContext("2d");
  if (!context || sheets.length !== 10) throw new Error("Launch animation unavailable");
  const bitmaps = new Map<number, ImageBitmap>();
  let finished = false;
  let animation = 0;
  let failPlayback: (error: unknown) => void = () => undefined;
  try {
    // 压缩素材一起取得，播放中不再争用 Host 连接；解码缓存不随序列总帧数增长。
    const blobs = await Promise.all(sheets.map(async (url) => {
      const response = await fetch(url, { signal });
      if (!response.ok) throw new Error("Launch animation unavailable");
      return response.blob();
    }));
    async function decode(index: number) {
      const bitmap = await createImageBitmap(blobs[index]);
      if (finished || signal.aborted) bitmap.close();
      else bitmaps.set(index, bitmap);
    }
    await Promise.all([decode(0), decode(1)]);
    signal.throwIfAborted();
    await new Promise<void>((resolve, reject) => {
      const started = performance.now();
      let previous = -1;
      let currentSheet = 0;
      let missingSince: number | null = null;
      const finish = (error?: unknown) => {
        finished = true;
        signal.removeEventListener("abort", cancel);
        cancelAnimationFrame(animation);
        if (error) reject(error); else resolve();
      };
      const cancel = () => finish(signal.reason);
      failPlayback = finish;
      signal.addEventListener("abort", cancel, { once: true });
      const draw = (now: number) => {
        const frame = Math.floor((now - started) * 30 / 1000);
        if (frame >= 120) { finish(); return; }
        const sheet = Math.floor(frame / 12);
        const bitmap = bitmaps.get(sheet);
        if (!bitmap) {
          missingSince ??= now;
          // 解码跟不上时立即进入已就绪的工作台，不在半透明层后阻塞用户。
          if (now - missingSince > 200) { finish(); return; }
        } else {
          missingSince = null;
          if (sheet !== currentSheet) {
            for (const [index, previousBitmap] of bitmaps) {
              if (index < sheet) { previousBitmap.close(); bitmaps.delete(index); }
            }
            currentSheet = sheet;
            if (sheet + 1 < blobs.length) void decode(sheet + 1).catch(failPlayback);
          }
          if (frame !== previous) {
            context.clearRect(0, 0, 1024, 640);
            context.drawImage(bitmap, (frame % 4) * 1024, Math.floor((frame % 12) / 4) * 640, 1024, 640, 0, 0, 1024, 640);
            onFrame(frame + 1);
            previous = frame;
          }
        }
        animation = requestAnimationFrame(draw);
      };
      animation = requestAnimationFrame(draw);
    });
  } finally {
    finished = true;
    cancelAnimationFrame(animation);
    for (const bitmap of bitmaps.values()) bitmap.close();
    bitmaps.clear();
    context.clearRect(0, 0, 1024, 640);
  }
}
