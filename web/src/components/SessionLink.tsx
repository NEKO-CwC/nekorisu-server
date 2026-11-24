import { Link } from "react-router";
import { splitRef } from "../api/client";

export function sessionHref(agent: string, id: string): string {
  return `/sessions/${encodeURIComponent(agent)}/${encodeURIComponent(id)}`;
}

/** Link to a session given its `<agent>:<id>` ref; plain text when the ref is malformed. */
export function SessionLink({ sessionRef, label }: { sessionRef: string; label?: string }) {
  const parts = splitRef(sessionRef);
  if (!parts) return <code>{label ?? sessionRef}</code>;
  return <Link to={sessionHref(parts.agent, parts.id)}>{label ?? sessionRef}</Link>;
}
