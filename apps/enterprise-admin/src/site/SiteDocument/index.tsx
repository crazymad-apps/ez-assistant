import { lazy, Suspense, useState } from 'react';
import type { ReactNode } from 'react';
import { Link } from 'react-router';
import { Alert, Typography } from 'antd';
import type { SiteNode } from '../../request/openapi';
import styles from './index.module.scss';

const CodeBlock = lazy(() => import('./CodeBlock').then((module) => ({ default: module.SiteCodeBlock })));

/** 正文只渲染白名单节点，首页／手册与版本日志预览共用；不解释 HTML。 */
export function SiteDocument({
  body,
  images = {},
  preview = true,
  headingPrefix = '',
}: {
  body: SiteNode[];
  images?: Readonly<Record<string, string>>;
  preview?: boolean;
  headingPrefix?: string;
}) {
  function nodes(values: SiteNode[]): ReactNode {
    return values.map((node, index) => {
      const children = nodes(node.children ?? []),
        key = index;
      switch (node.kind) {
        case 'text':
          return <span key={key}>{node.text}</span>;
        case 'paragraph':
          return <p key={key}>{children}</p>;
        case 'heading': {
          const Tag = `h${Math.min(6, Math.max(1, node.level ?? 2))}` as 'h1' | 'h2';
          return (
            <Tag key={key} id={node.id ? headingPrefix + node.id : undefined}>
              {children}
            </Tag>
          );
        }
        case 'strong':
          return <strong key={key}>{children}</strong>;
        case 'em':
          return <em key={key}>{children}</em>;
        case 's':
          return <s key={key}>{children}</s>;
        case 'blockquote':
          return <blockquote key={key}>{children}</blockquote>;
        case 'bullet_list':
          return <ul key={key}>{children}</ul>;
        case 'ordered_list':
          return (
            <ol key={key} start={node.start}>
              {children}
            </ol>
          );
        case 'list_item':
          return <li key={key}>{children}</li>;
        case 'code':
          return (
            <Suspense
              key={key}
              fallback={
                <pre>
                  <code>{node.text}</code>
                </pre>
              }
            >
              <CodeBlock code={node.text ?? ''} language={node.language ?? ''} />
            </Suspense>
          );
        case 'code_inline':
          return (
            <code key={key} data-streamdown="inline-code">
              {node.text}
            </code>
          );
        case 'link': {
          const href = safeHref(node.href);
          return preview ? (
            <Typography.Text key={key} type="secondary">
              {children}（{node.href}）
            </Typography.Text>
          ) : href?.startsWith('/') ? (
            <Link key={key} to={href}>
              {children}
            </Link>
          ) : (
            <a key={key} href={href} rel="noopener noreferrer">
              {children}
            </a>
          );
        }
        case 'image':
          return images[node.href ?? ''] ? (
            <ContentImage
              key={`${key}:${images[node.href ?? '']}`}
              src={images[node.href ?? '']!}
              alt={node.text ?? ''}
            />
          ) : (
            <Alert key={key} type="warning" title={`图片暂不可用：${node.text ?? ''}`} />
          );
        case 'break':
          return <br key={key} />;
        case 'hr':
          return <hr key={key} />;
        case 'table':
          return (
            <div data-streamdown="table-wrapper" key={key} role="region" aria-label="表格" tabIndex={0}>
              <div>
                <table data-streamdown="table">{children}</table>
              </div>
            </div>
          );
        case 'thead':
          return <thead key={key}>{children}</thead>;
        case 'tbody':
          return <tbody key={key}>{children}</tbody>;
        case 'tr':
          return <tr key={key}>{children}</tr>;
        case 'th':
          return (
            <th key={key} scope="col">
              {children}
            </th>
          );
        case 'td':
          return <td key={key}>{children}</td>;
      }
    });
  }
  return <div className={styles.reader}>{nodes(body)}</div>;
}

/** 静态目录是发布边界；阅读器仍拒绝可执行 scheme，损坏内容不会产生脚本链接。 */
function safeHref(href?: string): string | undefined {
  if (!href || /[\s\\]/.test(href) || href.startsWith('//')) return undefined;
  if (/^https?:\/\//i.test(href)) {
    try {
      const url = new URL(href);
      return url.username || url.password ? undefined : href;
    } catch {
      return undefined;
    }
  }
  return /^\/(?:$|download(?:[?#]|$)|changelog(?:#|$)|manual(?:\/|#|$))/.test(href) || href.startsWith('/#')
    ? href
    : undefined;
}

/** 同一正文快照引用不可变图片；失败保留可读说明，不自动重载。 */
function ContentImage({ src, alt }: { src: string; alt: string }) {
  const [failed, setFailed] = useState(false);
  return failed ? (
    <Alert type="warning" title={`图片暂不可用：${alt || '正文配图'}`} />
  ) : (
    <img src={src} alt={alt} onError={() => setFailed(true)} />
  );
}
