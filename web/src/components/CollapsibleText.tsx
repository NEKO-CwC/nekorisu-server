import { useState } from "react";
import { t } from "../messages";

const MAX_CHARS = 600;
const MAX_LINES = 12;

function isLong(text: string): boolean {
  if (text.length > MAX_CHARS) return true;
  let lines = 1;
  for (let i = 0; i < text.length; i++) {
    if (text.charCodeAt(i) === 10 && ++lines > MAX_LINES) return true;
  }
  return false;
}

function preview(text: string): string {
  const byLines = text.split("\n").slice(0, MAX_LINES).join("\n");
  const cut = [...byLines.slice(0, MAX_CHARS * 2)].slice(0, MAX_CHARS).join("");
  return `${cut}…`;
}

/** Untrusted text, rendered as text only, whitespace preserved; long text is collapsed. */
export function CollapsibleText({ text }: { text: string }) {
  const [open, setOpen] = useState(false);
  const long = isLong(text);
  return (
    <>
      <pre className="text">{!long || open ? text : preview(text)}</pre>
      {long && (
        <button type="button" aria-expanded={open} onClick={() => setOpen((v) => !v)}>
          {open ? t.common.collapse : t.common.expand}
        </button>
      )}
    </>
  );
}
