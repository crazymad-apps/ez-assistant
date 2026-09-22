// macOS WKWebView 验证夹具：运行产品 hook，真实布局与 scroll 事件，不装配 Runtime。
import { createRoot } from "react-dom/client";
import { useRef, useState } from "react";
import { useOutputFollow } from "../../src/features/conversation/ConversationView/useOutputFollow";
import type { ConversationReadingPosition } from "../../src/stores/NavigationStore";

let grow: () => void;
const positions = new Map<string, ConversationReadingPosition>();
function Output({ name, nested = false }: { name: string; nested?: boolean }) {
  const scroll = useRef<HTMLDivElement>(null); const content = useRef<HTMLDivElement>(null);
  const [rows, setRows] = useState(60);
  const follow = useOutputFollow({ scroll, content, identity: name, active: true,
    restore: () => positions.get(name), save: position => positions.set(name, position) });
  if (nested) grow = () => setRows(value => value + 30);
  return <div id={name} ref={scroll} tabIndex={0} style={{ height: nested ? 420 : 160, overflowY: "auto", overflowAnchor: "none", border: "1px solid gray" }}>
    <div ref={content}>
      {nested && <Output name="reasoning" />}
      {Array.from({ length: rows }, (_, i) => <div key={i} data-message-id={`${name}-${i}`} style={{ height: 28 }}>Line {i}</div>)}
    </div>
    <button onClick={follow.bottom}>Bottom</button>
  </div>;
}
const wait = () => new Promise<void>(resolve => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
const assert = (ok: boolean, message: string) => { if (!ok) throw new Error(message); };
async function probe() {
  await wait();
  const outer = document.getElementById("outer")!;
  const reasoning = document.getElementById("reasoning")!;
  const atBottom = (node: HTMLElement) => node.scrollHeight - node.clientHeight - node.scrollTop < 3;
  assert(atBottom(outer) && atBottom(reasoning), "initial bottom");
  grow(); await wait(); assert(atBottom(outer), "large growth follows");
  outer.dispatchEvent(new WheelEvent("wheel", { deltaY: -400 })); outer.scrollTop -= 400; await wait();
  const top = outer.scrollTop; grow(); await wait(); assert(Math.abs(outer.scrollTop - top) < 3, "paused anchor survives growth");
  outer.dispatchEvent(new KeyboardEvent("keydown", { key: "End" })); outer.scrollTop = outer.scrollHeight; await wait();
  grow(); await wait(); assert(atBottom(outer), "End resumes follow");
  reasoning.dispatchEvent(new WheelEvent("wheel", { deltaY: -80, bubbles: true })); reasoning.scrollTop -= 80; await wait();
  assert(positions.get("outer")?.following === true, "inner input keeps outer intent");
  assert(positions.get("reasoning")?.following === false, "inner pauses independently");
  return "PASS: WKWebView growth, paused anchor, keyboard return, independent reasoning";
}
createRoot(document.getElementById("root")!).render(<Output name="outer" nested />);
Object.assign(window, { runFollowProbe: probe });
