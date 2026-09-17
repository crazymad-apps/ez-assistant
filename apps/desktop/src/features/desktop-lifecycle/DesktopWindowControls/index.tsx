import { useEffect, useState } from "react";
import { Icon } from "../../../components/Icon";
import { Tooltip } from "../../../components/Tooltip";
import { getDesktopPlatform, isDesktopWindowMaximized, listenDesktopWindowMaximized, minimizeDesktopWindow, requestDesktopClose, toggleMaximizeDesktopWindow } from "../../../native-bridge/desktopLifecycle";
import styles from "./index.module.scss";

export function DesktopWindowControls() {
  const [visible, setVisible] = useState(false);
  const [maximized, setMaximized] = useState(false);
  useEffect(() => {
    let active = true;
    let unlisten = () => {};
    void getDesktopPlatform().then(async (platform) => {
      if (!active || (platform !== "linux" && platform !== "windows")) return;
      setVisible(true);
      const value = await isDesktopWindowMaximized();
      if (active) setMaximized(value);
      const dispose = await listenDesktopWindowMaximized(value => { if (active) setMaximized(value); });
      if (active) unlisten = dispose;
      else dispose();
    });
    return () => { active = false; unlisten(); };
  }, []);
  if (!visible) return null;
  return (
    <div className={styles.window_controls} aria-label="窗口控制">
      <Tooltip content="最小化">
        <button aria-label="最小化窗口" onClick={() => void minimizeDesktopWindow()} type="button">
          <span className={styles.minimize_icon} aria-hidden="true" />
        </button>
      </Tooltip>
      <Tooltip content={maximized ? "还原" : "最大化"}>
        <button
          aria-label={maximized ? "还原窗口" : "最大化窗口"}
          onClick={() => void toggleMaximizeDesktopWindow().then(setMaximized)}
          type="button"
        >
          {maximized ? <RestoreWindowGlyph /> : <span className={styles.maximize_icon} aria-hidden="true" />}
        </button>
      </Tooltip>
      <Tooltip content="关闭">
        <button
          aria-label="关闭窗口"
          className={styles.close_window}
          onClick={() => void requestDesktopClose()}
          type="button"
        >
          <Icon name="x" size={15} />
        </button>
      </Tooltip>
    </div>
  );
}

/** 标题栏还原态只绘制后窗外露边缘，避免完整后框透过前窗形成第三层内线。 */
function RestoreWindowGlyph() {
  return <svg
    aria-hidden="true"
    className={styles.restore_icon}
    data-window-glyph="restore"
    viewBox="0 0 12 12"
  >
    <path d="M3.5 2.5V1.5H10.5V8.5H9.5" />
    <rect height="7" width="7" x="1.5" y="3.5" />
  </svg>;
}
