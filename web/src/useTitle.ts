import { useEffect } from "react";
import { t } from "./messages";

export function useTitle(title: string): void {
  useEffect(() => {
    document.title = `${title} · ${t.appName}`;
  }, [title]);
}
