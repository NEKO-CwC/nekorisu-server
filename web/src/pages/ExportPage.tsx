import { useState } from "react";
import { useSearchParams } from "react-router";
import { buildQuery } from "../api/client";
import { useDevices, useExportFields } from "../api/hooks";
import type { ExportFormat } from "../api/types";
import { QueryView } from "../components/QueryView";
import { localInputToMs } from "../format";
import { t } from "../messages";
import { useTitle } from "../useTitle";

const AGENTS = ["claude-code", "codex"];
const FALLBACK_FORMATS: ExportFormat[] = ["jsonl", "csv"];

export function ExportPage() {
  useTitle(t.export.title);
  const [searchParams] = useSearchParams();
  const fieldsQuery = useExportFields();
  const devices = useDevices();

  const [format, setFormat] = useState<ExportFormat>("jsonl");
  // null means "all fields selected" (the default, before the user touches the checklist).
  const [selected, setSelected] = useState<Set<string> | null>(null);
  const [agent, setAgent] = useState("");
  const [device, setDevice] = useState("");
  const [sessionText, setSessionText] = useState(() => searchParams.getAll("session").join("\n"));
  const [since, setSince] = useState("");
  const [until, setUntil] = useState("");

  const allNames = fieldsQuery.data?.fields.map((f) => f.name) ?? [];
  const chosen = selected ?? new Set(allNames);
  const allSelected = allNames.length > 0 && allNames.every((n) => chosen.has(n));

  const sinceMs = localInputToMs(since);
  const untilMs = localInputToMs(until);
  const badRange = sinceMs !== undefined && untilMs !== undefined && sinceMs > untilMs;
  const noFields = allNames.length > 0 && chosen.size === 0;
  const canDownload = fieldsQuery.isSuccess && !noFields && !badRange;

  function toggleField(name: string, on: boolean) {
    const next = new Set(chosen);
    if (on) next.add(name);
    else next.delete(name);
    setSelected(next);
  }

  function download() {
    if (!canDownload) return;
    const sessions = sessionText.split(/[\s,]+/).filter(Boolean);
    const fields = allSelected ? undefined : allNames.filter((n) => chosen.has(n)).join(",");
    const url = `/api/export${buildQuery({
      format,
      fields,
      agent,
      device,
      session: sessions,
      since_ms: sinceMs,
      until_ms: untilMs,
    })}`;
    // The response is an attachment, so the page stays where it is.
    window.location.assign(url);
  }

  return (
    <>
      <h1>{t.export.title}</h1>
      <p>{t.export.explain}</p>
      <form
        onSubmit={(e) => {
          e.preventDefault();
          download();
        }}
      >
        <fieldset>
          <legend>{t.export.format}</legend>
          {(fieldsQuery.data?.formats ?? FALLBACK_FORMATS).map((f) => (
            <label key={f}>
              <input type="radio" name="format" value={f} checked={format === f} onChange={() => setFormat(f)} />{" "}
              {t.export.formats[f] ?? f}{" "}
            </label>
          ))}
        </fieldset>

        <fieldset>
          <legend>{t.export.fields}</legend>
          <QueryView query={fieldsQuery}>
            {({ fields }) => (
              <>
                <p>
                  <button type="button" onClick={() => setSelected(new Set(fields.map((f) => f.name)))}>
                    {t.export.selectAll}
                  </button>{" "}
                  <button type="button" onClick={() => setSelected(new Set())}>
                    {t.export.clear}
                  </button>
                </p>
                <ul>
                  {fields.map((f) => (
                    <li key={f.name}>
                      <label>
                        <input
                          type="checkbox"
                          checked={chosen.has(f.name)}
                          onChange={(e) => toggleField(f.name, e.target.checked)}
                        />{" "}
                        <code>{f.name}</code> — {f.description}
                      </label>
                    </li>
                  ))}
                </ul>
                {noFields && <p className="error">{t.export.noFields}</p>}
              </>
            )}
          </QueryView>
        </fieldset>

        <fieldset>
          <legend>{t.export.filters}</legend>
          <p>
            <label>
              {t.export.agent}{" "}
              <select value={agent} onChange={(e) => setAgent(e.target.value)}>
                <option value="">{t.export.allAgents}</option>
                {AGENTS.map((a) => (
                  <option key={a} value={a}>
                    {a}
                  </option>
                ))}
              </select>
            </label>{" "}
            <label>
              {t.export.device}{" "}
              <select value={device} onChange={(e) => setDevice(e.target.value)}>
                <option value="">{t.export.allDevices}</option>
                {(devices.data?.items ?? []).map((d) => (
                  <option key={d.id} value={d.id}>
                    {d.name ? `${d.name} (${d.id})` : d.id}
                  </option>
                ))}
              </select>
            </label>
          </p>
          <p>
            <label>
              {t.export.session}
              <br />
              <textarea
                className="mono"
                rows={3}
                cols={40}
                spellCheck={false}
                value={sessionText}
                onChange={(e) => setSessionText(e.target.value)}
                aria-describedby="export-session-hint"
              />
            </label>
            <br />
            <span id="export-session-hint" className="muted">
              {t.export.sessionHint}
            </span>
          </p>
          <p>
            <label>
              {t.export.since}{" "}
              <input type="datetime-local" value={since} onChange={(e) => setSince(e.target.value)} />
            </label>{" "}
            <label>
              {t.export.until}{" "}
              <input type="datetime-local" value={until} onChange={(e) => setUntil(e.target.value)} />
            </label>
          </p>
          {badRange && <p className="error">{t.export.badRange}</p>}
        </fieldset>

        <p>
          <button type="submit" disabled={!canDownload}>
            {t.export.download}
          </button>
        </p>
      </form>
    </>
  );
}
