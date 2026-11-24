import { useState, type FormEvent } from "react";
import { Link, useParams, useSearchParams } from "react-router";
import { useSession, useSessionLines } from "../api/hooks";
import type { LineKind, SessionDetail, SessionLine } from "../api/types";
import { CollapsibleText } from "../components/CollapsibleText";
import { ErrorMessage } from "../components/QueryView";
import { SessionLink, sessionHref } from "../components/SessionLink";
import { Time } from "../components/Time";
import { formatBytes, formatNumber } from "../format";
import { t } from "../messages";
import { useTitle } from "../useTitle";

const ALL_KINDS: LineKind[] = ["user", "assistant", "reasoning", "tool", "meta"];
const DEFAULT_KINDS: LineKind[] = ["user", "assistant"];

function LineItem({ item }: { item: SessionLine }) {
  return (
    <article className="line" aria-label={t.reader.lineNo(item.line)}>
      <header>
        <code>#{item.line}</code> <strong>[{t.reader.kindLabels[item.kind] ?? item.kind}]</strong>{" "}
        {item.raw_type && <span className="muted">{item.raw_type}</span>} <Time ms={item.ts_ms} relative={false} />
      </header>
      {item.text === "" ? <p className="muted">{t.reader.emptyText}</p> : <CollapsibleText text={item.text} />}
      {item.text_truncated && (
        <p className="warning">
          {t.reader.textTruncated} <span className="muted">{t.reader.rawLen(item.raw_len)}</span>
        </p>
      )}
    </article>
  );
}

function parseLineNumber(value: string): number | undefined {
  const n = Math.floor(Number(value));
  return value.trim() !== "" && Number.isFinite(n) && n >= 1 ? n : undefined;
}

interface ReaderProps {
  agent: string;
  id: string;
  version: string | undefined;
  file: string | undefined;
}

function Reader({ agent, id, version, file }: ReaderProps) {
  const [kinds, setKinds] = useState<LineKind[]>(DEFAULT_KINDS);
  const [fromInput, setFromInput] = useState("");
  const [toInput, setToInput] = useState("");
  const [range, setRange] = useState<{ from?: number; to?: number }>({});

  const lines = useSessionLines(agent, id, { version, file, from: range.from, to: range.to, kinds });
  const items = lines.data?.pages.flatMap((p) => p.items) ?? [];
  const first = lines.data?.pages[0];
  const last = lines.data?.pages[lines.data.pages.length - 1];

  function toggle(kind: LineKind, on: boolean) {
    setKinds(ALL_KINDS.filter((k) => (k === kind ? on : kinds.includes(k))));
  }

  const from = parseLineNumber(fromInput);
  const to = parseLineNumber(toInput);
  const badRange = from !== undefined && to !== undefined && from > to;

  function onJump(e: FormEvent) {
    e.preventDefault();
    if (badRange) return;
    setRange({ from, to });
  }

  function clearRange() {
    setFromInput("");
    setToInput("");
    setRange({});
  }

  return (
    <section id="reader" aria-labelledby="reader-title">
      <h2 id="reader-title">{t.reader.title}</h2>
      <p className="muted">{t.reader.untrusted}</p>
      <fieldset>
        <legend>{t.reader.kinds}</legend>
        {ALL_KINDS.map((k) => (
          <label key={k}>
            <input type="checkbox" checked={kinds.includes(k)} onChange={(e) => toggle(k, e.target.checked)} />{" "}
            {t.reader.kindLabels[k]}{" "}
          </label>
        ))}
      </fieldset>
      <form onSubmit={onJump}>
        <label>
          {t.reader.from}{" "}
          <input type="number" min={1} value={fromInput} onChange={(e) => setFromInput(e.target.value)} />
        </label>{" "}
        <label>
          {t.reader.to}{" "}
          <input type="number" min={1} value={toInput} onChange={(e) => setToInput(e.target.value)} />
        </label>{" "}
        <button type="submit" disabled={badRange}>
          {t.reader.apply}
        </button>{" "}
        <button type="button" onClick={clearRange} disabled={range.from === undefined && range.to === undefined}>
          {t.reader.clearRange}
        </button>
      </form>

      {kinds.length === 0 && <p role="status">{t.reader.noKinds}</p>}
      {kinds.length > 0 && lines.isPending && <p role="status">{t.common.loading}</p>}
      {lines.isError && <ErrorMessage error={lines.error} onRetry={() => void lines.refetch()} />}
      {first && last && (
        <>
          <p role="status">
            {t.reader.reading(first.file, first.version)} · {t.reader.stats(first.total_lines, last.matched, items.length)}
          </p>
          {items.length === 0 && <p>{t.reader.empty}</p>}
          <div>
            {items.map((item) => (
              <LineItem key={item.line} item={item} />
            ))}
          </div>
          <p>
            {lines.hasNextPage ? (
              <button type="button" onClick={() => void lines.fetchNextPage()} disabled={lines.isFetchingNextPage}>
                {lines.isFetchingNextPage ? t.reader.loadingMore : t.reader.loadMore}
              </button>
            ) : (
              items.length > 0 && <span className="muted">{t.reader.end}</span>
            )}
          </p>
        </>
      )}
    </section>
  );
}

function Summary({ detail }: { detail: SessionDetail }) {
  const s = detail.summary;
  return (
    <>
      <h2>{t.session.summary}</h2>
      <dl className="fields">
        <dt>{t.session.ref}</dt>
        <dd>
          <code>{s.ref}</code>
        </dd>
        <dt>{t.session.agent}</dt>
        <dd>{s.agent}</dd>
        <dt>{t.session.cwd}</dt>
        <dd>
          <code>{s.cwd ?? "—"}</code>
        </dd>
        <dt>{t.session.agentVersion}</dt>
        <dd>{s.agent_version ?? "—"}</dd>
        <dt>{t.session.devices}</dt>
        <dd>{s.devices.map((d) => (d.name ? `${d.name} (${d.id})` : d.id)).join("、") || "—"}</dd>
        <dt>{t.session.firstTs}</dt>
        <dd>
          <Time ms={s.first_ts_ms} />
        </dd>
        <dt>{t.session.lastTs}</dt>
        <dd>
          <Time ms={s.last_ts_ms} />
        </dd>
        <dt>{t.session.uploaded}</dt>
        <dd>
          <Time ms={s.uploaded_ms} />
        </dd>
        <dt>{t.session.bytes}</dt>
        <dd>{formatBytes(s.bytes)}</dd>
        <dt>{t.session.files}</dt>
        <dd>{formatNumber(s.files)}</dd>
        <dt>{t.session.versions}</dt>
        <dd>{formatNumber(s.versions)}</dd>
        <dt>{t.session.heads}</dt>
        <dd>
          {s.heads}
          {s.heads > 1 && (
            <>
              {" "}
              <strong>{t.sessions.forkMarker(s.heads)}</strong>
            </>
          )}
        </dd>
        <dt>{t.session.parent}</dt>
        <dd>{s.parent ? <SessionLink sessionRef={s.parent} /> : "—"}</dd>
        <dt>{t.session.childrenCount}</dt>
        <dd>{s.children}</dd>
        {detail.fork_from && (
          <>
            <dt>{t.session.forkFrom}</dt>
            <dd>
              <SessionLink sessionRef={detail.fork_from.session} /> {t.session.forkFromAt(detail.fork_from.byte)}
            </dd>
          </>
        )}
      </dl>
      {s.heads > 1 && <p className="warning">{t.session.forkWarning(s.heads)}</p>}
      <p>
        <Link to={`/export?${new URLSearchParams({ session: s.ref }).toString()}`}>{t.session.exportThis}</Link>
      </p>
    </>
  );
}

export function SessionDetailPage() {
  const { agent = "", id = "" } = useParams();
  const [params, setParams] = useSearchParams();
  const version = params.get("version") || undefined;
  const file = params.get("file") || undefined;
  const detail = useSession(agent, id, version);

  useTitle(detail.data?.summary.title || `${agent}:${id}`);

  function select(patch: { version?: string; file?: string }) {
    setParams((prev) => {
      const next = new URLSearchParams(prev);
      if (patch.version !== undefined) {
        next.set("version", patch.version);
        next.delete("file"); // the file may not exist in another version
      }
      if (patch.file !== undefined) next.set("file", patch.file);
      return next;
    });
    if (patch.file !== undefined) {
      document.getElementById("reader")?.scrollIntoView();
    }
  }

  const d = detail.data;
  const activeFile = file ?? d?.files.find((f) => f.role === "transcript")?.path;

  return (
    <>
      <p>
        <Link to="/sessions">{t.session.back}</Link>
      </p>
      <h1>{d ? d.summary.title || t.sessions.untitled : `${agent}:${id}`}</h1>
      {detail.isPending && <p role="status">{t.common.loading}</p>}
      {detail.isError && <ErrorMessage error={detail.error} onRetry={() => void detail.refetch()} />}
      {d && (
        <>
          <Summary detail={d} />

          <section aria-labelledby="versions-title">
            <h2 id="versions-title">{t.session.versionsTitle}</h2>
            <div className="table-wrap">
              <table>
                <thead>
                  <tr>
                    <th scope="col">{t.session.colVersion}</th>
                    <th scope="col">{t.session.colVersionParent}</th>
                    <th scope="col">{t.session.colDevice}</th>
                    <th scope="col">{t.session.colCreated}</th>
                    <th scope="col">{t.session.colSize}</th>
                    <th scope="col">{t.session.colMarks}</th>
                    <th scope="col">{t.session.colAction}</th>
                  </tr>
                </thead>
                <tbody>
                  {d.versions.map((v) => {
                    const selected = v.id === d.selected_version;
                    return (
                      <tr key={v.id}>
                        <td>
                          <code>{v.id}</code>
                          {selected && <strong> ◀</strong>}
                        </td>
                        <td>{v.parent ? <code>{v.parent}</code> : "—"}</td>
                        <td>{v.device.name ?? v.device.id}</td>
                        <td>
                          <Time ms={v.created_ms} />
                        </td>
                        <td className="num">{formatBytes(v.bytes)}</td>
                        <td>
                          {v.is_head && (
                            <strong>{d.summary.heads > 1 ? t.session.forkHeadMarker : t.session.headMarker}</strong>
                          )}{" "}
                          {v.rewrite && <strong>{t.session.rewriteMarker}</strong>}
                        </td>
                        <td>
                          <button
                            type="button"
                            aria-pressed={selected}
                            disabled={selected}
                            onClick={() => select({ version: v.id })}
                          >
                            {selected ? t.session.currentVersion : t.session.viewVersion}
                          </button>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          </section>

          <section aria-labelledby="files-title">
            <h2 id="files-title">{t.session.filesTitle}</h2>
            <div className="table-wrap">
              <table>
                <thead>
                  <tr>
                    <th scope="col">{t.session.colPath}</th>
                    <th scope="col">{t.session.colRole}</th>
                    <th scope="col">{t.session.colSize}</th>
                    <th scope="col">{t.session.colLines}</th>
                    <th scope="col">{t.session.colSegments}</th>
                    <th scope="col">{t.session.colAction}</th>
                  </tr>
                </thead>
                <tbody>
                  {d.files.map((f) => {
                    const active = f.path === activeFile;
                    return (
                      <tr key={f.path}>
                        <td>
                          <code>{f.path}</code>
                        </td>
                        <td>{t.session.roles[f.role] ?? f.role}</td>
                        <td className="num">{formatBytes(f.bytes)}</td>
                        <td className="num">{formatNumber(f.lines)}</td>
                        <td className="num">{f.segments}</td>
                        <td>
                          <button
                            type="button"
                            aria-pressed={active}
                            disabled={active}
                            onClick={() => select({ file: f.path })}
                          >
                            {active ? t.session.reading : t.session.readFile}
                          </button>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          </section>

          <Reader key={`${agent}:${id}`} agent={agent} id={id} version={version} file={file} />

          <section aria-labelledby="children-title">
            <h2 id="children-title">{t.session.childrenTitle}</h2>
            {d.children.length === 0 ? (
              <p>{t.session.noChildren}</p>
            ) : (
              <ul>
                {d.children.map((c) => (
                  <li key={c.ref}>
                    <Link to={sessionHref(c.agent, c.id)}>{c.title || t.sessions.untitled}</Link> <code>{c.ref}</code>{" "}
                    <Time ms={c.last_ts_ms ?? c.uploaded_ms} />
                  </li>
                ))}
              </ul>
            )}
          </section>

          <section aria-labelledby="digests-title">
            <h2 id="digests-title">{t.session.digestsTitle}</h2>
            {d.digests.length === 0 ? (
              <p>{t.session.noDigests}</p>
            ) : (
              <table>
                <thead>
                  <tr>
                    <th scope="col">{t.session.colDigest}</th>
                    <th scope="col">{t.session.colUpdated}</th>
                  </tr>
                </thead>
                <tbody>
                  {d.digests.map((dg) => (
                    <tr key={dg.id}>
                      <td>
                        <code>{dg.id}</code>
                      </td>
                      <td>
                        <Time ms={dg.updated_ms} />
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            )}
          </section>
        </>
      )}
    </>
  );
}
