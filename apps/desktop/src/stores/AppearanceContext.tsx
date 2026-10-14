import { createContext, useContext, type PropsWithChildren } from 'react';
import type { AppearanceStore } from './AppearanceStore';

const AppearanceContext = createContext<AppearanceStore | null>(null);

export function AppearanceProvider({ store, children }: PropsWithChildren<{ readonly store: AppearanceStore }>) {
  return <AppearanceContext.Provider value={store}>{children}</AppearanceContext.Provider>;
}

export function useAppearance(): AppearanceStore {
  const store = useContext(AppearanceContext);
  if (!store) throw new Error('AppearanceProvider is missing');
  return store;
}
