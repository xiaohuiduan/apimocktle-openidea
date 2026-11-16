//! ApiMocktle for OpenIDEA.
//!
//! OpenIDEA plugins are WebAssembly components, so there is no Java parser to
//! borrow: routes are recovered from tokens (`spring`), described with JSON
//! Schema (`model`) and handed to three consumers — Search Everywhere through
//! the `endpoints` contribution, a dashboard view, and an ApiMocktle / YApi
//! server through `host::fetch`.

pub mod classfile;
pub mod deps;
pub mod java;
pub mod model;
pub mod scan;
pub mod spring;
pub mod yapi;
pub mod zip;

use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use openidea_plugin::endpoints::{Endpoint as Route, EndpointsProvider};
use openidea_plugin::views::{Body, Button, View, ViewEvent, Viewport, Views};
use openidea_plugin::{
    Context, Error, Event, Invocation, Location, LogLevel, NotificationAction, NotificationLevel, PickItem, Plugin,
    Position, Project, Range, StatusItem, host, register_plugin,
};
use serde_json::{Value, json};

use crate::model::Endpoint;
use crate::scan::ScanResult;

pub const VIEW: &str = "dashboard";
const PAGE: &str = include_str!("page.html");
/// Interfaces sent per call. A command runs inside a five-second budget, so
/// the loop also stops itself on `BUDGET`.
/// Interfaces one call may start. A local ApiMocktle answers in a few
/// milliseconds, so this is only a guard; `BUDGET` is what actually stops a
/// batch when the server is slow.
const BATCH: usize = 200;
/// Stop starting new interfaces past this; the host kills a command at 5 s.
const BUDGET: Duration = Duration::from_millis(4_200);
const QUEUE_KEY: &str = "export:queue";
/// Category ids per server and project, kept across batches. The base URL is
/// part of the key so a `catid` from one server is never used on another.
const CATS_KEY: &str = "cats:";
const CATEGORY_DESC: &str = "由 OpenIDEA ApiMocktle 插件维护";

#[derive(Default)]
pub struct ApiMocktle {
    /// Project root to its last full scan (DTOs resolved); cleared by refresh.
    cache: HashMap<String, Rc<ScanResult>>,
    /// Routes only, for the endpoints provider and its 2-second budget.
    routes: HashMap<String, Rc<ScanResult>>,
}

struct Config {
    base: String,
    token: String,
}

fn config() -> Result<Config, Error> {
    let base = host::setting_string("serverUrl").unwrap_or_default().trim().to_owned();
    let token = host::setting_string("token").unwrap_or_default().trim().to_owned();
    if base.is_empty() {
        return Err(Error::Invalid("先在设置 › ApiMocktle 里填写服务器地址".into()));
    }
    if token.is_empty() {
        return Err(Error::Invalid("先在设置 › ApiMocktle 里填写个人令牌".into()));
    }
    Ok(Config { base, token })
}

fn scan_options() -> scan::Options {
    scan::Options {
        include_kotlin: host::setting_bool("includeKotlin").unwrap_or(true),
        depth: host::setting_number("schemaDepth").map(|n| n as u32).unwrap_or(3).clamp(1, 4),
        deps: crate::deps::Config {
            enabled: host::setting_bool("resolveDependencyJars").unwrap_or(true),
            repo: crate::deps::repository(),
        },
        ..scan::Options::default()
    }
}

/// Routes only: no DTO resolution, fewer candidate files.
fn route_options() -> scan::Options {
    scan::Options { resolve_dto: false, file_limit: 400, ..scan_options() }
}

fn post(message: Value) {
    let _ = host::post_view_message(VIEW, &message.to_string());
}

/// Deterministic, dependency-free fingerprint of the token, recorded in the
/// export queue so a queue can tell "same authorization" from "another token".
///
/// FNV-1a 64 is spelled out on purpose: `DefaultHasher` promises nothing across
/// processes or Rust versions, so the same token could fingerprint differently
/// after an IDE restart and pause a perfectly healthy queue. `sha2` would add a
/// dependency for a change detector. This is not a secret and not a security
/// boundary — the token itself is never written to storage.
fn fingerprint(text: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{hash:016x}")
}

fn cats_key(base: &str, project_id: &str) -> String {
    format!("{CATS_KEY}{base}:{project_id}")
}

fn load_categories(base: &str, project_id: &str) -> HashMap<String, String> {
    host::storage::get(&cats_key(base, project_id))
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_categories(base: &str, project_id: &str, map: &HashMap<String, String>) {
    if let Ok(raw) = serde_json::to_string(map) {
        let _ = host::storage::set(&cats_key(base, project_id), &raw);
    }
}

/// The category id for `name`, from the cache or the server.
fn resolve_category(client: &yapi::Client, name: &str, cache: &mut HashMap<String, String>) -> Option<String> {
    if let Some(id) = cache.get(name).filter(|id| !id.is_empty()) {
        return Some(id.clone());
    }
    let found = client.find_or_create_category(name, CATEGORY_DESC).ok()?;
    if found.id.is_empty() {
        return None;
    }
    cache.insert(name.to_owned(), found.id.clone());
    Some(found.id)
}

/// Forgets a cached id and creates the category again, for when the server says
/// the old one does not exist (it was deleted, or belongs to another project).
fn rebuild_category(client: &yapi::Client, name: &str, cache: &mut HashMap<String, String>) -> Option<String> {
    cache.remove(name);
    let created = client.create_category(name, CATEGORY_DESC).ok()?;
    if created.id.is_empty() {
        return None;
    }
    cache.insert(name.to_owned(), created.id.clone());
    Some(created.id)
}

/// Stops a queue that was built for another server or another token: nothing is
/// sent, the queue stays, and the status bar says why. The error names both ways
/// out, because silently resending is the failure this replaces.
fn pause_for_another_target(queued_base: &str, current_base: &str) -> Error {
    let same_server = queued_base == current_base;
    let text = if same_server {
        "同步已暂停 · 队列用的是另一个令牌"
    } else {
        "同步已暂停 · 队列属于另一服务器"
    };
    let mismatch = if same_server {
        "导出队列用的是另一个个人令牌".to_owned()
    } else {
        format!("导出队列属于 {queued_base}，当前服务器是 {current_base}")
    };
    host::set_status_item(
        "sync",
        &StatusItem {
            text: text.into(),
            tooltip: Some("改回原来的服务器地址和令牌即可继续；要发到当前服务器请重新导出".into()),
            command: Some("open-dashboard".into()),
        },
    );
    Error::Failed(format!(
        "{mismatch}。改回原来的服务器地址和令牌即可继续；要发到当前服务器，请重新点「导出全部」重新导出。"
    ))
}

/// `path` is `target` or inside it, whichever separator the host used.
fn under(path: &str, target: &str) -> bool {
    let target = target.trim_end_matches(['/', '\\']);
    path == target || path.starts_with(&format!("{target}/")) || path.starts_with(&format!("{target}\\"))
}

/// The file or folder a command was run on: the argument first (a Project view
/// or editor tab menu passes the first selected path), then the selection, then
/// the focused editor.
fn target_path(ctx: &Invocation, arg: Option<&str>) -> Option<String> {
    arg.map(str::to_owned)
        .or_else(|| ctx.selected_paths().into_iter().next())
        .or_else(|| ctx.editor().map(|editor| editor.path()))
}

impl ApiMocktle {
    fn scan(&mut self, project: &Project) -> Result<Rc<ScanResult>, Error> {
        let root = project.root()?;
        if let Some(hit) = self.cache.get(&root) {
            return Ok(hit.clone());
        }
        let result = Rc::new(scan::scan(project, &scan_options())?);
        host::log(
            LogLevel::Info,
            &format!(
                "ApiMocktle: {} 个控制器文件 → {} 个接口，{} 个 DTO 类型{}",
                result.files,
                result.endpoints.len(),
                result.dto.fields.len(),
                if result.truncated { "（已达搜索上限）" } else { "" }
            ),
        );
        self.cache.insert(root, result.clone());
        Ok(result)
    }

    /// The route list the endpoints provider publishes, without DTO work.
    fn routes(&mut self, project: &Project) -> Result<Rc<ScanResult>, Error> {
        let root = project.root()?;
        if let Some(hit) = self.cache.get(&root) {
            return Ok(hit.clone());
        }
        if let Some(hit) = self.routes.get(&root) {
            return Ok(hit.clone());
        }
        let result = Rc::new(scan::scan(project, &route_options())?);
        self.routes.insert(root, result.clone());
        Ok(result)
    }

    /// Builds one export queue for the endpoints and sends its first batch.
    /// `only_path` limits it to one file or folder.
    fn start_export(
        &mut self,
        ctx: &Invocation,
        only_path: Option<String>,
        target: Option<String>,
    ) -> Result<String, Error> {
        let project = ctx.project().ok_or("先打开一个项目")?;
        let cfg = config()?;
        let scanned = self.scan(&project)?;
        let picked: Vec<&Endpoint> = scanned
            .endpoints
            .iter()
            .filter(|e| only_path.as_ref().is_none_or(|path| under(&e.file, path)))
            .collect();
        if picked.is_empty() {
            return Err(Error::Invalid("没有找到可导出的接口".into()));
        }

        let probe = yapi::Client { base: cfg.base.clone(), token: cfg.token.clone(), project_id: String::new() };
        let projects = probe.list_projects()?;
        if projects.is_empty() {
            return Err(Error::Failed("服务器上还没有项目，请先在 ApiMocktle 里创建一个".into()));
        }
        let project_id = match target.filter(|id| !id.trim().is_empty()) {
            Some(id) => id,
            None if projects.len() == 1 => projects[0].id.clone(),
            None => {
                let choices: Vec<PickItem> = projects
                    .iter()
                    .map(|p| {
                        let label = if p.desc.is_empty() {
                            p.name.clone()
                        } else {
                            format!("{} — {}", p.name, p.desc)
                        };
                        PickItem::new(&p.id, &label)
                    })
                    .collect();
                host::pick("导出到哪个项目", &choices)?.ok_or(Error::Cancelled)?
            }
        };

        // One category per controller: `TicketOrderController`, `PayController`, …
        let depth = scan_options().depth;
        let items: Vec<Value> = picked
            .iter()
            .map(|endpoint| {
                json!({
                    "cat": endpoint.class_name.clone(),
                    "doc": yapi::api_doc(endpoint, &scanned.dto, depth),
                })
            })
            .collect();
        let total = items.len();
        // The queue records the destination it was built for. Later batches read
        // it back, so a settings change cannot silently redirect what is left.
        let queue = json!({
            "base": yapi::normalize_base(&cfg.base),
            "auth": fingerprint(&cfg.token),
            "project_id": project_id,
            "items": items,
        });
        host::storage::set(QUEUE_KEY, &queue.to_string())?;
        // A new queue retires whatever the status bar said about the old one,
        // including a pause notice left by a queue bound to another server.
        host::remove_status_item("sync");
        host::log(
            openidea_plugin::LogLevel::Info,
            &format!("ApiMocktle: 待导出 {total} 个接口，目标项目 {project_id}"),
        );
        self.flush_export(&cfg, true)
    }

    /// Sends the next batch of the queued export. `notify` false keeps the
    /// interface quiet when the host's schedule drives the batch.
    fn flush_export(&mut self, cfg: &Config, notify: bool) -> Result<String, Error> {
        let Some(raw) = host::storage::get(QUEUE_KEY)? else {
            return Ok("没有待导出的接口".into());
        };
        let mut queue: Value =
            serde_json::from_str(&raw).map_err(|e| Error::Failed(format!("导出队列已损坏：{e}")))?;
        let items = queue.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
        let queued_base = queue.get("base").and_then(Value::as_str).unwrap_or_default().to_owned();
        let queued_auth = queue.get("auth").and_then(Value::as_str).unwrap_or_default().to_owned();
        // A queue that cannot name its target — one written by 0.2.0 — is
        // dropped instead of being guessed at. The command shows
        // `没有待导出的接口`; the log says what was thrown away, because the
        // schedule path has no other way to report it.
        if items.is_empty() {
            host::storage::delete(QUEUE_KEY)?;
            return Ok("没有待导出的接口".into());
        }
        if queued_base.is_empty() || queued_auth.is_empty() {
            host::storage::delete(QUEUE_KEY)?;
            host::log(
                LogLevel::Warn,
                &format!(
                    "ApiMocktle: 旧格式的导出队列（{} 个接口）没有目标服务器信息，已丢弃，请重新导出",
                    items.len()
                ),
            );
            return Ok("没有待导出的接口".into());
        }
        let base = yapi::normalize_base(&cfg.base);
        let auth = fingerprint(&cfg.token);
        // Sending these items to another server would write them into whatever
        // project happens to share the id, so the queue stops here instead.
        if queued_base != base || queued_auth != auth {
            return Err(pause_for_another_target(&queued_base, &base));
        }
        let project_id = queue.get("project_id").and_then(Value::as_str).unwrap_or_default().to_owned();
        let queued = items.len();
        let client = yapi::Client { base: cfg.base.clone(), token: cfg.token.clone(), project_id: project_id.clone() };

        // Category ids are kept between batches: every lookup is a round trip,
        // and a five-second command budget only fits so many.
        let mut categories = load_categories(&base, &project_id);
        let mut known: HashMap<String, HashMap<String, String>> = HashMap::new();
        let mut saved = 0usize;
        let mut failed: Vec<String> = Vec::new();
        let mut rest: Vec<Value> = Vec::new();
        let mut sent = 0usize;
        let started = Instant::now();

        for item in items {
            // The clock guard never fires before a few interfaces went out, so
            // an unexpected clock cannot shrink a batch to one.
            if sent >= BATCH || (sent >= 5 && started.elapsed() > BUDGET) {
                rest.push(item);
                continue;
            }
            sent += 1;
            let category = item.get("cat").and_then(Value::as_str).unwrap_or("默认分类").to_owned();
            let doc = item.get("doc").cloned().unwrap_or(Value::Null);
            let Some(cat_id) = resolve_category(&client, &category, &mut categories) else {
                failed.push(format!("分类「{category}」不可用"));
                continue;
            };
            let existing = match known.get(&cat_id) {
                Some(map) => map.clone(),
                None => {
                    let map = client
                        .list_apis(&cat_id)
                        .map(|apis| {
                            apis.into_iter()
                                .map(|a| (format!("{} {}", a.method.to_uppercase(), a.path), a.id))
                                .collect::<HashMap<_, _>>()
                        })
                        .unwrap_or_default();
                    known.insert(cat_id.clone(), map.clone());
                    map
                }
            };
            let method = doc.get("method").and_then(Value::as_str).unwrap_or("GET");
            let path = doc.get("path").and_then(Value::as_str).unwrap_or("");
            let key = format!("{method} {path}");
            match client.save_api(&doc, &cat_id, existing.get(&key).map(String::as_str)) {
                Ok(()) => saved += 1,
                Err(first) => {
                    // A cached id can belong to a deleted or foreign category,
                    // which the server answers with a category error. Rebuild it
                    // once instead of failing the interface.
                    match rebuild_category(&client, &category, &mut categories) {
                        Some(fresh) if fresh != cat_id => match client.save_api(&doc, &fresh, None) {
                            Ok(()) => saved += 1,
                            Err(second) => failed.push(format!("{key}：{second}")),
                        },
                        _ => failed.push(format!("{key}：{first}")),
                    }
                }
            }
        }
        save_categories(&base, &project_id, &categories);

        let left = rest.len();
        if left == 0 {
            host::storage::delete(QUEUE_KEY)?;
        } else {
            if let Some(obj) = queue.as_object_mut() {
                obj.insert("items".into(), Value::Array(rest));
            }
            host::storage::set(QUEUE_KEY, &queue.to_string())?;
        }

        let failed_note = if failed.is_empty() {
            String::new()
        } else {
            let shown: Vec<String> = failed.iter().take(3).cloned().collect();
            format!("\n其中 {} 个失败：{}", failed.len(), shown.join("；"))
        };
        let summary = if left == 0 {
            format!("全部 {queued} 个接口已同步到 ApiMocktle。{failed_note}")
        } else {
            format!(
                "已同步 {saved} 个，还剩 {left} 个没发完。{failed_note}\n\
                 OpenIDEA 对单次插件调用限时约 5 秒，剩下的接口每分钟会自动续传，不用管也会走完；\
                 想马上推下一批就点「立即继续」。"
            )
        };
        host::log(
            LogLevel::Info,
            &format!(
                "ApiMocktle: 本批 {} 个（成功 {saved}，失败 {}），剩余 {left}，用时 {:?}",
                sent,
                failed.len(),
                started.elapsed()
            ),
        );
        if notify {
            if left > 0 {
                host::notify_with_actions(
                    NotificationLevel::Info,
                    "ApiMocktle",
                    &summary,
                    &[NotificationAction::new("立即继续", "export-continue")],
                )?;
            } else {
                host::notify(NotificationLevel::Info, "ApiMocktle", &summary);
            }
        } else if left > 0 {
            host::set_status_item(
                "sync",
                &StatusItem {
                    text: format!("同步中 · 剩 {left}"),
                    tooltip: Some("OpenIDEA 限时单次调用 5 秒，剩余接口每分钟自动续传，完成后会提示".into()),
                    command: Some("open-dashboard".into()),
                },
            );
        }
        Ok(summary)
    }

    /// One more batch, driven by the host's schedule instead of a click.
    fn auto_continue(&mut self) -> Result<(), Error> {
        if host::storage::get(QUEUE_KEY)?.is_none() {
            host::remove_status_item("sync");
            return Ok(());
        }
        let Ok(cfg) = config() else { return Ok(()) };
        match self.flush_export(&cfg, false) {
            Ok(text) => host::log(LogLevel::Info, &format!("ApiMocktle: 自动续传 {text}")),
            Err(e) => host::log(LogLevel::Warn, &format!("ApiMocktle: 自动续传失败：{e}")),
        }
        if host::storage::get(QUEUE_KEY)?.is_none() {
            host::remove_status_item("sync");
            host::notify(NotificationLevel::Info, "ApiMocktle", "后台续传完成，接口已全部同步。");
        }
        Ok(())
    }
}

impl Plugin for ApiMocktle {
    fn activate(&mut self, _ctx: &Context) -> Result<(), Error> {
        let base = host::setting_string("serverUrl").unwrap_or_default();
        host::log(
            LogLevel::Info,
            &format!("ApiMocktle {} 已激活（serverUrl={base}，API 1.7）", env!("CARGO_PKG_VERSION")),
        );
        Ok(())
    }

    fn run_command(&mut self, id: &str, ctx: &Invocation, arg: Option<&str>) -> Result<Option<String>, Error> {
        match id {
            "export-project" => self.start_export(ctx, None, None).map(Some),
            "export-file" => {
                let path = target_path(ctx, arg).ok_or_else(|| {
                    Error::Invalid("先在项目树里选中文件/目录，或打开一个控制器文件".into())
                })?;
                self.start_export(ctx, Some(path), None).map(Some)
            }
            "export-continue" => {
                let cfg = config()?;
                self.flush_export(&cfg, true).map(Some)
            }
            "open-dashboard" => {
                host::reveal_view(VIEW)?;
                Ok(None)
            }
            "refresh" => {
                self.cache.clear();
                self.routes.clear();
                host::refresh_view(VIEW);
                host::notify(NotificationLevel::Info, "ApiMocktle", "缓存已清除，下次打开仪表盘时重新扫描。");
                Ok(None)
            }
            other => Err(Error::NotFound(format!("command {other}"))),
        }
    }

    /// The host wakes the plugin every minute (`on-schedule:60`); a queued
    /// export advances one batch per wake-up, so no clicks are needed.
    fn handle_events(&mut self, events: &[Event], _ctx: &Invocation) {
        if events.iter().any(|event| matches!(event, Event::Scheduled)) {
            let _ = self.auto_continue();
        }
    }
}

impl EndpointsProvider for ApiMocktle {
    fn endpoints(&mut self, ctx: &Invocation) -> Result<Vec<Route>, Error> {
        let Some(project) = ctx.project() else { return Ok(Vec::new()) };
        let scanned = match self.routes(&project) {
            Ok(result) => result,
            Err(e) => {
                host::log(openidea_plugin::LogLevel::Warn, &format!("ApiMocktle: 扫描失败：{e}"));
                return Ok(Vec::new());
            }
        };
        Ok(scanned
            .endpoints
            .iter()
            .map(|endpoint| Route {
                method: endpoint.method.clone(),
                path: endpoint.path.clone(),
                framework: endpoint.framework.clone(),
                handler: Some(endpoint.handler()),
                location: Location {
                    path: endpoint.file.clone(),
                    range: Range::new(
                        Position::new(endpoint.line, endpoint.col),
                        Position::new(endpoint.line, endpoint.col),
                    ),
                },
            })
            .collect())
    }
}

impl Views for ApiMocktle {
    fn render(&mut self, view: &str, _viewport: Option<Viewport>, _ctx: &Invocation) -> Result<View, Error> {
        if view != VIEW {
            return Err(Error::NotFound(view.into()));
        }
        // The page keeps its own state and asks for data over messages, so the
        // frame is never re-rendered underneath the user.
        let start = json!({
            "configured": config().is_ok(),
            "base": host::setting_string("serverUrl").unwrap_or_default(),
        })
        .to_string()
        .replace('<', "\\u003c");
        let page = PAGE.replacen(r#""__START__""#, &start, 1);
        Ok(View::new(Body::Html(page))
            .button(Button::new("refresh", "重新扫描").icon("refresh"))
            .button(Button::new("export", "导出全部").icon("run")))
    }

    fn handle(&mut self, view: &str, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
        if view != VIEW {
            return Err(Error::NotFound(view.into()));
        }
        match event {
            ViewEvent::Button(id) if id == "refresh" => {
                self.cache.clear();
                self.routes.clear();
                let payload = self.dashboard_payload(ctx);
                post(payload);
                Ok(())
            }
            ViewEvent::Button(id) if id == "export" => {
                let result = self.start_export(ctx, None, None);
                self.report(result);
                Ok(())
            }
            ViewEvent::Message(text) => {
                let Ok(value) = serde_json::from_str::<Value>(&text) else { return Ok(()) };
                match value.get("op").and_then(Value::as_str) {
                    Some("scan") => {
                        let payload = self.dashboard_payload(ctx);
                        post(payload);
                    }
                    Some("refresh") => {
                        self.cache.clear();
                        self.routes.clear();
                        let payload = self.dashboard_payload(ctx);
                        post(payload);
                    }
                    Some("export-all") => {
                        let target = value.get("project").and_then(Value::as_str).map(str::to_owned);
                        let result = self.start_export(ctx, None, target);
                        self.report(result);
                    }
                    Some("export-one") => {
                        let index = value.get("index").and_then(Value::as_u64).unwrap_or(u64::MAX) as usize;
                        let target = value.get("project").and_then(Value::as_str).map(str::to_owned);
                        let result = self.export_one(ctx, index, target);
                        self.report(result);
                    }
                    Some("projects") => {
                        let payload = match config() {
                            Ok(cfg) => {
                                let probe = yapi::Client {
                                    base: cfg.base,
                                    token: cfg.token,
                                    project_id: String::new(),
                                };
                                match probe.list_projects() {
                                    Ok(list) => json!({
                                        "op": "projects",
                                        "items": list.iter().map(|p| json!({ "id": p.id, "name": p.name })).collect::<Vec<_>>(),
                                    }),
                                    Err(e) => json!({ "op": "projects", "items": [], "error": e.to_string() }),
                                }
                            }
                            Err(e) => json!({ "op": "projects", "items": [], "error": e.to_string() }),
                        };
                        post(payload);
                    }
                    Some("open") => {
                        let index = value.get("index").and_then(Value::as_u64).unwrap_or(u64::MAX) as usize;
                        if let Some(project) = ctx.project()
                            && let Ok(scanned) = self.scan(&project)
                            && let Some(endpoint) = scanned.endpoints.get(index)
                        {
                            let _ = host::open_location(&endpoint.file, endpoint.line, endpoint.col);
                        }
                    }
                    _ => {}
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

impl ApiMocktle {
    fn dashboard_payload(&mut self, ctx: &Invocation) -> Value {
        let Some(project) = ctx.project() else {
            return json!({ "op": "items", "error": "先打开一个项目", "items": [], "count": 0 });
        };
        match self.scan(&project) {
            Ok(scanned) => {
                let mut payload = scan::to_json(&scanned);
                payload["op"] = Value::String("items".into());
                let configured = config().is_ok();
                payload["configured"] = Value::Bool(configured);
                payload
            }
            Err(e) => json!({ "op": "items", "error": e.to_string(), "items": [], "count": 0 }),
        }
    }

    fn export_one(&mut self, ctx: &Invocation, index: usize, target: Option<String>) -> Result<String, Error> {
        let project = ctx.project().ok_or("先打开一个项目")?;
        let scanned = self.scan(&project)?;
        let endpoint = scanned
            .endpoints
            .get(index)
            .cloned()
            .ok_or_else(|| Error::Invalid("接口不存在，请重新扫描".into()))?;
        self.export_single(ctx, &endpoint, &scanned, target)
    }

    fn export_single(
        &mut self,
        _ctx: &Invocation,
        endpoint: &Endpoint,
        scanned: &ScanResult,
        target: Option<String>,
    ) -> Result<String, Error> {
        let cfg = config()?;
        let probe = yapi::Client { base: cfg.base.clone(), token: cfg.token.clone(), project_id: String::new() };
        let projects = probe.list_projects()?;
        let project_id = match target.filter(|id| !id.trim().is_empty()) {
            Some(id) => id,
            None if projects.len() == 1 => projects[0].id.clone(),
            None => {
                let choices: Vec<PickItem> = projects.iter().map(|p| PickItem::new(&p.id, &p.name)).collect();
                host::pick("导出到哪个项目", &choices)?.ok_or(Error::Cancelled)?
            }
        };
        let client = yapi::Client { base: cfg.base, token: cfg.token, project_id };
        let cat = client.find_or_create_category(&endpoint.class_name, CATEGORY_DESC)?;
        let existing = client
            .list_apis(&cat.id)
            .unwrap_or_default()
            .into_iter()
            .find(|a| a.path == endpoint.path && a.method.eq_ignore_ascii_case(&endpoint.yapi_method()))
            .map(|a| a.id);
        let doc = yapi::api_doc(endpoint, &scanned.dto, scan_options().depth);
        client.save_api(&doc, &cat.id, existing.as_deref())?;
        Ok(format!("已同步 {}", endpoint.label()))
    }

    fn report(&mut self, result: Result<String, Error>) {
        let (ok, text) = match result {
            Ok(text) => (true, text),
            Err(e) => (false, e.to_string()),
        };
        post(json!({ "op": "toast", "text": text, "ok": ok }));
        host::notify(if ok { NotificationLevel::Info } else { NotificationLevel::Warning }, "ApiMocktle", &text);
    }
}

register_plugin!(ApiMocktle, views, endpoints);

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;
    use openidea_plugin::{HttpRequest, HttpResponse, TextMatch, TextSearch};

    const CONTROLLER: &str = r#"
package com.acme;

@RestController
@RequestMapping("/api/users")
public class UserController {
    /** 列表。 */
    @GetMapping("/list")
    public Result<List<UserDto>> list(@RequestParam("q") String q) { return null; }

    @PostMapping("/create")
    public Result<UserDto> create(@RequestBody CreateUser form) { return null; }
}
"#;
    const DTO: &str = "package com.acme; public class UserDto { private Long id; private String name; }";
    const CREATE: &str = "package com.acme; public class CreateUser { private String name; private int age; }";
    const RESULT: &str = "package com.acme; public class Result<T> { private int code; private String msg; private T data; }";

    fn reply(status: u16, body: serde_json::Value) -> Result<HttpResponse, Error> {
        Ok(HttpResponse { status, headers: Vec::new(), body: body.to_string().into_bytes() })
    }

    /// A project with one controller and two DTOs, and a YApi server that
    /// answers like ApiMocktle does.
    fn host() -> MockHost {
        let host = MockHost::install().with_capabilities(&[
            "project.read",
            "storage",
            "net:localhost",
            "net:127.0.0.1",
            "clipboard",
        ]);
        host.add_project("/proj");
        host.set_project_root(Some("/proj"));
        host.add_file("/proj/src/UserController.java", CONTROLLER);
        host.add_file("/proj/src/UserDto.java", DTO);
        host.add_file("/proj/src/CreateUser.java", CREATE);
        // The wrapper lives in the project source, so it is resolved and kept.
        host.add_file("/proj/src/Result.java", RESULT);
        host.set_setting("serverUrl", "\"http://localhost:4000\"");
        host.set_setting("token", "\"secret-token\"");
        host.set_setting("includeKotlin", "true");
        host.set_setting("schemaDepth", "2");
        // These tests run without the fs.read grant and must not poke at the
        // developer's real Maven repository.
        host.set_setting("resolveDependencyJars", "false");
        // Only the controller matches the annotation search.
        host.on_search(|query, _limit| {
            if query.pattern.starts_with('@') {
                Ok(TextSearch {
                    matches: vec![TextMatch {
                        location: Location {
                            path: "/proj/src/UserController.java".into(),
                            range: Range::new(Position::new(0, 0), Position::new(0, 1)),
                        },
                        line: String::new(),
                    }],
                    limit_hit: false,
                })
            } else {
                Ok(TextSearch::default())
            }
        });
        host.on_fetch(|request| {
            let url = request.url.as_str();
            if url.contains("/api/project/list") {
                reply(200, json!({ "errcode": 0, "data": [{ "_id": "p1", "name": "Demo" }] }))
            } else if url.contains("/api/interface/getCatMenu") {
                reply(200, json!({ "errcode": 0, "data": [] }))
            } else if url.contains("/api/interface/add_cat") {
                reply(200, json!({ "errcode": 0, "data": { "_id": "c1", "name": "UserController" } }))
            } else if url.contains("/api/interface/list_cat") {
                reply(200, json!({ "errcode": 0, "data": { "list": [] } }))
            } else if url.contains("/api/interface/save") {
                reply(200, json!({ "errcode": 0 }))
            } else {
                reply(404, json!({ "errcode": 404, "errmsg": "not found" }))
            }
        });
        host
    }

    #[test]
    fn scans_the_project_into_endpoints() {
        let host = host();
        let project = host.invocation().project().expect("a project");
        let scanned = scan::scan(&project, &scan::Options::default()).unwrap();
        assert_eq!(scanned.files, 1);
        assert_eq!(scanned.endpoints.len(), 2);
        assert_eq!(scanned.endpoints[0].path, "/api/users/create");
        assert_eq!(scanned.endpoints[0].method.as_deref(), Some("POST"));
        assert_eq!(scanned.endpoints[1].path, "/api/users/list");
        assert_eq!(scanned.endpoints[1].summary, "列表。");
        // Both DTOs were resolved, so the response schema is not an empty object.
        assert!(scanned.dto.fields.contains_key("UserDto"), "{:?}", scanned.dto.fields.keys().collect::<Vec<_>>());
        assert!(scanned.dto.fields.contains_key("CreateUser"));
        assert_eq!(scanned.dto.fields["CreateUser"][0].name, "name");
        assert_eq!(scanned.dto.fields["CreateUser"][1].ty, "int");
        assert_eq!(scanned.dto.params["Result"], vec!["T".to_owned()]);
    }

    #[test]
    fn exports_a_batch_and_queues_the_rest() {
        let host = host();
        let mut plugin = ApiMocktle::default();
        let text = plugin.run_command("export-project", &host.invocation(), None).unwrap().unwrap();
        assert!(text.contains("已同步到 ApiMocktle"), "{text}");

        let requests = host.requests();
        let saved: Vec<&HttpRequest> = requests.iter().filter(|r| r.url.contains("/api/interface/save")).collect();
        assert_eq!(saved.len(), 2);
        let body = String::from_utf8(saved[0].body.clone().unwrap()).unwrap();
        let doc: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(doc["project_id"], "p1");
        assert_eq!(doc["catid"], "c1");
        assert_eq!(doc["token"], "secret-token");
        assert_eq!(doc["method"], "POST");
        assert_eq!(doc["path"], "/api/users/create");
        let request_schema: Value = serde_json::from_str(doc["req_body_other"].as_str().unwrap()).unwrap();
        assert_eq!(request_schema["properties"]["name"]["type"], "string");
        // `Result` is declared in the project, so the response keeps it and
        // substitutes `UserDto` into `data` instead of unwrapping it.
        let response_schema: Value = serde_json::from_str(doc["res_body"].as_str().unwrap()).unwrap();
        assert_eq!(response_schema["type"], "object");
        assert_eq!(response_schema["properties"]["data"]["properties"]["id"]["type"], "number");

        // The list endpoint substitutes `List<UserDto>`, so `data` stays an
        // array and the field table can read the item shape.
        let list_body = String::from_utf8(saved[1].body.clone().unwrap()).unwrap();
        let list_doc: Value = serde_json::from_str(&list_body).unwrap();
        assert_eq!(list_doc["path"], "/api/users/list");
        let list_schema: Value = serde_json::from_str(list_doc["res_body"].as_str().unwrap()).unwrap();
        assert_eq!(list_schema["type"], "object");
        assert_eq!(list_schema["properties"]["data"]["type"], "array");
        let item = &list_schema["properties"]["data"]["items"]["properties"];
        assert_eq!(item["id"]["type"], "number");
        assert_eq!(item["name"]["type"], "string");
    }

    #[test]
    fn endpoints_are_published_for_search() {
        let host = host();
        let mut plugin = ApiMocktle::default();
        let routes = plugin.endpoints(&host.invocation()).unwrap();
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0].framework, "Spring");
        assert_eq!(routes[0].handler.as_deref(), Some("UserController#create"));
        assert_eq!(routes[0].location.path, "/proj/src/UserController.java");
    }

    #[test]
    fn the_view_answers_scan_messages() {
        let host = host();
        let mut plugin = ApiMocktle::default();
        let view = plugin.render(VIEW, None, &host.invocation()).unwrap();
        let Body::Html(page) = view.body else { panic!("an html body") };
        assert!(page.contains("ApiMocktle"));
        assert!(page.contains("导出全部"));
        assert!(!page.contains("发送请求"), "the dashboard only browses and exports");

        let message = json!({ "op": "scan" }).to_string();
        plugin.handle(VIEW, ViewEvent::Message(message), &host.invocation()).unwrap();
        let (name, payload) = host.view_messages().pop().unwrap();
        assert_eq!(name, VIEW);
        let payload: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload["op"], "items");
        assert_eq!(payload["count"], 2);
    }

    #[test]
    fn the_schedule_advances_the_queue_without_a_click() {
        let host = host();
        let queue = json!({
            "base": "http://localhost:4000",
            "auth": fingerprint("secret-token"),
            "project_id": "p1",
            "items": [
                { "cat": "UserController", "doc": { "method": "GET", "path": "/api/users/list" } },
                { "cat": "UserController", "doc": { "method": "POST", "path": "/api/users/create" } },
            ],
        });
        host::storage::set(QUEUE_KEY, &queue.to_string()).unwrap();

        let mut plugin = ApiMocktle::default();
        plugin.handle_events(&[Event::Scheduled], &host.invocation());

        assert!(host::storage::get(QUEUE_KEY).unwrap().is_none(), "the queue is drained");
        let saved = host.requests().iter().filter(|r| r.url.contains("/api/interface/save")).count();
        assert_eq!(saved, 2);
    }

    /// A queue belongs to the server and token it was built for. Once the
    /// settings point elsewhere the queue stays put and no request goes out —
    /// the ids it carries would otherwise write into an unrelated project.
    #[test]
    fn a_queue_for_another_server_is_paused_not_resent() {
        let host = host();
        let items: Vec<Value> = (0..201)
            .map(|i| json!({ "cat": "UserController", "doc": { "method": "GET", "path": format!("/api/items/{i}") } }))
            .collect();
        let queue = json!({
            "base": "http://localhost:4000",
            "auth": fingerprint("secret-token"),
            "project_id": "p1",
            "items": items,
        });
        host::storage::set(QUEUE_KEY, &queue.to_string()).unwrap();

        let mut plugin = ApiMocktle::default();
        // The first wake-up posts the batch the queue was built for.
        plugin.handle_events(&[Event::Scheduled], &host.invocation());
        let left = {
            let raw = host::storage::get(QUEUE_KEY).unwrap().expect("a batch leaves items behind");
            let queued: Value = serde_json::from_str(&raw).unwrap();
            queued["items"].as_array().map(Vec::len).unwrap_or(0)
        };
        assert!(left > 0 && left < 201, "the queue is partially sent: {left}");
        let before = host.requests().len();
        assert!(before > 0, "the first batch did go out");

        // The setting moves to another server before the next wake-up.
        host.set_setting("serverUrl", "\"http://localhost:5000\"");
        host.set_setting("token", "\"other-token\"");
        plugin.handle_events(&[Event::Scheduled], &host.invocation());

        assert_eq!(host.requests().len(), before, "a paused queue makes no request at all");
        assert!(
            !host.requests().iter().any(|r| r.url.contains("localhost:5000")),
            "nothing reaches the new server"
        );
        let kept = host::storage::get(QUEUE_KEY).unwrap().expect("the queue is kept for later");
        let kept: Value = serde_json::from_str(&kept).unwrap();
        assert_eq!(kept["items"].as_array().map(Vec::len), Some(left), "the rest is neither sent nor dropped");
        let status = host.status_items();
        let item = status.get("sync").expect("the pause shows in the status bar");
        assert!(item.text.contains("暂停"), "{}", item.text);
        assert!(
            host.logs().iter().any(|(level, text)| *level == LogLevel::Warn && text.contains("自动续传失败")),
            "{:?}",
            host.logs()
        );
    }

    /// The guard must not fire on a healthy export: the same queue with the
    /// setting untouched keeps advancing.
    #[test]
    fn an_unchanged_target_keeps_the_queue_moving() {
        let host = host();
        let queue = json!({
            "base": "http://localhost:4000",
            "auth": fingerprint("secret-token"),
            "project_id": "p1",
            "items": [
                { "cat": "UserController", "doc": { "method": "GET", "path": "/api/users/list" } },
                { "cat": "UserController", "doc": { "method": "POST", "path": "/api/users/create" } },
            ],
        });
        host::storage::set(QUEUE_KEY, &queue.to_string()).unwrap();

        let mut plugin = ApiMocktle::default();
        plugin.handle_events(&[Event::Scheduled], &host.invocation());

        assert!(host::storage::get(QUEUE_KEY).unwrap().is_none(), "the queue drains");
        let saved = host.requests().iter().filter(|r| r.url.contains("/api/interface/save")).count();
        assert_eq!(saved, 2);
        assert!(!host.status_items().contains_key("sync"), "a finished export leaves no status item");
    }

    /// A queue written before it recorded a target (0.2.0) cannot be routed:
    /// it is dropped instead of guessed at, and nothing is sent.
    #[test]
    fn an_old_queue_without_a_target_is_dropped() {
        let host = host();
        let queue = json!({
            "project_id": "p1",
            "items": [{ "cat": "UserController", "doc": { "method": "GET", "path": "/api/users/list" } }],
        });
        host::storage::set(QUEUE_KEY, &queue.to_string()).unwrap();

        let mut plugin = ApiMocktle::default();
        let text = plugin.run_command("export-continue", &host.invocation(), None).unwrap().unwrap();

        assert_eq!(text, "没有待导出的接口");
        assert!(host::storage::get(QUEUE_KEY).unwrap().is_none(), "the old queue is dropped");
        assert!(host.requests().is_empty(), "a queue of unknown origin sends nothing");
        // Dropping is silent in the command's own text, so the log has to say it.
        assert!(
            host.logs().iter().any(|(level, text)| *level == LogLevel::Warn && text.contains("已丢弃")),
            "{:?}",
            host.logs()
        );
    }

    /// A queue that needs several batches must survive its own write-back: the
    /// target it records has to match the settings on the next wake-up, or a
    /// healthy export would pause itself after the first batch.
    #[test]
    fn a_multi_batch_queue_keeps_moving_across_wake_ups() {
        let host = host();
        let items: Vec<Value> = (0..201)
            .map(|i| json!({ "cat": "UserController", "doc": { "method": "GET", "path": format!("/api/items/{i}") } }))
            .collect();
        let queue = json!({
            "base": "http://localhost:4000",
            "auth": fingerprint("secret-token"),
            "project_id": "p1",
            "items": items,
        });
        host::storage::set(QUEUE_KEY, &queue.to_string()).unwrap();

        let mut plugin = ApiMocktle::default();
        plugin.handle_events(&[Event::Scheduled], &host.invocation());
        let raw = host::storage::get(QUEUE_KEY).unwrap().expect("the first batch leaves the rest");
        let queued: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(queued["items"].as_array().map(Vec::len), Some(1), "one item is left over");
        assert_eq!(queued["base"], "http://localhost:4000", "the write-back keeps its target");
        assert_eq!(queued["auth"], fingerprint("secret-token"));
        assert!(host.status_items()["sync"].text.contains("同步中"), "{:?}", host.status_items());

        plugin.handle_events(&[Event::Scheduled], &host.invocation());
        assert!(host::storage::get(QUEUE_KEY).unwrap().is_none(), "the second wake-up drains it");
        assert!(!host.status_items().contains_key("sync"), "no pause is reported");
        let saved = host.requests().iter().filter(|r| r.url.contains("/api/interface/save")).count();
        assert_eq!(saved, 201, "every queued interface went out exactly once");
    }

    /// Starting a new export retires the pause notice and the queue that caused
    /// it: the notice must not outlive the queue it describes.
    #[test]
    fn a_new_export_clears_the_pause_notice() {
        let host = host();
        let queue = json!({
            "base": "http://localhost:5000",
            "auth": fingerprint("other-token"),
            "project_id": "p1",
            "items": [{ "cat": "UserController", "doc": { "method": "GET", "path": "/api/users/list" } }],
        });
        host::storage::set(QUEUE_KEY, &queue.to_string()).unwrap();

        let mut plugin = ApiMocktle::default();
        plugin.handle_events(&[Event::Scheduled], &host.invocation());
        assert!(host.status_items()["sync"].text.contains("暂停"), "{:?}", host.status_items());

        let text = plugin.run_command("export-project", &host.invocation(), None).unwrap().unwrap();
        assert!(text.contains("已同步到 ApiMocktle"), "{text}");
        assert!(!host.status_items().contains_key("sync"), "the pause notice is retired: {:?}", host.status_items());
        let saved = host.requests().iter().filter(|r| r.url.contains("/api/interface/save")).count();
        assert_eq!(saved, 2, "the new queue goes to the current server");
    }

    /// A hand-typed repository is normalised, and an empty field means Maven's
    /// default. There is no folder chooser: the settings window cannot open a
    /// dialog, and a grant only comes from the manifest anyway.
    #[test]
    fn the_typed_repository_is_normalised() {
        let host = host();
        assert_eq!(deps::repository(), deps::DEFAULT_REPO, "nothing set yet");
        host.set_setting("mavenRepository", "\"D:\\\\maven\\\\repo\\\\\"");
        assert_eq!(deps::repository(), "D:/maven/repo");
        host.set_setting("mavenRepository", "\"  \"");
        assert_eq!(deps::repository(), deps::DEFAULT_REPO, "a blank field falls back");
    }

    #[test]
    fn a_category_cache_is_scoped_to_its_server() {
        let _host = host();
        let mut first = HashMap::new();
        first.insert("UserController".to_owned(), "c1".to_owned());
        save_categories("http://localhost:4000", "p1", &first);
        let mut second = HashMap::new();
        second.insert("UserController".to_owned(), "c2".to_owned());
        save_categories("http://localhost:5000", "p1", &second);

        assert_eq!(load_categories("http://localhost:4000", "p1")["UserController"], "c1");
        assert_eq!(load_categories("http://localhost:5000", "p1")["UserController"], "c2");
        assert!(load_categories("http://localhost:9000", "p1").is_empty(), "a third server starts empty");
    }

    /// Multipart parts reach the model and the document; cookie parameters
    /// still stop at the parser, exactly as the upstream plugin leaves them out.
    #[test]
    fn multipart_parts_reach_the_model_and_cookies_do_not() {
        let host = host();
        host.add_file(
            "/proj/src/UploadController.java",
            r#"
package com.acme;

@RestController
@RequestMapping("/api/files")
public class UploadController {
    @PostMapping("/upload")
    public void upload(@RequestPart("file") MultipartFile file,
                       @RequestPart(value = "meta", required = false) String meta,
                       @CookieValue("session") String session) { }
}
"#,
        );
        let paths = ["/proj/src/UserController.java", "/proj/src/UploadController.java"];
        host.on_search(move |_query, _limit| {
            Ok(TextSearch {
                matches: paths
                    .iter()
                    .map(|path| TextMatch {
                        location: Location {
                            path: (*path).to_owned(),
                            range: Range::new(Position::new(0, 0), Position::new(0, 1)),
                        },
                        line: String::new(),
                    })
                    .collect(),
                limit_hit: false,
            })
        });

        let project = host.invocation().project().expect("a project");
        let scanned = scan::scan(&project, &scan::Options::default()).unwrap();
        let upload = scanned
            .endpoints
            .iter()
            .find(|e| e.path == "/api/files/upload")
            .expect("the upload route");
        let parts: Vec<&str> = upload.form_params.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(parts, vec!["file", "meta"]);
        assert_eq!(upload.form_params[0].ty, "MultipartFile");
        assert!(upload.form_params[0].required);
        assert!(!upload.form_params[1].required);
        let query: Vec<&str> = upload.query_params.iter().map(|p| p.name.as_str()).collect();
        assert!(query.is_empty(), "a cookie parameter is not a query parameter either: {query:?}");

        let doc = yapi::api_doc(upload, &scanned.dto, 2);
        assert_eq!(doc["req_body_type"], "form");
        assert_eq!(doc["req_body_form"][0]["type"], "file");
        assert_eq!(doc["req_body_form"][1]["type"], "text");
        assert_eq!(doc["req_body_form"][0]["required"], 1);
        assert_eq!(doc["req_body_form"][1]["required"], 0);
    }

    #[test]
    fn the_route_scan_skips_dto_resolution() {
        let host = host();
        let project = host.invocation().project().expect("a project");
        let light = scan::scan(&project, &route_options()).unwrap();
        assert_eq!(light.endpoints.len(), 2);
        assert!(light.dto.fields.is_empty(), "routes do not need the DTO graph");
        let full = scan::scan(&project, &scan::Options { resolve_dto: true, ..route_options() }).unwrap();
        assert!(!full.dto.fields.is_empty());
    }

    #[test]
    fn a_stale_category_id_is_rebuilt_once() {
        let host = host();
        let saves = std::rc::Rc::new(std::cell::Cell::new(0usize));
        let counter = saves.clone();
        host.on_fetch(move |request| {
            let url = request.url.as_str();
            if url.contains("/api/project/list") {
                reply(200, json!({ "errcode": 0, "data": [{ "_id": "p1", "name": "Demo" }] }))
            } else if url.contains("/api/interface/getCatMenu") {
                // The server keeps offering an id that its save endpoint rejects.
                reply(200, json!({ "errcode": 0, "data": [{ "_id": "stale", "name": "UserController" }] }))
            } else if url.contains("/api/interface/add_cat") {
                reply(200, json!({ "errcode": 0, "data": { "_id": "fresh", "name": "UserController" } }))
            } else if url.contains("/api/interface/list_cat") {
                reply(200, json!({ "errcode": 0, "data": { "list": [] } }))
            } else if url.contains("/api/interface/save") {
                counter.set(counter.get() + 1);
                let body = String::from_utf8(request.body.clone().unwrap_or_default()).unwrap_or_default();
                if body.contains("\"catid\":\"stale\"") {
                    reply(200, json!({ "errcode": 400, "errmsg": "分类(catid)不存在或不属于该项目" }))
                } else {
                    reply(200, json!({ "errcode": 0 }))
                }
            } else {
                reply(404, json!({ "errcode": 404 }))
            }
        });

        let mut plugin = ApiMocktle::default();
        let text = plugin.run_command("export-project", &host.invocation(), None).unwrap().unwrap();
        assert!(text.contains("已同步到 ApiMocktle"), "{text}");
        assert!(!text.contains("失败"), "{text}");
        assert!(saves.get() >= 3, "each interface tried the stale id and its rebuild");

        let bodies: Vec<String> = host
            .requests()
            .iter()
            .filter(|r| r.url.contains("/api/interface/save"))
            .filter_map(|r| r.body.clone())
            .map(|b| String::from_utf8(b).unwrap_or_default())
            .collect();
        assert_eq!(bodies.iter().filter(|b| b.contains("\"catid\":\"fresh\"")).count(), 2);
    }
}
