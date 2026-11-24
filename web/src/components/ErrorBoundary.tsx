import { Component, type ErrorInfo, type ReactNode } from "react";
import { t } from "../messages";

interface State {
  error: Error | null;
}

/** Last-resort boundary so a render error does not leave a blank page. */
export class ErrorBoundary extends Component<{ children: ReactNode }, State> {
  state: State = { error: null };

  static getDerivedStateFromError(error: Error): State {
    return { error };
  }

  componentDidCatch(error: Error, info: ErrorInfo): void {
    console.error(error, info.componentStack);
  }

  render() {
    if (this.state.error) {
      return (
        <main>
          <p role="alert" className="error">
            {this.state.error.message}
          </p>
          <button type="button" onClick={() => window.location.reload()}>
            {t.common.reload}
          </button>
        </main>
      );
    }
    return this.props.children;
  }
}
