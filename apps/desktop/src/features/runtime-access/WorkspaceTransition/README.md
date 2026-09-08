# 首次进入工作台转场

素材来自用户委托的任务 `ez-assistant@v0.25.0 火箭动画`
（`01a07291-5257-70c0-836b-7970cce51c96`），使用其 `rocket-animation/v01/frames`
交付的 120 张 RGBA PNG。此处只转换网页素材并合成实际工作台，未重做火箭模型或动画。
素材及页面合成仍待用户审阅，不能将技术播放通过等同于视觉验收。

- 原始序列 1024 × 640、30 fps、4 秒，首尾完全透明。
- `assets/rocket-01.webp` 至 `rocket-10.webp` 每张含 4 × 3 帧，合计约 3.2 MiB。
  WebP 有损 RGB、无损 alpha，质量 90；不使用有背景的视频或依赖 VP9 alpha。
- 通过 `python3 scripts/prepare-rocket-assets.py <frames 目录>` 重新导出；仅导出需要 Pillow，
  普通前端和 Host 构建直接消费源图集。不可手改 `dist/` 或 Rust 嵌入产物。
- Desktop 首次连接或 Web 登录与快照已就绪后，才懒加载播放器和素材。16—48 帧入口上移淡出，
  68—100 帧显露已挂载的实际工作台；后续设置切换不重播。
- 只保留当前和下一张解码图集，像素缓存约 60 MiB；退出时关闭 ImageBitmap、取消 rAF、
  请求和监听。首次加载超过 1.2 秒、解码停顿超过 200 ms、媒体失败、减少动态或后台切换
  都直接结束转场，不延迟已成功的业务连接。
- Desktop 通过 `connect-src 'self'` 读取包内图集，保留原有 xterm 样式策略。
  Web 密码登录、token 快捷登录及 Cookie 恢复共用同一转场；退出或失效后重新登录会重播。

背景视觉依赖的 MIT 声明随 `licenses/entry-visual.txt` 一同内嵌交付。
