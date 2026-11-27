import { useStorage } from "../api/hooks";
import { QueryView } from "../components/QueryView";
import { t } from "../messages";
import { useTitle } from "../useTitle";

export function StoragePage() {
  useTitle(t.storage.title);
  const storage = useStorage();

  return (
    <>
      <h1>{t.storage.title}</h1>
      <QueryView query={storage}>
        {(s) => (
          <dl className="fields">
            <dt>{t.storage.description}</dt>
            <dd>
              <code>{s.description}</code>
            </dd>
            <dt>{t.storage.forDevices}</dt>
            <dd>
              <dl className="fields">
                <dt>{t.storage.address}</dt>
                <dd>{s.address ? <code>{s.address}</code> : t.storage.notSet}</dd>
                <dt>{t.storage.username}</dt>
                <dd>{s.username ? <code>{s.username}</code> : t.storage.notSet}</dd>
              </dl>
              <p className="muted">{t.storage.forDevicesHint}</p>
            </dd>
            <dt>{t.storage.status}</dt>
            <dd>
              {s.ok ? (
                t.storage.ok
              ) : (
                <strong className="error">
                  {t.storage.error}
                  {s.error ? `：${s.error}` : ""}
                </strong>
              )}
            </dd>
            <dt>{t.storage.latency}</dt>
            <dd>{s.latency_ms === null ? "—" : t.storage.latencyMs(s.latency_ms)}</dd>
            <dt>{t.storage.rangeRead}</dt>
            <dd>
              {s.range_read === null ? (
                t.storage.rangeUntested
              ) : s.range_read ? (
                t.storage.rangeYes
              ) : (
                <strong className="warning">{t.storage.rangeNo}</strong>
              )}
            </dd>
            <dt>{t.storage.layout}</dt>
            <dd>{s.layout.found ? t.storage.layoutFound : <strong className="warning">{t.storage.layoutMissing}</strong>}</dd>
            <dt>{t.storage.entries}</dt>
            <dd>
              {s.layout.entries.length === 0 ? (
                t.storage.noEntries
              ) : (
                <ul>
                  {s.layout.entries.map((e) => (
                    <li key={e}>
                      <code>{e}</code>
                    </li>
                  ))}
                </ul>
              )}
            </dd>
          </dl>
        )}
      </QueryView>
    </>
  );
}
