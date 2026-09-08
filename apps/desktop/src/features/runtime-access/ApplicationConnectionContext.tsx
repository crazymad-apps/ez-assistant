import { createContext, useContext } from "react";
import type { ApplicationConnectionStore } from "./ApplicationConnectionStore";

export const ApplicationConnectionContext = createContext<ApplicationConnectionStore | null>(null);
export function useApplicationConnection() { return useContext(ApplicationConnectionContext); }
