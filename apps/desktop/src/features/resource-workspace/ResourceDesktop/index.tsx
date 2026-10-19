import { useDesktopInput } from "./useDesktopInput";
import { DesktopTextDialog } from "../DesktopTextDialog";
import { useEffect, useRef, useState } from "react";
import { observer } from "mobx-react-lite";
import { SelectionPopover } from "../../../components/SelectionPopover";
import { Button } from "../../../components/Button";
import type { DesktopController } from "../DesktopController";
import styles from "./index.module.scss";

export const ResourceDesktop = observer(function ResourceDesktop({ controller, active }: Readonly<{ controller: DesktopController; active: boolean }>) {
  const root = useRef<HTMLDivElement>(null);
  const video = useRef<HTMLVideoElement>(null);
  const [displayOpen, setDisplayOpen] = useState(false);
  const [textOpen, setTextOpen] = useState(false);
  useDesktopInput(controller, video, active);
  const [original, setOriginal] = useState(false);
  const [fullscreenError, setFullscreenError] = useState<string | null>(null);
  useEffect(() => {
    const element = video.current;
    if (!element) return;
    element.srcObject = controller.stream;
    let frame: number | undefined;
    const decoded = () => { if (element.videoWidth > 0 && element.readyState >= HTMLMediaElement.HAVE_CURRENT_DATA) controller.decoded(); };
    if (controller.stream) {
      if (element.requestVideoFrameCallback) frame = element.requestVideoFrameCallback(decoded);
      else element.addEventListener("loadeddata", decoded, { once: true });
      void element.play().catch(() => { /* muted autoplay；loadeddata 仍验证解码事实。 */ });
    }
    return () => {
      if (frame !== undefined) element.cancelVideoFrameCallback(frame);
      element.removeEventListener("loadeddata", decoded);
      element.srcObject = null;
    };
  }, [controller, controller.stream]);
  useEffect(() => {
    if (!active && document.fullscreenElement === root.current) void document.exitFullscreen().catch(() => {});
  }, [active]);

  useEffect(() => { if (!active || controller.status !== "viewing") setTextOpen(false); }, [active, controller.status]);
  let controlLabel = "只读";
  if (controller.control === "requesting") controlLabel = "正在申请控制…";
  if (controller.control === "controlled") controlLabel = controller.input_focused ? "控制中 · Esc 停止" : "控制中 · 点击画面输入";

  return <div className={styles.desktop} ref={root}>
    <div className={styles.toolbar}>
      <span className={styles.name} title={controller.name}>{controller.name}</span>
      {controller.displays.length > 0 && <SelectionPopover aria_label="选择显示器" size="small" trigger_variant="compact"
        open={displayOpen} on_open_change={(open) => { if (open) controller.focusInput(false); setDisplayOpen(open); }}
        placeholder="原显示器不可用" selected={controller.displays.some((display) => display.id === controller.display_id) ? controller.display_id! : ""} on_select={(id) => controller.selectDisplay(id)}
        options={controller.displays.map((display, index) => ({ value: display.id, label: `显示器 ${index + 1}${display.primary ? " · 主屏" : ""}`, description: `${display.width} × ${display.height}` }))} />}
      <span className={styles.readonly}>{controlLabel}</span>
      {controller.can_control && <Button variant="text" size="small" disabled={controller.status !== "viewing"} onClick={() => {
        if (controller.control === "readonly") controller.startControl(); else controller.stopControl();
      }}>{controller.control === "readonly" ? "开启控制" : "停止控制"}</Button>}
      {controller.control === "controlled" && <Button variant="text" size="small" onClick={() => { controller.focusInput(false); setTextOpen(true); }}>输入文本</Button>}
      <Button variant="text" size="small" aria-pressed={original} onClick={() => setOriginal(!original)}>{original ? "适应窗口" : "原始大小"}</Button>
      <Button variant="text" size="small" disabled={controller.status !== "viewing"} onClick={() => {
        const request = document.fullscreenElement === root.current ? document.exitFullscreen() : root.current?.requestFullscreen();
        void request?.catch(() => setFullscreenError("当前窗口无法进入全屏。"));
      }}>全屏</Button>
      {controller.status === "disconnected" ? <Button variant="text" size="small" onClick={() => controller.connect()}>重新连接</Button>
        : <Button variant="text" size="small" onClick={() => controller.disconnect()}>{controller.status === "connecting" ? "取消" : "断开"}</Button>}
    </div>
    <div className={styles.viewport} data-original={original}>
      <video ref={video} tabIndex={controller.control === "controlled" ? 0 : -1} muted autoPlay playsInline aria-label={`${controller.name}的远程桌面`} hidden={!controller.stream} />
      {controller.status !== "viewing" && <div className={styles.notice} role={controller.error ? "alert" : "status"}>
        {controller.error ?? (controller.status === "connecting" ? "正在连接桌面…" : "桌面已断开")}
      </div>}
    </div>
    {controller.control_error && <div className={styles.controlNotice} role="status">{controller.control_error}</div>}
    {textOpen && <DesktopTextDialog controller={controller} on_close={() => setTextOpen(false)} />}
    {fullscreenError && <div role="alert">{fullscreenError}</div>}
  </div>;
});
