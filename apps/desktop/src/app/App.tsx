import { useEffect, useState } from "react";
import { observer } from "mobx-react-lite";
import { AppErrorBoundary } from "./AppErrorBoundary";
import { AppShell } from "./AppShell";
import type { RootStore } from "../stores/RootStore";
import { RootStoreProvider } from "../stores/RootStoreContext";
import { ApplicationConnectionStore } from "../features/runtime-access/ApplicationConnectionStore";
import { ApplicationConnectionContext } from "../features/runtime-access/ApplicationConnectionContext";
import { DesktopEntryPage } from "../features/runtime-access/DesktopEntryPage";
import { DesktopLifecycleDialog } from "../features/desktop-lifecycle/DesktopLifecycleDialog";
import { WebLoginPage } from "../features/runtime-access/WebLoginPage";
import { WorkspaceTransition } from "../features/runtime-access/WorkspaceTransition";

type AppProps = {
  readonly store: RootStore;
};

export const App = observer(function App(props: AppProps) {
  const [connection] = useState(() => new ApplicationConnectionStore(props.store));
  useEffect(() => {
    void connection.initialize();

    // The RootStore owns process-lifetime projections and native listeners. React
    // StrictMode intentionally mounts effects twice in development, so disposing
    // it from a simulated component unmount leaves the second mount disconnected.
    // Release it when the document itself is going away instead.
    const dispose = () => connection.dispose();
    const accept_token = () => { void connection.acceptTokenLink(); };
    window.addEventListener("pagehide", dispose);
    window.addEventListener("hashchange", accept_token);
    return () => {
      window.removeEventListener("pagehide", dispose);
      window.removeEventListener("hashchange", accept_token);
    };
  }, [connection]);

  let entry = <WebLoginPage connection={connection} />;
  // Web 退出或失效时释放本次转场，下一次成功登录重新播放；Desktop 设置切换保持原实例。
  let transition_key = connection.current ? "web-session" : "web-entry";
  if (connection.desktop) {
    transition_key = "desktop";
    entry = <>
      <DesktopEntryPage connection={connection} />
      {connection.phase !== "workspace" && connection.current && <RootStoreProvider store={connection.current}><DesktopLifecycleDialog /></RootStoreProvider>}
    </>;
  }

  return (
    <AppErrorBoundary>
      <ApplicationConnectionContext.Provider value={connection}>
        <WorkspaceTransition
          key={transition_key}
          ready={connection.phase === "workspace"}
          connected={connection.current?.connection.state === "connected"}
          entry={entry}
        >
          {connection.phase === "workspace" && connection.current && <RootStoreProvider store={connection.current}><AppShell /></RootStoreProvider>}
        </WorkspaceTransition>
      </ApplicationConnectionContext.Provider>
    </AppErrorBoundary>
  );
});
