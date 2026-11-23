import { InlineIconButton } from "../../../components/InlineIconButton";
import styles from "./index.module.scss";

type PageControls = Readonly<{
  loading: boolean;
  error: string | null;
  page: number;
  pages: number;
  refresh: () => void;
  navigate: (page: number) => void;
}>;

export function ContextPageControls({ label, page }: Readonly<{ label: string; page: PageControls }>) {
  return <>
    {page.error && <div className={styles.context_page_error} role="alert">
      <span>{page.error}</span>
      <InlineIconButton icon="refresh" label={`重试读取${label}`} onClick={page.refresh} />
    </div>}
    {page.pages > 1 && <nav aria-label={`${label}分页`} className={styles.context_pagination}>
      <InlineIconButton disabled={page.loading || page.page === 0} icon="chevron-left" label={`上一页${label}`} onClick={() => page.navigate(page.page - 1)} />
      <span>{page.page + 1} / {page.pages}</span>
      <InlineIconButton disabled={page.loading || page.page + 1 >= page.pages} icon="chevron-right" label={`下一页${label}`} onClick={() => page.navigate(page.page + 1)} />
    </nav>}
  </>;
}
