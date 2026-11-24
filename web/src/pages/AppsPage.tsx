import { useOauthApps, useRevokeOauthApp } from "../api/hooks";
import type { OauthApp } from "../api/types";
import { CopyBox } from "../components/CopyBox";
import { describeError, QueryView } from "../components/QueryView";
import { Time } from "../components/Time";
import { t } from "../messages";
import { useTitle } from "../useTitle";

function appName(a: OauthApp): string {
  return a.client_name || t.apps.unnamed;
}

export function AppsPage() {
  useTitle(t.apps.title);
  const apps = useOauthApps();
  const revoke = useRevokeOauthApp();

  function onRevoke(a: OauthApp) {
    if (window.confirm(t.apps.confirmRevoke(appName(a)))) revoke.mutate(a.id);
  }

  return (
    <>
      <h1>{t.apps.title}</h1>
      <p>{t.apps.intro}</p>
      <QueryView query={apps}>
        {(data) => (
          <>
            <h2>{t.apps.connect}</h2>
            {data.mcp_url === null ? (
              <p className="warning">{t.apps.mcpOff}</p>
            ) : (
              <>
                <p>
                  {t.apps.mcpUrl} <code>{data.mcp_url}</code>
                </p>
                <p>{t.apps.connectHint}</p>
                <CopyBox label={t.apps.claudeCode} value={t.apps.claudeCommands(data.mcp_url)} rows={2} />
                <CopyBox label={t.apps.codex} value={t.apps.codexCommands(data.mcp_url)} rows={2} />
              </>
            )}

            <h2>{t.apps.listTitle}</h2>
            {revoke.isError && (
              <p role="alert" className="error">
                {t.apps.revokeFailed}
                {describeError(revoke.error)}
              </p>
            )}
            {data.items.length === 0 ? (
              <p>{t.apps.empty}</p>
            ) : (
              <div className="table-wrap">
                <table>
                  <thead>
                    <tr>
                      <th scope="col">{t.apps.colApp}</th>
                      <th scope="col">{t.apps.colReturns}</th>
                      <th scope="col">{t.apps.colCreated}</th>
                      <th scope="col">{t.apps.colLastUsed}</th>
                      <th scope="col">{t.apps.colAction}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {data.items.map((a) => (
                      <tr key={a.id}>
                        <td>
                          {appName(a)}
                          <br />
                          <small className="muted">
                            {a.metadata_document ? t.apps.fromDocument(a.client_id) : t.apps.registered}
                          </small>
                        </td>
                        <td>
                          <code>{a.redirect_host}</code>
                          {a.loopback && <span className="muted"> {t.apps.loopback}</span>}
                        </td>
                        <td>
                          <Time ms={a.created_ms} />
                        </td>
                        <td>{a.last_used_ms === null ? t.apps.neverUsed : <Time ms={a.last_used_ms} />}</td>
                        <td>
                          <button
                            type="button"
                            onClick={() => onRevoke(a)}
                            disabled={revoke.isPending && revoke.variables === a.id}
                          >
                            {revoke.isPending && revoke.variables === a.id ? t.apps.revoking : t.apps.revoke}
                          </button>
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </>
        )}
      </QueryView>
    </>
  );
}
