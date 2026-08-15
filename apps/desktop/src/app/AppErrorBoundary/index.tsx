import { Component, type ErrorInfo, type ReactNode } from "react";
import styles from "./index.module.scss";

type AppErrorBoundaryProps = {
  readonly children: ReactNode;
};

type AppErrorBoundaryState = {
  readonly has_error: boolean;
};

export class AppErrorBoundary extends Component<AppErrorBoundaryProps, AppErrorBoundaryState> {
  state: AppErrorBoundaryState = { has_error: false };

  static getDerivedStateFromError(): AppErrorBoundaryState {
    return { has_error: true };
  }

  componentDidCatch(_error: Error, _info: ErrorInfo): void {
    // Do not serialize application data or Runtime credentials into WebView logs.
  }

  render(): ReactNode {
    if (this.state.has_error) {
      return (
        <main className={styles.error_shell}>
          <strong>桌面界面无法继续显示</strong>
          <span>请重新打开窗口；Runtime 中的任务不会因此停止。</span>
        </main>
      );
    }
    return this.props.children;
  }
}
