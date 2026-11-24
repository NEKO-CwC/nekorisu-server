// Formatting helpers: ms timestamps, byte sizes, relative time.

const dateTime = new Intl.DateTimeFormat("zh-CN", {
  year: "numeric",
  month: "2-digit",
  day: "2-digit",
  hour: "2-digit",
  minute: "2-digit",
  second: "2-digit",
  hour12: false,
});

const relative = new Intl.RelativeTimeFormat("zh-CN", { numeric: "auto" });
const number = new Intl.NumberFormat("zh-CN");

const DASH = "—";

/** ms timestamp -> local date and time, or "—". */
export function formatDateTime(ms: number | null | undefined): string {
  if (ms === null || ms === undefined || !Number.isFinite(ms)) return DASH;
  return dateTime.format(new Date(ms));
}

/** ms timestamp -> "3 分钟前" style text, or "—". */
export function formatRelative(ms: number | null | undefined, now: number = Date.now()): string {
  if (ms === null || ms === undefined || !Number.isFinite(ms)) return DASH;
  const diffSec = Math.round((ms - now) / 1000);
  const abs = Math.abs(diffSec);
  if (abs < 45) return relative.format(0, "second");
  if (abs < 45 * 60) return relative.format(Math.round(diffSec / 60), "minute");
  if (abs < 22 * 3600) return relative.format(Math.round(diffSec / 3600), "hour");
  if (abs < 26 * 86400) return relative.format(Math.round(diffSec / 86400), "day");
  if (abs < 320 * 86400) return relative.format(Math.round(diffSec / (30 * 86400)), "month");
  return relative.format(Math.round(diffSec / (365 * 86400)), "year");
}

/** bytes -> "1.2 MB" (1024-based). */
export function formatBytes(bytes: number | null | undefined): string {
  if (bytes === null || bytes === undefined || !Number.isFinite(bytes)) return DASH;
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1024;
  let i = 0;
  while (value >= 1024 && i < units.length - 1) {
    value /= 1024;
    i += 1;
  }
  return `${value < 10 ? value.toFixed(1) : Math.round(value)} ${units[i]}`;
}

export function formatNumber(n: number | null | undefined): string {
  if (n === null || n === undefined || !Number.isFinite(n)) return DASH;
  return number.format(n);
}

/** ms -> value for an `<input type="datetime-local">` (local time). */
export function msToLocalInput(ms: number): string {
  const d = new Date(ms);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

/** `<input type="datetime-local">` value (local time) -> ms, or undefined when empty/invalid. */
export function localInputToMs(value: string): number | undefined {
  if (!value) return undefined;
  const ms = new Date(value).getTime();
  return Number.isFinite(ms) ? ms : undefined;
}

/** Pretty JSON for display; falls back to the original string when it is not JSON. */
export function prettyJson(value: unknown): string {
  return JSON.stringify(value, null, 2) ?? "null";
}
