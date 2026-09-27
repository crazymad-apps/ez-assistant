import { createContext, useCallback, useContext, useEffect, useRef, useState, type PropsWithChildren } from "react";
import { createPortal } from "react-dom";
import { Icon } from "../Icon";
import styles from "./index.module.scss";

type MessageKind = "success" | "error";
type Message = Readonly<{ id: number; kind: MessageKind; text: string }>;
type ShowMessage = (text: string, kind?: MessageKind) => void;

const AppMessageContext = createContext<ShowMessage | null>(null);

export function AppMessageProvider({ children }: PropsWithChildren) {
  const [messages, setMessages] = useState<readonly Message[]>([]);
  const next_id = useRef(0);
  const timers = useRef(new Map<number, number>());

  useEffect(() => () => {
    for (const timer of timers.current.values()) window.clearTimeout(timer);
    timers.current.clear();
  }, []);

  const show_message = useCallback<ShowMessage>((text, kind = "success") => {
    const id = ++next_id.current;
    setMessages((current) => [...current.slice(-2), { id, kind, text }]);
    const timer = window.setTimeout(() => {
      timers.current.delete(id);
      setMessages((current) => current.filter((item) => item.id !== id));
    }, kind === "error" ? 5000 : 3500);
    timers.current.set(id, timer);
  }, []);

  return <AppMessageContext.Provider value={show_message}>
    {children}
    {createPortal(<div className={styles.host}>
      {messages.map((item) => <div className={styles.bubble} data-kind={item.kind} key={item.id} role={item.kind === "error" ? "alert" : "status"}>
        <Icon name={item.kind === "error" ? "x" : "check"} size={16} />
        <span>{item.text}</span>
      </div>)}
    </div>, document.body)}
  </AppMessageContext.Provider>;
}

export function useAppMessage(): ShowMessage {
  const show_message = useContext(AppMessageContext);
  if (!show_message) throw new Error("AppMessageProvider is missing");
  return show_message;
}
