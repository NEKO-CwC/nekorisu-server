import { useId, useRef, useState } from "react";
import { t } from "../messages";

/** A read-only text area with a copy button. */
export function CopyBox({ label, value, rows = 3 }: { label: string; value: string; rows?: number }) {
  const id = useId();
  const ref = useRef<HTMLTextAreaElement>(null);
  const [state, setState] = useState<"idle" | "copied" | "failed">("idle");

  async function copy() {
    try {
      await navigator.clipboard.writeText(value);
      setState("copied");
    } catch {
      // Clipboard API is unavailable on non-secure origins: fall back to selecting the text.
      const el = ref.current;
      let ok = false;
      if (el) {
        el.select();
        try {
          ok = document.execCommand("copy");
        } catch {
          ok = false;
        }
      }
      setState(ok ? "copied" : "failed");
    }
  }

  return (
    <div className="copybox">
      <label htmlFor={id}>{label}</label>
      <textarea
        id={id}
        ref={ref}
        readOnly
        rows={rows}
        value={value}
        spellCheck={false}
        onFocus={(e) => e.currentTarget.select()}
      />
      <div>
        <button type="button" onClick={() => void copy()}>
          {t.common.copy}
        </button>{" "}
        <span role="status">
          {state === "copied" ? t.common.copied : state === "failed" ? t.common.copyFailed : ""}
        </span>
      </div>
    </div>
  );
}
