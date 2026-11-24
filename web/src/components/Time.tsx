import { formatDateTime, formatRelative } from "../format";

/** A ms timestamp as local date-time, optionally followed by relative time. */
export function Time({ ms, relative = true }: { ms: number | null | undefined; relative?: boolean }) {
  if (ms === null || ms === undefined) return <>—</>;
  const date = new Date(ms);
  if (Number.isNaN(date.getTime())) return <>—</>;
  return (
    <time dateTime={date.toISOString()}>
      {formatDateTime(ms)}
      {relative ? `（${formatRelative(ms)}）` : ""}
    </time>
  );
}
