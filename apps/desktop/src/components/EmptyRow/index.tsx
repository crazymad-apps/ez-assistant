import styles from "./index.module.scss";

export function EmptyRow({ children }: Readonly<{ children: string }>) {
  return <p className={styles.row}>{children}</p>;
}
