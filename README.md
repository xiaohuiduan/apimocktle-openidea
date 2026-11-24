# ApiMocktle for OpenIDEA

**English** | [简体中文](README.zh-CN.md)

The core of the IntelliJ [ApiMocktle-java-plugin](https://github.com/xiaohuiduan/ApiMocktle-java-plugin), brought to
[OpenIDEA](https://openidea.sh/plugins)'s plugin model: extract HTTP endpoints from Java / Kotlin sources, browse and
navigate them, and sync them to ApiMocktle. This is the English page; the [Chinese one is here](README.zh-CN.md).

The plugin is a WebAssembly component. `plugin.toml` declares its contribution points, and its code reaches the
project and the network only through the host interfaces exposed by `wit/`.

## What ApiMocktle is

[ApiMocktle](https://github.com/xiaohuiduan/ApiMocktle) is a local API-management desktop app
(Tauri + React + Rust + SQLite): API documents, request debugging, automated test runs, OpenAPI / Swagger import
and export, and LAN sharing all happen on your machine. This plugin covers the "generate API docs from code" part:
it scans a project's sources and writes the endpoints into an ApiMocktle project with a personal token.

## Relation to the IntelliJ plugin

OpenIDEA plugins run in a WASM sandbox — no Java PSI, no Swing, no JVM agent. So this is not a port but a
reimplementation on OpenIDEA's own capabilities: the semantics are kept, the implementation path differs.

| IntelliJ plugin | This plugin |
| - | - |
| Java PSI + type resolution | Hand-written lexer + structural parser (`src/java.rs`, `src/spring.rs`) |
| ToolWindow (Swing / JBList) | `[[contributes.views]]` + sandboxed HTML view (`postMessage`) |
| AnAction / context menu | `[[contributes.commands]]` + `[[contributes.menus]]` |
| PersistentStateComponent | `host::storage` + `[[contributes.settings]]` |
| Apache HttpClient | `host::fetch` |
| GotoDeclaration | `host::open_location` |
| Goto Symbol global search | `[[contributes.endpoints]]`: endpoints land in **Search Everywhere** and participate in `.http` URL completion |

The original plugin's `mock-agent` (JVM runtime interception) has no counterpart in the OpenIDEA plugin model and
is not part of this plugin.

## Features

Two things only: **browsing endpoints** and **exporting to ApiMocktle**.

- **Scanning**: Spring MVC (`@RestController`, `@RequestMapping`, `@GetMapping`, …), JAX-RS (`@Path` + `@GET`),
  Feign (`@FeignClient`); Java and Kotlin (including `data class` and `fun ... : T` return types).
- **Route constants**: paths written as `static final` constants — `@PostMapping(RouteConstant.ITEM_QUERY)` — are
  resolved across files. A constant is identified by the fully qualified name of the class that declares it:
  `com.a.Routes.PATH` and `com.b.Routes.PATH` are different, even when the classes share a simple name or two
  controllers import a same-named `Routes`. Without a qualifier the current class is tried first, then
  `import static`, and a bare name is only used when a single declaration exists project-wide; an ambiguous name
  falls back to `/` instead of being guessed. Class-level `@RequestMapping` prefixes are supported; an unresolved
  path falls back to `/` without affecting the class's other endpoints.
- **Endpoint titles**: `@Operation(summary)` (Swagger) first, then the method's Javadoc, then the method name. A
  method without its own Javadoc does **not** inherit the class comment.
- **Parameter kinds**: `@PathVariable` / `@RequestParam` / `@RequestHeader` / `@RequestBody` / `@CookieValue` /
  `@RequestPart`, in both the Java and the Kotlin forms. Form parameters (`@RequestPart` / `@FormParam` /
  `@ModelAttribute`) go to `req_body_form` with `req_body_type` set to `form`; cookie parameters are recognized but
  not exported (the upstream IntelliJ plugin does not export them either).
- **Dashboard**: a bottom tool window grouped by controller, with search, path and parameter details, and
  go-to-source.
- **Export**: a personal token plus a target-project picker; categories are created automatically and existing
  endpoints are matched and updated by `method + path`. Request and response bodies are written as JSON Schema:
  wrapper types declared in the project (`Result<T>`, `ApiResponse<T>`, …) keep their own fields and have the type
  argument substituted into `data` and friends (`Result<List<UserDto>>` exports as `{code, msg, data: [...]}`),
  while framework wrappers with no definition (`ResponseEntity<T>`, `Optional<T>`) are still unwrapped.
  `Map<K, V>` produces one example key under `properties` (both ApiMocktle and YApi render only `properties`;
  `additionalProperties` shows as `{}`). `@JsonProperty` renames, `@JsonIgnore` exclusions, record components and
  Kotlin constructor properties are all honoured.
- **Types from dependency jars**: DTOs and wrappers that are not in the sources (`ApiEnvelope<T>` living in a
  shared jar) are resolved to jars in the local Maven repository and their `.class` files read for fields, generic
  parameters and superclass fields (`class ApiEnvelope<T> extends BaseReply` exports `BaseReply`'s
  `code`/`message` plus its own `errorNo`/`errorInfo`/`result: T`). Coordinates come from the dependency graph the
  IDE already resolved (`build.read` grant): multi-module layouts, BOMs and version conflicts are its work, so the
  plugin neither walks the POMs itself nor guesses a version. Without that grant it falls back to reading the POMs
  (transitive included) — usually the same answer, just slower. Jars are matched by import package against Maven
  group / artifact ordering, so usually only one jar is read; the first scan of a project remembers "class → jar"
  and later scans hit it directly. Reading the repository still needs the `fs.read` grant; with neither, the plugin
  exports only the type argument.
- **Search Everywhere**: scanned endpoints appear in the Endpoints group, and the declaration also offers
  **Open in HTTP Client**.

Not included: sending requests, copying as cURL, converting fields to JSON, connection tests — all removed on
request.

## Installing

### From a built package

`dist/plugin.wasm` ships with the repository. `pwsh script/package.ps1` rebuilds the component and packs
`dist/apimocktle-openidea-<version>.zip` (containing `plugin.toml`, `settings.schema.json`, `locales/`,
`icons/`, `README.md` and `dist/plugin.wasm`). Install it with **Settings › Plugins › ⚙ › Install Plugin from
Package File…**. After upgrading, use **Reload** on the plugin page — a full IDE restart is not needed.

### Linking during development

```sh
rustup target add wasm32-wasip2
cargo build --release --target wasm32-wasip2
pwsh script/package.ps1      # also refreshes dist/plugin.wasm

# openidea-plugin (validation + linking) is optional:
cargo install --locked --git https://github.com/l0ng-ai/openidea ide-host --tag v0.1.50 --bin openidea-plugin
openidea-plugin check
openidea-plugin link
```

`ide-host` depends on wasmtime 49 and needs rustc ≥ 1.96 to build; on an older toolchain `check`/`link` cannot run,
so use the packed zip and **Install Plugin from Package File** instead. After rebuilding, use **Reload** on the
plugin page.

When the machine has no MSVC linker (the `link.exe` on `PATH` is coreutils' `link`), run the same steps with the GNU
toolchain:

```sh
rustup toolchain install 1.96.0-x86_64-pc-windows-gnu --profile minimal
rustup target add wasm32-wasip2 --toolchain 1.96.0-x86_64-pc-windows-gnu
$env:CARGO_TARGET_DIR = "$env:TEMP\apimocktle-target"   # GNU ld cannot open non-ASCII output paths
cargo +1.96.0-x86_64-pc-windows-gnu test
pwsh script/package.ps1 -Toolchain 1.96.0-x86_64-pc-windows-gnu   # reuses CARGO_TARGET_DIR
```

The component must match the host: `wasm-tools component wit dist/plugin.wasm` should list `wasi:*` and
`openidea:plugin/*@1.14.0`, exporting the `commands`, `lifecycle`, `event-handler`, `endpoints-provider` and
`view-provider` interfaces.

### Local install notes (Windows, OpenIDEA 0.1.45)

`%APPDATA%\ai.l0ng.openidea\plugins\apimocktle.openidea\<version>\` is the unpacked plugin directory, and
`grants.json` records the grants for `apimocktle.openidea` (`project.read`, `storage`, `net:localhost`,
`net:127.0.0.1`). Grants can be revoked one by one on the plugin page; the ledger before a change is backed up as
`grants.json.bak`.

> A hand-written `grants.json` must be **UTF-8 without a BOM**. With a BOM the host fails to deserialize it, the
> ledger is silently reset to empty, and the plugin sits at "needs permission" without logging an error — this was
> the first thing that went wrong here.

## Verified on real projects

Run against several mid-sized multi-module Spring projects (hundreds of Java sources, twenty-odd files with mapping
annotations); those projects' names stay out of the repository:

- Dozens of endpoints (Spring and Feign mixed) with correct multi-module path prefixes, parameter kinds and
  `Result<List<T>>` return types.
- `@FeignClient(value = "service-name")` is no longer treated as a path prefix — that bug was found on a real
  project and fixed.
- An XXL-Job handler carrying `@RestController` + `@GetMapping` is correctly recognized as an endpoint (the project
  author deliberately exposed that trigger endpoint).
- Projects with paths written as `static final` constants, and with wrapper classes living in dependency jars, also
  resolve correctly (see the last two bullets under _Features_).

To rerun the same scan, or to measure each endpoint's schema size at different depths:

```sh
cargo run --example scan_dir -- C:\path\to\project
cargo run --example schema_size -- C:\path\to\project
```

`schema_size` is where the "default depth 3" conclusion comes from: it computes total and largest-per-endpoint byte
sizes at depths 1–4, to judge whether one more level is worth it. Both examples read sources only; the project does
not have to compile.

### Large projects and what a scan costs

A scan's cost is not parsing — it is the **number of host round trips**. Resolving one DTO type used to take two
project-wide file searches (`.java` then `.kt`), so 400 types meant 800 calls and the 5-second budget was gone. A
single **type index** is now built per scan and looked up by file name; a miss falls back to one declaration search
whose result is remembered.

The dashboard and "go to source" render routes only — never DTO fields — so they use a **routes-only tier** that
skips the DTO closure.

Three tools reproduce and measure this (all under `examples/`, none of it in the wasm artifact):

```sh
cargo run --release --example gen_demo   -- --preset big        # generate a Spring project of a chosen size
cargo run --release --example scan_bench -- "%TEMP%\apimocktle-demo\big"
cargo run --release --example component_api -- dist/plugin.wasm # the component's plugin API version
```

`big` is 6 modules / 300 controllers / 1214 endpoints / 900 DTOs. Measured on `MockHost`, pricing a round trip at
3 ms:

| tier | round trips | estimated |
| - | - | - |
| export (DTO closure) | 716 | 2.1 s |
| dashboard (routes only) | 316 | 0.9 s |

The same project cost 1526 before the fix. **Caveat:** `MockHost` is in-memory — `read_file` and `find_files` are
hash lookups — so the milliseconds are not real; only the round-trip counts transfer.

> When validating against real projects, record conclusions only — no project, package or endpoint names in code or
> docs. `tests/` and `fixtures/` use `com.acme` / `com.example` example naming throughout.

## Configuration

**Settings › ApiMocktle**:

| Setting | Meaning |
| - | - |
| Server address | ApiMocktle server address, default `http://localhost:4000` |
| Personal token | Generated in ApiMocktle; stored in this machine's plugin settings |
| Also scan Kotlin sources | On by default |
| Request/response body depth | DTO recursion depth, 1–4, default 3 |
| Resolve types from dependency jars | On by default; reads jars from the local Maven repository to fill in DTOs and wrappers missing from the sources. Needs the `fs.read` grant |
| Local Maven repository | Empty means `~/.m2/repository` (leave it empty when your repository is there). Fill in an absolute path after moving it |

The repository path is **typed by hand** — there is no directory picker: the settings page is a separate window
that cannot open a system dialog, and the plugin has no way to request file permissions at runtime.

### UI language

The plugin's text follows the IDE's UI language (`appearance.locale` in the settings): a Chinese IDE shows Chinese,
an English IDE shows English. Two mechanisms share the job:

- **Manifest, settings and view toolbar buttons** are translated by the host from the package's
  `locales/<tag>.json`. Those places are written in the catalog's source language (English) in `plugin.toml` and
  `settings.schema.json`, with Chinese in `locales/zh-CN.json` (keys are the English source text). A language
  without catalog entries shows English.
- **The dashboard page, notifications and error messages** are built by the component itself, which reads
  `host::locale()` when the text is made (`src/i18n.rs`); the page's text table lives in `src/page.html`.

A view is rendered once, the host has no language-change event, and it does not re-render an HTML body by itself.
So after loading, the page **asks the component for the current language** (and again every 5 seconds), and swaps
its text **in place** — the list, the selected endpoint and the status line survive, no reopening needed. A switch
is visible within seconds. To apply it immediately, close and reopen the dashboard, or run the command
*ApiMocktle: Rescan Endpoints* (it calls `host::refresh_view`, which rebuilds the page). The "Rescan" buttons inside
the view and on its toolbar only clear the cache and post a message; they do not re-render the page.
The marketplace overview (`plugin.toml`'s `description`) and `change-notes` are not translated and always show
Chinese: the host renders those two verbatim and has no translation entry for them.

### Permissions when the repository moves

`fs.read` is granted once at install time from `plugin.toml` and **cannot change at runtime**; matching is a literal
prefix comparison, so granting `~/.m2/repository` does not cover `D:\maven\repo`. Do both of these:

1. Put the new path in the settings page (`Plugins › ApiMocktle › Local Maven repository`);
2. Change `optional` in **`plugin.toml`** to the new directory:

   ```toml
   [capabilities]
   required = ["project.read", "storage", "net:localhost", "net:127.0.0.1"]
   optional = ["fs.read:~/company-repo/m2"]    # drop the default line if unused, keep both if both apply
   ```

   Then repackage, reinstall and tick the new grant under **Settings › Plugins › ApiMocktle › Permissions**.

Skipping step 2 does not fail silently: the plugin posts a notification and logs the exact `fs.read:` path to use,
the dependency types degrade to `object`, and everything else keeps working.

Categories are always generated from the controller class name (`TicketOrderController`, `PayController`, …); there
is no switch for a single shared category.

## Usage

1. Open a Spring / Kotlin project; the **ApiMocktle** tool window at the bottom scans and lists endpoints grouped by
   controller (or run *ApiMocktle: Open API Dashboard* from the command palette).
2. Select an endpoint to see its path, parameters and origin; click **Go to source** to jump to the annotation.
3. Pick the target project in the toolbar and click **Export All** (*ApiMocktle: Export Project Endpoints* in the
   command palette); or right-click a file/folder and choose
   **ApiMocktle: Export Endpoints in Current File** (`Alt+Shift+E`).
4. `Ctrl/Cmd+Shift+A` → search endpoints, or complete URLs directly in a `.http` file.

One click is enough to export. The host gives a plugin call a 5-second wall-clock budget and `host::fetch` is
synchronous, so a batch has a ceiling; on a local server a round trip is a few milliseconds, and 56 endpoints finish
in under a second. Batching actually triggers on a slow server: the loop stops at 4.2 seconds, the rest is queued,
and the plugin advances one batch per minute through `on-schedule:60` while the status bar shows
"Syncing · N left"; a notification appears when everything is done.

To push the next batch immediately, click **Continue now** in the notification or run
*ApiMocktle: Continue Syncing Remaining Endpoints*. The queue lives in plugin storage, survives closing the window,
and resumes after an IDE restart.

The queue remembers which server and which token it was built for. Changing the server address or token mid-export
**pauses** the remaining batches instead of sending them elsewhere: the status bar shows "Sync paused · the queue
belongs to another server", and either clicking the notification or restoring the original settings continues; to
send to the new server, click **Export All** again.

## Permissions and network

Capabilities declared in the manifest:

- `project.read`: read project files, text search, file enumeration.
- `storage`: the export queue, category-id cache, jar lookup results and scan cache.
- `net:localhost`, `net:127.0.0.1`: reach the local mock server (loopback allows plain HTTP).
- `fs.read:~/.m2/repository` (**optional**): read dependency jars outside the project. Tick it in the permission
  list when installing or upgrading, or grant it under **Settings › Plugins › ApiMocktle › Permissions**. Without
  it the plugin works as before, but types from jars degrade to `object`. When the repository moves (say a company
  `settings.xml`'s `localRepository`), change this `fs.read:` entry to the new directory and reinstall to grant it —
  grant paths are declared statically and cannot be appended at runtime (see "Permissions when the repository
  moves" above).
- `build.read` (**optional**): read the Maven / Gradle dependency graph the IDE already resolved, instead of walking
  the POMs yourself. Without it the plugin reads the POMs, and nothing else changes. The project must be trusted or
  the host answers `denied`, which falls back the same way.

The host only allows hosts declared in the manifest. For a server on the LAN, or over HTTPS, edit `plugin.toml`:

```toml
[capabilities]
required = ["project.read", "storage", "net:api.example.com"]   # HTTPS
# or plain HTTP: add "net.http:192.168.1.20"
```

Rebuild and re-grant afterwards. This is the host's capability model, not a choice of this plugin.

## Known limitations

- **No type system**: DTO fields come from text-level parsing, so cross-file inheritance, complex generic inference
  and interface polymorphism are incomplete; common shapes like `List<User>` are reliable, while
  `Result<? extends Base<T>>` degrades to `object`.
- **Route constants accept literals only**: concatenation such as `Constants.PREFIX + "/x"`, `String.format(...)`
  and enum constants are not resolved, and those endpoints fall back to `/`. A constant's declaring class is
  identified by fully qualified name, but when same-named constants live in several classes and there is neither a
  qualifier nor an import, the name cannot be decided and the path falls back to `/`. Constants nested inside other
  classes are not supported (source parsing reads top-level classes only).
- **Dependency-jar boundaries**: Maven only (reading the project's `pom.xml` dependency and transitive POMs, at most
  200 POMs); Gradle projects cannot see repository dependencies yet. A jar must be findable in the local repository
  (`~/.m2/repository` or the path in the settings); a jar larger than 12 MiB is skipped, at most 120 jars are read
  per scan and at most 32 classes per jar (beyond that the type counts as unresolved). Jackson annotations such as
  `@JsonProperty` inside `.class` files are not read (source DTOs are), and Kotlin `@Metadata` is not parsed (the
  fields themselves are readable). Interfaces and abstract classes without sources only merge superclass fields.
  Without the `fs.read` grant the plugin falls back to the old behaviour. Classes read during a scan are remembered
  per project in plugin storage, so the next scan hits the same jar directly.
- **Map renders with an example key**: `Map<K, V>` exports as
  `{ "type": "object", "properties": { "<example name for K>": V } }` rather than `additionalProperties` — the
  latter shows as an empty object in ApiMocktle / YApi field tables.
- **5-second command budget**: scanning and exporting are both designed around it. Exporting sends in batches;
  scanning stops itself at 4.2 s (1.5 s for the endpoints provider) and returns what it found with a "results may be
  incomplete" note, instead of being killed by the host. Very large projects also hit the text-search limit.
- **No export when the DTO closure did not finish**: an incomplete route list is a display problem — a few endpoints
  are simply missing. An incomplete DTO graph is different: schemas built from it lose fields, and export updates an
  existing interface by `method + path`, so sending one would replace the server's correct document with a poorer
  copy and still report "all endpoints synced". When the closure misses the 4.2 s budget
  (`ScanResult::dto_complete` is false), export therefore refuses before a single request goes out and says why. The
  partial result is also not cached as a complete scan, so a retry scans again instead of replaying the damaged
  graph. The dashboard only reads routes and is unaffected. To get past it, narrow the scope (export the current
  file or folder) or retry.
- **Diagnostic-level positions**: an endpoint's position is the annotation's line and column (0-based line, UTF-16
  column), used for navigation only — there is no syntax highlighting.
- **No mock runtime**: no JVM agent; the alignment is at the source level only.

## Code layout

| File | Purpose |
| - | - |
| `src/java.rs` | Java/Kotlin lexer: identifiers, strings, comments, bracket matching, UTF-16 positions |
| `src/spring.rs` | Structural parsing: classes/methods/fields/parameters/annotations → `RawEndpoint` |
| `src/model.rs` | Endpoint model, generic splitting, JSON Schema generation |
| `src/scan.rs` | Project scan: text search → parsing → DTO transitive closure (project sources + dependency jars) |
| `src/deps.rs` | Dependency-jar resolution: POM tree → local-repository jar → class definitions, with caching and read budgets |
| `src/classfile.rs` | `.class` parsing: fields, generic `Signature`, superclass and type parameters |
| `src/zip.rs` | Minimal ZIP reading for jars (central directory + stored/deflate entries) |
| `src/yapi.rs` | YApi protocol client and endpoint-document construction |
| `src/lib.rs` | Commands, view, the `endpoints` provider, batched export queue |
| `src/i18n.rs` | Text the host does not translate (page/notifications/errors), chosen from `host::locale()` |
| `src/page.html` | Dashboard page (sandboxed iframe, messaging with the component) |

## Tests

```sh
cargo test                      # 115 tests: parsing, schema, ZIP/class, POM, route constants, the type index, the scan budget, refusing an export on a DTO deadline, YApi encoding, queue, i18n, and the end-to-end path
node script/check-pages.mjs     # the dashboard's real rendering in both languages (cargo test cannot run JS)
pwsh script/package.ps1         # rebuild the component and pack it (including icons/ and locales/)
```

The end-to-end cases run on the SDK's `MockHost`: `tests/fixtures_export.rs` injects every `.java`/`.kt` under
`fixtures/spring-demo` as project files and walks the full `scan` → `api_doc` → `save_api` path, asserting that the
Feign service name does not become a path, that `Result<List<Order>>`'s `data` is an array, that Kotlin and JAX-RS
resources are both recognized, and that a multipart endpoint carries two form parts; `tests/unknown_wrapper.rs`
keeps only a controller and a DTO so `ApiEnvelope` resolves like a dependency jar that cannot be found, asserting
that `ApiEnvelope<Map<String, List<ItemInfoDTO>>>` still exports a readable `Map` plus DTO fields;
`tests/jar_deps.rs` builds a Maven repository in memory (a POM chain plus `fixtures/deps/api-envelope.jar`) and
asserts that `ApiEnvelope` from a transitive dependency exports its superclass `code`/`message` and its type
argument, that a wrapper implementing an interface (`JailedEnvelope`) keeps its generic payload field, and that the
plugin falls back quietly without the grant; `tests/route_constants.rs` covers constant-written paths: a qualified
name resolves through its declaring class (two classes each with `PATH` do not cross), two packages each with a
same-named `Routes` resolve through their own `import`, a fully qualified name in the annotation keeps the package,
an unqualified name prefers the current class, `import static` resolves, and an ambiguous same-name is not guessed;
`tests/translations.rs` replays the host's catalog-tag matching over `locales/zh-CN.json`, asserting a Chinese IDE
finds it and an English one keeps the source text, that the file name is a valid language tag, and that every
manifest/settings string has a Chinese entry.

`fixtures/spring-demo` is scan input only and does not need to compile or run — the plugin reads source text;
`fixtures/deps/api-envelope.jar` is built from `BaseReply.java` / `ApiEnvelope.java` / `PageRequest.java` /
`GenericBase.java` / `JailedEnvelope.java` / `PagedEnvelope.java` with `javac --release 8` (the last two
deliberately carry an interface and a generic superclass, to pin class `Signature` parsing). Tests and docs use
`com.acme` / `com.example` example naming throughout; real projects' class names stay out of the repository.

The SDK version is pinned by the tag in `Cargo.toml` (currently `v0.1.50`, plugin API 1.14); when upgrading the IDE,
change the tag and `engines.openidea` together — they must match or the component's WIT interfaces will not line up
with the host. A fresh build imports `openidea:plugin/*@1.14.0` and exports `commands`, `lifecycle`, `view-provider`,
`endpoints-provider` and `event-handler`; check any component with
`cargo run --example component_api -- dist/plugin.wasm`.

`dist/plugin.wasm` is committed with the source and rebuilt by `script/package.ps1` before each release; check its plugin API version with `cargo run --example component_api -- dist/plugin.wasm`.

## Releasing

The registry manual's two commands are the whole flow; there are no extra build steps:

```sh
openidea-plugin build .                                    # produces dist/plugin.wasm
openidea-plugin pack . --key ~/.openidea-keys/apimocktle.key --publisher apimocktle
```

`script/release.ps1` chains "pack → create release → upload assets" into one command and prints the `version` /
`package` / `sha256` to put into [l0ng-ai/openidea-plugins](https://github.com/l0ng-ai/openidea-plugins):

```sh
git tag v0.3.1 && git push origin v0.3.1
pwsh script/release.ps1            # -DryRun only builds the signature, without touching the release
```

The signing key lives **on this machine only**: it is not committed (`.gitignore` has `*.key`) and is not put into
CI secrets. Anyone who reads it can publish as `apimocktle.*`, so that step stays local, behind a repeatable
command rather than a pipeline.
