import { useRef, type ReactNode } from "react";
import { usePresence } from "../Presence";
import styles from "./index.module.scss";

export function Collapse(props: Readonly<{
  children: ReactNode;
  class_name?: string;
  id?: string;
  open: boolean;
}>) {
  const presence = usePresence(props.open, 120);
  const retained_children_ref = useRef(props.children);
  if (props.open) retained_children_ref.current = props.children;

  if (!presence.mounted) return null;
  return (
    <div
      aria-hidden={presence.state === "exiting" ? true : undefined}
      className={styles.collapse}
      data-presence={presence.state}
      id={props.id}
      inert={presence.state === "exiting" ? true : undefined}
      onTransitionEnd={presence.onTransitionEnd}
    >
      <div className={[styles.content, props.class_name].filter(Boolean).join(" ")}>
        {props.open ? props.children : retained_children_ref.current}
      </div>
    </div>
  );
}
