//! The pages a person sees while an agent signs in. Rendered here as plain
//! HTML: no script, a few lines of style, every value escaped. Function
//! only; the look is designed later.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use super::client::{csp_origin, describe_redirect};

/// No script, no framing, nothing loaded from elsewhere.
const CSP: &str = "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'";

pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

fn document(title: &str, body: &str) -> String {
    format!(
        "<!doctype html>\n<html lang=\"zh-CN\">\n<head>\n<meta charset=\"utf-8\">\n\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n\
         <title>{title} · nekorisu</title>\n\
         <style>body{{font-family:system-ui,sans-serif;max-width:36rem;margin:2rem auto;padding:0 1rem;line-height:1.6}}\
         dt{{font-weight:bold}}.problem{{color:#b00}}</style>\n</head>\n<body>\n{body}\n</body>\n</html>\n",
        title = escape(title),
    )
}

fn respond(status: StatusCode, html: String, csp: String) -> Response {
    let mut response = (status, html).into_response();
    let h = response.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    if let Ok(value) = HeaderValue::from_str(&csp) {
        h.insert(header::CONTENT_SECURITY_POLICY, value);
    }
    response
}

/// Something is wrong with the request itself; nothing is redirected.
pub fn error(status: StatusCode, message: &str) -> Response {
    let body = format!(
        "<h1>无法授权</h1>\n<p>{}</p>\n<p>可以关掉这个页面，回到应用重新登录。</p>",
        escape(message)
    );
    respond(
        status,
        document("无法授权", &body),
        format!("{CSP}; form-action 'none'"),
    )
}

/// No administrator password yet, so nobody can approve anything.
pub fn no_admin() -> Response {
    let body = "<h1>还不能授权</h1>\n<p>这台服务端还没有设置管理员密码，所以没法确认是你本人在授权。</p>\n\
                <p>先<a href=\"/\">打开 panel</a>，用服务端日志里的设置码设好管理员密码，再回到应用重新登录。</p>";
    respond(
        StatusCode::OK,
        document("还不能授权", body),
        format!("{CSP}; form-action 'none'"),
    )
}

pub struct Consent<'a> {
    pub client_id: &'a str,
    pub client_name: &'a str,
    pub metadata_document: bool,
    pub redirect_uri: &'a str,
    pub request_id: &'a str,
    pub csrf: &'a str,
    /// Shown above the form, e.g. a wrong password.
    pub problem: Option<&'a str>,
}

/// The consent page: who asks, where the answer goes, what it gets, and the
/// administrator password to say yes.
pub fn consent(c: &Consent<'_>) -> Response {
    let name = if c.client_name.is_empty() {
        "（没有名字的应用）"
    } else {
        c.client_name
    };
    let identity = if c.metadata_document {
        let host = url::Url::parse(c.client_id)
            .ok()
            .and_then(|u| u.host_str().map(str::to_owned))
            .unwrap_or_default();
        format!(
            "名字取自 <code>{}</code> 上的应用说明（<code>{}</code>）。",
            escape(&host),
            escape(c.client_id)
        )
    } else {
        "名字是应用注册时自己填的，服务端无法核实。".to_string()
    };
    let (target, loopback) = describe_redirect(c.redirect_uri);
    let destination = if loopback {
        format!(
            "这台电脑上的一个程序（<code>{}</code>）。<strong>只有你刚刚在自己的电脑上运行了登录命令\
             （例如 <code>claude mcp login</code> 或 <code>codex mcp login</code>）时才批准</strong>：\
             别的程序也可以冒用一个应用的名字。",
            escape(&target)
        )
    } else {
        format!("<code>{}</code>", escape(&target))
    };
    let problem = c
        .problem
        .map(|p| format!("<p class=\"problem\" role=\"alert\">{}</p>\n", escape(p)))
        .unwrap_or_default();
    let body = format!(
        "<h1>授权一个应用读取 nekorisu</h1>\n\
         <p><strong>{name}</strong> 请求访问这台 nekorisu 服务端。</p>\n\
         <dl>\n\
         <dt>应用</dt><dd>{name}<br><small>{identity}</small></dd>\n\
         <dt>授权后返回到</dt><dd>{destination}</dd>\n\
         <dt>能做什么</dt><dd>只读：查看各台设备上传的会话和设备列表，按字段导出。不能修改或删除任何东西。\
         授权在 panel 的“应用”页可以随时撤销。</dd>\n\
         </dl>\n\
         {problem}\
         <form method=\"post\" action=\"/oauth/authorize\">\n\
         <input type=\"hidden\" name=\"request_id\" value=\"{request_id}\">\n\
         <input type=\"hidden\" name=\"csrf\" value=\"{csrf}\">\n\
         <p><label>panel 的管理员密码<br>\
         <input type=\"password\" name=\"password\" autocomplete=\"current-password\" autofocus></label></p>\n\
         <p><button type=\"submit\" name=\"action\" value=\"approve\">批准</button> \
         <button type=\"submit\" name=\"action\" value=\"deny\">拒绝</button></p>\n\
         </form>",
        name = escape(name),
        request_id = escape(c.request_id),
        csrf = escape(c.csrf),
    );
    // The form posts here and is then redirected to the app; browsers check
    // that redirect against form-action too, so the app's origin is allowed.
    let csp = match csp_origin(c.redirect_uri) {
        Some(origin) => format!("{CSP}; form-action 'self' {origin}"),
        None => CSP.to_string(),
    };
    respond(StatusCode::OK, document("授权", &body), csp)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escapes_markup() {
        assert_eq!(
            escape("<script>\"a\" & 'b'</script>"),
            "&lt;script&gt;&quot;a&quot; &amp; &#39;b&#39;&lt;/script&gt;"
        );
    }
}
