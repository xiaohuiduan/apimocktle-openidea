//! ApiMocktle for OpenIDEA.
//!
//! OpenIDEA plugins are WebAssembly components, so there is no Java parser to
//! borrow: routes are recovered from tokens (`spring`), described with JSON
//! Schema (`model`) and handed to three consumers — Search Everywhere through
//! the `endpoints` contribution, a dashboard view, and an ApiMocktle / YApi
//! server through `host::fetch`.

pub mod classfile;
pub mod deps;
pub mod i18n;
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
/// Interfaces one call may start.
///
/// A guard, not the real limit: the loop also stops itself on the budget, and a
/// local ApiMocktle answers in a few milliseconds, so the budget is what stops a
/// slow server. High enough that a whole project fits in one call — 3 598
/// interfaces at the measured ~7.7 ms each is ~28 s, inside the task budget.
const BATCH: usize = 20_000;
/// How long one batch may keep starting interfaces.
///
/// Export always runs inside a `host::begin_task` (`flush_export`), which raises
/// the call from the host's 5 s to 60 s and renews it on every report — so 50 s
/// is the real allowance, leaving 10 s to finish and report. At the measured
/// ~7.7 ms per interface this fits ~6 500 of them, i.e. a whole project in one
/// call instead of one batch per minute.
const EXPORT_BUDGET: Duration = Duration::from_millis(50_000);
const QUEUE_KEY: &str = "export:queue";
/// Category ids per server and project, kept across batches. The base URL is
/// part of the key so a `catid` from one server is never used on another.
const CATS_KEY: &str = "cats:";
/// Routes of the last completed scan, keyed by project root.
///
/// The endpoints provider gets a hard **2 s** call budget
/// (`ide-host/src/plugins/endpoints.rs:99`, `rt.endpoints(ctx, 2s)`), and a
/// project-wide `search_text` alone costs 3.7 s on a large one — so a scan
/// started from inside that provider can never finish; it is killed, the
/// instance is reaped, and the IDE shows "Plugin ApiMocktle crashed". A
/// `begin_task` does not help: the provider's deadline is a hard bound that no
/// plugin-side call can move.
///
/// So the provider does not scan. The dashboard and export already scan on a
/// task's budget; they publish the route list here, and the provider answers
/// from it. A miss returns an empty list (the status bar simply shows no
/// endpoints) and the next dashboard open fills it. Storage allows 64 MiB; a
/// few thousand routes are a few hundred KiB.
const ROUTES_KEY: &str = "routes:";

/// The routes of the last completed scan for `root`, as the endpoints provider
/// needs them.
fn routes_cache_key(root: &str) -> String {
    format!("{ROUTES_KEY}{root}")
}

/// Publishes `scanned`'s routes for the endpoints provider.
///
/// Only the fields a `Route` uses are stored: the provider renders method, path,
/// framework, handler and location, and nothing else, so the DTO graph and the
/// rest are left out — this is what keeps the value small enough to be a cheap
/// `storage` write on every scan.
fn publish_routes(root: &str, scanned: &scan::ScanResult) {
    let items: Vec<Value> = scanned
        .endpoints
        .iter()
        .map(|endpoint| {
            serde_json::json!({
                "method": endpoint.method,
                "path": endpoint.path,
                "framework": endpoint.framework,
                "handler": endpoint.handler(),
                "file": endpoint.file,
                "line": endpoint.line,
                "col": endpoint.col,
            })
        })
        .collect();
    let payload = serde_json::json!({ "items": items });
    if let Err(e) = host::storage::set(&routes_cache_key(root), &payload.to_string()) {
        host::log(
            LogLevel::Warn,
            &format!("ApiMocktle: 路由缓存写入失败：{e}"),
        );
    }
}

/// The routes published by the last scan, or an empty list.
///
/// Never scans: the endpoints provider has 2 s and a scan cannot fit.
fn cached_routes(root: &str) -> Vec<Value> {
    let Ok(Some(raw)) = host::storage::get(&routes_cache_key(root)) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return Vec::new();
    };
    value
        .get("items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The description every category this plugin creates carries.
fn category_desc() -> &'static str {
    i18n::CATEGORY_DESC.get(i18n::lang())
}

#[derive(Default)]
pub struct ApiMocktle {
    /// Project root to its last full scan (DTOs resolved); cleared by refresh.
    cache: HashMap<String, Rc<ScanResult>>,
    /// Routes only, for the endpoints provider and its 2-second budget.
    routes: HashMap<String, Rc<ScanResult>>,
    /// Overrides `SCAN_BUDGET`, for tests that need the DTO pass to run out of
    /// time on purpose. `None` in production; not a plugin setting.
    ///
    /// This is not `#[cfg(test)]` on purpose: `tests/` links the library as a
    /// normal dependency, where `cfg(test)` is off, so a test-gated field would
    /// be invisible to exactly the integration test that needs it. The entry
    /// point that sets it carries `#[doc(hidden)]` and the shipped wasm build
    /// never calls it.
    scan_budget_override: Option<Duration>,
    /// Overrides `scan::TYPE_BUDGET`, for the same reason: the test that pins
    /// the cap's refusal has to reach the cap without a 5 000-type project.
    type_budget_override: Option<usize>,
    /// Overrides `EXPORT_BUDGET`; see `set_export_budget_for_tests`.
    export_budget_override: Option<Duration>,
}

struct Config {
    base: String,
    token: String,
}

fn config() -> Result<Config, Error> {
    let base = host::setting_string("serverUrl").unwrap_or_default().trim().to_owned();
    let token = host::setting_string("token").unwrap_or_default().trim().to_owned();
    let lang = i18n::lang();
    if base.is_empty() {
        return Err(Error::Invalid(i18n::SET_SERVER_ADDRESS.get(lang).into()));
    }
    if token.is_empty() {
        return Err(Error::Invalid(i18n::SET_PERSONAL_TOKEN.get(lang).into()));
    }
    Ok(Config { base, token })
}

/// Stop a scan just under the host's five-second call limit. Overrunning gets
/// the instance reaped — and the in-memory scan cache with it — so the next
/// call starts from zero and overruns again. 4.2 s leaves 800 ms to finish and
/// return, the same margin the export batch uses.
const SCAN_BUDGET: Duration = Duration::from_millis(4_200);
/// The budget a scan gets while a `host::begin_task` is open: the host raises
/// the call's wall clock from 5 s to 60 s for as long as the task is open and
/// keeps renewing it on every `report` (`plugins/README.md`, `tasks.begin`).
///
/// 50 s leaves 10 s of the 60 to build the export queue and send its first
/// batch inside the same call. Without this, a project whose DTO closure needs
/// more than 4.2 s could never be exported at all — retrying reproduced the
/// same refusal every time, because the scan always started from zero.
const TASK_SCAN_BUDGET: Duration = Duration::from_millis(50_000);

fn scan_options() -> scan::Options {
    scan::Options {
        include_kotlin: host::setting_bool("includeKotlin").unwrap_or(true),
        depth: host::setting_number("schemaDepth").map(|n| n as u32).unwrap_or(3).clamp(1, 4),
        deps: crate::deps::Config {
            enabled: host::setting_bool("resolveDependencyJars").unwrap_or(true),
            repo: crate::deps::repository(),
        },
        budget: Some(SCAN_BUDGET),
        ..scan::Options::default()
    }
}

/// `scan_options`, narrowed to one file or folder and given `budget`.
///
/// Narrowing here rather than filtering endpoints afterwards is what makes the
/// refusal's advice ("export the current file or folder") true: the parse and
/// the DTO closure then only cover the controllers that were asked for.
fn scan_options_narrowed(only_path: Option<&str>, budget: Duration) -> scan::Options {
    scan::Options {
        only_path: only_path.map(str::to_owned),
        budget: Some(budget),
        ..scan_options()
    }
}

/// Routes only, but over every candidate file.
///
/// The dashboard and "go to source" need the route list and nothing else —
/// `scan::to_json` reads `body_type`, not the DTO graph, and the page shows the
/// request body's *type name*, never its fields. Resolving the closure for them
/// read one source file per DTO type for nothing: on a 300-controller project
/// that was 388 of 713 `read_file` calls.
///
/// `route_options()` cannot be reused here: its 400-file cap exists for the
/// endpoints provider's 2 s budget, and a dashboard that silently listed fewer
/// controllers than the project has would be a wrong answer, not a fast one.
fn route_only_options() -> scan::Options {
    scan::Options { resolve_dto: false, ..scan_options() }
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
    let found = client.find_or_create_category(name, category_desc()).ok()?;
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
    let created = client.create_category(name, category_desc()).ok()?;
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
    let lang = i18n::lang();
    let same_server = queued_base == current_base;
    let text = if same_server {
        i18n::PAUSE_OTHER_TOKEN.get(lang)
    } else {
        i18n::PAUSE_OTHER_SERVER.get(lang)
    };
    let mismatch = if same_server {
        i18n::queue_other_token()
    } else {
        i18n::queue_other_server(queued_base, current_base)
    };
    host::set_status_item(
        "sync",
        &StatusItem {
            text: text.into(),
            tooltip: Some(i18n::PAUSE_TOOLTIP.get(lang).into()),
            command: Some("open-dashboard".into()),
        },
    );
    Error::Failed(i18n::paused_for_target(&mismatch))
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
    /// The full scan, for an export that already holds a task.
    ///
    /// `narrow` restricts the parse and the DTO closure to one file or folder,
    /// which is what makes "export the current file or folder" a way past a
    /// deadline. The task is what supplies the 60-second call budget; a scan
    /// without one would be cut off at the host's 5 s.
    ///
    /// A narrowed result is never cached: it describes one corner of the
    /// project, and handing it back as a full scan would silently drop every
    /// endpoint outside it. It also does not reuse a cached full scan, since the
    /// point of narrowing is to look at less.
    fn scan_export(
        &mut self,
        project: &Project,
        narrow: Option<&str>,
    ) -> Result<Rc<ScanResult>, Error> {
        let root = project.root()?;
        if narrow.is_none()
            && let Some(hit) = self.cache.get(&root)
        {
            return Ok(hit.clone());
        }
        // `scan_budget_override` is a test hook: production always takes the
        // task's 60-second allowance.
        let budget = self.scan_budget_override.unwrap_or(TASK_SCAN_BUDGET);
        let options = scan::Options {
            type_budget: self.type_budget_override,
            ..scan_options_narrowed(narrow, budget)
        };

        // The task is what raises this call's wall clock from 5 s to 60 s, and
        // only `report`/`is_cancelled` keep renewing it. `None` when the host
        // refuses one: the scan then runs on the 4.2 s budget as before.
        let task = host::begin_task(i18n::SCAN_TASK_TITLE.get(i18n::lang()), true);
        task.report(0, None, Some(i18n::SCAN_TASK_ROUTES.get(i18n::lang())));

        let result = Rc::new(scan::scan_with_progress(project, &options, |done, total| {
            task.report(done as u32, Some(total as u32), None);
        })?);
        drop(task);

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
        // A partial DTO graph is never cached as the full result: it would be
        // handed to every later export in this session. The route list stays
        // correct, so it is kept for display, and the next call retries the
        // closure from scratch.
        if result.dto_stop.is_some() {
            host::log(
                LogLevel::Warn,
                &format!(
                    "ApiMocktle: DTO 解析未完成（{} 个类型，原因 {}），本次结果不写入缓存",
                    result.dto.fields.len(),
                    match result.dto_stop {
                        Some(scan::DtoStop::Deadline) => "超时",
                        Some(scan::DtoStop::TypeBudget) => "类型数上限",
                        None => "无",
                    }
                ),
            );
            if narrow.is_none() {
                self.routes.insert(root.clone(), result.clone());
                publish_routes(&root, &result);
            }
        } else if narrow.is_none() {
            self.cache.insert(root.clone(), result.clone());
            // The route-only entry is now redundant: `scan()` prefers `cache`.
            self.routes.remove(&root);
            // An export scans the whole project, so its routes are good for the
            // endpoints provider too — and export is the path that runs on the
            // long task budget.
            publish_routes(&root, &result);
        }
        Ok(result)
    }

    /// Route list over every candidate file, for the dashboard and for
    /// "go to source". Skips the DTO closure, which neither of them reads.
    ///
    /// A view call gets 5 s of wall clock, which is not enough for a real
    /// project: a large one needs one `read_file` per controller, and the host
    /// charges a few milliseconds for each. The dashboard therefore opens a
    /// task, exactly like export does, and scans on the task's budget — the page
    /// shows every route instead of a truncated prefix.
    ///
    /// The task is closed before the caller builds the payload: once it is gone
    /// the call is back on its 5 s clock, and serialising a few hundred
    /// kilobytes is far inside that.
    fn routes_for_display(&mut self, project: &Project) -> Result<Rc<ScanResult>, Error> {
        let root = project.root()?;
        if let Some(hit) = self.cache.get(&root) {
            return Ok(hit.clone());
        }
        if let Some(hit) = self.routes.get(&root) {
            return Ok(hit.clone());
        }
        // `scan_budget_override` is the test hook; production takes the task's
        // allowance, same as export.
        let budget = self.scan_budget_override.unwrap_or(TASK_SCAN_BUDGET);
        let task = host::begin_task(i18n::SCAN_TASK_TITLE.get(i18n::lang()), true);
        task.report(0, None, Some(i18n::SCAN_TASK_ROUTES.get(i18n::lang())));
        let options = scan::Options { budget: Some(budget), ..route_only_options() };
        let result = scan::scan_with_progress(project, &options, |done, total| {
            task.report(done as u32, Some(total as u32), None);
        });
        drop(task);
        let result = Rc::new(result?);
        host::log(
            LogLevel::Info,
            &format!(
                "ApiMocktle: {} 个控制器文件 → {} 个接口（仅路由，未解析 DTO）{}{}",
                result.files,
                result.endpoints.len(),
                if result.truncated { "（已达搜索上限）" } else { "" },
                if result.stopped_early { "（预算内未扫完，结果不全）" } else { "" }
            ),
        );
        self.routes.insert(root.clone(), result.clone());
        // Hand the routes to the endpoints provider, which cannot scan itself
        // (2 s budget, and a project-wide search alone costs more than that).
        publish_routes(&root, &result);
        Ok(result)
    }

    /// Scans for an export, refusing a partial DTO graph.
    ///
    /// Both export entry points need this: a schema built from a graph that
    /// stopped on the deadline silently loses fields, and `save_api` *updates*
    /// the existing document by `method + path`, so sending one would replace a
    /// correct document with a poorer one and still report success. Refusing
    /// before any request goes out is the only outcome that cannot damage the
    /// project's API documents.
    fn scan_for_export(
        &mut self,
        ctx: &Invocation,
        narrow: Option<&str>,
    ) -> Result<(std::rc::Rc<ScanResult>, Project), Error> {
        let project =
            ctx.project().ok_or_else(|| Error::Failed(i18n::OPEN_A_PROJECT.get(i18n::lang()).into()))?;
        let scanned = self.scan_export(&project, narrow)?;
        if let Some(stop) = scanned.dto_stop {
            let lang = i18n::lang();
            host::log(
                LogLevel::Warn,
                &format!(
                    "ApiMocktle: 拒绝导出：DTO 解析未完成（已解析 {} 个类型，路由 {} 个，原因 {:?}）",
                    scanned.dto.fields.len(),
                    scanned.endpoints.len(),
                    stop
                ),
            );
            // The two reasons get different words because they need different
            // actions: a deadline is worth retrying (and narrowing helps), while
            // the type cap stops at the same type every time.
            let text = match stop {
                scan::DtoStop::Deadline => i18n::DTO_INCOMPLETE.get(lang),
                scan::DtoStop::TypeBudget => i18n::DTO_TYPE_LIMIT.get(lang),
            };
            return Err(Error::Failed(text.into()));
        }
        Ok((scanned, project))
    }

    /// Builds one export queue for the endpoints and sends its first batch.
    /// `only_path` limits it to one file or folder.
    fn start_export(
        &mut self,
        ctx: &Invocation,
        only_path: Option<String>,
        target: Option<String>,
    ) -> Result<String, Error> {
        // `Failed` rather than `Invalid`: these strings are shown to the user as
        // written (`Display`), while `Invalid` would prefix them with "invalid: ".
        let cfg = config()?;
        // The scope reaches the scan, so narrowing really narrows: the parse and
        // the DTO closure only cover the chosen file or folder.
        let (scanned, _project) = self.scan_for_export(ctx, only_path.as_deref())?;
        let picked: Vec<&Endpoint> = scanned
            .endpoints
            .iter()
            .filter(|e| only_path.as_ref().is_none_or(|path| under(&e.file, path)))
            .collect();
        if picked.is_empty() {
            return Err(Error::Invalid(i18n::NO_ENDPOINTS_FOUND.get(i18n::lang()).into()));
        }

        let probe = yapi::Client { base: cfg.base.clone(), token: cfg.token.clone(), project_id: String::new() };
        let projects = probe.list_projects()?;
        if projects.is_empty() {
            return Err(Error::Failed(i18n::NO_PROJECTS_ON_SERVER.get(i18n::lang()).into()));
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
                host::pick(i18n::PICK_PROJECT.get(i18n::lang()), &choices)?.ok_or(Error::Cancelled)?
            }
        };

        // One category per controller: `TicketOrderController`, `PayController`, …
        let depth = scan_options().depth;
        let items: Vec<Value> = picked
            .iter()
            .map(|endpoint| {
                json!({
                    "cat": endpoint.class_name.clone(),
                    "doc": yapi::api_doc(endpoint, &scanned.dto, depth, i18n::lang()),
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
    ///
    /// Opens a background task first: the host gives a plain command 5 s, and at
    /// the measured ~7.7 ms per interface that is about 200 of them — which is
    /// why a large project needed eighteen clicks of "continue". A task raises
    /// the call to 60 s and every `report` renews it, so the whole queue goes out
    /// in one call instead.
    fn flush_export(&mut self, cfg: &Config, notify: bool) -> Result<String, Error> {
        let task = host::begin_task(i18n::EXPORT_TASK_TITLE.get(i18n::lang()), true);
        task.report(0, None, Some(i18n::EXPORT_TASK_SENDING.get(i18n::lang())));
        let result = self.flush_export_on_task(cfg, notify, &task);
        drop(task);
        result
    }

    /// The batch loop, reporting progress on `task` so its budget keeps renewing.
    fn flush_export_on_task(
        &mut self,
        cfg: &Config,
        notify: bool,
        task: &host::Task,
    ) -> Result<String, Error> {
        let Some(raw) = host::storage::get(QUEUE_KEY)? else {
            return Ok(i18n::NOTHING_TO_EXPORT.get(i18n::lang()).into());
        };
        let lang = i18n::lang();
        let mut queue: Value =
            serde_json::from_str(&raw).map_err(|e| Error::Failed(i18n::queue_corrupt(&e.to_string())))?;
        let items = queue.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
        let queued_base = queue.get("base").and_then(Value::as_str).unwrap_or_default().to_owned();
        let queued_auth = queue.get("auth").and_then(Value::as_str).unwrap_or_default().to_owned();
        // A queue that cannot name its target — one written by 0.2.0 — is
        // dropped instead of being guessed at. The command shows
        // `没有待导出的接口`; the log says what was thrown away, because the
        // schedule path has no other way to report it.
        if items.is_empty() {
            host::storage::delete(QUEUE_KEY)?;
            return Ok(i18n::NOTHING_TO_EXPORT.get(lang).into());
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
            return Ok(i18n::NOTHING_TO_EXPORT.get(lang).into());
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
        let total = items.len();
        let budget = self.export_budget_override.unwrap_or(EXPORT_BUDGET);

        for item in items {
            // The clock guard never fires before a few interfaces went out, so
            // an unexpected clock cannot shrink a batch to one. The budget is the
            // task's now (50 s, renewed by every report below), so a normal
            // project finishes in one call instead of one batch per minute.
            if sent >= BATCH || (sent >= 5 && started.elapsed() > budget) {
                rest.push(item);
                continue;
            }
            sent += 1;
            // Renews the task's 60 s allowance. Cheap enough to do per interface:
            // the host throttles UI updates to ~10/s on its side.
            task.report(sent as u32, Some(total as u32), None);
            let category = item
                .get("cat")
                .and_then(Value::as_str)
                .unwrap_or(i18n::DEFAULT_CATEGORY.get(lang))
                .to_owned();
            let doc = item.get("doc").cloned().unwrap_or(Value::Null);
            let Some(cat_id) = resolve_category(&client, &category, &mut categories) else {
                failed.push(i18n::category_unavailable(&category));
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
                            Err(second) => failed.push(i18n::failed_endpoint(&key, &second.to_string())),
                        },
                        _ => failed.push(i18n::failed_endpoint(&key, &first.to_string())),
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
            i18n::failed_note(failed.len(), &shown)
        };
        let summary = if left == 0 {
            i18n::export_done(queued, &failed_note)
        } else {
            i18n::export_partial(saved, left, &failed_note)
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
                    &[NotificationAction::new(i18n::CONTINUE_NOW.get(lang), "export-continue")],
                )?;
            } else {
                host::notify(NotificationLevel::Info, "ApiMocktle", &summary);
            }
        } else if left > 0 {
            host::set_status_item(
                "sync",
                &StatusItem {
                    text: i18n::syncing_left(left),
                    tooltip: Some(i18n::SYNC_LEFT_TOOLTIP.get(lang).into()),
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
            host::notify(
                NotificationLevel::Info,
                "ApiMocktle",
                i18n::BACKGROUND_DONE.get(i18n::lang()),
            );
        }
        Ok(())
    }

    /// Runs an export command with a chosen scan deadline.
    ///
    /// The production entry points read `SCAN_BUDGET`, which no host setting can
    /// change — so the integration test that pins the DTO-deadline refusal (it
    /// has to make the closure miss its deadline while a complete scan still
    /// succeeds) drives the command through this door instead.
    #[doc(hidden)]
    pub fn run_command_in_tests(
        &mut self,
        id: &str,
        ctx: &Invocation,
        scan_budget: Option<Duration>,
    ) -> Result<String, Error> {
        self.scan_budget_override = scan_budget;
        self.run_command(id, ctx, None)?.ok_or_else(|| Error::Failed("no text".into()))
    }

    /// Overrides `scan::TYPE_BUDGET` for the tests that pin the cap's refusal.
    ///
    /// The cap is deliberately far above any real project, so reaching it with a
    /// fixture would mean generating thousands of types. The override lets those
    /// tests drive the same code path with three.
    #[doc(hidden)]
    pub fn set_type_budget_for_tests(&mut self, budget: Option<usize>) {
        self.type_budget_override = budget;
    }

    /// Overrides the export batch budget, for the tests that need a queue to
    /// span more than one call.
    ///
    /// Production sends a whole project in one call (50 s task budget); a test
    /// that wants to observe the multi-batch path cannot wait for that, and
    /// cannot make a mock server slow on demand.
    #[doc(hidden)]
    pub fn set_export_budget_for_tests(&mut self, budget: Option<Duration>) {
        self.export_budget_override = budget;
    }

    /// Delivers one dashboard message with a chosen scan deadline.
    ///
    /// The dashboard's own entry point (`ViewEvent::Message`) reports through a
    /// toast and reads the production budget — so a test that needs the DTO pass
    /// to run out of time while checking the page's outcome drives it through
    /// this door instead. The page's payload is read back with
    /// `MockHost::view_messages`, exactly as the other dashboard tests do.
    #[doc(hidden)]
    pub fn run_view_message_in_tests(
        &mut self,
        ctx: &Invocation,
        message: &str,
        scan_budget: Option<Duration>,
    ) -> Result<(), Error> {
        self.scan_budget_override = scan_budget;
        self.handle(VIEW, ViewEvent::Message(message.to_owned()), ctx)
    }
}

impl Plugin for ApiMocktle {
    fn activate(&mut self, _ctx: &Context) -> Result<(), Error> {
        let base = host::setting_string("serverUrl").unwrap_or_default();
        // The UI language is only visible to the plugin through `locale()`, and a
        // wrong dashboard language is otherwise indistinguishable from a wrong
        // catalog; logging it once makes the difference diagnosable.
        host::log(
            LogLevel::Info,
            &format!(
                "ApiMocktle {} 已激活（serverUrl={base}，API {}，host::locale()={}）",
                env!("CARGO_PKG_VERSION"),
                openidea_plugin::API_VERSION,
                host::locale()
            ),
        );
        Ok(())
    }

    fn run_command(&mut self, id: &str, ctx: &Invocation, arg: Option<&str>) -> Result<Option<String>, Error> {
        match id {
            "export-project" => self.start_export(ctx, None, None).map(Some),
            "export-file" => {
                let path = target_path(ctx, arg).ok_or_else(|| {
                    Error::Invalid(i18n::SELECT_FILE_OR_FOLDER.get(i18n::lang()).into())
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
                host::notify(
                    NotificationLevel::Info,
                    "ApiMocktle",
                    i18n::CACHE_CLEARED.get(i18n::lang()),
                );
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
        let Ok(root) = project.root() else { return Ok(Vec::new()) };
        // Read-only on purpose. This call has 2 s (`endpoints.rs:99`) and a
        // project-wide `search_text` alone costs 3.7 s on a large project, so any
        // scan started here is killed and the instance reaped with it — the
        // "Plugin ApiMocktle crashed: timed out after 2s" the user saw. The
        // dashboard and export scan on a task budget and publish into this
        // cache; until that has happened once, the provider simply reports no
        // endpoints.
        let cached = cached_routes(&root);
        if cached.is_empty() {
            return Ok(Vec::new());
        }
        let routes: Vec<Route> = cached
            .iter()
            .filter_map(|item| {
                let path = item.get("path").and_then(Value::as_str)?;
                let file = item.get("file").and_then(Value::as_str)?;
                let line = item.get("line").and_then(Value::as_u64).unwrap_or(0) as u32;
                let col = item.get("col").and_then(Value::as_u64).unwrap_or(0) as u32;
                Some(Route {
                    method: item.get("method").and_then(Value::as_str).map(str::to_owned),
                    path: path.to_owned(),
                    framework: item
                        .get("framework")
                        .and_then(Value::as_str)
                        .unwrap_or("Spring")
                        .to_owned(),
                    handler: item.get("handler").and_then(Value::as_str).map(str::to_owned),
                    location: Location {
                        path: file.to_owned(),
                        range: Range::new(Position::new(line, col), Position::new(line, col)),
                    },
                })
            })
            .collect();
        Ok(routes)
    }
}

impl Views for ApiMocktle {
    fn render(&mut self, view: &str, _viewport: Option<Viewport>, _ctx: &Invocation) -> Result<View, Error> {
        if view != VIEW {
            return Err(Error::NotFound(view.into()));
        }
        // The page keeps its own state and asks for data over messages, so the
        // frame is never re-rendered underneath the user. `locale` travels with
        // the start data because the host does not translate an `html` body:
        // the page picks its own text from it.
        // The raw `host::locale()` tag is baked in, because the page gets it
        // verbatim and applies its own fallback (`page.html`). It can be stale —
        // the host may render a view before telling the plugin the current
        // language — so the page also asks for it again after loading.
        let locale = host::locale();
        let start = json!({
            "configured": config().is_ok(),
            "base": host::setting_string("serverUrl").unwrap_or_default(),
            "locale": locale,
        })
        .to_string()
        .replace('<', "\\u003c");
        let page = PAGE.replacen(r#""__START__""#, &start, 1);
        // These two labels are translated by the host from `locales/<tag>.json`,
        // so they are written in the catalog's source language (English).
        Ok(View::new(Body::Html(page))
            .button(Button::new("refresh", "Rescan").icon("refresh"))
            .button(Button::new("export", "Export All").icon("run")))
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
                    Some("locale") => {
                        // The page asks rather than waiting for a push: the host
                        // may render a view before it has told the plugin the
                        // current language, so the value baked in at render time
                        // can be stale. Answering here lets the page correct
                        // itself on load and keep following the IDE afterwards.
                        post(json!({ "op": "locale", "locale": host::locale() }));
                    }
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
                            // Only a file and a position are needed; resolving
                            // the DTO closure for this would be wasted reads.
                            && let Ok(scanned) = self.routes_for_display(&project)
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
            let error = i18n::OPEN_A_PROJECT.get(i18n::lang());
            return json!({ "op": "items", "error": error, "items": [], "count": 0 });
        };
        // The dashboard renders routes only — `to_json` never reads `dto` — so
        // it does not pay for the closure. Export still asks `scan()` for the
        // full result when it needs the graph.
        match self.routes_for_display(&project) {
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
        // The dashboard's per-endpoint sync writes the same documents
        // `export-project` does, so it needs the same refusal: a partial graph
        // here would overwrite one correct document with a poorer one.
        let (scanned, _project) = self.scan_for_export(ctx, None)?;
        let endpoint = scanned
            .endpoints
            .get(index)
            .cloned()
            .ok_or_else(|| Error::Invalid(i18n::ENDPOINT_GONE.get(i18n::lang()).into()))?;
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
                host::pick(i18n::PICK_PROJECT.get(i18n::lang()), &choices)?.ok_or(Error::Cancelled)?
            }
        };
        let client = yapi::Client { base: cfg.base, token: cfg.token, project_id };
        let cat = client.find_or_create_category(&endpoint.class_name, category_desc())?;
        let existing = client
            .list_apis(&cat.id)
            .unwrap_or_default()
            .into_iter()
            .find(|a| a.path == endpoint.path && a.method.eq_ignore_ascii_case(&endpoint.yapi_method()))
            .map(|a| a.id);
        let doc = yapi::api_doc(endpoint, &scanned.dto, scan_options().depth, i18n::lang());
        client.save_api(&doc, &cat.id, existing.as_deref())?;
        Ok(i18n::synced(&endpoint.label()))
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

    /// A CJK ideograph: the character range every Chinese string here uses.
    fn is_cjk(c: char) -> bool {
        ('\u{4e00}'..='\u{9fff}').contains(&c)
    }

    /// A project with one controller and two DTOs, and a YApi server that
    /// answers like ApiMocktle does.
    ///
    /// The host's UI language is `zh-CN` here, so the assertions below can pin
    /// the Chinese text they were written against; `the_same_export_reads_
    /// english_on_an_english_ide` covers the other side.
    fn host() -> MockHost {
        let host = MockHost::install().with_capabilities(&[
            "project.read",
            "storage",
            "net:localhost",
            "net:127.0.0.1",
            "clipboard",
        ]);
        host.set_locale("zh-CN");
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
        let ctx = host.invocation();
        let project = ctx.project().expect("a project");

        // The provider never scans (2 s, and a project-wide search alone costs
        // more): it reads what the dashboard/export published.
        assert!(plugin.endpoints(&ctx).unwrap().is_empty(), "nothing published yet");

        plugin.routes_for_display(&project).expect("the dashboard scans");
        let routes = plugin.endpoints(&ctx).unwrap();
        assert_eq!(routes.len(), 2, "the dashboard published the routes");
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
        // The page carries both sides of its own text table; which one is shown
        // depends on the `locale` in the start data (see below).
        assert!(page.contains("导出全部") && page.contains("Export All"), "the page has both labels");
        assert!(!page.contains("发送请求"), "the dashboard only browses and exports");

        // The host does not translate an `html` body, so the start data tells the
        // page which language to use.
        // The start data carries the raw `host::locale()` tag; the page decides
        // what to show from it plus its own `navigator.language`.
        assert!(page.contains(r#""locale":"zh-CN""#), "the start data carries the raw language tag");
        // The injected value is a JSON *object literal*: the page must read it as
        // a value. `JSON.parse` on it throws, which used to leave the page in
        // English on a Chinese IDE (caught at the page level by check-pages.mjs).
        assert!(
            page.contains(r#"typeof START === "object""#),
            "the page reads the start data as a value, not through JSON.parse"
        );
        // `render` replaces the first quoted placeholder only; a second one
        // would be left in the page as literal text.
        assert!(!page.contains(r#""__START__""#), "the placeholder is gone once rendered");
        let view_buttons: Vec<(&str, &str)> =
            view.toolbar.iter().map(|b| (b.id.as_str(), b.label.as_str())).collect();
        assert_eq!(
            view_buttons,
            vec![("refresh", "Rescan"), ("export", "Export All")],
            "the host translates these from locales/<tag>.json, so they are written in its source language"
        );

        let message = json!({ "op": "scan" }).to_string();
        plugin.handle(VIEW, ViewEvent::Message(message), &host.invocation()).unwrap();
        let (name, payload) = host.view_messages().pop().unwrap();
        assert_eq!(name, VIEW);
        let payload: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload["op"], "items");
        assert_eq!(payload["count"], 2);

        // The dashboard scan skips the DTO closure, but that must not thin out
        // what the page receives: every route and every field the page renders
        // still has to be there.
        let items = payload["items"].as_array().expect("items");
        assert_eq!(items.len(), 2);
        for item in items {
            assert!(item["method"].is_string(), "method: {item}");
            assert!(item["path"].as_str().is_some_and(|p| p.starts_with('/')), "path: {item}");
            assert!(item["class"].is_string(), "class: {item}");
            assert!(item["handler"].is_string(), "handler: {item}");
            assert!(item["framework"].is_string(), "framework: {item}");
            assert!(item["file"].is_string(), "file: {item}");
            assert!(item["line"].is_number(), "line: {item}");
            assert!(item["params"].is_array(), "params: {item}");
            // `body` is the request body's type name — a string or null. The
            // page never renders DTO fields, which is why the closure is skipped.
            assert!(item["body"].is_string() || item["body"].is_null(), "body: {item}");
        }
    }

    /// Both "no project" messages were `ok_or("…")` — that is `Error::Failed`,
    /// whose `Display` is the text itself. They are localized now, so they must
    /// stay `Failed`: `Error::Invalid` would prefix them with "invalid: ".
    ///
    /// (The other user-facing errors — missing settings, an endpoint that is
    /// gone, nothing selected — were already `Error::Invalid` before this
    /// change; that prefix is pre-existing behaviour and is left alone.)
    #[test]
    fn the_no_project_message_stays_unprefixed() {
        let host = host();
        host.set_locale("zh-CN");
        host.set_project_root(None);
        let mut plugin = ApiMocktle::default();

        // `start_export`, reached by both export commands.
        let err = plugin.run_command("export-project", &host.invocation(), None).unwrap_err();
        assert_eq!(err.to_string(), "先打开一个项目", "no variant prefix leaks in");

        // `export_one`, reached by the dashboard's "sync this one".
        let err = plugin.export_one(&host.invocation(), 0, None).unwrap_err();
        assert_eq!(err.to_string(), "先打开一个项目", "same message from the single export");
    }

    /// The page asks for the language instead of waiting for a push: the host can
    /// render a view before it has told the plugin the current language, so the
    /// value baked in at render time is not trustworthy on its own. That stale
    /// value is why the dashboard used to need a reload to switch.
    #[test]
    fn the_page_can_ask_for_the_current_language() {
        let host = host();
        host.set_locale("zh-CN");
        let mut plugin = ApiMocktle::default();
        plugin.render(VIEW, None, &host.invocation()).unwrap();

        // The IDE switches to English after the page was drawn.
        host.set_locale("en");

        // The page asks; the answer is the *current* language, not the baked one.
        plugin
            .handle(VIEW, ViewEvent::Message(json!({ "op": "locale" }).to_string()), &host.invocation())
            .unwrap();
        let (view, payload) = host.view_messages().pop().expect("an answer");
        assert_eq!(view, VIEW);
        let payload: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload["op"], "locale");
        assert_eq!(payload["locale"], "en");
        assert!(host.refreshed_views().is_empty(), "answering does not rebuild the page");

        // Asking again is always answered, so the page can poll.
        plugin
            .handle(VIEW, ViewEvent::Message(json!({ "op": "locale" }).to_string()), &host.invocation())
            .unwrap();
        let (_, payload) = host.view_messages().pop().unwrap();
        let payload: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload["locale"], "en");
    }

    /// The IDE's own setting decides the language, whatever the frame's system
    /// language is: a user who set the IDE to English must get English even on a
    /// Chinese Windows whose frame reports `zh-CN` (that is what `page.html`
    /// cross-checks, and the plugin itself must not second-guess it).
    #[test]
    fn the_ide_setting_decides_the_language_not_the_system_one() {
        let host = host();
        // The IDE is English.
        host.set_locale("en");
        let mut plugin = ApiMocktle::default();
        plugin.render(VIEW, None, &host.invocation()).unwrap();

        // The page polling for the language gets the IDE's answer, not a guess.
        plugin
            .handle(VIEW, ViewEvent::Message(json!({ "op": "locale" }).to_string()), &host.invocation())
            .unwrap();
        let (_, payload) = host.view_messages().pop().unwrap();
        let payload: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload["locale"], "en", "the IDE setting decides");

        assert!(i18n::lang() == i18n::Lang::En, "the plugin's own text follows it");
        let text = plugin.run_command("export-project", &host.invocation(), None).unwrap().unwrap();
        assert!(text.contains("synced to ApiMocktle"), "{text}");

        // And when the IDE itself is Chinese, that decides too.
        host.set_locale("zh-CN");
        let text = plugin.run_command("export-project", &host.invocation(), None).unwrap().unwrap();
        assert!(text.contains("已同步到 ApiMocktle"), "{text}");
    }

    /// The same export path in English: everything the plugin builds itself
    /// follows `host::locale()`, while the code does not change.
    #[test]
    fn the_same_export_reads_english_on_an_english_ide() {
        let host = host();
        host.set_locale("en");
        let mut plugin = ApiMocktle::default();

        let text = plugin.run_command("export-project", &host.invocation(), None).unwrap().unwrap();
        assert!(text.contains("synced to ApiMocktle"), "{text}");
        assert!(!text.chars().any(is_cjk), "an English IDE gets English text: {text}");
        // The category description the plugin sends is prose too.
        let cat = host
            .requests()
            .into_iter()
            .find(|r| r.url.contains("/api/interface/add_cat"))
            .expect("a category is created");
        let body: Value = serde_json::from_str(&String::from_utf8(cat.body.unwrap()).unwrap()).unwrap();
        assert_eq!(body["desc"], "Maintained by the OpenIDEA ApiMocktle plugin");

        // And the settings error a user can hit before anything else.
        host.set_setting("token", "\"\"");
        let err = plugin.run_command("export-project", &host.invocation(), None).unwrap_err();
        assert!(
            err.to_string().contains("Set the personal token in Settings › ApiMocktle first"),
            "{err}"
        );
    }

    /// The page picks its own text from the `locale` in the start data: the host
    /// leaves an `html` body alone. This is the page half of the guarantee; the
    /// Rust half is `the_same_export_reads_english_on_an_english_ide`.
    #[test]
    fn the_page_carries_both_languages_and_picks_one() {
        let page = include_str!("page.html");
        // Both sides live in the table, so a switch needs no rebuild.
        assert!(page.contains("扫描中…") && page.contains("Scanning…"));
        assert!(page.contains("let ZH = isChinese(hostLocale"), "the page reads the start language");
        // `host::locale()` decides; the frame's system language only fills a gap,
        // so an English IDE stays English on a Chinese Windows.
        assert!(
            page.contains("isChinese(hostLocale || navigatorLanguage)"),
            "the frame language is a fallback, never an override"
        );
        // The page asks for the language instead of trusting the baked value (the
        // host can render before it pushes the language) and re-renders its labels
        // in place when the answer differs, so the selection survives.
        assert!(page.contains(r#"payload.op === "locale""#), "the page handles a locale answer");
        assert!(page.contains(r#"askLocale();"#), "and asks for the language on load");
        assert!(page.contains(r#"const relocalize ="#), "and applies the answer in place");
        // Nothing the page renders is hardcoded Chinese outside that table: every
        // CJK character sits between the table's braces.
        let (before, after) = page.split_once("const STR = {").expect("the table");
        let (_, after) = after.split_once("\n  };").expect("the table ends");
        let outside = format!("{before}{after}");
        assert!(!outside.chars().any(is_cjk), "the page keeps its Chinese in the table only");
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
        // Production sends a whole project in one call (50 s task budget), so the
        // cross-call behaviour only shows up when a batch stops early. A budget
        // spent from the start does that deterministically.
        plugin.set_export_budget_for_tests(Some(Duration::ZERO));
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
        // One batch per call, so what is left of the queue is observable.
        plugin.set_export_budget_for_tests(Some(Duration::ZERO));
        plugin.handle_events(&[Event::Scheduled], &host.invocation());
        let raw = host::storage::get(QUEUE_KEY).unwrap().expect("the first batch leaves the rest");
        let queued: Value = serde_json::from_str(&raw).unwrap();
        // The loop never stops before five interfaces went out, so a spent
        // budget still sends a small batch; the point here is that the rest stays
        // queued with its target intact.
        let left = queued["items"].as_array().map(Vec::len).unwrap_or(0);
        assert!(left > 0 && left < 201, "one batch went out, the rest stayed queued: {left}");
        assert_eq!(queued["base"], "http://localhost:4000", "the write-back keeps its target");
        assert_eq!(queued["auth"], fingerprint("secret-token"));
        assert!(host.status_items()["sync"].text.contains("同步中"), "{:?}", host.status_items());

        // The next call gets the real budget, so it finishes the queue: that is
        // what a project sees now that a batch is no longer capped at 200.
        plugin.set_export_budget_for_tests(None);
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

        let doc = yapi::api_doc(upload, &scanned.dto, 2, i18n::Lang::En);
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
        let light = scan::scan(&project, &route_only_options()).unwrap();
        assert_eq!(light.endpoints.len(), 2);
        assert!(light.dto.fields.is_empty(), "routes do not need the DTO graph");
        let full = scan::scan(&project, &scan::Options { resolve_dto: true, ..route_only_options() }).unwrap();
        assert!(!full.dto.fields.is_empty());
    }

    /// The dashboard shows routes and nothing else, so opening it must not pay
    /// for the DTO closure — but an export started afterwards must still get the
    /// graph. Those two pulls in opposite directions and only the cache layers
    /// keep them apart.
    #[test]
    fn the_dashboard_scan_skips_dtos_and_an_export_still_finds_them() {
        let host = host();
        let mut plugin = ApiMocktle::default();
        let ctx = host.invocation();
        let project = ctx.project().expect("a project");

        // What the dashboard does on open.
        let display = plugin.routes_for_display(&project).expect("a display scan");
        assert_eq!(display.endpoints.len(), 2, "the dashboard still lists every route");
        assert!(display.dto.fields.is_empty(), "the dashboard does not resolve DTOs");

        // What export does next: it must not be handed the route-only result.
        let full = plugin.scan_export(&project, None).expect("a full scan");
        assert!(
            !full.dto.fields.is_empty(),
            "export needs the DTO graph even when the dashboard scanned first"
        );
        assert!(full.dto.fields.contains_key("UserDto") || full.dto.fields.contains_key("CreateUser"));
        assert_eq!(full.endpoints.len(), 2);

        // And the full result now serves the dashboard too, rather than
        // scanning again.
        let again = plugin.routes_for_display(&project).expect("a cached display scan");
        assert!(!again.dto.fields.is_empty(), "the full result is reused");
    }

    /// The dashboard lists every controller, not a capped prefix.
    ///
    /// It used to inherit the endpoints provider's 400-file cap, which silently
    /// listed fewer controllers than the project has. The provider no longer
    /// scans (it reads the published cache), so the display tier is the only
    /// routes-only scan left — and it must keep `FILE_LIMIT`.
    #[test]
    fn a_display_scan_keeps_the_full_file_limit() {
        // `route_only_options` reads the host's settings, so a mock must be
        // installed before it is called.
        let _host = host();
        assert_eq!(route_only_options().file_limit, scan::FILE_LIMIT);
        assert!(!route_only_options().resolve_dto);
    }

    /// "Go to source" reads the same route-only scan the dashboard does, so it
    /// has to keep working off it: the file and the annotation's position are
    /// what it needs, and both are route-level data.
    #[test]
    fn going_to_source_works_from_the_route_only_scan() {
        let host = host();
        let mut plugin = ApiMocktle::default();
        plugin.render(VIEW, None, &host.invocation()).unwrap();

        plugin
            .handle(
                VIEW,
                ViewEvent::Message(json!({ "op": "open", "index": 1 }).to_string()),
                &host.invocation(),
            )
            .expect("the open message must not fail");

        let opened = host.opened_locations();
        assert_eq!(opened.len(), 1, "one jump was requested: {opened:?}");
        let (path, line, col) = &opened[0];
        assert!(path.ends_with("UserController.java"), "the route's own file: {path}");
        // Sorted by path, so index 1 is `/api/users/create`, whose annotation is
        // on line 11 (zero-based). The exact numbers matter less than the fact
        // that a real position came through rather than the 0/0 default.
        assert!(*line > 0, "the annotation's line, not 0: {line}");
        assert!(*col > 0, "the annotation's column, not 0: {col}");
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

    /// A non-2xx reply is not the only place a body can quote the request URL: a
    /// proxy error page or an HTML notice can arrive with `200` and echo the
    /// address it was asked for — token included. Both the dashboard's status line
    /// and a notification show whatever these paths return, so the token must not
    /// survive into either.
    #[test]
    fn an_html_error_page_with_a_200_never_leaks_the_token() {
        let host = host();
        // Exactly the reported shape: status 200, body not JSON, body quotes the
        // request URL, and that URL carries `?token=…`.
        const TOKEN: &str = "abcdef0123456789abcdef0123456789";
        host.set_setting("token", &format!("\"{TOKEN}\""));
        host.on_fetch(move |request| {
            // A real HTML page is raw bytes, not a JSON string: `reply` would
            // serialize it *with* quotes, which parses as valid JSON and takes a
            // different branch entirely.
            let page = format!("<p>Request failed: {}</p>", request.url);
            Ok(HttpResponse { status: 200, headers: Vec::new(), body: page.into_bytes() })
        });

        let mut plugin = ApiMocktle::default();
        // Path 1: the projects list, whose error reaches `projects.error` and the
        // status line.
        plugin.render(VIEW, None, &host.invocation()).unwrap();
        plugin
            .handle(VIEW, ViewEvent::Message(json!({ "op": "projects" }).to_string()), &host.invocation())
            .unwrap();
        let messages = host.view_messages();
        assert!(!messages.is_empty(), "the page is told the list failed");
        for (_, payload) in &messages {
            assert!(!payload.contains(TOKEN), "the dashboard must not show the token: {payload}");
        }

        // Path 2: the export the dashboard triggers (`export-all` → `report`),
        // whose error reaches both a toast and a notification.
        let before = host.notifications().len();
        plugin
            .handle(VIEW, ViewEvent::Message(json!({ "op": "export-all" }).to_string()), &host.invocation())
            .unwrap();
        let fresh: Vec<_> = host.notifications().into_iter().skip(before).collect();
        assert!(!fresh.is_empty(), "the user is told the export failed");
        for n in &fresh {
            assert!(!n.body.contains(TOKEN), "a notification must not show the token: {}", n.body);
        }
        for (_, payload) in host.view_messages() {
            assert!(!payload.contains(TOKEN), "the toast must not show the token: {payload}");
        }

        // The message stays useful: the body is still shown, only the secret is
        // gone. `list_projects` reaches `not_json` directly.
        let shown = yapi::Client { base: "http://localhost:4000".into(), token: TOKEN.into(), project_id: String::new() }
            .list_projects()
            .unwrap_err()
            .to_string();
        assert!(!shown.contains(TOKEN), "{shown}");
        assert!(shown.contains("Request failed"), "the response body is still shown: {shown}");
        assert!(shown.contains("token=…"), "the parameter is still named: {shown}");
    }
}
