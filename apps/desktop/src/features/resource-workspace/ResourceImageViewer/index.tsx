import { useEffect, useState, type Ref } from "react";
import { TransformComponent, TransformWrapper, type ReactZoomPanPinchRef } from "react-zoom-pan-pinch";
import { createResourceObjectUrl } from "../../../native-bridge/resourceObjectUrl";
import type { ResourceViewState } from "../resourceViewState";
import styles from "./index.module.scss";

export function ResourceImageViewer(props: Readonly<{
  active: boolean;
  alt: string;
  base64?: string | null;
  data_url?: string | null;
  media_type: string;
  ref: Ref<ReactZoomPanPinchRef>;
  view_state: NonNullable<ResourceViewState["preview"]>;
}>) {
  const [initial] = useState(() => props.view_state.image ? { ...props.view_state.image } : undefined);
  const [image, setImage] = useState<{ src: string; width: number; height: number } | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    const src = props.base64 ? createResourceObjectUrl(props.base64, props.media_type) : props.data_url;
    if (!src) { setFailed(true); return; }
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

  if (failed) return <p className={styles.error} role="alert">图片加载失败，请重新加载。</p>;
  if (!image) return null;
  return (
    <TransformWrapper
      ref={props.ref}
      disabled={!props.active}
      initialScale={initial?.scale}
      initialPositionX={initial?.position_x}
      initialPositionY={initial?.position_y}
      fitOnInit={!initial}
      minScale={0.01}
      maxScale={16}
      doubleClick={{ mode: "toggle" }}
      keyboard={{ disabled: false }}
      panning={{ velocityDisabled: true, allowMiddleClickPan: true, allowRightClickPan: false }}
      onTransform={(viewer, state) => {
        // 缓存页面隐藏时不保存零尺寸布局产生的位置。
        if (!props.active || !viewer.instance.wrapperComponent?.clientWidth) return;
        props.view_state.image = { scale: state.scale, position_x: state.positionX, position_y: state.positionY };
      }}
    >
      <TransformComponent
        wrapperClass={styles.viewport}
        contentClass={styles.content}
        wrapperProps={{ "aria-label": `${props.alt} 图片预览`, tabIndex: 0 }}
      >
        <img alt={props.alt} draggable={false} src={image.src} width={image.width} height={image.height} />
      </TransformComponent>
    </TransformWrapper>
  );
}
