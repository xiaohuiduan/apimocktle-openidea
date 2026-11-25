//! Text the host does not translate.
//!
//! OpenIDEA translates a package's declaration text — command and view titles,
//! the settings schema, the view toolbar — from `locales/<tag>.json`, whose keys
//! are the English source text (see `plugins/README.md`, "Translations"). Text
//! the plugin builds itself never passes through that catalog: notifications,
//! error messages, the dashboard page. Those pick a language here, from
//! `host::locale()`.

use openidea_plugin::host;

/// The languages this plugin has text for. Anything else shows English, which
/// is also what the host shows for a catalog without an entry.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    En,
    Zh,
}

/// The IDE's UI language, reduced to a language this plugin has text for.
///
/// Only the language subtag matters: the IDE offers `en` and `zh` as interface
/// languages and reports them as BCP 47 tags (`zh` goes out as `zh-CN`), so
/// `zh-CN`, `zh-TW` and `zh-Hans` all land on Chinese — the same granularity the
/// IDE itself resolves locales at.
///
/// Read it when the text is made. The host has no locale-change event, so a
/// cached language would outlive a switch.
pub fn lang() -> Lang {
    // `host::locale()` reports the IDE's own `appearance.locale` as a BCP 47 tag
    // (`zh` comes through as `zh-CN`). OpenIDEA 0.1.45's logs show it is accurate
    // for both `en` and `zh-CN`, including after a restart, so it is the single
    // source: no stored override, and no guessing from the system language.
    from_tag(&host::locale())
}

/// The tag-to-language rule, split out from the host call so it is testable.
pub fn from_tag(tag: &str) -> Lang {
    let language = tag.split(['-', '_']).next().unwrap_or_default();
    if language.eq_ignore_ascii_case("zh") { Lang::Zh } else { Lang::En }
}

/// One fixed message in both languages.
pub struct Text {
    pub en: &'static str,
    pub zh: &'static str,
}

impl Text {
    pub const fn new(en: &'static str, zh: &'static str) -> Self {
        Self { en, zh }
    }

    pub fn get(&self, lang: Lang) -> &'static str {
        match lang {
            Lang::En => self.en,
            Lang::Zh => self.zh,
        }
    }
}

// ---- Settings, project and target selection -------------------------------

pub const SET_SERVER_ADDRESS: Text =
    Text::new("Set the server address in Settings › ApiMocktle first", "先在设置 › ApiMocktle 里填写服务器地址");
pub const SET_PERSONAL_TOKEN: Text =
    Text::new("Set the personal token in Settings › ApiMocktle first", "先在设置 › ApiMocktle 里填写个人令牌");
pub const OPEN_A_PROJECT: Text = Text::new("Open a project first", "先打开一个项目");
pub const PICK_PROJECT: Text = Text::new("Export to which project", "导出到哪个项目");
pub const NO_ENDPOINTS_FOUND: Text = Text::new("No endpoints to export were found", "没有找到可导出的接口");
pub const NO_PROJECTS_ON_SERVER: Text = Text::new(
    "The server has no projects yet; create one in ApiMocktle first",
    "服务器上还没有项目，请先在 ApiMocktle 里创建一个",
);
pub const SELECT_FILE_OR_FOLDER: Text = Text::new(
    "Select a file or folder in the project view, or open a controller file first",
    "先在项目树里选中文件/目录，或打开一个控制器文件",
);
pub const ENDPOINT_GONE: Text =
    Text::new("That endpoint no longer exists; rescan the project", "接口不存在，请重新扫描");
pub const CACHE_CLEARED: Text = Text::new(
    "Cache cleared; the dashboard scans again the next time you open it.",
    "缓存已清除，下次打开仪表盘时重新扫描。",
);

// ---- The export queue -----------------------------------------------------

pub const NOTHING_TO_EXPORT: Text = Text::new("Nothing left to export", "没有待导出的接口");
pub const DEFAULT_CATEGORY: Text = Text::new("Default category", "默认分类");
pub const CATEGORY_DESC: Text =
    Text::new("Maintained by the OpenIDEA ApiMocktle plugin", "由 OpenIDEA ApiMocktle 插件维护");

pub const PAUSE_OTHER_TOKEN: Text =
    Text::new("Sync paused · the queue belongs to another token", "同步已暂停 · 队列用的是另一个令牌");
pub const PAUSE_OTHER_SERVER: Text =
    Text::new("Sync paused · the queue belongs to another server", "同步已暂停 · 队列属于另一服务器");
pub const PAUSE_TOOLTIP: Text = Text::new(
    "Put the original server address and token back to continue; to send to the current server, export again",
    "改回原来的服务器地址和令牌即可继续；要发到当前服务器请重新导出",
);
pub const CONTINUE_NOW: Text = Text::new("Continue now", "立即继续");
pub const SYNC_LEFT_TOOLTIP: Text = Text::new(
    "OpenIDEA allows 5 seconds per call; the remaining endpoints resume automatically every minute and a notice appears when they are done",
    "OpenIDEA 限时单次调用 5 秒，剩余接口每分钟自动续传，完成后会提示",
);
pub const BACKGROUND_DONE: Text =
    Text::new("Background sync finished; every endpoint is synced.", "后台续传完成，接口已全部同步。");

/// Export refused because the DTO graph ran out of time.
///
/// An export runs inside a `host::begin_task`, so the scan gets 50 s rather than
/// the 5 s a plain command would. Reaching this means the project really is
/// larger than that, and the routes are still good — but schemas built from a
/// partial graph lose fields, and a document saved without them replaces the
/// correct one on the server. Refusing is the only outcome that does not
/// silently damage the project's API documents.
///
/// "Export the current file or folder" is a real way out because the scope now
/// reaches the scan (`Options::only_path`), and the closure then only covers the
/// types those controllers reach.
pub const DTO_INCOMPLETE: Text = Text::new(
    "The DTO types were not fully resolved within the time limit, so nothing was exported; the server's documents are unchanged. Retry, or export a smaller scope (the current file or folder), which resolves only the types it reaches.",
    "未能在限时内解析完 DTO 类型，因此没有导出，服务器上的接口文档保持不变。可以重试，或用「导出当前文件/目录」只解析这部分接口用到的类型。",
);

/// Export refused because the project has more DTO types than one scan resolves.
///
/// Not a timeout: no clock was involved, so retrying stops at exactly the same
/// type every time and would be pointless advice. It is a fixed cap that keeps a
/// cyclic or generated model from running away, and the way out is to narrow the
/// scope.
pub const DTO_TYPE_LIMIT: Text = Text::new(
    "This project reaches more DTO types than a single scan resolves, so nothing was exported and the server's documents are unchanged. Export the current file or folder to resolve just the types it reaches.",
    "这个项目用到的 DTO 类型超过了单次扫描的上限，因此没有导出，服务器上的接口文档保持不变。请用「导出当前文件/目录」只解析这部分接口用到的类型。",
);

/// The background task a scan opens to get a 60-second call budget.
pub const SCAN_TASK_TITLE: Text = Text::new("ApiMocktle: resolving request and response types", "ApiMocktle: 解析请求响应类型");
pub const SCAN_TASK_ROUTES: Text = Text::new("reading controllers", "读取控制器");

/// The background task an export opens to get a 60-second call budget.
///
/// Sending a whole project in one call is only possible on a task: the host
/// gives a plain command 5 s, which fits about 200 interfaces and made a large
/// project need a click of "continue" every minute.
pub const EXPORT_TASK_TITLE: Text = Text::new("ApiMocktle: syncing endpoints", "ApiMocktle: 同步接口");
pub const EXPORT_TASK_SENDING: Text = Text::new("sending interfaces", "发送接口");

/// The token the queue was built with differs from the one in the settings.
pub fn queue_other_token() -> String {
    Text::new("The export queue was built with another personal token", "导出队列用的是另一个个人令牌")
        .get(lang())
        .to_owned()
}

/// The queue belongs to `queued_base`, the settings point at `current_base`.
pub fn queue_other_server(queued_base: &str, current_base: &str) -> String {
    match lang() {
        Lang::En => format!("The export queue belongs to {queued_base}; the current server is {current_base}"),
        Lang::Zh => format!("导出队列属于 {queued_base}，当前服务器是 {current_base}"),
    }
}

/// Why a queue stopped, naming both ways out: silently resending is the failure
/// this replaces.
pub fn paused_for_target(mismatch: &str) -> String {
    match lang() {
        Lang::En => format!(
            "{mismatch}. Put the original server address and token back to continue; \
             to send to the current server, click Export All again."
        ),
        Lang::Zh => format!(
            "{mismatch}。改回原来的服务器地址和令牌即可继续；要发到当前服务器，请重新点「导出全部」重新导出。"
        ),
    }
}

pub fn queue_corrupt(error: &str) -> String {
    match lang() {
        Lang::En => format!("The export queue is corrupted: {error}"),
        Lang::Zh => format!("导出队列已损坏：{error}"),
    }
}

pub fn category_unavailable(category: &str) -> String {
    match lang() {
        Lang::En => format!("Category \"{category}\" is unavailable"),
        Lang::Zh => format!("分类「{category}」不可用"),
    }
}

/// One failed endpoint: its `method path` key and what the server said.
pub fn failed_endpoint(key: &str, message: &str) -> String {
    match lang() {
        Lang::En => format!("{key}: {message}"),
        Lang::Zh => format!("{key}：{message}"),
    }
}

/// The tail every export summary carries: how many endpoints failed and the
/// first few reasons. Empty when nothing failed.
pub fn failed_note(count: usize, shown: &[String]) -> String {
    if count == 0 {
        return String::new();
    }
    let lang = lang();
    let list = shown.join(match lang {
        Lang::En => "; ",
        Lang::Zh => "；",
    });
    match lang {
        Lang::En => format!("\n{count} failed: {list}"),
        Lang::Zh => format!("\n其中 {count} 个失败：{list}"),
    }
}

pub fn export_done(queued: usize, failed_note: &str) -> String {
    match lang() {
        Lang::En => format!("All {queued} endpoints are synced to ApiMocktle.{failed_note}"),
        Lang::Zh => format!("全部 {queued} 个接口已同步到 ApiMocktle。{failed_note}"),
    }
}

pub fn export_partial(saved: usize, left: usize, failed_note: &str) -> String {
    match lang() {
        Lang::En => format!(
            "Synced {saved}, {left} left.{failed_note}\n\
             OpenIDEA allows about 5 seconds per plugin call; the remaining endpoints resume \
             automatically every minute, so they finish on their own. To push the next batch now, \
             click Continue now."
        ),
        Lang::Zh => format!(
            "已同步 {saved} 个，还剩 {left} 个没发完。{failed_note}\n\
             OpenIDEA 对单次插件调用限时约 5 秒，剩下的接口每分钟会自动续传，不用管也会走完；\
             想马上推下一批就点「立即继续」。"
        ),
    }
}

pub fn syncing_left(left: usize) -> String {
    match lang() {
        Lang::En => format!("Syncing · {left} left"),
        Lang::Zh => format!("同步中 · 剩 {left}"),
    }
}

pub fn synced(label: &str) -> String {
    match lang() {
        Lang::En => format!("Synced {label}"),
        Lang::Zh => format!("已同步 {label}"),
    }
}

// ---- YApi client errors ---------------------------------------------------

pub const YAPI_SET_SERVER: Text =
    Text::new("Set the ApiMocktle server address in the settings first", "先在设置里填写 ApiMocktle 服务器地址");
pub const YAPI_SET_TOKEN: Text = Text::new("Set the personal token in the settings first", "先在设置里填写个人令牌");
pub const YAPI_TOKEN_REJECTED: Text = Text::new(
    "The server rejected the token; update the personal token in the settings",
    "服务器拒绝了令牌，请在设置里更新个人令牌",
);
pub const YAPI_UNKNOWN_ERROR: Text = Text::new("Unknown error", "未知错误");

pub fn server_error(message: &str) -> String {
    let lang = lang();
    let message = if message.is_empty() { YAPI_UNKNOWN_ERROR.get(lang) } else { message };
    match lang {
        Lang::En => format!("The server returned an error: {message}"),
        Lang::Zh => format!("服务器返回错误：{message}"),
    }
}

pub fn http_status(status: u16, text: &str) -> String {
    match lang() {
        Lang::En => format!("HTTP {status}: {text}"),
        Lang::Zh => format!("HTTP {status}：{text}"),
    }
}

pub fn not_json(text: &str) -> String {
    match lang() {
        Lang::En => format!("The response is not JSON: {text}"),
        Lang::Zh => format!("响应不是 JSON：{text}"),
    }
}

/// The personal token travels in the request's query string (the YApi protocol
/// asks for it there), so a transport error that quotes the URL would print the
/// token into the dashboard's status line and the notification. Every message
/// that can carry a request URL goes through this first.
pub fn redact_token(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("token=") {
        out.push_str(&rest[..at]);
        out.push_str("token=…");
        rest = &rest[at + "token=".len()..];
        // Skip the value: everything up to the next separator.
        let end = rest.find(['&', ' ', '"', '\'', ')', ']', '\n']).unwrap_or(rest.len());
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

pub fn category_without_id(name: &str) -> String {
    match lang() {
        Lang::En => format!("The server created category \"{name}\" but returned no id"),
        Lang::Zh => format!("创建分类「{name}」后服务器没有返回 id"),
    }
}

/// The `markdown` field of an exported API document: a heading, the
/// description, and where the endpoint came from. `lang` is passed in because
/// the caller may build a document with no host mounted.
pub fn api_markdown(
    lang: Lang,
    summary: &str,
    description: &str,
    handler: &str,
    framework: &str,
    file: &str,
) -> String {
    match lang {
        Lang::En => format!("## {summary}\n\n{description}\n\nFrom: `{handler}` ({framework} {file})"),
        Lang::Zh => format!("## {summary}\n\n{description}\n\n来源：`{handler}`（{framework} {file}）"),
    }
}

// ---- Dependency jars -----------------------------------------------------

pub const DEP_JAR_TITLE: Text =
    Text::new("ApiMocktle cannot read dependency jars", "ApiMocktle 读不到依赖 jar");

pub fn dep_jar_advice(lang: Lang, path: &str, repo: &str) -> String {
    match lang {
        Lang::En => format!(
            "Reading a dependency jar needs the fs.read capability in the plugin manifest (reading {path}). \
             In the plugin package, change `optional` in plugin.toml to `fs.read:{repo}` (keeping the existing \
             fs.read:~/.m2/repository if you still use it), reinstall, and tick it under \
             Settings › Plugins › ApiMocktle › Permissions; keep both lines when both directories are used."
        ),
        Lang::Zh => format!(
            "读取依赖 jar 需要插件清单里的 fs.read 授权（正在读 {path}）。在插件包里把 plugin.toml 的 \
             `optional` 改成 `fs.read:{repo}`（连同已有的 fs.read:~/.m2/repository 一起），重新安装并在 \
             设置 › 插件 › ApiMocktle › 权限 里勾选；两个目录都用就两条都留。"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_chinese_tags_pick_chinese() {
        for tag in ["zh", "zh-CN", "zh-TW", "zh-Hans", "ZH-cn", "zh_CN"] {
            assert_eq!(from_tag(tag), Lang::Zh, "{tag}");
        }
        for tag in ["en", "en-GB", "fr", "", "ko", "ja"] {
            assert_eq!(from_tag(tag), Lang::En, "{tag}");
        }
    }

    #[test]
    fn every_fixed_text_has_both_languages() {
        let texts = [
            SET_SERVER_ADDRESS,
            SET_PERSONAL_TOKEN,
            OPEN_A_PROJECT,
            PICK_PROJECT,
            NO_ENDPOINTS_FOUND,
            NO_PROJECTS_ON_SERVER,
            SELECT_FILE_OR_FOLDER,
            ENDPOINT_GONE,
            CACHE_CLEARED,
            NOTHING_TO_EXPORT,
            DEFAULT_CATEGORY,
            CATEGORY_DESC,
            PAUSE_OTHER_TOKEN,
            PAUSE_OTHER_SERVER,
            PAUSE_TOOLTIP,
            CONTINUE_NOW,
            SYNC_LEFT_TOOLTIP,
            BACKGROUND_DONE,
            DTO_INCOMPLETE,
            YAPI_SET_SERVER,
            YAPI_SET_TOKEN,
            YAPI_TOKEN_REJECTED,
            YAPI_UNKNOWN_ERROR,
            DEP_JAR_TITLE,
        ];
        for text in texts {
            for lang in [Lang::En, Lang::Zh] {
                assert!(!text.get(lang).trim().is_empty(), "{lang:?}");
            }
            assert!(
                text.zh.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "the Chinese side is Chinese: {}",
                text.zh
            );
            assert!(
                !text.en.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c)),
                "the English side has no Chinese: {}",
                text.en
            );
        }
    }

    /// A transport error quotes the request URL, and the token is in its query
    /// string. The dashboard's status line and a notification both show the
    /// message, so the token must never survive into them.
    #[test]
    fn redaction_removes_the_token_from_anything_quoting_the_url() {
        let cases = [
            // The shape a failed request has: the token is in the query string of
            // the URL the transport error quotes.
            "error sending request for url (http://127.0.0.1:14202/api/project/list?token=0f9e8d7c6b5a4938271605f4e3d2c1b0)",
            "error sending request for url (http://x/api/interface/getCatMenu?project_id=p1&token=abc123&limit=5000)",
            "连接超时：http://x/api?token=deadbeef&project_id=p1",
        ];
        for text in cases {
            let redacted = redact_token(text);
            assert!(!redacted.contains("0f9e8d7c"), "{redacted}");
            assert!(!redacted.contains("abc123"), "{redacted}");
            assert!(!redacted.contains("deadbeef"), "{redacted}");
            assert!(redacted.contains("token=…"), "the parameter is still named: {redacted}");
        }
        // The rest of the message survives, so it still diagnoses the problem.
        let url = "error sending request for url (http://127.0.0.1:14202/api/project/list?token=secret)";
        let redacted = redact_token(url);
        assert!(redacted.starts_with("error sending request for url (http://127.0.0.1:14202/api/project/list?"));
        assert!(redacted.ends_with(')'));
        // Text without a token is untouched.
        assert_eq!(redact_token("plain message"), "plain message");
        assert_eq!(redact_token(""), "");
    }
}
