import { Children, useEffect, useRef, useState, type ReactNode } from "react";
import styles from "./index.module.scss";

const TWO_COLUMN_MIN_WIDTH = 620;

type ContextSectionLayoutProps = Readonly<{
  children: ReactNode;
}>;

/**
 * 宽栏使用两条独立纵向 Flex 列，避免普通 Grid 让短卡片等待同一行的高卡片。
 * WKWebView 尚无可依赖的 CSS masonry；ResizeObserver 只决定展示布局，不复制业务状态。
 */
export function ContextSectionLayout(props: ContextSectionLayoutProps) {
  const container_ref = useRef<HTMLDivElement>(null);
  const [two_columns, setTwoColumns] = useState(false);

  useEffect(() => {
    const container = container_ref.current;
    if (!container) return undefined;

    const updateLayout = (width: number) => {
      const next_two_columns = width >= TWO_COLUMN_MIN_WIDTH;
      setTwoColumns((current) => current === next_two_columns ? current : next_two_columns);
    };
    updateLayout(container.getBoundingClientRect().width);
    if (typeof ResizeObserver === "undefined") return undefined;

    const observer = new ResizeObserver((entries) => {
      const entry = entries[0];
      if (entry) updateLayout(entry.contentRect.width);
    });
    observer.observe(container);
    return () => observer.disconnect();
  }, []);

  const sections = Children.toArray(props.children);
  if (!two_columns) {
    return <div className={styles.panel_scroll} ref={container_ref}>{sections}</div>;
  }

  return (
    <div className={styles.panel_scroll} data-layout="two-columns" ref={container_ref}>
      <div className={styles.panel_column} data-context-column="left">
        {sections.filter((_, index) => index % 2 === 0)}
      </div>
      <div className={styles.panel_column} data-context-column="right">
        {sections.filter((_, index) => index % 2 === 1)}
      </div>
    </div>
  );
}
