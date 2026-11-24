import { Link } from "react-router";
import { useOverview, useRefresh } from "../api/hooks";
import { QueryView, describeError } from "../components/QueryView";
import { Time } from "../components/Time";
import { formatBytes, formatNumber } from "../format";
import { t } from "../messages";
import { useTitle } from "../useTitle";

export function OverviewPage() {
  useTitle(t.overview.title);
  const overview = useOverview();
  const refresh = useRefresh();

  return (
    <>
      <h1>{t.overview.title}</h1>
      <QueryView query={overview}>
        {(o) => (
          <>
            <dl className="fields">
              <dt>{t.overview.sessions}</dt>
              <dd>{formatNumber(o.counts.sessions)}</dd>
              <dt>{t.overview.rootSessions}</dt>
              <dd>{formatNumber(o.counts.root_sessions)}</dd>
              <dt>{t.overview.byAgent}</dt>
              <dd>
                {Object.entries(o.counts.by_agent).map(([agent, n], i) => (
                  <span key={agent}>
                    {i > 0 && "，"}
                    {agent}: {formatNumber(n)}
                  </span>
                ))}
              </dd>
              <dt>{t.overview.devices}</dt>
              <dd>{formatNumber(o.counts.devices)}</dd>
              <dt>{t.overview.bytes}</dt>
              <dd>{formatBytes(o.counts.bytes)}</dd>
              <dt>{t.overview.lastUpload}</dt>
              <dd>
                <Time ms={o.last_upload_ms} />
              </dd>
              <dt>{t.overview.config}</dt>
              <dd>
                {o.config ? (
                  <>
                    <Link to="/config">{t.overview.configSerial(o.config.serial)}</Link>，{t.overview.configIssued}{" "}
                    <Time ms={o.config.issued_at_ms} />
                  </>
                ) : (
                  t.overview.configNone
                )}
              </dd>
            </dl>

            <h2>{t.overview.storage}</h2>
            <dl className="fields">
              <dt>{t.storage.status}</dt>
              <dd>
                <Link to="/storage">
                  {o.storage.ok ? t.overview.storageOk : <strong>{t.overview.storageError}</strong>}
                </Link>
                {o.storage.error && (
                  <>
                    {" "}
                    <span className="error">{o.storage.error}</span>
                  </>
                )}
              </dd>
              <dt>{t.storage.description}</dt>
              <dd>
                <code>{o.storage.description}</code>
              </dd>
            </dl>

            <h2>{t.overview.index}</h2>
            <dl className="fields">
              <dt>{t.overview.refreshedAt}</dt>
              <dd>{o.index.refreshed_at_ms === null ? t.overview.notRefreshed : <Time ms={o.index.refreshed_at_ms} />}</dd>
              {o.index.skipped_files > 0 && (
                <>
                  <dt />
                  <dd className="warning">{t.overview.skippedFiles(o.index.skipped_files)}</dd>
                </>
              )}
            </dl>
          </>
        )}
      </QueryView>
      <p>
        <button type="button" onClick={() => refresh.mutate()} disabled={refresh.isPending}>
          {refresh.isPending ? t.overview.refreshing : t.overview.refresh}
        </button>{" "}
        <span className="muted">{t.overview.refreshHint}</span>
      </p>
      <div role="status">
        {refresh.isSuccess && <p>{t.overview.refreshed(refresh.data.sessions)}</p>}
        {refresh.isError && (
          <p role="alert" className="error">
            {describeError(refresh.error)}
          </p>
        )}
      </div>
    </>
  );
}
