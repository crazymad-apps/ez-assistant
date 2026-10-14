import { useEffect, useRef } from 'react';
import type * as Three from 'three';
import styles from './index.module.scss';

type DotsEffect = {
  renderer: Three.WebGLRenderer;
  scene: Three.Scene;
  starField: Three.Points;
  starsGeometry: Three.BufferGeometry;
  camera: Three.PerspectiveCamera & { tx: number; ty: number; tz: number };
  req: number;
  prevNow: number;
  destroy(): void;

  resize(): void;

  animationLoop(): void;

  onUpdate(): void;

  onMouseMove(x: number, y: number): void;
};

/** 沿用客户端登录页的 Vanta 0.5.24 / Three r134 点阵；官网保持浅色，卸载时释放 GPU 与事件。 */
export function DotsBackground() {
  const element = useRef<HTMLDivElement>(null);
  useEffect(() => {
    const host = element.current;
    if (!host) return;
    let disposed = false;
    let effect: DotsEffect | null = null;
    let paused = false;
    let previousFrameAt = performance.now();
    const reduced = matchMedia('(prefers-reduced-motion: reduce)');
    const fine = matchMedia('(pointer: fine)');

    function destroy() {
      const previous = effect;
      effect = null;
      if (previous) {
        // Vanta.destroy 会清空 renderer 引用，先保留它再释放 GPU 与上下文。
        const renderer = previous.renderer;
        previous.destroy();
        renderer?.dispose();
        renderer?.forceContextLoss();
      }
      paused = false;
      if (host) host.dataset.dotsState = 'static';
    }

    const playback = () => {
      if (!effect) return;
      if (document.hidden) {
        cancelAnimationFrame(effect.req);
        paused = true;
      } else if (paused) {
        paused = false;
        previousFrameAt = performance.now();
        effect.prevNow = previousFrameAt;
        effect.animationLoop();
      }
    };

    async function mount() {
      destroy();
      if (disposed || reduced.matches || !host) return;
      try {
        const THREE = await import('three');
        if (disposed || reduced.matches) return;
        // 固定版本的 DOTS 模块在载入时捕获 window.THREE。
        (window as Window & { THREE?: typeof Three }).THREE = THREE;
        const { default: create } = await import('vanta/src/vanta.dots.js');
        if (disposed || reduced.matches || effect) return;
        const color = 0x819ccf;
        const lineColor = 0xa6b8d8;
        const value = create({
          el: host,
          THREE,
          color,
          color2: lineColor,
          backgroundAlpha: 0,
          size: 3,
          spacing: 70,
          showLines: false,
          mouseControls: true,
          touchControls: false,
          gyroControls: false,
          minHeight: 100,
          minWidth: 100,
          scale: 1,
          scaleMobile: 1,
        }) as DotsEffect;
        effect = value;
        if (!value.renderer || !value.starField) {
          destroy();
          return;
        }
        const positions = value.starsGeometry.getAttribute('position') as Three.BufferAttribute;
        positions.setUsage(THREE.DynamicDrawUsage);
        const baseHeights = new Float32Array(positions.count);
        for (let index = 0; index < positions.count; index += 1) {
          baseHeights[index] = (positions.getY(index) + 150) * 0.35;
        }
        const side = Math.round(Math.sqrt(positions.count));
        const edges: number[] = [];
        for (let index = 0; index < positions.count; index += 1) {
          if (index % side < side - 1) edges.push(index, index + 1);
          if (Math.floor(index / side) < side - 1) edges.push(index, index + side);
        }
        const geometry = new THREE.BufferGeometry();
        geometry.setAttribute('position', positions);
        geometry.setIndex(edges);
        const material = new THREE.LineBasicMaterial({
          color: lineColor,
          transparent: true,
          opacity: 0.45,
          depthWrite: false,
        });
        const links = new THREE.LineSegments(geometry, material);
        links.frustumCulled = false;
        value.starField.add(links);
        const camera = value.camera;
        camera.tx = 160;
        camera.ty = 320;
        camera.tz = 310;
        camera.position.set(160, 320, 310);
        camera.lookAt(0, 0, 0);
        value.onMouseMove = (x, y) => {
          if (document.hidden || !fine.matches || !Number.isFinite(x) || !Number.isFinite(y)) return;
          const dx = Math.max(0, Math.min(1, x)) - 0.5,
            dy = Math.max(0, Math.min(1, y)) - 0.5;
          camera.tx = 160 + dx * 32;
          camera.ty = 320 + dy * 22;
          camera.tz = 310 - dx * 16;
        };
        previousFrameAt = performance.now();
        let waveTime = 0;
        value.onUpdate = () => {
          const now = performance.now(),
            elapsedMs = Math.max(0, now - previousFrameAt);
          previousFrameAt = now;
          waveTime += elapsedMs / 1000;
          // SDK 按帧累加高度，低帧率下波动会缩小；改为固定基准上的时间波形。
          // 点与规则连线共享位置缓冲，鼠标静止时也保持小幅起伏，不另开动画循环。
          for (const [index, baseHeight] of baseHeights.entries()) {
            const phase = positions.getX(index) * 0.012 + positions.getZ(index) * 0.01;
            positions.setY(index, baseHeight + Math.sin(phase + waveTime * 0.8) * 8);
          }
          positions.needsUpdate = true;
          const ease = 1 - Math.exp(-Math.min(50, elapsedMs) / 300);
          camera.position.x += (camera.tx - camera.position.x) * ease;
          camera.position.y += (camera.ty - camera.position.y) * ease;
          camera.position.z += (camera.tz - camera.position.z) * ease;
          camera.lookAt(0, 0, 0);
        };
        host.dataset.dotsState = 'ready';
        playback();
      } catch (error) {
        console.warn('Homepage DOTS unavailable', error);
        destroy();
      }
    }

    const reset = () => effect?.onMouseMove(0.5, 0.5);
    const refresh = () => {
      void mount();
    };
    const resize = new ResizeObserver(() => effect?.resize());
    resize.observe(host);
    document.addEventListener('visibilitychange', playback);
    document.documentElement.addEventListener('mouseleave', reset);
    reduced.addEventListener('change', refresh);
    void mount();
    return () => {
      disposed = true;
      resize.disconnect();
      reduced.removeEventListener('change', refresh);
      document.removeEventListener('visibilitychange', playback);
      document.documentElement.removeEventListener('mouseleave', reset);
      destroy();
    };
  }, []);
  return <div ref={element} className={styles.background} aria-hidden="true" data-dots-state="static" />;
}
