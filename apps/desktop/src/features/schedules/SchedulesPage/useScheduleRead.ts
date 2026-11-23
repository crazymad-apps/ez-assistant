import { useEffect, useState } from "react";
import { RuntimeClientError } from "../../../runtime-client/RuntimeClient";
import { errorText } from "./display";

type ReadState<T> = Readonly<{ value: T | null; error: string | null; missing: boolean }>;
const empty = { value: null, error: null, missing: false };

export function useScheduleRead<T>(
  load: ((signal: AbortSignal) => Promise<T>) | null, revision: number, poll_ms = 2000, enabled = true,
) {
  const [state, setState] = useState<ReadState<T> & { load: typeof load }>({ ...empty, load });
  useEffect(() => {
    if (!load || !enabled) return;
    const lifetime = new AbortController();
    let timer: number | undefined;
    const read = async () => {
      try {
        const value = await load(lifetime.signal);
        if (!lifetime.signal.aborted) setState({ load, value, error: null, missing: false });
      } catch (error) {
        if (lifetime.signal.aborted) return;
        const missing = error instanceof RuntimeClientError
          && ["schedule_not_found", "schedule_execution_not_found"].includes(error.code);
        setState((previous) => ({
          load, missing, value: previous.load === load && !missing ? previous.value : null, error: errorText(error),
        }));
      } finally {
        if (!lifetime.signal.aborted && poll_ms > 0) timer = window.setTimeout(read, poll_ms);
      }
    };
    void read();
    return () => { lifetime.abort(); window.clearTimeout(timer); };
  }, [load, revision, poll_ms, enabled]);
  return state.load === load ? state : empty;
}
