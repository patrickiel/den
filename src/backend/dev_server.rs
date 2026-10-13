//! The project's dev server, found from the project itself: which program
//! serves it (Vite, Next.js, ASP.NET Core, Django, …) and on which port, read
//! from `package.json`'s scripts, the framework's config (a `vite.config.ts`
//! with `server.port`), `.env`'s `PORT`, `launchSettings.json` and the like.
//! One tab-strip button opens it, with no preset to keep up by hand.
//!
//! A monorepo's apps are found two folders deep. Opening prefers a server
//! that answers now (the one running), and a port one on from the found one
//! for the servers that move when theirs is taken (Vite, Next.js, Nuxt).

use std::{
    collections::BTreeSet,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream},
    path::{Component, Path},
    time::Duration,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DevServer {
    /// What serves it: "Vite", "Next.js", "Django", ….
    pub name: String,
    pub port: u16,
    pub https: bool,
    /// The folder below the session root it was found in; empty for the root.
    pub folder: String,
    /// It takes the next port when its own is busy.
    pub roams: bool,
}

impl DevServer {
    fn new(name: &str, port: u16) -> Self {
        Self {
            name: name.into(),
            port,
            https: false,
            folder: String::new(),
            roams: false,
        }
    }

    pub fn url(&self) -> String {
        self.url_at(self.port)
    }

    fn url_at(&self, port: u16) -> String {
        let scheme = if self.https { "https" } else { "http" };
        format!("{scheme}://localhost:{port}")
    }

    /// For the button's tooltip: `Vite at http://localhost:5173` (and the
    /// folder, in a monorepo).
    pub fn describe(&self) -> String {
        if self.folder.is_empty() {
            format!("{} at {}", self.name, self.url())
        } else {
            format!("{} in {} at {}", self.name, self.folder, self.url())
        }
    }
}

/// Folders never looked into: dependencies, output and the like.
const SKIPPED: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    "bin",
    "obj",
    "vendor",
    "coverage",
    "venv",
    "__pycache__",
    "public",
    "static",
    "assets",
    "test",
    "tests",
];

const DEPTH: usize = 2;
const MAX_FOLDERS: usize = 300;

/// The dev servers of the project at `root`, the root's first, then its
/// subfolders' (two levels down), each folder's own ordered likeliest first.
pub fn detect(root: &Path) -> Vec<DevServer> {
    let mut found = Vec::new();
    let mut budget = MAX_FOLDERS;
    walk(root, root, DEPTH, &mut budget, &mut found);
    let mut seen = BTreeSet::new();
    found.retain(|s| seen.insert((s.folder.clone(), s.url())));
    found
}

fn walk(root: &Path, dir: &Path, depth: usize, budget: &mut usize, out: &mut Vec<DevServer>) {
    if *budget == 0 {
        return;
    }
    *budget -= 1;
    let folder = dir
        .strip_prefix(root)
        .ok()
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();
    out.extend(in_folder(dir).into_iter().map(|mut s| {
        s.folder = folder.clone();
        s
    }));
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut dirs: Vec<_> = entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .filter(|p| !skipped(p))
        .collect();
    dirs.sort();
    for sub in dirs {
        walk(root, &sub, depth - 1, budget, out);
    }
}

fn skipped(dir: &Path) -> bool {
    dir.file_name()
        .and_then(|n| n.to_str())
        .is_none_or(|name| name.starts_with('.') || SKIPPED.contains(&name))
}

/// Whether a change to `path` may change what [`detect`] finds.
pub fn is_project_file(path: &Path) -> bool {
    let in_skipped = path.parent().is_some_and(|dir| {
        dir.components().any(|c| match c {
            Component::Normal(name) => name
                .to_str()
                .is_none_or(|n| n.starts_with('.') || SKIPPED.contains(&n)),
            _ => false,
        })
    });
    if in_skipped {
        return false;
    }
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    name.starts_with(".env")
        || name.starts_with("vite.config.")
        || name.starts_with("nuxt.config.")
        || name.starts_with("astro.config.")
        || name.starts_with("hugo.")
        || name.starts_with("wrangler.")
        || matches!(
            name.as_str(),
            "package.json"
                | "angular.json"
                | "tauri.conf.json"
                | "launchsettings.json"
                | "manage.py"
                | "pyproject.toml"
                | "requirements.txt"
                | "gemfile"
                | "mix.exs"
                | "book.toml"
                | "trunk.toml"
                | "dioxus.toml"
                | "cargo.toml"
                | "application.properties"
                | "application.yml"
                | "application.yaml"
                | "artisan"
                | "composer.json"
                | "mkdocs.yml"
                | "_config.yml"
                | "config.toml"
        )
}

/// The URL to open among `found`: the first that answers now, else the port
/// a roaming server would have moved to, else the first found.
pub fn pick(found: &[DevServer]) -> Option<String> {
    let first = found.first()?;
    if let Some(server) = found.iter().find(|s| listening(s.port)) {
        return Some(server.url());
    }
    for server in found.iter().filter(|s| s.roams) {
        if let Some(port) = (1..=5)
            .filter_map(|n| server.port.checked_add(n))
            .find(|&port| listening(port))
        {
            return Some(server.url_at(port));
        }
    }
    Some(first.url())
}

fn listening(port: u16) -> bool {
    [
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(Ipv6Addr::LOCALHOST),
    ]
    .into_iter()
    .any(|ip| {
        TcpStream::connect_timeout(&SocketAddr::new(ip, port), Duration::from_millis(100)).is_ok()
    })
}

// -- One folder ---------------------------------------------------------------

fn read(dir: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(name)).ok()
}

/// The first of `names` in `dir`, by its text.
fn read_any(dir: &Path, names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| read(dir, name))
}

const VITE_CONFIGS: &[&str] = &[
    "vite.config.ts",
    "vite.config.js",
    "vite.config.mts",
    "vite.config.mjs",
    "vite.config.cts",
    "vite.config.cjs",
];
const NUXT_CONFIGS: &[&str] = &["nuxt.config.ts", "nuxt.config.js", "nuxt.config.mjs"];
const ASTRO_CONFIGS: &[&str] = &[
    "astro.config.mjs",
    "astro.config.ts",
    "astro.config.js",
    "astro.config.mts",
    "astro.config.cjs",
];

fn in_folder(dir: &Path) -> Vec<DevServer> {
    let mut out = Vec::new();
    if let Some(text) = read(dir, "package.json") {
        out.extend(from_package(dir, &text));
    }
    if let Some(text) = read(dir, "tauri.conf.json")
        && let Some(server) = from_tauri(&text)
    {
        out.push(server);
    }
    if let Some(text) = read(dir, "Properties/launchSettings.json")
        && let Some(server) = from_launch_settings(&text)
    {
        out.push(server);
    }
    out.extend(from_others(dir));
    out
}

/// Tauri's window shows `build.devUrl` (`devPath` before 2.0).
fn from_tauri(text: &str) -> Option<DevServer> {
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    let build = &json["build"];
    let url = build["devUrl"].as_str().or(build["devPath"].as_str())?;
    server_from_url("Tauri", url)
}

/// `https://localhost:7123;http://localhost:5123`: the first profile with an
/// `applicationUrl`, at its first URL, as `dotnet run` opens it.
fn from_launch_settings(text: &str) -> Option<DevServer> {
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    let profiles = json["profiles"].as_object()?;
    profiles
        .values()
        .filter_map(|p| p["applicationUrl"].as_str())
        .flat_map(|urls| urls.split(';'))
        .find_map(|url| server_from_url("ASP.NET Core", url.trim()))
}

fn server_from_url(name: &str, url: &str) -> Option<DevServer> {
    let (https, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else {
        (false, url.strip_prefix("http://")?)
    };
    let host = rest.split('/').next()?;
    let (_, port) = host.rsplit_once(':')?;
    let mut server = DevServer::new(name, port.parse().ok()?);
    server.https = https;
    Some(server)
}

/// The frameworks `package.json` may run, by the program their script
/// names (as in `"dev": "vite dev"`), then by their dependency.
struct Framework {
    /// The program's name on the script, and the dependency it comes with.
    program: &'static str,
    dep: &'static str,
    name: &'static str,
    port: u16,
    /// Takes `PORT` from the environment (`.env`).
    env: bool,
    roams: bool,
}

const fn fw(
    program: &'static str,
    dep: &'static str,
    name: &'static str,
    port: u16,
    env: bool,
    roams: bool,
) -> Framework {
    Framework {
        program,
        dep,
        name,
        port,
        env,
        roams,
    }
}

const FRAMEWORKS: &[Framework] = &[
    fw("vite", "vite", "Vite", 5173, false, true),
    fw("vitepress", "vitepress", "VitePress", 5173, false, true),
    fw("next", "next", "Next.js", 3000, true, true),
    fw("nuxt", "nuxt", "Nuxt", 3000, true, true),
    fw("nuxi", "nuxt", "Nuxt", 3000, true, true),
    fw("astro", "astro", "Astro", 4321, false, true),
    fw("ng", "@angular/cli", "Angular", 4200, false, false),
    fw(
        "react-scripts",
        "react-scripts",
        "Create React App",
        3000,
        true,
        false,
    ),
    fw("gatsby", "gatsby", "Gatsby", 8000, true, false),
    fw(
        "docusaurus",
        "@docusaurus/core",
        "Docusaurus",
        3000,
        true,
        false,
    ),
    fw("ember", "ember-cli", "Ember", 4200, false, false),
    fw(
        "vue-cli-service",
        "@vue/cli-service",
        "Vue CLI",
        8080,
        false,
        true,
    ),
    fw(
        "webpack",
        "webpack-dev-server",
        "webpack",
        8080,
        false,
        false,
    ),
    fw(
        "webpack-dev-server",
        "webpack-dev-server",
        "webpack",
        8080,
        false,
        false,
    ),
    fw("rsbuild", "@rsbuild/core", "Rsbuild", 3000, false, true),
    fw("parcel", "parcel", "Parcel", 1234, false, false),
    fw("eleventy", "@11ty/eleventy", "Eleventy", 8080, false, true),
    fw("wrangler", "wrangler", "Wrangler", 8787, false, false),
    fw("storybook", "storybook", "Storybook", 6006, false, false),
    fw("expo", "expo", "Expo", 8081, false, false),
    fw("remix", "@remix-run/dev", "Remix", 3000, true, false),
    fw("vinxi", "vinxi", "SolidStart", 3000, true, false),
    fw("nest", "@nestjs/cli", "NestJS", 3000, true, false),
    fw(
        "redwood",
        "@redwoodjs/core",
        "RedwoodJS",
        8910,
        false,
        false,
    ),
    fw("rw", "@redwoodjs/core", "RedwoodJS", 8910, false, false),
    fw("umi", "umi", "Umi", 8000, true, false),
    fw("ionic", "@ionic/cli", "Ionic", 8100, false, false),
    fw("stencil", "@stencil/core", "Stencil", 3333, false, false),
    fw("sanity", "sanity", "Sanity", 3333, false, false),
    fw("strapi", "@strapi/strapi", "Strapi", 1337, true, false),
    fw("medusa", "@medusajs/medusa", "Medusa", 9000, true, false),
    fw("meteor", "meteor", "Meteor", 3000, false, false),
    fw("sails", "sails", "Sails", 1337, true, false),
    fw("adonis", "@adonisjs/core", "AdonisJS", 3333, true, false),
    fw(
        "http-server",
        "http-server",
        "http-server",
        8080,
        false,
        false,
    ),
    fw(
        "live-server",
        "live-server",
        "live-server",
        8080,
        false,
        false,
    ),
    fw("serve", "serve", "serve", 3000, false, false),
    fw("deno", "", "Deno", 8000, true, false),
    fw("bun", "", "Bun", 3000, true, false),
    fw("nodemon", "", "Node", 3000, true, false),
    fw("ts-node", "", "Node", 3000, true, false),
    fw("ts-node-dev", "", "Node", 3000, true, false),
    fw("tsx", "", "Node", 3000, true, false),
    fw("node", "", "Node", 3000, true, false),
    fw("", "express", "Express", 3000, true, false),
    fw("", "fastify", "Fastify", 3000, true, false),
    fw("", "hono", "Hono", 3000, true, false),
    fw("", "koa", "Koa", 3000, true, false),
];

/// The script a dev server runs from, the first of these that exists.
const DEV_SCRIPTS: &[&str] = &["dev", "start", "serve", "develop", "start:dev"];

fn from_package(dir: &Path, text: &str) -> Vec<DevServer> {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(text) else {
        return Vec::new();
    };
    let deps: BTreeSet<&str> = ["dependencies", "devDependencies"]
        .iter()
        .filter_map(|key| json[key].as_object())
        .flat_map(|map| map.keys().map(String::as_str))
        .collect();
    let scripts = json["scripts"].as_object();
    let script = scripts
        .and_then(|s| DEV_SCRIPTS.iter().find_map(|key| s.get(*key)?.as_str()))
        .unwrap_or("");
    let tokens: Vec<&str> = script
        .split(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '&' | '|' | ';' | '(' | ')'))
        .filter(|t| !t.is_empty())
        .collect();
    let has_token = |t: &str| tokens.contains(&t);
    let mut out = Vec::new();

    // `tauri dev` shows what `beforeDevCommand` serves, at `devUrl`.
    if has_token("tauri")
        && let Some(server) = read(dir, "src-tauri/tauri.conf.json")
            .or_else(|| read(dir, "tauri.conf.json"))
            .and_then(|text| from_tauri(&text))
    {
        out.push(server);
    }

    // `remix vite:dev`, `svelte-kit dev`: Vite underneath.
    let vite = &FRAMEWORKS[0];
    let framework = if has_token("vite") || has_token("vite:dev") || has_token("svelte-kit") {
        Some(vite)
    } else {
        FRAMEWORKS
            .iter()
            .find(|f| !f.program.is_empty() && has_token(f.program))
    }
    .or_else(|| deps.contains("@sveltejs/kit").then_some(vite))
    .or_else(|| {
        FRAMEWORKS
            .iter()
            .find(|f| !f.dep.is_empty() && deps.contains(f.dep))
    });
    let Some(framework) = framework else {
        return out;
    };
    let mut server = DevServer::new(framework.name, framework.port);
    server.roams = framework.roams;
    // A better name for what runs on Vite.
    if framework.name == "Vite" {
        server.name = if deps.contains("@sveltejs/kit") {
            "SvelteKit"
        } else if deps.contains("@remix-run/dev") {
            "Remix"
        } else if deps.contains("@builder.io/qwik-city") {
            "Qwik"
        } else if deps.contains("vike") {
            "Vike"
        } else if deps.contains("@analogjs/platform") {
            "Analog"
        } else {
            "Vite"
        }
        .into();
    }

    // The port: the script's, the framework config's, `.env`'s, the default.
    let config_port = match framework.name {
        "Vite" | "VitePress" => read_any(dir, VITE_CONFIGS).and_then(|c| port_in(&c, "server")),
        "Nuxt" => read_any(dir, NUXT_CONFIGS).and_then(|c| port_in(&c, "devServer")),
        "Astro" => read_any(dir, ASTRO_CONFIGS).and_then(|c| port_in(&c, "server")),
        "Angular" => read(dir, "angular.json").and_then(|c| angular_port(&c)),
        "Wrangler" => read_any(dir, &["wrangler.toml", "wrangler.jsonc", "wrangler.json"])
            .and_then(|c| port_in(&c, "dev")),
        _ => None,
    };
    let env_port = framework.env.then(|| env_port(dir)).flatten();
    if let Some(port) = script_port(&tokens).or(config_port).or(env_port) {
        server.port = port;
    }

    // `vite --https`, `next dev --experimental-https`, `ng serve --ssl`, a
    // `server.https` in the Vite config.
    let config_https = matches!(framework.name, "Vite" | "VitePress")
        && read_any(dir, VITE_CONFIGS).is_some_and(|c| {
            c.find("server").is_some_and(|at| {
                let after = &c[at..];
                // `https:` the option, not a proxy target's `https://`.
                after.contains("basicSsl")
                    || after
                        .match_indices("https:")
                        .any(|(i, _)| !after[i + 6..].starts_with('/'))
            })
        });
    server.https = has_token("--https")
        || has_token("--experimental-https")
        || has_token("--ssl")
        || config_https;
    out.push(server);
    out
}

/// `--port 3001`, `--port=3001`, `-p 3001`, `PORT=3001`.
fn script_port(tokens: &[&str]) -> Option<u16> {
    tokens.iter().enumerate().find_map(|(ix, token)| {
        if let Some(value) = token
            .strip_prefix("--port=")
            .or(token.strip_prefix("PORT="))
        {
            return value.parse().ok();
        }
        if matches!(*token, "--port" | "-p") {
            return tokens.get(ix + 1)?.parse().ok();
        }
        None
    })
}

/// The first `port: 5273` after `block` (`server`, `devServer`) in a JS or
/// TOML config; the first anywhere when there is no such block.
fn port_in(config: &str, block: &str) -> Option<u16> {
    let from = config.find(block).unwrap_or(0);
    let text = &config[from..];
    let at = text.find("port")?;
    let rest = text[at + 4..]
        .trim_start_matches('"')
        .trim_start()
        .trim_start_matches([':', '='])
        .trim_start();
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// `projects.*.architect.serve.options.port`.
fn angular_port(text: &str) -> Option<u16> {
    let json: serde_json::Value = serde_json::from_str(text).ok()?;
    json["projects"].as_object()?.values().find_map(|project| {
        project["architect"]["serve"]["options"]["port"]
            .as_u64()
            .and_then(|p| u16::try_from(p).ok())
    })
}

/// `PORT=3001` from the `.env` files, the most specific first.
fn env_port(dir: &Path) -> Option<u16> {
    [
        ".env.development.local",
        ".env.local",
        ".env.development",
        ".env",
    ]
    .iter()
    .find_map(|name| {
        let text = read(dir, name)?;
        text.lines().find_map(|line| {
            let line = line.trim();
            let value = line.strip_prefix("PORT=")?;
            value.trim().trim_matches(['"', '\'']).parse().ok()
        })
    })
}

/// Everything that is not a Node project: by the files a stack leaves in
/// its folder.
fn from_others(dir: &Path) -> Vec<DevServer> {
    let mut out = Vec::new();
    let has = |name: &str| dir.join(name).exists();
    let text_has = |name: &str, needle: &str| read(dir, name).is_some_and(|t| t.contains(needle));

    if has("manage.py") {
        out.push(DevServer::new("Django", 8000));
    }
    let python = read_any(dir, &["pyproject.toml", "requirements.txt"]).unwrap_or_default();
    let python = python.to_ascii_lowercase();
    if let Some((_, name, port)) = [
        ("fastapi", "FastAPI", 8000),
        ("flask", "Flask", 5000),
        ("streamlit", "Streamlit", 8501),
        ("gradio", "Gradio", 7860),
        ("mkdocs", "MkDocs", 8000),
        ("django", "Django", 8000),
        ("uvicorn", "Uvicorn", 8000),
    ]
    .into_iter()
    .find(|(dep, _, _)| python.contains(dep))
        && !out.iter().any(|s| s.name == name)
    {
        out.push(DevServer::new(name, port));
    }
    if has("mkdocs.yml") && !out.iter().any(|s| s.name == "MkDocs") {
        out.push(DevServer::new("MkDocs", 8000));
    }

    if let Some(gemfile) = read(dir, "Gemfile") {
        for (dep, name, port) in [
            ("rails", "Rails", 3000),
            ("jekyll", "Jekyll", 4000),
            ("sinatra", "Sinatra", 4567),
            ("hanami", "Hanami", 2300),
        ] {
            if gemfile.contains(dep) {
                out.push(DevServer::new(name, port));
                break;
            }
        }
    } else if has("_config.yml") && text_has("_config.yml", "jekyll") {
        out.push(DevServer::new("Jekyll", 4000));
    }

    if text_has("mix.exs", "phoenix") {
        out.push(DevServer::new("Phoenix", 4000));
    }
    if text_has("artisan", "Illuminate") || text_has("composer.json", "laravel/framework") {
        out.push(DevServer::new("Laravel", 8000));
    } else if text_has("composer.json", "symfony/") {
        out.push(DevServer::new("Symfony", 8000));
    }

    if has("hugo.toml")
        || has("hugo.yaml")
        || has("hugo.json")
        || text_has("config.toml", "baseURL")
    {
        out.push(DevServer::new("Hugo", 1313));
    } else if text_has("config.toml", "base_url") {
        out.push(DevServer::new("Zola", 1111));
    }
    if has("book.toml") {
        out.push(DevServer::new("mdBook", 3000));
    }

    if let Some(trunk) = read(dir, "Trunk.toml") {
        out.push(DevServer::new(
            "Trunk",
            port_in(&trunk, "serve").unwrap_or(8080),
        ));
    } else if has("Dioxus.toml") {
        out.push(DevServer::new("Dioxus", 8080));
    } else if let Some(cargo) = read(dir, "Cargo.toml") {
        if cargo.contains("[package.metadata.leptos]") {
            let port = cargo
                .find("site-addr")
                .and_then(|at| cargo[at..].split_once(':').map(|(_, rest)| rest))
                .and_then(|rest| {
                    rest.chars()
                        .take_while(char::is_ascii_digit)
                        .collect::<String>()
                        .parse()
                        .ok()
                })
                .unwrap_or(3000);
            out.push(DevServer::new("Leptos", port));
        } else if let Some(deps) = cargo.find("[dependencies]").map(|at| &cargo[at..]) {
            for (dep, name, port) in [
                ("rocket", "Rocket", 8000),
                ("actix-web", "Actix Web", 8080),
                ("axum", "Axum", 3000),
                ("warp", "Warp", 3030),
            ] {
                if deps
                    .lines()
                    .any(|l| l.trim_start().starts_with(dep) && !l.trim_start().starts_with("#"))
                {
                    out.push(DevServer::new(name, port));
                    break;
                }
            }
        }
    }

    if let Some(properties) = read(dir, "src/main/resources/application.properties") {
        let port = properties
            .lines()
            .find_map(|l| l.trim().strip_prefix("server.port=")?.trim().parse().ok())
            .unwrap_or(8080);
        out.push(DevServer::new("Spring Boot", port));
    } else if let Some(yml) = read_any(
        dir,
        &[
            "src/main/resources/application.yml",
            "src/main/resources/application.yaml",
        ],
    ) {
        out.push(DevServer::new(
            "Spring Boot",
            port_in(&yml, "server").unwrap_or(8080),
        ));
    }

    if !has("package.json")
        && let Some(wrangler) = read_any(dir, &["wrangler.toml", "wrangler.jsonc", "wrangler.json"])
    {
        out.push(DevServer::new(
            "Wrangler",
            port_in(&wrangler, "dev").unwrap_or(8787),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("den-dev-server-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, text: &str) {
        let path = dir.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn sveltekit_with_vite_port() {
        let dir = temp("sveltekit");
        write(
            &dir,
            "package.json",
            r#"{"scripts":{"dev":"vite dev"},"devDependencies":{"@sveltejs/kit":"2","vite":"6"}}"#,
        );
        write(
            &dir,
            "vite.config.ts",
            "export default defineConfig({\n  plugins: [sveltekit()],\n  server: {\n    port: 5273,\n  },\n})\n",
        );
        let found = detect(&dir);
        assert_eq!(found[0].name, "SvelteKit");
        assert_eq!(found[0].url(), "http://localhost:5273");
        assert_eq!(found[0].describe(), "SvelteKit at http://localhost:5273");
        assert!(found[0].roams);
    }

    #[test]
    fn vite_preview_port_is_not_the_server_port() {
        let dir = temp("vite-preview");
        write(&dir, "package.json", r#"{"scripts":{"dev":"vite"}}"#);
        write(
            &dir,
            "vite.config.js",
            "export default { preview: { port: 4173 }, server: { port: 3000, https: {} } }",
        );
        let found = detect(&dir);
        assert_eq!(found[0].url(), "https://localhost:3000");
    }

    #[test]
    fn next_with_script_port_and_env() {
        let dir = temp("next");
        write(
            &dir,
            "package.json",
            r#"{"scripts":{"dev":"next dev --turbopack -p 3001"},"dependencies":{"next":"15"}}"#,
        );
        write(&dir, ".env", "PORT=4000\n");
        assert_eq!(detect(&dir)[0].url(), "http://localhost:3001");
        write(
            &dir,
            "package.json",
            r#"{"scripts":{"dev":"next dev"},"dependencies":{"next":"15"}}"#,
        );
        let found = detect(&dir);
        assert_eq!(found[0].name, "Next.js");
        assert_eq!(found[0].url(), "http://localhost:4000");
    }

    #[test]
    fn framework_from_dependency_when_the_script_is_opaque() {
        let dir = temp("deps");
        write(
            &dir,
            "package.json",
            r#"{"scripts":{"dev":"node scripts/dev.js"},"dependencies":{"express":"4"}}"#,
        );
        let found = detect(&dir);
        assert_eq!(found[0].name, "Node");
        assert_eq!(found[0].port, 3000);
        write(&dir, "package.json", r#"{"dependencies":{"astro":"5"}}"#);
        let found = detect(&dir);
        assert_eq!(found[0].name, "Astro");
        assert_eq!(found[0].port, 4321);
    }

    #[test]
    fn monorepo_apps_follow_the_root() {
        let dir = temp("mono");
        write(&dir, "package.json", r#"{"scripts":{"dev":"turbo dev"}}"#);
        write(
            &dir,
            "apps/web/package.json",
            r#"{"scripts":{"dev":"next dev"},"dependencies":{"next":"15"}}"#,
        );
        write(
            &dir,
            "apps/api/package.json",
            r#"{"scripts":{"dev":"wrangler dev --port 9000"}}"#,
        );
        write(
            &dir,
            "node_modules/next/package.json",
            r#"{"scripts":{"dev":"next dev"}}"#,
        );
        let found = detect(&dir);
        let seen: Vec<(String, String)> = found
            .iter()
            .map(|s| (s.folder.clone(), s.describe()))
            .collect();
        assert_eq!(
            seen,
            vec![
                (
                    "apps/api".into(),
                    "Wrangler in apps/api at http://localhost:9000".into()
                ),
                (
                    "apps/web".into(),
                    "Next.js in apps/web at http://localhost:3000".into()
                ),
            ]
        );
    }

    #[test]
    fn other_stacks() {
        let dir = temp("aspnet");
        write(
            &dir,
            "Properties/launchSettings.json",
            r#"{"profiles":{"https":{"applicationUrl":"https://localhost:7123;http://localhost:5123"}}}"#,
        );
        assert_eq!(
            detect(&dir)[0].describe(),
            "ASP.NET Core at https://localhost:7123"
        );

        let dir = temp("django");
        write(&dir, "manage.py", "");
        write(&dir, "requirements.txt", "Django==5\n");
        let found = detect(&dir);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name, "Django");

        let dir = temp("tauri");
        write(
            &dir,
            "src-tauri/tauri.conf.json",
            r#"{"build":{"devUrl":"http://localhost:1420"}}"#,
        );
        let found = detect(&dir);
        assert_eq!(
            found[0].describe(),
            "Tauri in src-tauri at http://localhost:1420"
        );

        let dir = temp("spring");
        write(
            &dir,
            "src/main/resources/application.properties",
            "server.port=9090\n",
        );
        assert_eq!(detect(&dir)[0].url(), "http://localhost:9090");

        let dir = temp("nothing");
        write(&dir, "README.md", "");
        assert!(detect(&dir).is_empty());
    }

    #[test]
    fn project_files() {
        assert!(is_project_file(Path::new("apps/web/vite.config.ts")));
        assert!(is_project_file(Path::new(".env.local")));
        assert!(!is_project_file(Path::new(
            "node_modules/vite/package.json"
        )));
        assert!(!is_project_file(Path::new(".svelte-kit/package.json")));
        assert!(!is_project_file(Path::new("src/main.rs")));
    }

    #[test]
    fn picks_the_first_when_none_answers() {
        let found = vec![DevServer::new("Vite", 1), DevServer::new("Next.js", 2)];
        assert_eq!(pick(&found).as_deref(), Some("http://localhost:1"));
        assert_eq!(pick(&[]), None);
    }
}
