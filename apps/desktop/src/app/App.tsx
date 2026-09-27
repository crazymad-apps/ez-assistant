import { SettingsDialog } from "../features/settings/SettingsDialog";
import { AccountDialog } from "../features/runtime-access/AccountDialog";
import { useEffect, useState } from "react";
import { observer } from "mobx-react-lite";
import { AppErrorBoundary } from "./AppErrorBoundary";
import { AppMessageProvider } from "../components/AppMessage";
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
    const recheck = () => { if (document.visibilityState !== "hidden") void connection.recheckSession(); };
    window.addEventListener("focus", recheck);
    document.addEventListener("visibilitychange", recheck);
    window.addEventListener("pagehide", dispose);
    window.addEventListener("hashchange", accept_token);
    return () => {
      window.removeEventListener("focus", recheck);
      document.removeEventListener("visibilitychange", recheck);
      window.removeEventListener("pagehide", dispose);
      window.removeEventListener("hashchange", accept_token);
    };
  }, [connection]);

  let entry = <WebLoginPage connection={connection} />;
  if (connection.desktop) {
    entry = <>
      <DesktopEntryPage connection={connection} />
      {connection.phase !== "workspace" && connection.current && <RootStoreProvider store={connection.current}><DesktopLifecycleDialog /><SettingsDialog /></RootStoreProvider>}
    </>;
  }

  return (
    <AppMessageProvider>
      <AppErrorBoundary>
        <ApplicationConnectionContext.Provider value={connection}>
          <AccountDialog connection={connection} />
          <WorkspaceTransition
            ready={connection.phase === "workspace"}
            entry={entry}
          >
            {connection.phase === "workspace" && connection.current && <RootStoreProvider store={connection.current}><AppShell /></RootStoreProvider>}
          </WorkspaceTransition>
        </ApplicationConnectionContext.Provider>
      </AppErrorBoundary>
    </AppMessageProvider>
  );
});
