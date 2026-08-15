import { useEffect } from "react";
import { AppErrorBoundary } from "./AppErrorBoundary";
import { AppShell } from "./AppShell";
import type { RootStore } from "../stores/RootStore";
import { RootStoreProvider } from "../stores/RootStoreContext";

type AppProps = {
  readonly store: RootStore;
};

export function App(props: AppProps) {
  useEffect(() => {
    void props.store.initializePreferences();
    void props.store.connect();

    // The RootStore owns process-lifetime projections and native listeners. React
    // StrictMode intentionally mounts effects twice in development, so disposing
    // it from a simulated component unmount leaves the second mount disconnected.
    // Release it when the document itself is going away instead.
    const dispose = () => props.store.dispose();
    window.addEventListener("pagehide", dispose);
    return () => window.removeEventListener("pagehide", dispose);
  }, [props.store]);

  return (
    <AppErrorBoundary>
      <RootStoreProvider store={props.store}>
        <AppShell />
      </RootStoreProvider>
    </AppErrorBoundary>
  );
}
