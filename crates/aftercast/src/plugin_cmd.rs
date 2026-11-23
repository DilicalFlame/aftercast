//! `aftercast overlay new` and `aftercast plugin add|list|remove|check`
//! (specs/2026-11-23-overlay-plugins.md, section 3).

use std::fs;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use ac_core::config::{self, Config, Value};
use ac_core::{Home, exit};
use ac_server::plugins::{self, Plugin};

use crate::cli::Template;
use crate::error::{AppError, Result};
use crate::plugin_pkg;

fn usage<T>(message: impl Into<String>) -> Result<T> {
    Err(AppError::Usage(message.into()))
}

/// `(path in the project, contents)` of a template; `__ID__` and `__NAME__` are filled in.
const PLAIN: &[(&str, &str)] = &[
    (
        "overlay.toml",
        include_str!("../templates/overlay/plain/overlay.toml"),
    ),
    (
        "index.html",
        include_str!("../templates/overlay/plain/index.html"),
    ),
    (
        "main.js",
        include_str!("../templates/overlay/plain/main.js"),
    ),
    (
        "style.css",
        include_str!("../templates/overlay/plain/style.css"),
    ),
    (
        "README.md",
        include_str!("../templates/overlay/plain/README.md"),
    ),
    (
        "aftercast.d.ts",
        include_str!("../templates/overlay/aftercast.d.ts"),
    ),
];
const SVELTE: &[(&str, &str)] = &[
    (
        "overlay.toml",
        include_str!("../templates/overlay/svelte/overlay.toml"),
    ),
    (
        "package.json",
        include_str!("../templates/overlay/svelte/package.json"),
    ),
    (
        "vite.config.js",
        include_str!("../templates/overlay/svelte/vite.config.js"),
    ),
    (
        "index.html",
        include_str!("../templates/overlay/svelte/index.html"),
    ),
    (
        "src/main.js",
        include_str!("../templates/overlay/svelte/src/main.js"),
    ),
    (
        "src/App.svelte",
        include_str!("../templates/overlay/svelte/src/App.svelte"),
    ),
    (
        "README.md",
        include_str!("../templates/overlay/svelte/README.md"),
    ),
    (
        ".gitignore",
        include_str!("../templates/overlay/svelte/.gitignore"),
    ),
    (
        "aftercast.d.ts",
        include_str!("../templates/overlay/aftercast.d.ts"),
    ),
];

/// "goal-bar" as "Goal bar".
fn title(id: &str) -> String {
    let spaced = id.replace(['-', '_'], " ");
    let mut c = spaced.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

/// `aftercast overlay new <id>`: a working project, ready for `plugin add`.
pub fn overlay_new(
    id: &str,
    template: Template,
    dir: Option<PathBuf>,
    name: Option<String>,
) -> Result<u8> {
    if !plugins::id_ok(id) {
        return usage(format!(
            "{id:?} is not a plugin id: 1 to 32 of a-z 0-9 - _, and not a name the host uses"
        ));
    }
    let dir = dir.unwrap_or_else(|| PathBuf::from(id));
    if dir.exists() && fs::read_dir(&dir).is_ok_and(|mut d| d.next().is_some()) {
        return usage(format!("{} is not empty", dir.display()));
    }
    let name = name.unwrap_or_else(|| title(id));
    let files = match template {
        Template::Plain => PLAIN,
        Template::Svelte => SVELTE,
    };
    for (rel, text) in files {
        let path = dir.join(rel);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, text.replace("__ID__", id).replace("__NAME__", &name))?;
        println!("created  {}", path.display());
    }
    // The Svelte project's entry is a build away, so the entry need not exist yet.
    if let Err(e) = Plugin::load_unbuilt(&dir) {
        return usage(format!("the new project does not validate: {e}"));
    }
    println!();
    println!("Next:");
    println!("  cd {}", dir.display());
    if template == Template::Svelte {
        println!("  npm install && npm run build");
    }
    println!("  aftercast plugin add . --link    # use it in place while you edit");
    println!("  aftercast rehearse               # see it in the editor, saves reload it");
    Ok(exit::OK)
}

/// Where a `plugin add` source comes from.
enum Source {
    Folder(PathBuf),
    Zip(PathBuf),
    Git(String),
}

fn classify(source: &str) -> Result<Source> {
    let looks_git = source.starts_with("git@")
        || source.starts_with("ssh://")
        || source.starts_with("git://")
        || source.starts_with("file://")
        || ((source.starts_with("https://") || source.starts_with("http://"))
            && !source.ends_with(".zip"));
    if looks_git {
        return Ok(Source::Git(source.to_owned()));
    }
    let path = PathBuf::from(source);
    if path.is_dir() {
        return Ok(Source::Folder(path));
    }
    if path.is_file() && source.to_ascii_lowercase().ends_with(".zip") {
        return Ok(Source::Zip(path));
    }
    usage(format!(
        "{source}: not a folder, a .zip file or a git URL (https://..., git@...)"
    ))
}

/// A scratch folder under the home that is removed when dropped.
struct Scratch(PathBuf);

impl Scratch {
    fn new(home: &Home) -> Result<Self> {
        let n = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let dir = home.join("state").join(format!("plugin-add-{n}"));
        fs::create_dir_all(&dir)?;
        Ok(Self(dir))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn unzip(zip: &Path, into: &Path) -> Result<()> {
    let file = fs::File::open(zip)?;
    let mut archive = zip::ZipArchive::new(file)
        .map_err(|e| AppError::Usage(format!("{}: {e}", zip.display())))?;
    for i in 0..archive.len() {
        let mut entry = archive
            .by_index(i)
            .map_err(|e| AppError::Usage(format!("{}: {e}", zip.display())))?;
        // Names that would leave the folder (`..`, absolute) are skipped.
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let out = into.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&out)?;
        } else {
            if let Some(parent) = out.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut f = fs::File::create(&out)?;
            std::io::copy(&mut entry, &mut f)?;
        }
    }
    Ok(())
}

/// The folder that holds `overlay.toml`: `root`, or its only subfolder (a zip of a folder,
/// a git repository).
fn manifest_root(root: &Path) -> Result<PathBuf> {
    if root.join("overlay.toml").is_file() {
        return Ok(root.to_path_buf());
    }
    let dirs: Vec<PathBuf> = fs::read_dir(root)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && !p
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with('.'))
        })
        .collect();
    match dirs.as_slice() {
        [one] if one.join("overlay.toml").is_file() => Ok(one.clone()),
        _ => usage("no overlay.toml in the top folder of the source"),
    }
}

/// Copies a folder, leaving out `node_modules`, `.git` and links.
fn copy_dir(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to)?;
    for e in fs::read_dir(from)?.flatten() {
        let name = e.file_name();
        let n = name.to_string_lossy();
        if n == "node_modules" || n == ".git" {
            continue;
        }
        let kind = e.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            copy_dir(&e.path(), &to.join(&name))?;
        } else {
            fs::copy(e.path(), to.join(&name))?;
        }
    }
    Ok(())
}

fn confirm(question: &str) -> Result<bool> {
    print!("{question} [y/N] ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn show(p: &Plugin) {
    println!(
        "{} {} (id {}, API {})",
        p.name,
        p.version,
        p.id,
        plugins::API
    );
    if !p.description.is_empty() {
        println!("  {}", p.description);
    }
    if !p.author.is_empty() {
        println!("  by {}", p.author);
    }
    println!("  permissions:");
    for line in p.permissions() {
        println!("    - {line}");
    }
}

/// `[overlay] order` as the file has it, with `id` in front.
fn order_with(config: &Config, id: &str) -> Vec<String> {
    let mut order: Vec<String> = config
        .overlay
        .order
        .iter()
        .filter(|n| *n != id)
        .cloned()
        .collect();
    order.insert(0, id.to_owned());
    order
}

/// `aftercast plugin add <path | zip | git-url>`.
pub fn plugin_add(
    home: &Home,
    config_path: &Path,
    source: &str,
    yes: bool,
    link: bool,
    require_signed: bool,
) -> Result<u8> {
    let scratch = Scratch::new(home)?;
    let settings = Config::load(config_path).unwrap_or_default();
    // A package (a zip file or URL, or a plugin of a registry) is checked before it is opened.
    let mut trust_line = None;
    let package =
        tokio::runtime::Runtime::new()?.block_on(package_of(source, &settings.plugin_registry))?;
    if let Some((bytes, expected, sig)) = package {
        if link {
            return usage("--link needs a folder, not a package");
        }
        let trust = plugin_pkg::check(
            &bytes,
            expected.as_deref(),
            sig.as_ref(),
            &settings.plugin_registry.trusted_keys,
        )
        .map_err(AppError::Usage)?;
        trust_line = Some(match &trust {
            plugin_pkg::Trust::Trusted(who) => format!("signed by {who}, whom you trust"),
            plugin_pkg::Trust::Unknown(who, fp) => {
                format!("signed by {who:?} (key {fp}), which is not in your trusted_keys")
            }
            plugin_pkg::Trust::Unsigned => "not signed".to_owned(),
        });
        if (require_signed || settings.plugin_registry.require_signed)
            && !matches!(trust, plugin_pkg::Trust::Trusted(_))
        {
            return usage(format!(
                "the package is {}; [plugin_registry] require_signed refuses it",
                trust_line.as_deref().unwrap_or("not signed")
            ));
        }
        let zip_file = scratch.0.join("package.zip");
        fs::write(&zip_file, &bytes)?;
        let into = scratch.0.join("zip");
        unzip(&zip_file, &into)?;
        return finish_add(
            home,
            config_path,
            &into,
            source,
            yes,
            false,
            trust_line,
            &scratch,
        );
    }
    let from = match classify(source)? {
        Source::Folder(dir) => dir,
        Source::Zip(zip) => {
            let into = scratch.0.join("zip");
            unzip(&zip, &into)?;
            into
        }
        Source::Git(url) => {
            if link {
                return usage("--link needs a folder on this computer");
            }
            let into = scratch.0.join("git");
            let out = Command::new("git")
                .args(["clone", "--depth", "1", "--quiet", &url])
                .arg(&into)
                .output()
                .map_err(|e| AppError::Usage(format!("git: {e} (is git installed?)")))?;
            if !out.status.success() {
                return usage(format!(
                    "git clone failed: {}",
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
            into
        }
    };
    if link && !matches!(classify(source)?, Source::Folder(_)) {
        return usage("--link needs a folder, not a zip");
    }
    finish_add(
        home,
        config_path,
        &from,
        source,
        yes,
        link,
        trust_line,
        &scratch,
    )
}

/// The rest of `plugin add`: the folder `from` holds the plugin; show it, ask, install it.
#[allow(clippy::too_many_arguments)]
fn finish_add(
    home: &Home,
    config_path: &Path,
    from: &Path,
    _source: &str,
    yes: bool,
    link: bool,
    trust_line: Option<String>,
    _scratch: &Scratch,
) -> Result<u8> {
    let root = manifest_root(from)?;
    let plugin = Plugin::load(&root).map_err(AppError::Usage)?;
    show(&plugin);
    if let Some(line) = &trust_line {
        println!("  package: {line}");
    }
    println!(
        "  where: {}",
        if link {
            "this folder, in place"
        } else {
            "a copy under <home>/plugins"
        }
    );
    let dir = home.join("plugins");
    let dest = dir.join(&plugin.id);
    let link_file = dir.join(format!("{}.link", plugin.id));
    let existing = dest.exists() || link_file.exists();
    if existing {
        println!(
            "  note: {} is installed already; its files are replaced, its settings kept",
            plugin.id
        );
    }
    if !yes && !confirm("Install it?")? {
        return usage("not installed");
    }
    fs::create_dir_all(&dir)?;
    // Whatever was there before, a folder or a link, goes.
    if dest.exists() {
        fs::remove_dir_all(&dest)?;
    }
    let _ = fs::remove_file(&link_file);
    if link {
        let abs = fs::canonicalize(&root)?;
        // canonicalize on Windows gives \\?\ paths; the plain form reads better in the file.
        let text = abs.to_string_lossy().trim_start_matches(r"\\?\").to_owned();
        fs::write(&link_file, text)?;
    } else {
        copy_dir(&root, &dest)?;
    }
    // Registered in the config: a new overlay goes to the front; settings are kept.
    let config = Config::load(config_path).unwrap_or_default();
    let mut keys: Vec<(String, Option<Value>)> = Vec::new();
    if !plugin.replaces() {
        keys.push((
            "overlay.order".into(),
            Some(Value::List(order_with(&config, &plugin.id))),
        ));
    }
    if !config.plugins.contains_key(&plugin.id) {
        keys.push((
            format!("plugins.{}.enabled", plugin.id),
            Some(Value::Bool(true)),
        ));
    }
    // Settings the overlay kept elsewhere before it was a plugin come along once.
    let mut moved = 0;
    for (from, to, invert) in &plugin.migrate {
        let key = format!("plugins.{}.params.{to}", plugin.id);
        if config::raw_value(config_path, &key).is_some() {
            continue;
        }
        let Some(old) = config::raw_value(config_path, from) else {
            continue;
        };
        let Ok(mut json) = serde_json::to_value(&old) else {
            continue;
        };
        if *invert && let Some(b) = json.as_bool() {
            json = serde_json::Value::Bool(!b);
        }
        if !plugin.accepts(to, &json) {
            continue;
        }
        let value = match json {
            serde_json::Value::Bool(b) => Value::Bool(b),
            serde_json::Value::Number(n) => match n.as_i64() {
                Some(i) => Value::Int(i),
                None => Value::Float(n.as_f64().unwrap_or(0.0)),
            },
            serde_json::Value::String(t) => Value::Text(t),
            serde_json::Value::Array(a) => Value::List(
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect(),
            ),
            _ => continue,
        };
        keys.push((key, Some(value)));
        moved += 1;
    }
    if !keys.is_empty() {
        config::set_values(
            config_path,
            keys.iter().map(|(k, v)| (k.as_str(), v.clone())),
        )
        .map_err(AppError::Core)?;
    }
    if moved > 0 {
        println!("took over {moved} settings from the config of the overlay it replaces");
    }
    println!(
        "installed {} ({})",
        plugin.id,
        if link { "linked" } else { "copied" }
    );
    println!(
        "Open `aftercast rehearse`: it is a layer next to the built-in overlays; right-click a"
    );
    println!(
        "scene to add it to OBS as a Browser source (/overlay/{}).",
        plugin.id
    );
    Ok(exit::OK)
}

/// `aftercast plugin list`.
pub fn plugin_list(home: &Home) -> Result<u8> {
    let dir = home.join("plugins");
    let found = plugins::scan_report(&dir);
    if found.is_empty() {
        println!("no plugins in {}", dir.display());
        println!(
            "make one with `aftercast overlay new <id>`, install one with `aftercast plugin add <source>`"
        );
        return Ok(exit::OK);
    }
    for (path, result) in found {
        match result {
            Ok(p) => println!(
                "{:<20} {:<24} v{:<8} {}{}",
                p.id,
                p.name,
                p.version,
                if p.overrides_system() {
                    "replaces the system one  "
                } else {
                    ""
                },
                path.display()
            ),
            Err(e) => println!(
                "{:<20} not loaded: {e}",
                path.file_name()
                    .map_or(String::new(), |n| n.to_string_lossy().into_owned())
            ),
        }
    }
    Ok(exit::OK)
}

/// `aftercast plugin remove <id>`.
pub fn plugin_remove(home: &Home, config_path: &Path, id: &str, yes: bool) -> Result<u8> {
    let dir = home.join("plugins");
    let dest = dir.join(id);
    let link_file = dir.join(format!("{id}.link"));
    if !plugins::id_ok(id) && !plugins::scan(&dir).iter().any(|p| p.id == id) {
        return usage(format!("{id:?} is not a plugin id"));
    }
    if !dest.exists() && !link_file.exists() {
        return usage(format!("no plugin {id} in {}", dir.display()));
    }
    if !yes && !confirm(&format!("Remove {id} and its settings?"))? {
        return usage("not removed");
    }
    if dest.exists() {
        fs::remove_dir_all(&dest)?;
    }
    let _ = fs::remove_file(&link_file);
    let config = Config::load(config_path).unwrap_or_default();
    // A system overlay stays in the order: removing the plugin that replaced it brings it back.
    let system = ac_server::OVERLAYS.contains(&id);
    let order: Vec<String> = config
        .overlay
        .order
        .iter()
        .filter(|n| system || *n != id)
        .cloned()
        .collect();
    let mut keys: Vec<(String, Option<Value>)> = vec![(format!("plugins.{id}"), None)];
    if order.len() != config.overlay.order.len() {
        keys.push(("overlay.order".into(), Some(Value::List(order))));
    }
    config::set_values(
        config_path,
        keys.iter().map(|(k, v)| (k.as_str(), v.clone())),
    )
    .map_err(AppError::Core)?;
    println!("removed {id}");
    Ok(exit::OK)
}

/// `aftercast plugin check <path>`.
pub fn plugin_check(path: &Path) -> Result<u8> {
    let root = manifest_root(path)?;
    match Plugin::load(&root) {
        Ok(p) => {
            show(&p);
            println!(
                "  {} params, {} skins, {} scenarios: ok",
                p.params.len(),
                p.skins.len(),
                p.scenarios.len()
            );
            Ok(exit::OK)
        }
        Err(e) => usage(e),
    }
}

/// The bytes of a package, what the registry says its digest is, and its signature: for a zip file
/// or URL, and for the id of a plugin in a registry. `None` for anything else (a folder, git).
async fn package_of(
    source: &str,
    registry: &config::PluginRegistry,
) -> Result<Option<(Vec<u8>, Option<String>, Option<plugin_pkg::SigFile>)>> {
    let is_url = source.starts_with("http://")
        || source.starts_with("https://")
        || source.starts_with("file://");
    let is_zip = source.to_ascii_lowercase().ends_with(".zip");
    if is_zip && (is_url || Path::new(source).is_file()) {
        let bytes = plugin_pkg::fetch(source).await?;
        let sig = plugin_pkg::fetch(&format!("{source}.sig.json"))
            .await
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());
        return Ok(Some((bytes, None, sig)));
    }
    if !is_url && !Path::new(source).exists() && plugins::id_ok(source) {
        if registry.urls.is_empty() {
            return usage(format!(
                "{source} is not a folder, a .zip or a git URL, and no registry is set ([plugin_registry] urls)"
            ));
        }
        let entry = plugin_pkg::entries(&registry.urls)
            .await?
            .into_iter()
            .find(|e| e.id == source)
            .ok_or_else(|| AppError::Usage(format!("no plugin {source:?} in the registries")))?;
        if entry.sha256.is_empty() {
            return usage(format!(
                "the registry lists {source} without a sha256; not installing it"
            ));
        }
        let bytes = plugin_pkg::fetch(&entry.source).await?;
        return Ok(Some((bytes, Some(entry.sha256.clone()), entry.sig())));
    }
    Ok(None)
}

/// `aftercast plugin search [query]`.
pub fn plugin_search(config_path: &Path, query: Option<&str>) -> Result<u8> {
    let settings = Config::load(config_path).unwrap_or_default();
    if settings.plugin_registry.urls.is_empty() {
        return usage("no registry: add one under [plugin_registry] urls in aftercast.toml");
    }
    let entries = tokio::runtime::Runtime::new()?
        .block_on(plugin_pkg::entries(&settings.plugin_registry.urls))?;
    let q = query.unwrap_or("").to_ascii_lowercase();
    let mut shown = 0;
    for e in entries.iter().filter(|e| {
        q.is_empty()
            || [&e.id, &e.name, &e.description, &e.author]
                .iter()
                .any(|f| f.to_ascii_lowercase().contains(&q))
    }) {
        let trusted = e.sig().is_some_and(|s| {
            settings
                .plugin_registry
                .trusted_keys
                .iter()
                .any(|t| t.split_once(':').is_some_and(|(_, k)| k.trim() == s.key))
        });
        println!(
            "{:<20} {:<8} {:<28} {}{}",
            e.id,
            e.version,
            e.name,
            if trusted {
                "[signed, trusted] "
            } else if e.sig().is_some() {
                "[signed] "
            } else {
                ""
            },
            e.description
        );
        shown += 1;
    }
    if shown == 0 {
        println!("nothing found");
    } else {
        println!("install one with `aftercast plugin add <id>`");
    }
    Ok(exit::OK)
}

/// `aftercast plugin keygen <name>`.
pub fn plugin_keygen(home: &Home, name: &str) -> Result<u8> {
    let public = plugin_pkg::keygen(home, name)?;
    println!("made a signing key {name:?}; the secret stays in <home>/state/plugin-keys");
    println!("share this line, others put it in [plugin_registry] trusted_keys:");
    println!("  {public}");
    Ok(exit::OK)
}

/// `aftercast plugin pack <dir> [--sign key] [--out file.zip]`.
pub fn plugin_pack(
    home: &Home,
    dir: &Path,
    out: Option<PathBuf>,
    sign: Option<&str>,
) -> Result<u8> {
    let plugin = Plugin::load(dir).map_err(AppError::Usage)?;
    let out = out.unwrap_or_else(|| {
        dir.parent()
            .unwrap_or(Path::new("."))
            .join(format!("{}-{}.zip", plugin.id, plugin.version))
    });
    let sha = plugin_pkg::pack(home, dir, &out, sign)?;
    println!("wrote {}", out.display());
    println!("sha256 {sha}");
    if sign.is_some() {
        println!(
            "signature in {}.sig.json (keep it next to the zip)",
            out.display()
        );
    }
    Ok(exit::OK)
}
