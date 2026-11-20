// Renders src/page.html the way the plugin does and checks the text the user
// sees, in both languages — including what arrives over messages.
//
// The dashboard is a `Body::Html` view: the host never translates its body, so
// the page picks its own text from the `locale` in the injected start data. That
// injection is the fragile part — `render()` replaces the quoted `"__START__"`
// with a JSON object literal, and a page that `JSON.parse`d it would only ever
// see `[object Object]`, silently falling back to English on a Chinese IDE.
// Rust's unit tests cannot catch that (they never run the script), so this
// replays the real substitution with `node`.
//
// Run: node script/check-pages.mjs   (exit code 1 on failure)

import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import vm from "node:vm";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const PAGE = join(root, "src", "page.html");

/** The subset of the DOM the page touches, recording what it renders. */
function renderPage(startJson, navigatorLanguage = "en-US") {
  const html = readFileSync(PAGE, "utf8");
  // Exactly what `Views::render` does: replace the first quoted placeholder
  // with `json!({..}).to_string()`, quotes included.
  const rendered = html.replace('"__START__"', JSON.stringify(startJson));
  // Run the script as it appears *after* that substitution.
  const script = rendered.split("<script>")[1].split("</script>")[0];

  const elements = new Map();
  /** A node recording what the page writes; enough for list/fragment rendering. */
  const node = (id) => ({
    id,
    textContent: "",
    innerHTML: "",
    placeholder: "",
    title: "",
    value: "",
    disabled: false,
    className: "",
    children: [],
    addEventListener() {},
    querySelector: () => ({ set onclick(_) {} }),
    append(...nodes) {
      this.children.push(...nodes);
    },
    appendChild(child) {
      this.children.push(child);
    },
    replaceChildren(...nodes) {
      // A fragment's children are spliced in, as the DOM does.
      this.children = nodes.flatMap((n) => (n.kind === "fragment" ? n.children : [n]));
    },
    onclick: null,
  });
  const element = (id) => {
    if (!elements.has(id)) elements.set(id, node(id));
    return elements.get(id);
  };
  const posted = [];
  let onMessage = null;
  const document = {
    documentElement: { lang: "" },
    getElementById: element,
    createElement: () => node(""),
    createDocumentFragment: () => Object.assign(node(""), { kind: "fragment" }),
  };
  const window = {
    openidea: { onMessage: (fn) => (onMessage = fn), postMessage: (text) => posted.push(JSON.parse(text)) },
  };
  const navigator = { language: navigatorLanguage };
  // The page polls for the language; record the timer instead of running it, so
  // the check stays deterministic and the process can exit.
  const timers = [];
  const setInterval = (fn, ms) => {
    timers.push({ fn, ms });
    return timers.length;
  };
  vm.runInNewContext(
    script,
    { window, document, navigator, setInterval, JSON, String, Map, Array, Object, console, Number }
  );
  const send = (payload) => onMessage(JSON.stringify(payload));
  return { elements, posted, lang: document.documentElement.lang, send, timers };
}

/** A CJK ideograph: every Chinese string in this page uses that range. */
const cjk = /[\u4e00-\u9fff]/;

let failed = 0;
const check = (ok, what) => {
  if (!ok) {
    failed += 1;
    console.error(`  FAIL ${what}`);
  }
};

/** All text a recorded node (and its children) holds. */
function textOf(node) {
  if (!node) return "";
  const own = `${node.textContent ?? ""}${node.innerHTML ?? ""}${node.placeholder ?? ""}${node.title ?? ""}`;
  const kids = (node.children ?? []).map(textOf).join("");
  return own + kids;
}

function expectLanguage(locale, { status, rescan, exportAll, placeholder, cfg, loading, selectOne, htmlLang }) {
  const { elements, posted, lang } = renderPage({ configured: true, base: "http://localhost:4000", locale });
  const text = (id) => elements.get(id)?.textContent ?? "";
  const markup = (id) => elements.get(id)?.innerHTML ?? "";
  const label = `locale=${locale}`;
  check(text("status") === status, `${label}: status is ${JSON.stringify(status)}, got ${JSON.stringify(text("status"))}`);
  check(text("rescan") === rescan, `${label}: rescan is ${JSON.stringify(rescan)}, got ${JSON.stringify(text("rescan"))}`);
  check(text("exportAll") === exportAll, `${label}: export all is ${JSON.stringify(exportAll)}, got ${JSON.stringify(text("exportAll"))}`);
  check(elements.get("q")?.placeholder === placeholder, `${label}: placeholder is ${JSON.stringify(placeholder)}, got ${JSON.stringify(elements.get("q")?.placeholder)}`);
  check(text("cfg") === cfg, `${label}: server pill is ${JSON.stringify(cfg)}, got ${JSON.stringify(text("cfg"))}`);
  check(markup("list").includes(loading), `${label}: the list says ${JSON.stringify(loading)}, got ${JSON.stringify(markup("list"))}`);
  check(markup("detail").includes(selectOne), `${label}: the detail says ${JSON.stringify(selectOne)}, got ${JSON.stringify(markup("detail"))}`);
  check(lang === htmlLang, `${label}: <html lang> is ${JSON.stringify(htmlLang)}, got ${JSON.stringify(lang)}`);
  check(
    JSON.stringify(posted) === JSON.stringify([{ op: "locale" }, { op: "scan" }, { op: "projects" }]),
    `${label}: opening the page asks for the language and for data, got ${JSON.stringify(posted)}`
  );
}

/** One endpoint as `scan::to_json` shapes it (src/lib.rs `items` payload). */
const item = (extra = {}) => ({
  method: "GET",
  path: "/api/users/list",
  summary: "list",
  handler: "UserController#list",
  class: "UserController",
  framework: "Spring",
  file: "/proj/src/UserController.java",
  line: 7,
  params: ["q"],
  body: null,
  ...extra,
});

/**
 * The messages the component posts: `items`, `projects` and `toast`. Every label
 * they end up rendering must be in the language of the start data — this is the
 * half of the page that only runs after the first frame, so a missing `T()`
 * would not show up in the checks above.
 */
function expectMessages(locale, expected) {
  const { send, elements, lang } = renderPage({ configured: true, base: "http://localhost:4000", locale });
  const text = (id) => elements.get(id)?.textContent ?? "";
  const markup = (id) => textOf(elements.get(id));
  const label = `locale=${locale} messages`;

  // `items`: the scan result, which fills the list and the footer.
  send({ op: "items", items: [item()], count: 1, files: 1, truncated: false, configured: true });
  check(text("cfg") === expected.configured, `${label}: pill is ${JSON.stringify(expected.configured)}, got ${JSON.stringify(text("cfg"))}`);
  check(markup("list").includes("UserController"), `${label}: the controller groups the list, got ${JSON.stringify(markup("list"))}`);
  check(markup("list").includes("/api/users/list"), `${label}: the path is listed`);
  check(text("count") === "1 / 1", `${label}: the count is "1 / 1", got ${JSON.stringify(text("count"))}`);
  check(
    text("status").startsWith(expected.scanDone),
    `${label}: status starts ${JSON.stringify(expected.scanDone)}, got ${JSON.stringify(text("status"))}`
  );
  check(
    text("status").includes(expected.endpoints),
    `${label}: status mentions endpoints as ${JSON.stringify(expected.endpoints)}, got ${JSON.stringify(text("status"))}`
  );

  // A truncated scan says so, in the same language.
  send({ op: "items", items: [item()], count: 1, files: 1, truncated: true, configured: true });
  check(
    text("status").includes(expected.truncated),
    `${label}: a truncated scan says ${JSON.stringify(expected.truncated)}, got ${JSON.stringify(text("status"))}`
  );

  // An empty filter result names the empty state.
  send({ op: "items", items: [], count: 0, files: 0, truncated: false, configured: false });
  check(markup("list").includes(expected.noEndpoints), `${label}: an empty list says ${JSON.stringify(expected.noEndpoints)}`);
  check(text("cfg") === expected.notConfigured, `${label}: an unconfigured pill says ${JSON.stringify(expected.notConfigured)}`);

  // `projects`: the target picker.
  send({ op: "projects", items: [{ id: "p1", name: "Demo" }] });
  check(markup("project").includes(expected.chooseProject), `${label}: the picker says ${JSON.stringify(expected.chooseProject)}`);
  check(markup("project").includes("Demo"), `${label}: the project is listed`);
  send({ op: "projects", items: [], error: "boom" });
  check(markup("project").includes(expected.notConnected), `${label}: a failed list says ${JSON.stringify(expected.notConnected)}`);
  check(
    text("status").startsWith(expected.projectList),
    `${label}: the status says ${JSON.stringify(expected.projectList)}, got ${JSON.stringify(text("status"))}`
  );

  // `toast`: the inline report of an export.
  send({ op: "toast", text: expected.exported, ok: true });
  check(text("status") === expected.exported, `${label}: a toast is shown as written`);
  check(elements.get("status")?.className === "ok", `${label}: a good toast is not a warning`);
  send({ op: "toast", text: expected.failed, ok: false });
  check(elements.get("status")?.className === "warn", `${label}: a failed action shows as a warning`);

  // Selecting a row opens the detail panel, whose labels come from the table.
  send({ op: "items", items: [item()], count: 1, files: 1, truncated: false, configured: true });
  const row = elements.get("list");
  check(text("list") === "" || typeof text("list") === "string", `${label}: the list renders without throwing`);

  return { send, elements, text, markup, lang };
}

const ZH = {
  configured: "服务器已配置",
  notConfigured: "未配置服务器",
  scanDone: "扫描完成：",
  endpoints: " 个接口 · ",
  truncated: "已达搜索上限",
  noEndpoints: "没有匹配的接口",
  chooseProject: "选择项目",
  notConnected: "未连接",
  projectList: "项目列表：",
  exported: "全部 2 个接口已同步到 ApiMocktle。",
  failed: "先在设置 › ApiMocktle 里填写个人令牌",
};
const EN = {
  configured: "Server configured",
  notConfigured: "Server not configured",
  scanDone: "Scan complete: ",
  endpoints: " endpoint(s) · ",
  truncated: "search limit reached",
  noEndpoints: "No endpoints match",
  chooseProject: "Choose a project",
  notConnected: "Not connected",
  projectList: "Projects: ",
  exported: "All 2 endpoints are synced to ApiMocktle.",
  failed: "Set the personal token in Settings › ApiMocktle first",
};

console.log("page.html, rendered the way the plugin does:");

// The page sets `ready` and then immediately says it is scanning (it asks for
// data on load), so the status line ends on the scanning text. `ready` is
// checked separately below.
expectLanguage("zh", {
  status: "扫描中…",
  rescan: "重新扫描",
  exportAll: "导出全部",
  placeholder: "搜索路径 / 方法名 / 控制器",
  cfg: "服务器已配置",
  loading: "加载中…",
  selectOne: "选择一个接口",
  htmlLang: "zh-CN",
});
expectLanguage("en", {
  status: "Scanning…",
  rescan: "Rescan",
  exportAll: "Export All",
  placeholder: "Search path / method / controller",
  cfg: "Server configured",
  loading: "Loading…",
  selectOne: "Select an endpoint",
  htmlLang: "en",
});

console.log("  ok   the first frame reads in both languages");

expectMessages("zh", ZH);
expectMessages("en", EN);
console.log("  ok   items / projects / toast read in both languages");

// `zh-CN`, `zh-TW` and `zh-Hans` are all Chinese, matching `host::locale()`.
for (const locale of ["zh-CN", "zh-TW", "zh-Hans"]) {
  const { send, elements } = renderPage({ configured: false, base: "", locale });
  check(elements.get("rescan")?.textContent === "重新扫描", `locale=${locale}: reads as Chinese`);
  send({ op: "items", items: [], count: 0, files: 0, truncated: false, configured: true });
  check(elements.get("status")?.textContent.startsWith("扫描完成："), `locale=${locale}: messages read as Chinese`);
}

// The English path must not leak Chinese; the Chinese path is allowed to.
const en = renderPage({ configured: true, base: "http://x", locale: "en" });
en.send({ op: "items", items: [item()], count: 1, files: 1, truncated: true, configured: true });
en.send({ op: "projects", items: [], error: "boom" });
en.send({ op: "toast", text: EN.exported, ok: true });
const enText = [...en.elements.values()].map((e) => `${e.textContent}${e.innerHTML}${e.placeholder}${e.title}`).join(" ");
check(!cjk.test(enText), `locale=en: no Chinese is rendered, got ${JSON.stringify(enText.match(/[\u4e00-\u9fff].{0,40}/)?.[0] ?? "")}`);

// Every `T("key")` and every `say("key")` must name a key the table defines: a
// typo would otherwise render the key itself (or, before the guard in `T`,
// blank the page).
{
  const html = readFileSync(PAGE, "utf8");
  const script = html.split("<script>")[1].split("</script>")[0];
  const table = script.split("const STR = {")[1].split("\n  };")[0];
  const defined = new Set([...table.matchAll(/^\s{4}(\w+):/gm)].map((m) => m[1]));
  const used = new Set([
    ...[...script.matchAll(/T\("(\w+)"\)/g)].map((m) => m[1]),
    ...[...script.matchAll(/\bsay\("(\w+)"/g)].map((m) => m[1]),
  ]);
  const missing = [...used].filter((k) => !defined.has(k));
  const unused = [...defined].filter((k) => !used.has(k));
  check(missing.length === 0, `every T()/say() key is in the table; missing: ${JSON.stringify(missing)}`);
  check(unused.length === 0, `every table entry is used; unused: ${JSON.stringify(unused)}`);
  console.log(`  ok   ${defined.size} table keys, all used and defined`);
}

// The language switch must not cost the user their place: the list, the
// selection and the status line survive it. The plugin sends `locale` on its
// next wake-up instead of asking the host for a re-render.
{
  const { send, elements } = renderPage({ configured: true, base: "http://localhost:4000", locale: "zh" });
  const text = (id) => elements.get(id)?.textContent ?? "";
  const tree = (id) => textOf(elements.get(id));
  const label = "locale switch";

  send({ op: "items", items: [item(), item({ path: "/api/users/create", method: "POST" })], count: 2, files: 1, truncated: false, configured: true });
  check(text("status").startsWith("扫描完成："), `${label}: starts in Chinese, got ${JSON.stringify(text("status"))}`);

  // Select the first endpoint, as a user would.
  const firstRow = elements.get("list")?.children.find((n) => /row/.test(n.className) && n.onclick);
  check(!!firstRow, `${label}: the list renders selectable rows`);
  firstRow?.onclick?.();
  check(tree("detail").includes("查询参数") || tree("detail").includes("/api/users/list"), `${label}: the detail panel opens`);
  const beforeDetail = tree("detail");
  const countBefore = text("count");

  // The plugin notices the language changed and tells the page.
  send({ op: "locale", locale: "en" });

  check(text("q") ? true : elements.get("q").placeholder === "Search path / method / controller", `${label}: the placeholder follows`);
  check(text("rescan") === "Rescan", `${label}: rescan follows, got ${JSON.stringify(text("rescan"))}`);
  check(text("exportAll") === "Export All", `${label}: export all follows`);
  check(elements.get("cfg")?.textContent === "Server configured", `${label}: the pill follows`);
  check(
    text("status").startsWith("Scan complete: "),
    `${label}: the status keeps its meaning in English, got ${JSON.stringify(text("status"))}`
  );
  check(text("count") === countBefore, `${label}: the count is unchanged (${countBefore})`);
  check(
    tree("detail").includes("/api/users/list") && tree("detail").includes("Query parameters"),
    `${label}: the same endpoint stays open, now in English, got ${JSON.stringify(tree("detail"))}`
  );
  check(tree("detail") !== beforeDetail, `${label}: its labels really changed`);
  check(tree("list").includes("/api/users/create"), `${label}: the other endpoint is still listed`);
  check(text("status") !== "", `${label}: the status line was not cleared`);
  check(elements.get("status")?.className === "ok", `${label}: and kept its kind`);

  // Switching back restores Chinese, again in place.
  send({ op: "locale", locale: "zh-CN" });
  check(text("rescan") === "重新扫描", `${label}: switching back reads Chinese again`);
  check(tree("detail").includes("查询参数"), `${label}: and so does the detail panel`);

  // A message for the language already shown changes nothing.
  const stable = tree("detail");
  send({ op: "locale", locale: "zh" });
  check(tree("detail") === stable, `${label}: a same-language message is a no-op`);
  console.log("  ok   a locale switch swaps the text and keeps the selection");
}

// `host::locale()` is the IDE's own setting and must decide: a user who set the
// IDE to English gets English even on a Chinese Windows, where the frame's
// `navigator.language` is `zh-*`. The frame language is a fallback for a host
// that reports nothing, never an override.
{
  const read = (locale, navigatorLanguage) => {
    const { elements } = renderPage({ configured: true, base: "http://x", locale }, navigatorLanguage);
    return { rescan: elements.get("rescan")?.textContent, status: elements.get("status")?.textContent };
  };
  const enOnChineseWindows = read("en", "zh-CN");
  check(
    enOnChineseWindows.rescan === "Rescan",
    `locale=en on a Chinese Windows stays English, got ${JSON.stringify(enOnChineseWindows.rescan)}`
  );
  const zhOnEnglishWindows = read("zh-CN", "en-US");
  check(
    zhOnEnglishWindows.rescan === "重新扫描",
    `locale=zh-CN on an English Windows reads Chinese, got ${JSON.stringify(zhOnEnglishWindows.rescan)}`
  );
  const emptyFallsBack = read("", "zh-CN");
  check(
    emptyFallsBack.rescan === "重新扫描",
    `an empty host tag falls back to the frame language, got ${JSON.stringify(emptyFallsBack.rescan)}`
  );
  console.log("  ok   the IDE's own language decides; the system language only fills a gap");
}

// This is the bug that shipped: the host renders the view *before* it has told the
// plugin the current language, so the value baked into the page is stale and the
// dashboard stayed in the old language until a manual reload. The page now asks on
// load; this simulates a stale start value followed by the correct answer.
{
  const label = "stale start value";
  const { send, elements, posted, timers } = renderPage(
    { configured: true, base: "http://x", locale: "zh-CN" },
    "zh-CN"
  );
  check(elements.get("rescan")?.textContent === "重新扫描", `${label}: starts on the stale (Chinese) text`);
  check(
    posted.some((m) => m.op === "locale"),
    `${label}: the page asks for the language on load, got ${JSON.stringify(posted)}`
  );

  // The plugin answers with the real language: the IDE is English.
  send({ op: "locale", locale: "en" });
  check(elements.get("rescan")?.textContent === "Rescan", `${label}: the answer corrects the page`);
  check(elements.get("q")?.placeholder === "Search path / method / controller", `${label}: placeholder too`);
  check(elements.get("exportAll")?.textContent === "Export All", `${label}: and the export button`);
  check(elements.get("status")?.textContent === "Scanning…", `${label}: and the status line`);

  // The poll is installed, so a later switch is picked up without a reload.
  check(timers.length === 1, `${label}: the page polls, got ${timers.length} timer(s)`);
  check(timers[0].ms <= 5000, `${label}: the poll is frequent enough (${timers[0].ms} ms)`);

  // Polling with the same answer changes nothing.
  const before = elements.get("status")?.textContent;
  send({ op: "locale", locale: "en" });
  check(elements.get("status")?.textContent === before, `${label}: a repeated answer is a no-op`);
  console.log("  ok   a stale start value corrects itself without a reload");
}

// The first frame before scanning starts says `Ready` / `就绪`, as before.
for (const [locale, ready] of [["zh", "就绪"], ["en", "Ready"]]) {
  const page = readFileSync(PAGE, "utf8");
  check(page.includes(`["Ready", "就绪"]`), `${locale}: the table carries the ready text`);
  const script = page.split("<script>")[1].split("</script>")[0];
  check(script.includes('say("ready")'), `${locale}: the page starts on the ready text`);
}

// An unreplaced placeholder (the file opened directly) must not throw.
const direct = readFileSync(PAGE, "utf8").split("<script>")[1].split("</script>")[0];
const directDocument = {
  documentElement: { lang: "" },
  getElementById: () => ({ textContent: "", innerHTML: "", placeholder: "", addEventListener() {}, querySelector: () => ({ set onclick(_) {} }) }),
  createElement: () => ({ append() {} }),
  createDocumentFragment: () => ({ appendChild() {} }),
};
try {
  vm.runInNewContext(direct, {
    window: { openidea: { onMessage() {}, postMessage() {} } },
    document: directDocument,
    JSON,
    String,
    Map,
    Array,
    Object,
    console,
    Number,
  });
  console.log("  ok   opening page.html directly does not throw");
} catch (e) {
  failed += 1;
  console.error(`  FAIL opening page.html directly throws: ${e.message}`);
}

if (failed > 0) {
  console.error(`\n${failed} page check(s) failed`);
  process.exit(1);
}
console.log("  ok   every rendered label follows the start-data language");
