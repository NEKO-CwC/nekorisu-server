import { useState } from "react";
import { useActivity } from "../api/hooks";
import { ErrorMessage } from "../components/QueryView";
import { SessionLink } from "../components/SessionLink";
import { Time } from "../components/Time";
import { formatBytes } from "../format";
import { t } from "../messages";
import { useTitle } from "../useTitle";

const LIMITS = [50, 100, 200, 500];

export function ActivityPage() {
  useTitle(t.activity.title);
  const [limit, setLimit] = useState(100);
  const activity = useActivity(limit);

  return (
    <>
      <h1>{t.activity.title}</h1>
      <p>
        <label>
          {t.activity.limit}{" "}
          <select value={limit} onChange={(e) => setLimit(Number(e.target.value))}>
            {LIMITS.map((n) => (
              <option key={n} value={n}>
                {n}
              </option>
            ))}
          </select>
        </label>
      </p>
      {activity.isPending && <p role="status">{t.common.loading}</p>}
      {activity.isError && <ErrorMessage error={activity.error} onRetry={() => void activity.refetch()} />}
      {activity.data && (
        <>
          {activity.data.skipped_lines > 0 && (
            <p className="warning">{t.activity.skipped(activity.data.skipped_lines)}</p>
          )}
          {activity.data.items.length === 0 ? (
            <p>{t.activity.empty}</p>
          ) : (
            <div className="table-wrap">
              <table>
                <thead>
                  <tr>
                    <th scope="col">{t.activity.colTime}</th>
                    <th scope="col">{t.activity.colDevice}</th>
                    <th scope="col">{t.activity.colOp}</th>
                    <th scope="col">{t.activity.colSessions}</th>
                    <th scope="col">{t.activity.colTags}</th>
                    <th scope="col">{t.activity.colNote}</th>
                    <th scope="col">{t.activity.colBytes}</th>
                  </tr>
                </thead>
                <tbody>
                  {activity.data.items.map((a) => (
                    <tr key={`${a.device.id}:${a.seq}:${a.at_ms}`}>
                      <td>
                        <Time ms={a.at_ms} relative={false} />
                      </td>
                      <td>{a.device.name ?? a.device.id}</td>
                      <td>{a.op}</td>
                      <td>
                        {a.sessions.length === 0 ? (
                          "—"
                        ) : (
                          <ul>
                            {a.sessions.map((ref) => (
                              <li key={ref}>
                                <SessionLink sessionRef={ref} />
                              </li>
                            ))}
                          </ul>
                        )}
                      </td>
                      <td>{a.tags.join("、") || "—"}</td>
                      <td>{a.note ?? "—"}</td>
                      <td className="num">{formatBytes(a.bytes)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </>
      )}
    </>
  );
}
