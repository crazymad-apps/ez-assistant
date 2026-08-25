import { useEffect, useRef } from "react";
import type { KeyboardEvent as ReactKeyboardEvent } from "react";

/**
 * 统一保护输入法组合期间的键盘操作。
 *
 * WKWebView 等环境可能先发送 compositionend，再发送用于确认本次组合的 Enter keydown。
 * 因此除标准 isComposing 外，还要把组合结束后的当前键盘序列视为输入法事件。
 */
export function useInputMethodGuard() {
  const composing_ref = useRef(false);
  const composition_just_ended_ref = useRef(false);
  const release_frame_ref = useRef<number | null>(null);

  function cancelReleaseFrame() {
    if (release_frame_ref.current !== null) {
      cancelAnimationFrame(release_frame_ref.current);
      release_frame_ref.current = null;
    }
  }

  useEffect(() => () => cancelReleaseFrame(), []);

  function handleCompositionStart() {
    cancelReleaseFrame();
    composition_just_ended_ref.current = false;
    composing_ref.current = true;
  }

  function handleCompositionEnd() {
    composing_ref.current = false;
    composition_just_ended_ref.current = true;
    cancelReleaseFrame();
    release_frame_ref.current = requestAnimationFrame(() => {
      composition_just_ended_ref.current = false;
      release_frame_ref.current = null;
    });
  }

  function handleKeyUp(event: ReactKeyboardEvent<HTMLElement>) {
    if (event.key === "Enter") {
      composition_just_ended_ref.current = false;
      cancelReleaseFrame();
    }
  }

  function shouldIgnoreKeyDown(event: ReactKeyboardEvent<HTMLElement>) {
    return composing_ref.current
      || event.nativeEvent.isComposing
      // 部分 WebKit/第三方输入法只通过遗留的 229 标识组合键盘事件。
      || event.nativeEvent.keyCode === 229
      || composition_just_ended_ref.current;
  }

  return {
    onCompositionEnd: handleCompositionEnd,
    onCompositionStart: handleCompositionStart,
    onKeyUp: handleKeyUp,
    shouldIgnoreKeyDown,
  };
}
