//! Connection notes: what a person or an agent needs to connect a device to
//! this server, readable without signing in. `/.well-known/nekorisu` is JSON
//! for programs; `/connect` is the same as Markdown (for `curl` and agents) or,
//! for a browser, a plain HTML page.
//!
//! Nothing secret is on them: no password, token or private key. The storage
//! address and user name say where the archive lives, so they are given only
//! after sign-in (the MCP tool `setup_info`, the panel's storage page), unless
//! NEKORISU_CONNECT_SHOW_STORAGE is on. NEKORISU_CONNECT_PAGE=false turns both
//! paths off (404). Building the notes reads no storage, so they stay cheap.

use std::collections::HashMap;

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::state::Shared;

/// The first client release that has `nekorisu source`.
pub const CLIENT_MIN_VERSION: &str = "0.2.0";
const MANUAL: &str = "https://raw.githubusercontent.com/NEKO-CwC/nekorisu/main/docs/agent-install.md";
const DEVICE_PROTOCOL: &str = "nekorisu.cloud.v1";

const GLOSSARY: [(&str, &str); 8] = [
    (
        "服务端",
        "这台 nekorisu-server：在一个地方看所有电脑存进来的会话，也给设备下发设置。",
    ),
    ("panel", "服务端的网页，用管理员密码登录。"),
    ("客户端", "装在每台电脑上的 nekorisu 命令，和它在后台运行的 core。"),
    (
        "存储",
        "放会话的 WebDAV 文件夹（NAS、网盘、OpenList / Alist）。客户端往里写，服务端从里读，两边要填同一个地址；会话放在它下面的 nekorisu/v1/。",
    ),
    (
        "MCP",
        "Agent 调用工具的接口。服务端的 MCP 让 Agent 读所有电脑的会话，登录在浏览器里完成。",
    ),
    (
        "设备令牌",
        "电脑向服务端取设置、上报状态时用的口令，部署时设的 NEKORISU_DEVICE_TOKEN；不是 panel 的管理员密码。",
    ),
    (
        "公钥",
        "服务端给下发的设置签名，电脑用这把公钥核对设置确实来自这台服务端。",
    ),
    (
        "来源",
        "客户端对服务端的叫法：nekorisu source add 把这台服务端设成这台电脑的来源。",
    ),
];

fn missing(setting: &str, effect: &str, fix: &str) -> Value {
    json!({ "setting": setting, "effect": effect, "fix": fix })
}

/// The notes as data. `with_storage` adds the storage address and user name.
pub async fn info(s: &Shared, with_storage: bool) -> Value {
    let set = &s.settings;
    let public = set.public_url.clone();
    let device_url = set.device_url.clone().or_else(|| public.clone());
    let mcp = s.oauth.endpoints().map(|ep| ep.resource.clone());
    let ready = s.auth.initialized().await.ok();
    let key = s.signer.public_key();
    let dav = set.webdav.as_ref();

    let mut gaps = Vec::new();
    if ready == Some(false) {
        gaps.push(missing(
            "管理员密码",
            "还没设：panel 登录不了，MCP 也授权不了。",
            "打开 panel，用启动日志里的设置码（Setup code）设一个。",
        ));
    }
    if public.is_none() {
        gaps.push(missing(
            "NEKORISU_PUBLIC_URL",
            "没设：MCP 关着，连接命令里也填不了服务端的地址。",
            "设成人和 Agent 访问服务端用的地址，比如 https://nekorisu.example.com（端口不是 443 就带上），然后重启。",
        ));
    }
    let local_only = |url: &Option<String>| {
        url.as_deref()
            .and_then(|u| url::Url::parse(u).ok())
            .is_some_and(|u| matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
    };
    if local_only(&public) || local_only(&device_url) {
        gaps.push(missing(
            "NEKORISU_PUBLIC_URL",
            "是本机地址（localhost）：只有服务端这台机器自己连得上，别的电脑照下面的命令连不上。",
            "要接别的电脑，就设成它们能访问的地址（https），然后重启；只在这台机器上试用的话可以不管。",
        ));
    }
    if dav.is_none() {
        gaps.push(missing(
            "NEKORISU_WEBDAV_URL",
            "没设：服务端读不到任何会话。",
            "设成存储的地址（nekorisu/ 的上一层），和各台电脑填同一个；再设 NEKORISU_WEBDAV_USER 和密码，然后重启。",
        ));
    }
    if let Some(d) = dav
        && d.device_url.is_none()
        && crate::settings::internal_host(&d.url)
    {
        gaps.push(missing(
            "NEKORISU_DEVICE_WEBDAV_URL",
            "服务端读存储用的是内部地址（集群里或本机上的），别的电脑连不上它，setup_info 和 panel 给电脑的地址也就不对。",
            "设成电脑访问同一个存储文件夹用的地址（nekorisu/ 的上一层），然后重启。",
        ));
    }
    if set.device_token.is_none() {
        gaps.push(missing(
            "NEKORISU_DEVICE_TOKEN",
            "没设：电脑取不到服务端的设置，也不能上报。",
            "设一个随机的长字符串（比如 openssl rand -hex 32 的输出），然后重启。",
        ));
    }

    let mcp_info = match &mcp {
        Some(url) => json!({
            "on": true,
            "url": url,
            "sign_in": "OAuth：运行 login 命令后浏览器会打开授权页，输入 panel 的管理员密码批准。不用复制令牌。",
            "commands": {
                "claude-code": [
                    format!("claude mcp add --transport http -s user nekorisu-server {url}"),
                    "claude mcp login nekorisu-server".to_string(),
                ],
                "codex": [
                    format!("codex mcp add nekorisu-server --url {url}"),
                    "codex mcp login nekorisu-server".to_string(),
                ],
            },
            "setup_tool": "setup_info",
        }),
        None => json!({ "on": false, "url": null, "why": "没设 NEKORISU_PUBLIC_URL" }),
    };

    let device = match &device_url {
        Some(url) => json!({
            "on": set.device_token.is_some(),
            "url": url,
            "protocol": DEVICE_PROTOCOL,
            "key_id": s.signer.key_id(),
            "public_key": key,
            "commands": [
                format!("nekorisu source add home {url} --key {key}"),
                "nekorisu source login".to_string(),
            ],
            "token": "设备令牌在这台电脑的终端里输入（nekorisu source login），不要发给 Agent。",
            "client_min_version": CLIENT_MIN_VERSION,
        }),
        None => json!({ "on": false, "url": null, "why": "没设 NEKORISU_PUBLIC_URL（或 NEKORISU_DEVICE_URL）" }),
    };

    let storage_note = match (dav, with_storage) {
        (None, _) => "服务端还没配存储（NEKORISU_WEBDAV_URL）。",
        (Some(_), true) => {
            "电脑上填同一个地址和用户名：nekorisu storage add <名字> --url <地址> --username <用户名>。密码在这台电脑的终端里输入（nekorisu storage login <名字>），不要发给 Agent。"
        }
        (Some(_), false) => {
            "地址和用户名登录后才给：接好 MCP、重启 Agent 后调用服务端的 setup_info，或者在 panel 的“存储”页看。"
        }
    };
    let storage = json!({
        "configured": dav.is_some(),
        "kind": dav.map(|_| "webdav"),
        "shown": with_storage,
        "address": if with_storage { dav.map(|d| d.for_devices().to_string()) } else { None },
        "username": if with_storage { dav.and_then(|d| d.user.clone()) } else { None },
        "note": storage_note,
    });

    json!({
        "service": "nekorisu-server",
        "version": env!("CARGO_PKG_VERSION"),
        "commit": crate::api::COMMIT,
        "address": public,
        "notes": public.as_ref().map(|u| json!({ "markdown": format!("{u}/connect"), "json": format!("{u}/.well-known/nekorisu") })),
        "panel": { "url": public, "ready": ready },
        "mcp": mcp_info,
        "device": device,
        "storage": storage,
        "client": { "min_version": CLIENT_MIN_VERSION, "manual": MANUAL },
        "missing": gaps,
        "glossary": GLOSSARY.iter().map(|(term, meaning)| json!({ "term": term, "meaning": meaning })).collect::<Vec<_>>(),
    })
}

// ---- the page ----

enum Block {
    H1(String),
    H2(String),
    P(String),
    Items(Vec<String>),
    Code(Vec<String>),
}

fn text(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

fn lines(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().map(text).collect()
}

fn blocks(i: &Value) -> Vec<Block> {
    use Block::*;
    let mut out = vec![
        H1("nekorisu 服务端：连接说明".into()),
        P("这一页不用登录，写给要把电脑接到这台服务端的人和 Agent。页面上没有密码、令牌这类秘密。".into()),
    ];
    if let Some(json) = i["notes"]["json"].as_str() {
        out.push(P(format!("给程序读的版本：`{json}`")));
    }

    let on = |v: &Value| v.as_bool() == Some(true);
    let mut state = vec![format!("版本：{}（{}）", text(&i["version"]), text(&i["commit"]))];
    state.push(match i["address"].as_str() {
        Some(a) => format!("地址：`{a}`"),
        None => "地址：没设（NEKORISU_PUBLIC_URL）".into(),
    });
    state.push(match i["panel"]["ready"].as_bool() {
        Some(true) => "panel：已设管理员密码".into(),
        Some(false) => "panel：还没设管理员密码".into(),
        None => "panel：查不到状态".into(),
    });
    state.push(if on(&i["mcp"]["on"]) {
        format!("MCP：开着，`{}`", text(&i["mcp"]["url"]))
    } else {
        format!("MCP：关着（{}）", text(&i["mcp"]["why"]))
    });
    state.push(match (i["device"]["url"].as_str(), on(&i["device"]["on"])) {
        (Some(u), true) => format!("电脑取设置：开着，`{u}`"),
        (Some(_), false) => "电脑取设置：关着（没设 NEKORISU_DEVICE_TOKEN）".into(),
        (None, _) => format!("电脑取设置：关着（{}）", text(&i["device"]["why"])),
    });
    state.push(if on(&i["storage"]["configured"]) {
        "存储：已配置（WebDAV）".into()
    } else {
        "存储：没配置".into()
    });
    out.push(H2("这台服务端".into()));
    out.push(Items(state));

    let gaps: Vec<String> = i["missing"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|g| format!("**{}**：{}{}", text(&g["setting"]), text(&g["effect"]), text(&g["fix"])))
        .collect();
    if !gaps.is_empty() {
        out.push(H2("还差什么（给部署的人）".into()));
        out.push(Items(gaps));
    }

    out.push(H2("给 Agent：怎么接".into()));
    out.push(P(format!(
        "整个安装照客户端的手册走：{MANUAL} 。手册里和这台服务端有关的几步，要用的东西都在下面。"
    )));

    out.push(P("**1. 接上服务端的 MCP。**".into()));
    if on(&i["mcp"]["on"]) {
        out.push(P("Claude Code：".into()));
        out.push(Code(lines(&i["mcp"]["commands"]["claude-code"])));
        out.push(P("Codex：".into()));
        out.push(Code(lines(&i["mcp"]["commands"]["codex"])));
        out.push(P(format!(
            "{}重启 Agent 以后，调用服务端的 `setup_info`，它会给出存储的地址和用户名。",
            text(&i["mcp"]["sign_in"])
        )));
    } else {
        out.push(P("MCP 现在关着，先请部署的人补上上面缺的设置。".into()));
    }

    out.push(P("**2. 接上存储。**".into()));
    if on(&i["storage"]["shown"]) && on(&i["storage"]["configured"]) {
        out.push(Items(vec![
            format!("地址：`{}`", text(&i["storage"]["address"])),
            match i["storage"]["username"].as_str() {
                Some(u) => format!("用户名：`{u}`"),
                None => "用户名：没设".into(),
            },
        ]));
    }
    out.push(P(text(&i["storage"]["note"])));

    out.push(P("**3. 让电脑从服务端取设置（可选）。**".into()));
    if i["device"]["url"].is_string() {
        if !on(&i["device"]["on"]) {
            out.push(P(
                "现在还用不了：服务端没设 NEKORISU_DEVICE_TOKEN，先请部署的人补上。下面的命令补好以后照样能用。".into(),
            ));
        }
        out.push(P(format!(
            "要客户端 {} 或更新的版本。在这台电脑上原样运行：",
            text(&i["device"]["client_min_version"])
        )));
        out.push(Code(lines(&i["device"]["commands"])));
        out.push(P(text(&i["device"]["token"])));
    } else {
        out.push(P("现在还不能接：先请部署的人补上上面缺的设置。".into()));
    }

    out.push(H2("术语".into()));
    out.push(Items(
        i["glossary"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|g| format!("**{}**：{}", text(&g["term"]), text(&g["meaning"])))
            .collect(),
    ));
    out
}

fn markdown(blocks: &[Block]) -> String {
    let mut out = String::new();
    for b in blocks {
        match b {
            Block::H1(t) => out.push_str(&format!("# {t}\n\n")),
            Block::H2(t) => out.push_str(&format!("## {t}\n\n")),
            Block::P(t) => out.push_str(&format!("{t}\n\n")),
            Block::Items(items) => {
                for item in items {
                    out.push_str(&format!("- {item}\n"));
                }
                out.push('\n');
            }
            Block::Code(lines) => out.push_str(&format!("```bash\n{}\n```\n\n", lines.join("\n"))),
        }
    }
    out
}

fn escape(t: &str) -> String {
    t.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// `code` and **bold** in a line of text, everything else escaped.
fn inline(t: &str) -> String {
    let mut out = String::new();
    for (n, part) in t.split('`').enumerate() {
        if n % 2 == 1 {
            out.push_str(&format!("<code>{}</code>", escape(part)));
            continue;
        }
        for (m, piece) in part.split("**").enumerate() {
            if m % 2 == 1 {
                out.push_str(&format!("<strong>{}</strong>", escape(piece)));
            } else {
                out.push_str(&escape(piece));
            }
        }
    }
    out
}

fn html(blocks: &[Block]) -> String {
    let mut body = String::new();
    for b in blocks {
        match b {
            Block::H1(t) => body.push_str(&format!("<h1>{}</h1>\n", inline(t))),
            Block::H2(t) => body.push_str(&format!("<h2>{}</h2>\n", inline(t))),
            Block::P(t) => body.push_str(&format!("<p>{}</p>\n", inline(t))),
            Block::Items(items) => {
                body.push_str("<ul>\n");
                for item in items {
                    body.push_str(&format!("<li>{}</li>\n", inline(item)));
                }
                body.push_str("</ul>\n");
            }
            Block::Code(lines) => body.push_str(&format!("<pre><code>{}</code></pre>\n", escape(&lines.join("\n")))),
        }
    }
    format!(
        "<!doctype html>\n<html lang=\"zh-CN\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>nekorisu 服务端：连接说明</title>\n</head>\n<body>\n<main>\n{body}</main>\n</body>\n</html>\n"
    )
}

// ---- handlers ----

/// `GET /.well-known/nekorisu`
pub async fn document(State(s): State<Shared>) -> Response {
    if !s.settings.connect_page {
        return StatusCode::NOT_FOUND.into_response();
    }
    let info = info(&s, s.settings.connect_show_storage).await;
    ([(header::CACHE_CONTROL, "no-cache")], Json(info)).into_response()
}

/// `GET /connect`: Markdown, or HTML when the client asks for it (a browser
/// does). `?format=md|html|json` chooses explicitly.
pub async fn page(State(s): State<Shared>, headers: HeaderMap, Query(q): Query<HashMap<String, String>>) -> Response {
    if !s.settings.connect_page {
        return StatusCode::NOT_FOUND.into_response();
    }
    let info = info(&s, s.settings.connect_show_storage).await;
    let wants_html = headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|a| a.contains("text/html"));
    let format = match q.get("format").map(String::as_str) {
        Some("md" | "markdown") => "md",
        Some("html") => "html",
        Some("json") => "json",
        _ if wants_html => "html",
        _ => "md",
    };
    let headers = [(header::CACHE_CONTROL, "no-cache"), (header::VARY, "Accept")];
    match format {
        "json" => (headers, Json(info)).into_response(),
        "html" => (
            headers,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            html(&blocks(&info)),
        )
            .into_response(),
        _ => (
            headers,
            [(header::CONTENT_TYPE, "text/markdown; charset=utf-8")],
            markdown(&blocks(&info)),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_marks_are_escaped_and_kept() {
        assert_eq!(
            inline("a `<b>` **c&d**"),
            "a <code>&lt;b&gt;</code> <strong>c&amp;d</strong>"
        );
    }
}
