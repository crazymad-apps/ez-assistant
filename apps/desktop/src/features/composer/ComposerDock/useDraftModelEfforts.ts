import { useEffect, useState } from "react";
import type { ModelSelection, ReasoningEffortKey, ReasoningEffortOptionSnapshot } from "../../../generated/assistant-protocol";
import { useRootStore } from "../../../stores/RootStoreContext";

const effort_keys: readonly ReasoningEffortKey[] = ["low", "medium", "high", "x_high", "max"];

/** 草稿没有 SessionView，菜单从 Runtime 的生效配置读取档位，复用模板与固定配置优先级。 */
export function useDraftModelEfforts(selection: ModelSelection | null, open: boolean): readonly ReasoningEffortOptionSnapshot[] {
  const store = useRootStore();
  const provider_id = selection?.provider_instance_id;
  const model_id = selection?.model_id;
  const model_key = JSON.stringify([provider_id, model_id]);
  const [loaded, setLoaded] = useState<{ key: string; options: ReasoningEffortOptionSnapshot[] } | null>(null);
  const connection_state = store.connection.state;

  useEffect(() => {
    if (!open || !provider_id || !model_id || connection_state !== "connected") return;
    const controller = new AbortController();
    setLoaded(null);
    void store.settings.getModelConfiguration({ provider_instance_id: provider_id, model_id }, controller.signal)
      .then(({ parameters }) => {
        if (controller.signal.aborted) return;
        const enabled = parameters.reasoning === "supported"
          && (parameters.reasoning_mode === "optional" || parameters.reasoning_mode === "always");
        const options = enabled ? effort_keys
          .filter((key) => Boolean(parameters.reasoning_efforts?.[key]?.trim()))
          .map((key) => ({ key, label: key === "x_high" ? "xhigh" : key })) : [];
        setLoaded({ key: model_key, options });
      })
      .catch((error: unknown) => {
        if (!controller.signal.aborted) store.showInteractionError(error instanceof Error ? error.message : "读取模型强度失败，请重新打开菜单重试。");
      });
    return () => controller.abort();
  }, [store, open, provider_id, model_id, model_key, connection_state]);

  return loaded?.key === model_key && connection_state === "connected" ? loaded.options : [];
}
