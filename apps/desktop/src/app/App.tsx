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
    return () => props.store.dispose();
  }, [props.store]);

  return (
    <AppErrorBoundary>
      <RootStoreProvider store={props.store}>
        <AppShell />
      </RootStoreProvider>
    </AppErrorBoundary>
  );
}
