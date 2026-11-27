import gsap from "gsap";
import { Flip } from "gsap/Flip";
import { useGSAP } from "@gsap/react";
import { useLayoutEffect, useRef, type RefObject } from "react";

// 在应用级模块顶层集中注册一次插件，避免在组件渲染周期内重复注册。
gsap.registerPlugin(useGSAP, Flip);

export { gsap, Flip, useGSAP };

/** 与 styles/tokens.scss 的 --ez-motion-* 时长基线对齐，保持克制且无过冲。 */
export const MOTION_DURATION = {
  fast: 0.14,
  medium: 0.2,
  entrance: 0.26,
  ring: 0.36,
} as const;

export const MOTION_EASE = {
  enter: "power2.out",
  emphasis: "power3.out",
  exit: "power2.in",
} as const;

/**
 * 检查是否开启减少动态效果。
 * 在无 matchMedia 的 jsdom 测试环境下保守返回 true，避免阻塞同步交互与焦点断言。
 */
export function prefersReducedMotion(): boolean {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return true;
  }
  return window.matchMedia("(prefers-reduced-motion: reduce)").matches;
}

/**
 * 通过 gsap.matchMedia() 响应式订阅 prefers-reduced-motion，卸载时自动 revert。
 * 无 matchMedia 时直接按降级模式执行，保证单元测试与无障碍路径行为一致。
 */
export function runMotion(
  setup: (reduced: boolean) => void | (() => void),
  scope?: Element | RefObject<Element | null> | null,
): (() => void) | void {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return setup(true);
  }
  const mm = gsap.matchMedia();
  mm.add(
    {
      reduce_motion: "(prefers-reduced-motion: reduce)",
      standard_motion: "(prefers-reduced-motion: no-preference)",
    },
    (context) => {
      const reduced = Boolean(context.conditions?.reduce_motion);
      return setup(reduced);
    },
    scope ?? undefined,
  );
  return () => mm.revert();
}

type FlipIndicatorOptions = Readonly<{
  active_key: string;
  active_selector: string;
  container_ref: RefObject<HTMLElement | null>;
  indicator_ref: RefObject<HTMLElement | null>;
}>;

/**
 * 为分段选择器提供基于 GSAP Flip 的共享高亮滑块。
 * 首次定位与窗口缩放直接贴合目标按钮不补播入场；仅在 active_key 变更时播放平滑滑动，
 * 并在无真实布局几何（offsetWidth === 0，如 jsdom）时保持 CSS 静态回退样式。
 */
export function useFlipIndicator(options: FlipIndicatorOptions): void {
  const { active_key, active_selector, container_ref, indicator_ref } = options;
  const previous_key_ref = useRef<string | null>(null);
  const measured_ref = useRef(false);

  useLayoutEffect(() => {
    const container = container_ref.current;
    const indicator = indicator_ref.current;
    if (!container || !indicator) return;

    function applyBox(target: HTMLElement): boolean {
      const width = target.offsetWidth;
      const height = target.offsetHeight;
      if (width <= 0 || height <= 0) return false;
      indicator!.style.left = `${target.offsetLeft}px`;
      indicator!.style.top = `${target.offsetTop}px`;
      indicator!.style.width = `${width}px`;
      indicator!.style.height = `${height}px`;
      indicator!.style.opacity = "1";
      container!.dataset.indicatorReady = "true";
      return true;
    }

    const active = container.querySelector<HTMLElement>(active_selector);
    if (!active) {
      indicator.style.opacity = "0";
      delete container.dataset.indicatorReady;
      previous_key_ref.current = active_key;
      measured_ref.current = false;
      return;
    }

    const should_animate = measured_ref.current
      && previous_key_ref.current !== null
      && previous_key_ref.current !== active_key
      && !prefersReducedMotion();

    let flip_tween: gsap.core.Timeline | null = null;
    if (should_animate) {
      const state = Flip.getState(indicator);
      if (applyBox(active)) {
        flip_tween = Flip.from(state, {
          duration: MOTION_DURATION.medium,
          ease: MOTION_EASE.enter,
          overwrite: true,
          simple: true,
        });
      }
    } else {
      measured_ref.current = applyBox(active);
    }
    previous_key_ref.current = active_key;

    if (typeof ResizeObserver === "undefined") {
      return () => {
        flip_tween?.kill();
      };
    }

    const sync = () => {
      const current = container.querySelector<HTMLElement>(active_selector);
      if (!current) return;
      flip_tween?.kill();
      gsap.set(indicator, { clearProps: "transform" });
      measured_ref.current = applyBox(current);
    };

    const observer = new ResizeObserver(sync);
    observer.observe(container);
    observer.observe(active);
    return () => {
      flip_tween?.kill();
      observer.disconnect();
    };
  }, [active_key, active_selector, container_ref, indicator_ref]);
}
