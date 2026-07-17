import { invoke } from "@tauri-apps/api/core";
import "./style.css";

const button = document.querySelector<HTMLButtonElement>("#health-check");
const result = document.querySelector<HTMLOutputElement>("#health-result");

if (!button || !result) {
  throw new Error("Desktop shell elements are missing");
}

button.addEventListener("click", async () => {
  button.disabled = true;
  result.textContent = "检查中…";

  try {
    result.textContent = await invoke<string>("health");
  } catch (error) {
    result.textContent = `连接失败：${String(error)}`;
  } finally {
    button.disabled = false;
  }
});
