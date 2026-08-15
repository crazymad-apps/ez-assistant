import { createContext, useContext, type PropsWithChildren } from "react";
import type { RootStore } from "./RootStore";

const RootStoreContext = createContext<RootStore | null>(null);

type RootStoreProviderProps = PropsWithChildren<{
  readonly store: RootStore;
}>;

export function RootStoreProvider(props: RootStoreProviderProps) {
  return <RootStoreContext.Provider value={props.store}>{props.children}</RootStoreContext.Provider>;
}

export function useRootStore(): RootStore {
  const store = useContext(RootStoreContext);
  if (!store) {
    throw new Error("RootStoreProvider is missing");
  }
  return store;
}
