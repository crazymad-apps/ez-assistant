import { opencodePresets, opencodeUrls, type ProviderConnection } from "@ez-assistant/protocol";
import { Button } from "../../../components/Button";
import styles from "./index.module.scss";

export function OpenCodeConnectionFields(props: Readonly<{
  connection: ProviderConnection;
  onChange: (patch: Partial<ProviderConnection>) => void;
}>) {
  const urls = opencodeUrls(props.connection);
  return <>
    <div className={styles.runtime_actions}>
      {opencodePresets.map((preset) => <Button key={preset.id} type="button" onClick={() => props.onChange({ endpoint: preset.endpoint, models_path: preset.models_path })}>
        使用{preset.id === "go" ? "Go 订阅" : "普通按量"}地址
      </Button>)}
    </div>
    <p>点击预设会替换服务地址及模型列表路径；保留所选协议。</p>
    {urls && <div className={styles.model_form}>
      <label>模型列表地址<input aria-label="模型列表地址" readOnly value={urls.discovery} /></label>
      <label>推理地址<input aria-label="推理地址" readOnly value={urls.inference} /></label>
    </div>}
    <p>模型列表接口填写同源绝对路径（如 /inference/go/v1/models），将替换服务地址的整个路径。列表可能包含多种协议的模型，请核对所选协议；发现成功不代表推理可用。</p>
    <p>支持本机 Chat／Responses 流式调用；请使用连接测试核验所选账号和模型。服务端历史续接、压缩和输入计量暂不开放。</p>
  </>;
}
