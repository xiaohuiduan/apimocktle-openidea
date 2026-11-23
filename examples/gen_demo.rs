//! Generates a Spring Boot project of a chosen size, so the scanner's cost can
//! be reproduced and measured against the host's call budget.
//!
//! The scanner walks six stages (project text search → read+parse → route
//! constants → sort/dedup → DTO closure → dependency jars). On a real project
//! of a few hundred controllers it overruns the host's five-second call limit,
//! the instance is reaped, and the next call starts from an empty cache and
//! overruns again. This generator produces inputs that make that failure
//! repeatable without shipping thousands of files.
//!
//! ```sh
//! cargo run --release --example gen_demo -- --preset big --out %TEMP%\apimocktle-demo\big
//! cargo run --release --example gen_demo -- --controllers 300 --dtos 900 --modules 6 --out <dir>
//! ```
//!
//! `--out` defaults to `<temp>/apimocktle-demo/<preset>`. Do **not** point it at
//! an ignored directory (`target/`, `build/`): the host skips ignored files in
//! `find_files`, so a generated project parked there scans to zero endpoints and
//! reproduces nothing.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Interfaces every generated controller declares.
const METHODS_PER_CONTROLLER: usize = 4;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Preset {
    Small,
    Big,
    Huge,
}

impl Preset {
    /// `(controllers, dtos, modules)` — the sizes documented in the task's PRD.
    fn shape(self) -> (usize, usize, usize) {
        match self {
            Preset::Small => (20, 60, 2),
            Preset::Big => (300, 900, 6),
            Preset::Huge => (600, 1_800, 8),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Preset::Small => "small",
            Preset::Big => "big",
            Preset::Huge => "huge",
        }
    }
}

struct Spec {
    out: PathBuf,
    controllers: usize,
    dtos: usize,
    modules: usize,
    feign: usize,
    kotlin: usize,
    constants: bool,
    force: bool,
}

impl Spec {
    fn parse(args: &[String]) -> Result<Spec, String> {
        let mut preset = Preset::Big;
        let mut out: Option<PathBuf> = None;
        let mut controllers: Option<usize> = None;
        let mut dtos: Option<usize> = None;
        let mut modules: Option<usize> = None;
        let mut feign: Option<usize> = None;
        let mut kotlin: Option<usize> = None;
        let mut constants: Option<bool> = None;
        let mut force = false;

        let mut i = 0;
        while i < args.len() {
            let flag = args[i].as_str();
            // The value-taking flags all need the next argument; a missing one
            // is a usage error, never a silent default.
            let next = |i: usize, flag: &str| -> Result<String, String> {
                args.get(i + 1).cloned().ok_or_else(|| format!("{flag} needs a value"))
            };
            match flag {
                "--preset" => {
                    let value = next(i, flag)?;
                    preset = match value.as_str() {
                        "small" => Preset::Small,
                        "big" => Preset::Big,
                        "huge" => Preset::Huge,
                        other => return Err(format!("unknown preset {other:?} (small|big|huge)")),
                    };
                    i += 1;
                }
                "--out" => {
                    out = Some(PathBuf::from(next(i, flag)?));
                    i += 1;
                }
                "--controllers" => {
                    controllers = Some(count(&next(i, flag)?, flag)?);
                    i += 1;
                }
                "--dtos" => {
                    dtos = Some(count(&next(i, flag)?, flag)?);
                    i += 1;
                }
                "--modules" => {
                    modules = Some(count(&next(i, flag)?, flag)?);
                    i += 1;
                }
                "--feign" => {
                    feign = Some(count(&next(i, flag)?, flag)?);
                    i += 1;
                }
                "--kotlin" => {
                    kotlin = Some(count(&next(i, flag)?, flag)?);
                    i += 1;
                }
                "--constants" => {
                    constants = Some(match next(i, flag)?.as_str() {
                        "on" | "true" | "1" => true,
                        "off" | "false" | "0" => false,
                        other => return Err(format!("--constants expects on|off, got {other:?}")),
                    });
                    i += 1;
                }
                "--force" => force = true,
                "--help" | "-h" => return Err(help()),
                other => return Err(format!("unknown argument {other:?}\n\n{}", help())),
            }
            i += 1;
        }

        let (base_controllers, base_dtos, base_modules) = preset.shape();
        let controllers = controllers.unwrap_or(base_controllers);
        let dtos = dtos.unwrap_or(base_dtos);
        let modules = modules.unwrap_or(base_modules).max(1);
        if controllers == 0 || dtos == 0 {
            return Err("--controllers and --dtos must be at least 1".into());
        }
        let out = out.unwrap_or_else(|| std::env::temp_dir().join("apimocktle-demo").join(preset.name()));
        Ok(Spec {
            out,
            controllers,
            dtos,
            modules,
            feign: feign.unwrap_or(modules),
            kotlin: kotlin.unwrap_or(1),
            constants: constants.unwrap_or(true),
            force,
        })
    }
}

fn help() -> String {
    [
        "usage: gen_demo [--preset small|big|huge] [--out DIR]",
        "               [--controllers N] [--dtos N] [--modules N]",
        "               [--feign N] [--kotlin N] [--constants on|off] [--force]",
    ]
    .join("\n")
}

fn count(text: &str, flag: &str) -> Result<usize, String> {
    text.parse::<usize>().map_err(|_| format!("{flag} expects a number, got {text:?}"))
}

/// Files and endpoints a spec produces; also written to `demo.json` so the
/// harness and the test read the size instead of hard-coding it.
struct Manifest {
    controllers: usize,
    dtos: usize,
    methods_per_controller: usize,
    modules: usize,
    feign: usize,
    kotlin: usize,
    constants: bool,
    source_files: usize,
    endpoints: usize,
}

impl Manifest {
    fn new(spec: &Spec) -> Manifest {
        // Every controller, DTO, RouteConstants and Feign client is one file,
        // plus `Result<T>`, one Kotlin controller per unit and one data class.
        let source_files = spec.controllers
            + spec.dtos
            + spec.modules
            + spec.feign
            + 1
            + spec.kotlin
            + usize::from(spec.kotlin > 0);
        let endpoints =
            spec.controllers * METHODS_PER_CONTROLLER + spec.feign * 2 + spec.kotlin * 2;
        Manifest {
            controllers: spec.controllers,
            dtos: spec.dtos,
            methods_per_controller: METHODS_PER_CONTROLLER,
            modules: spec.modules,
            feign: spec.feign,
            kotlin: spec.kotlin,
            constants: spec.constants,
            source_files,
            endpoints,
        }
    }

    fn to_json(&self) -> String {
        format!(
            "{{\n  \"controllers\": {},\n  \"dtos\": {},\n  \"methodsPerController\": {},\n  \
             \"modules\": {},\n  \"feign\": {},\n  \"kotlin\": {},\n  \"constants\": {},\n  \
             \"sourceFiles\": {},\n  \"endpoints\": {}\n}}\n",
            self.controllers,
            self.dtos,
            self.methods_per_controller,
            self.modules,
            self.feign,
            self.kotlin,
            self.constants,
            self.source_files,
            self.endpoints
        )
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let spec = match Spec::parse(&args) {
        Ok(spec) => spec,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let started = Instant::now();
    let manifest = Manifest::new(&spec);

    if let Err(message) = prepare(&spec) {
        eprintln!("{message}");
        std::process::exit(1);
    }
    warn_ignored_location(&spec.out);

    let mut written = 0usize;
    let mut failure: Option<String> = None;
    {
        let mut step = |result: Result<(), (String, std::io::Error)>| {
            if let Err((what, error)) = result
                && failure.is_none()
            {
                failure = Some(format!("写入失败：{what}: {error}"));
            }
        };
        step(gen_poms(&spec, &mut written));
        step(gen_result(&spec, &mut written));
        step(gen_constants(&spec, &mut written));
        step(gen_dtos(&spec, &mut written));
        step(gen_controllers(&spec, &mut written));
        step(gen_feign(&spec, &mut written));
        step(gen_kotlin(&spec, &mut written));
        step(write(&spec.out.join("demo.json"), &manifest.to_json()));
        written += 1;
    }
    if let Some(message) = failure {
        eprintln!("{message}");
        std::process::exit(1);
    }

    println!("已生成 {written} 个文件 → {}", spec.out.display());
    println!(
        "规模：{} 控制器 × {METHODS_PER_CONTROLLER} 接口 = {} 个接口，{} 个 DTO，{} 个模块，{} 个 Feign 客户端，{} 个 Kotlin 控制器",
        spec.controllers, manifest.endpoints, spec.dtos, spec.modules, spec.feign, spec.kotlin
    );
    println!(
        "路由常量：{}",
        if spec.constants { "开（注解引用 RouteConstants）" } else { "关（注解写字面量）" }
    );
    println!("耗时 {:?}", started.elapsed());
    println!();
    println!("下一步：");
    println!("  1. 在 OpenIDEA 里打开 {}，打开 ApiMocktle 仪表盘，观察超时日志", spec.out.display());
    println!("  2. cargo run --release --example scan_bench -- \"{}\"", spec.out.display());
}

/// Refuses to run into a non-empty directory unless `--force`.
fn prepare(spec: &Spec) -> Result<(), String> {
    let out = &spec.out;
    if out.exists() {
        if !out.is_dir() {
            return Err(format!("{} 已存在且不是目录", out.display()));
        }
        let empty = fs::read_dir(out).map(|mut d| d.next().is_none()).unwrap_or(false);
        if !empty && !spec.force {
            return Err(format!("{} 不是空目录；换一个 --out，或加 --force 覆盖", out.display()));
        }
        if !empty {
            fs::remove_dir_all(out).map_err(|e| format!("无法清理 {}: {e}", out.display()))?;
        }
    }
    fs::create_dir_all(out).map_err(|e| format!("无法创建 {}: {e}", out.display()))
}

/// The host's `find_files` skips ignored files, so a generated project parked
/// under `target/` or another build directory would scan to zero endpoints.
fn warn_ignored_location(out: &Path) {
    const IGNORED: &[&str] = &["target", "build", "out", "node_modules", ".git", ".idea", "dist"];
    let offending: Vec<String> = out
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(str::to_owned))
        .filter(|c| IGNORED.contains(&c.to_ascii_lowercase().as_str()))
        .collect();
    if offending.is_empty() {
        return;
    }
    eprintln!("警告：{} 位于被忽略的目录（{}）内。", out.display(), offending.join(", "));
    eprintln!("宿主的 find_files 会跳过被忽略的文件，工程在那里可能扫出 0 个接口。");
    eprintln!("请换一个不在 .gitignore / build 输出目录下的路径。");
}

fn write(path: &Path, text: &str) -> Result<(), (String, std::io::Error)> {
    let shown = path.display().to_string();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| (shown.clone(), e))?;
    }
    fs::write(path, text).map_err(|e| (shown, e))
}

fn module_of(index: usize, modules: usize) -> usize {
    index % modules.max(1)
}

/// The `src/main/java/com/acme/m<K>` tree of module `k`.
fn java_root(out: &Path, module: usize) -> PathBuf {
    out.join(format!("module-{module}"))
        .join("src/main/java/com/acme")
        .join(format!("m{module}"))
}

fn gen_poms(spec: &Spec, written: &mut usize) -> Result<(), (String, std::io::Error)> {
    let modules: String = (0..spec.modules)
        .map(|k| format!("        <module>module-{k}</module>\n"))
        .collect();
    let parent = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<project xmlns="http://maven.apache.org/POM/4.0.0">
    <modelVersion>4.0.0</modelVersion>
    <groupId>com.acme</groupId>
    <artifactId>demo</artifactId>
    <version>1.0.0</version>
    <packaging>pom</packaging>

    <properties>
        <java.version>17</java.version>
        <spring-boot.version>3.2.0</spring-boot.version>
    </properties>

    <modules>
{modules}    </modules>

    <dependencyManagement>
        <dependencies>
            <dependency>
                <groupId>org.springframework.boot</groupId>
                <artifactId>spring-boot-dependencies</artifactId>
                <version>${{spring-boot.version}}</version>
                <type>pom</type>
                <scope>import</scope>
            </dependency>
        </dependencies>
    </dependencyManagement>
</project>
"#
    );
    write(&spec.out.join("pom.xml"), &parent)?;
    *written += 1;

    for k in 0..spec.modules {
        let module = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<project xmlns="http://maven.apache.org/POM/4.0.0">
    <modelVersion>4.0.0</modelVersion>
    <parent>
        <groupId>com.acme</groupId>
        <artifactId>demo</artifactId>
        <version>1.0.0</version>
    </parent>
    <artifactId>module-{k}</artifactId>

    <dependencies>
        <dependency>
            <groupId>org.springframework.boot</groupId>
            <artifactId>spring-boot-starter-web</artifactId>
        </dependency>
    </dependencies>
</project>
"#
        );
        write(&spec.out.join(format!("module-{k}")).join("pom.xml"), &module)?;
        *written += 1;
    }
    Ok(())
}

fn gen_result(spec: &Spec, written: &mut usize) -> Result<(), (String, std::io::Error)> {
    let source = r#"package com.acme;

/**
 * 统一响应包装。
 */
public class Result<T> {

    /**
     * 业务码。
     */
    private int code;

    /**
     * 提示文本。
     */
    private String msg;

    /**
     * 载荷。
     */
    private T data;
}
"#;
    write(&spec.out.join("module-0/src/main/java/com/acme/Result.java"), source)?;
    *written += 1;
    Ok(())
}

fn gen_constants(spec: &Spec, written: &mut usize) -> Result<(), (String, std::io::Error)> {
    if !spec.constants {
        return Ok(());
    }
    for k in 0..spec.modules {
        let source = r#"package PKG;

/**
 * 本模块的路由常量。
 */
public final class RouteConstants {

    /**
     * 列表。
     */
    public static final String LIST = "/list";

    /**
     * 详情。
     */
    public static final String DETAIL = "/{id}";

    /**
     * 新建。
     */
    public static final String CREATE = "/create";

    /**
     * 更新。
     */
    public static final String UPDATE = "/{id}";

    private RouteConstants() {
    }
}
"#
        .replace("PKG", &format!("com.acme.m{k}"));
        write(&java_root(&spec.out, k).join("RouteConstants.java"), &source)?;
        *written += 1;
    }
    Ok(())
}

fn gen_dtos(spec: &Spec, written: &mut usize) -> Result<(), (String, std::io::Error)> {
    for i in 0..spec.dtos {
        let module = module_of(i, spec.modules);
        // The chain closes into a cycle so the scanner's visited-set is
        // exercised; a real model has cycles too.
        let next = (i + 1) % spec.dtos;
        let next_module = module_of(next, spec.modules);
        let mut source = String::new();
        let _ = write!(
            source,
            "package com.acme.m{module}.dto;\n\nimport com.acme.m{next_module}.dto.Dto{next};\n\n\
             /**\n * 第 {i} 号数据传输对象。\n */\npublic class Dto{i} {{\n\n"
        );
        for (name, ty, doc) in [
            ("name", "String", "名称。"),
            ("seq", "long", "序号。"),
            ("enabled", "boolean", "是否启用。"),
        ] {
            let _ = write!(
                source,
                "    /**\n     * {doc}\n     */\n    private {ty} {name}{i};\n\n"
            );
        }
        let _ = write!(source, "    /**\n     * 下一环。\n     */\n    private Dto{next} next;\n}}\n");
        write(&java_root(&spec.out, module).join(format!("dto/Dto{i}.java")), &source)?;
        *written += 1;
    }
    Ok(())
}

fn gen_controllers(spec: &Spec, written: &mut usize) -> Result<(), (String, std::io::Error)> {
    for j in 0..spec.controllers {
        let module = module_of(j, spec.modules);
        let dto = j % spec.dtos;
        let dto_module = module_of(dto, spec.modules);
        let source = controller(&format!("m{module}"), j, dto, dto_module, spec.constants);
        write(
            &java_root(&spec.out, module).join(format!("controller/Ctrl{j}Controller.java")),
            &source,
        )?;
        *written += 1;
    }
    Ok(())
}

fn controller(module: &str, j: usize, dto: usize, dto_module: usize, constants: bool) -> String {
    let package = format!("com.acme.{module}");
    // With constants the annotation names a field of the module's
    // `RouteConstants`; without them the path is written inline. Both shapes
    // occur in real projects, and only the first forces the extra source reads.
    let (list, detail, create, update) = if constants {
        (
            "RouteConstants.LIST",
            "RouteConstants.DETAIL",
            "RouteConstants.CREATE",
            "RouteConstants.UPDATE",
        )
    } else {
        ("\"/list\"", "\"/{id}\"", "\"/create\"", "\"/{id}\"")
    };
    let constants_import =
        if constants { format!("import {package}.RouteConstants;\n") } else { String::new() };
    format!(
        r#"package {package}.controller;

import com.acme.Result;
import com.acme.m{dto_module}.dto.Dto{dto};
{constants_import}import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.PathVariable;
import org.springframework.web.bind.annotation.PostMapping;
import org.springframework.web.bind.annotation.PutMapping;
import org.springframework.web.bind.annotation.RequestBody;
import org.springframework.web.bind.annotation.RequestMapping;
import org.springframework.web.bind.annotation.RequestParam;
import org.springframework.web.bind.annotation.RestController;

/**
 * 第 {j} 号控制器。
 */
@RestController
@RequestMapping("/api/{module}/c{j}")
public class Ctrl{j}Controller {{

    /**
     * 列表。
     */
    @GetMapping({list})
    public Result<Dto{dto}> list(@RequestParam(value = "page", defaultValue = "1") int page,
                             @RequestParam(value = "size", defaultValue = "20") int size) {{
        return null;
    }}

    /**
     * 详情。
     */
    @GetMapping({detail})
    public Result<Dto{dto}> detail(@PathVariable("id") long id) {{
        return null;
    }}

    /**
     * 新建。
     */
    @PostMapping({create})
    public Result<Dto{dto}> create(@RequestBody Dto{dto} form) {{
        return null;
    }}

    /**
     * 更新。
     */
    @PutMapping({update})
    public Result<Dto{dto}> update(@RequestBody Dto{dto} form) {{
        return null;
    }}
}}
"#
    )
}

fn gen_feign(spec: &Spec, written: &mut usize) -> Result<(), (String, std::io::Error)> {
    for k in 0..spec.feign {
        let module = module_of(k, spec.modules);
        let dto = k % spec.dtos;
        let dto_module = module_of(dto, spec.modules);
        let source = format!(
            r#"package com.acme.m{module}.client;

import com.acme.Result;
import com.acme.m{dto_module}.dto.Dto{dto};
import org.springframework.cloud.openfeign.FeignClient;
import org.springframework.web.bind.annotation.GetMapping;
import org.springframework.web.bind.annotation.PathVariable;
import org.springframework.web.bind.annotation.PostMapping;

/**
 * 第 {k} 号远端服务客户端。
 */
@FeignClient(name = "svc-{k}", path = "/api/feign/{k}")
public interface Feign{k}Client {{

    /**
     * 查询。
     */
    @GetMapping("/{{id}}")
    Result<Dto{dto}> get(@PathVariable("id") long id);

    /**
     * 保存。
     */
    @PostMapping("/save")
    Result<Dto{dto}> save(Dto{dto} form);
}}
"#
        );
        write(
            &java_root(&spec.out, module).join(format!("client/Feign{k}Client.java")),
            &source,
        )?;
        *written += 1;
    }
    Ok(())
}

fn gen_kotlin(spec: &Spec, written: &mut usize) -> Result<(), (String, std::io::Error)> {
    if spec.kotlin == 0 {
        return Ok(());
    }
    let root = spec.out.join("module-0/src/main/kotlin/com/acme/kt");
    let dto = 0usize;
    let dto_module = module_of(dto, spec.modules);
    for n in 0..spec.kotlin {
        let source = format!(
            r#"package com.acme.kt

import com.acme.Result
import com.acme.m{dto_module}.dto.Dto{dto}
import org.springframework.web.bind.annotation.GetMapping
import org.springframework.web.bind.annotation.PathVariable
import org.springframework.web.bind.annotation.RequestMapping
import org.springframework.web.bind.annotation.RequestParam
import org.springframework.web.bind.annotation.RestController

/**
 * 第 {n} 个 Kotlin 控制器。
 */
@RestController
@RequestMapping("/api/kotlin/{n}")
class KtController{n} {{

    /** 字段初始化器出现在方法之前，是最容易吞掉后续方法的写法。 */
    private val hits = {n}

    /**
     * 健康检查。
     */
    @GetMapping("/health")
    fun health(): Result<String> {{
        return Result()
    }}

    /**
     * 快照。
     */
    @GetMapping("/orders/{{id}}")
    fun order(@PathVariable("id") id: Long,
              @RequestParam(value = "detail", required = false) detail: Boolean): Result<Dto{dto}> =
        Result()
}}
"#
        );
        write(&root.join(format!("KtController{n}.kt")), &source)?;
        *written += 1;
    }
    let source = format!(
        r#"package com.acme.kt

/**
 * Kotlin 侧的数据类。
 */
data class KtDto{dto}(
    val name: String,
    val seq: Long,
    val enabled: Boolean
)
"#
    );
    write(&root.join(format!("KtDto{dto}.kt")), &source)?;
    *written += 1;
    Ok(())
}
