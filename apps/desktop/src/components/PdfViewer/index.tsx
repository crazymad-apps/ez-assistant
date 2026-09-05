import { useEffect, useState } from "react";
import { createResourceObjectUrl } from "../../native-bridge/resourceObjectUrl";
import styles from "./index.module.scss";

export function PdfViewer(props: Readonly<{ base64: string; title: string }>) {
  const [url, setUrl] = useState<string | null>(null);

  useEffect(() => {
    const next = createResourceObjectUrl(props.base64, "application/pdf");
    setUrl(next);
    return () => URL.revokeObjectURL(next);
  }, [props.base64]);

  return url ? <iframe className={styles.viewer} src={url} title={props.title} /> : null;
}
