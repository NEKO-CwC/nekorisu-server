import { useEffect, useState, type FormEvent } from "react";
import { Link, useSearchParams } from "react-router";
import { useDevices, useSessions } from "../api/hooks";
import type { SessionSummary } from "../api/types";
import { ErrorMessage } from "../components/QueryView";
import { sessionHref } from "../components/SessionLink";
import { Time } from "../components/Time";
import { formatBytes } from "../format";
import { t } from "../messages";
import { useTitle } from "../useTitle";

const PAGE_SIZE = 50;
const AGENTS = ["claude-code", "codex"];

function SessionRow({ s }: { s: SessionSummary }) {
  return (
    <tr>
      <td>
        <Link to={sessionHref(s.agent, s.id)}>{s.title || t.sessions.untitled}</Link>
        {s.parent && (
          <>
            {" "}
            <span title={s.parent}>{t.sessions.childMarker}</span>
          </>
        )}
        <br />
        <code>{s.ref}</code>
      </td>
      <td>
        <code>{s.cwd ?? "—"}</code>
      </td>
      <td>{s.devices.map((d) => d.name ?? d.id).join("、") || "—"}</td>
      <td>
        <Time ms={s.last_ts_ms ?? s.uploaded_ms} />
      </td>
      <td className="num">{formatBytes(s.bytes)}</td>
      <td>
        {t.sessions.versionsCount(s.versions)}
        {s.heads > 1 && (
          <>
            {" "}
            <strong>{t.sessions.forkMarker(s.heads)}</strong>
          </>
        )}
      </td>
      <td className="num">{s.children}</td>
    </tr>
  );
}

export function SessionsPage() {
  useTitle(t.sessions.title);
  const [params, setParams] = useSearchParams();
  const agent = params.get("agent") ?? "";
  const device = params.get("device") ?? "";
  const q = params.get("q") ?? "";
  const children = params.get("children") === "1";
  const page = Math.max(1, Math.floor(Number(params.get("page"))) || 1);

  const [qInput, setQInput] = useState(q);

  function update(patch: Record<string, string | null>, keepPage = false) {
    setParams(
      (prev) => {
        const next = new URLSearchParams(prev);
        for (const [key, value] of Object.entries(patch)) {
          if (value === null || value === "") next.delete(key);
          else next.set(key, value);
        }
        if (!keepPage) next.delete("page");
        return next;
      },
      { replace: true },
    );
  }

  // Debounce the search box into the URL.
  useEffect(() => {
    const trimmed = qInput.trim();
    if (trimmed === q) return;
    const id = setTimeout(() => update({ q: trimmed }), 300);
    return () => clearTimeout(id);
  }, [qInput, q]);

  const devices = useDevices();
  const sessions = useSessions({
    agent: agent || undefined,
    device: device || undefined,
    q: q || undefined,
    limit: PAGE_SIZE,
    offset: (page - 1) * PAGE_SIZE,
    children,
  });

  const deviceOptions = devices.data?.items ?? [];
  const knownDevice = deviceOptions.some((d) => d.id === device);

  function onSubmit(e: FormEvent) {
    e.preventDefault();
    update({ q: qInput.trim() });
  }

  const data = sessions.data;
  const from = data && data.items.length > 0 ? (page - 1) * PAGE_SIZE + 1 : 0;
  const to = data ? (page - 1) * PAGE_SIZE + data.items.length : 0;
  const hasNext = data ? to < data.total : false;

  return (
    <>
      <h1>{t.sessions.title}</h1>
      <form role="search" onSubmit={onSubmit}>
        <fieldset>
          <legend>{t.sessions.filters}</legend>
          <label>
            {t.sessions.agent}{" "}
            <select value={agent} onChange={(e) => update({ agent: e.target.value })}>
              <option value="">{t.sessions.allAgents}</option>
              {AGENTS.map((a) => (
                <option key={a} value={a}>
                  {a}
                </option>
              ))}
            </select>
          </label>{" "}
          <label>
            {t.sessions.device}{" "}
            <select value={device} onChange={(e) => update({ device: e.target.value })}>
              <option value="">{t.sessions.allDevices}</option>
              {device && !knownDevice && <option value={device}>{device}</option>}
              {deviceOptions.map((d) => (
                <option key={d.id} value={d.id}>
                  {d.name ? `${d.name} (${d.id})` : d.id}
                </option>
              ))}
            </select>
          </label>{" "}
          <label>
            {t.sessions.q}{" "}
            <input
              type="search"
              value={qInput}
              placeholder={t.sessions.qHint}
              onChange={(e) => setQInput(e.target.value)}
            />
          </label>{" "}
          <label>
            <input
              type="checkbox"
              checked={children}
              onChange={(e) => update({ children: e.target.checked ? "1" : null })}
            />{" "}
            {t.sessions.children}
          </label>
        </fieldset>
      </form>

      {sessions.isPending && <p role="status">{t.common.loading}</p>}
      {sessions.isError && <ErrorMessage error={sessions.error} onRetry={() => void sessions.refetch()} />}
      {data && (
        <>
          <p role="status">
            {data.total === 0 ? t.sessions.empty : t.sessions.range(from, to, data.total)}
            {sessions.isPlaceholderData && ` ${t.common.updating}`}
          </p>
          {data.items.length > 0 && (
            <div className="table-wrap">
              <table>
                <thead>
                  <tr>
                    <th scope="col">{t.sessions.colTitle}</th>
                    <th scope="col">{t.sessions.colCwd}</th>
                    <th scope="col">{t.sessions.colDevices}</th>
                    <th scope="col">{t.sessions.colLast}</th>
                    <th scope="col">{t.sessions.colSize}</th>
                    <th scope="col">{t.sessions.colVersions}</th>
                    <th scope="col">{t.sessions.colChildren}</th>
                  </tr>
                </thead>
                <tbody>
                  {data.items.map((s) => (
                    <SessionRow key={s.ref} s={s} />
                  ))}
                </tbody>
              </table>
            </div>
          )}
          <p>
            <button type="button" disabled={page <= 1} onClick={() => update({ page: String(page - 1) }, true)}>
              {t.common.prev}
            </button>{" "}
            <button type="button" disabled={!hasNext} onClick={() => update({ page: String(page + 1) }, true)}>
              {t.common.next}
            </button>
          </p>
        </>
      )}
    </>
  );
}
