import { Link } from "react-router";
import { t } from "../messages";
import { useTitle } from "../useTitle";

export function NotFoundPage() {
  useTitle(t.notFound.title);
  return (
    <>
      <h1>{t.notFound.title}</h1>
      <p>{t.notFound.body}</p>
      <p>
        <Link to="/">{t.notFound.home}</Link>
      </p>
    </>
  );
}
