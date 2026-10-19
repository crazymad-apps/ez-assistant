import { useEffect, useRef, useState, type Ref } from 'react';
import { TransformComponent, TransformWrapper, type ReactZoomPanPinchRef } from 'react-zoom-pan-pinch';
import { Button } from '../../../components/Button';
import { Icon } from '../../../components/Icon';
import { createResourceObjectUrl } from '../../../native-bridge/resourceObjectUrl';
import type { ResourceViewState } from '../resourceViewState';
import styles from './index.module.scss';

export function ResourceImageViewer(
  props: Readonly<{
    active: boolean;
    alt: string;
    base64?: string | null;
    data_url?: string | null;
    media_type: string;
    ref: Ref<ReactZoomPanPinchRef>;
    view_state: NonNullable<ResourceViewState['preview']>;
  }>,
) {
  const [initial] = useState(() => (props.view_state.image ? { ...props.view_state.image } : undefined));
  const [image, setImage] = useState<{ src: string; width: number; height: number } | null>(null);
  const [failed, setFailed] = useState(false);
  const [viewer, setViewer] = useState<ReactZoomPanPinchRef | null>(null);
  const [scale, setScale] = useState(initial?.scale ?? 1);
  const [min_scale, setMinScale] = useState(0.01);
  const size = useRef<{ width: number; height: number; src: string } | null>(null);

  useEffect(() => {
    const viewport = viewer?.instance.wrapperComponent;
    if (!viewport || !image || !props.active) return;
    const resize = () => {
      const width = viewport.clientWidth,
        height = viewport.clientHeight;
      if (
        !width ||
        !height ||
        (size.current?.width === width && size.current.height === height && size.current.src === image.src)
      )
        return;
      const previous = size.current;
      size.current = { width, height, src: image.src };
      const fit = Math.min(width / image.width, height / image.height, 1);
      setMinScale(fit / 2);
      // 同尺寸的缓存页恢复保持用户位置；实际栏宽/高度变化时重新适配。
      if (!previous && initial) {
        const restored = Math.min(4, Math.max(fit / 2, initial.scale));
        setScale(restored);
        if (restored !== initial.scale) void viewer.centerView(restored, 0);
      } else {
        void viewer.centerView(fit, 0);
      }
    };
    resize();
    const observer = new ResizeObserver(resize);
    observer.observe(viewport);
    return () => observer.disconnect();
  }, [viewer, image, props.active, initial]);

  useEffect(() => {
    const src = props.base64 ? createResourceObjectUrl(props.base64, props.media_type) : props.data_url;
    if (!src) {
      setFailed(true);
      return;
    }
    setImage(null);
    setFailed(false);
    // 解码后再初始化查看器，避免把未加载图片的占位尺寸当成快照恢复边界。
    const decoded = new Image();
    decoded.onload = () => setImage({ src, width: decoded.naturalWidth, height: decoded.naturalHeight });
    decoded.onerror = () => setFailed(true);
    decoded.src = src;
    return () => {
      decoded.onload = null;
      decoded.onerror = null;
      if (props.base64) URL.revokeObjectURL(src);
    };
  }, [props.base64, props.data_url, props.media_type]);

  if (failed)
    return (
      <p className={styles.error} role="alert">
        图片加载失败，请重新加载。
      </p>
    );
  if (!image) return null;
  return (
    <div className={styles.viewer}>
      <TransformWrapper
        ref={props.ref}
        disabled={!props.active}
        initialScale={initial?.scale}
        initialPositionX={initial?.position_x}
        initialPositionY={initial?.position_y}
        minScale={min_scale}
        maxScale={4}
        wheel={{ step: scale * 0.001 }}
        // 小于视口的轴保持居中，避免 disablePadding 的边界夹取将图片拉回左上角。
        centerZoomedOut
        disablePadding
        zoomAnimation={{ disabled: true }}
        doubleClick={{ disabled: true }}
        keyboard={{ disabled: false, zoomStep: scale * 0.1, animationTime: 0 }}
        panning={{ velocityDisabled: true, allowMiddleClickPan: true, allowRightClickPan: false }}
        onInit={setViewer}
        onTransform={(viewer, state) => {
          // 缓存页面隐藏时不保存零尺寸布局产生的位置。
          if (!props.active || !viewer.instance.wrapperComponent?.clientWidth) return;
          setScale(state.scale);
          props.view_state.image = { scale: state.scale, position_x: state.positionX, position_y: state.positionY };
        }}
      >
        <TransformComponent
          wrapperClass={styles.viewport}
          contentClass={styles.content}
          wrapperProps={{
            'aria-label': `${props.alt} 图片预览`,
            tabIndex: 0,
            onDoubleClick: () => {
              if (props.active) void viewer?.fitToView({ maxScale: 1, animationTime: 0 });
            },
          }}
        >
          <img alt={props.alt} draggable={false} src={image.src} width={image.width} height={image.height} />
        </TransformComponent>
      </TransformWrapper>
      <div className={styles.toolbar} role="toolbar" aria-label="图片缩放">
        <Button
          size="small"
          iconOnly
          aria-label="缩小图片"
          disabled={scale <= min_scale}
          onClick={() => void viewer?.zoomOut(scale * 0.1, 0)}
        >
          <Icon name="zoom-out" size={16} />
        </Button>
        <span className={styles.scale}>{Math.round(scale * 100)}%</span>
        <Button
          size="small"
          iconOnly
          aria-label="放大图片"
          disabled={scale >= 4}
          onClick={() => void viewer?.zoomIn(scale * 0.1, 0)}
        >
          <Icon name="zoom-in" size={16} />
        </Button>
        <Button size="small" onClick={() => void viewer?.fitToView({ maxScale: 1, animationTime: 0 })}>
          适应窗口
        </Button>
        <Button size="small" aria-label="原始尺寸（100%）" onClick={() => void viewer?.centerView(1, 0)}>
          100%
        </Button>
      </div>
    </div>
  );
}
