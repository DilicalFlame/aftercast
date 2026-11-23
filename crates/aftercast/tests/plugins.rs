//! Overlay plugins from the command line (specs/2026-11-23-overlay-plugins.md, section 3):
//! `overlay new` makes a project that validates, `plugin add` installs it from a folder (copied
//! or linked) and from a zip, registers it in the config, `plugin list` shows it, a manifest
//! with a wrong API is refused, and `plugin remove` takes it and its settings away.

use std::fs;
use std::path::Path;
use std::process::Command;

fn run(home: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_aftercast"))
        .arg("--home")
        .arg(home)
        .args(args)
        .output()
        .unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}

fn ok(home: &Path, args: &[&str]) -> String {
    let (good, text) = run(home, args);
    assert!(good, "{args:?}\n{text}");
    text
}

#[test]
fn a_new_overlay_installs_lists_and_goes() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    ok(&home, &["init"]);

    // A plain project and a Svelte one both validate (the Svelte entry is a build away).
    let plain = tmp.path().join("goal-bar");
    ok(
        &home,
        &[
            "overlay",
            "new",
            "goal-bar",
            "--dir",
            plain.to_str().unwrap(),
        ],
    );
    for file in [
        "overlay.toml",
        "index.html",
        "main.js",
        "style.css",
        "aftercast.d.ts",
    ] {
        assert!(plain.join(file).is_file(), "{file}");
    }
    let svelte = tmp.path().join("sv");
    ok(
        &home,
        &[
            "overlay",
            "new",
            "sv",
            "--template",
            "svelte",
            "--dir",
            svelte.to_str().unwrap(),
        ],
    );
    assert!(svelte.join("src/App.svelte").is_file());
    let x = tmp.path().join("x");
    let (good, _) = run(
        &home,
        &["overlay", "new", "Bad Id", "--dir", x.to_str().unwrap()],
    );
    assert!(!good, "an id that is not an id");
    let (good, _) = run(
        &home,
        &[
            "overlay",
            "new",
            "goal-bar",
            "--dir",
            plain.to_str().unwrap(),
        ],
    );
    assert!(!good, "a folder that is not empty");

    ok(&home, &["plugin", "check", plain.to_str().unwrap()]);

    // Copied from a folder: listed, ordered in front, with a settings table.
    ok(&home, &["plugin", "add", plain.to_str().unwrap(), "--yes"]);
    assert!(home.join("plugins/goal-bar/overlay.toml").is_file());
    let list = ok(&home, &["plugin", "list"]);
    assert!(list.contains("goal-bar"), "{list}");
    let config = fs::read_to_string(home.join("aftercast.toml")).unwrap();
    assert!(config.contains("order = [\"goal-bar\"]"), "{config}");
    assert!(config.contains("[plugins.goal-bar]"), "{config}");

    // From a zip with one top folder.
    let zip_path = tmp.path().join("goal.zip");
    {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(fs::File::create(&zip_path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        for file in ["overlay.toml", "index.html", "main.js"] {
            zip.start_file(format!("goal-bar/{file}"), options).unwrap();
            zip.write_all(&fs::read(plain.join(file)).unwrap()).unwrap();
        }
        zip.finish().unwrap();
    }
    fs::remove_dir_all(home.join("plugins/goal-bar")).unwrap();
    ok(
        &home,
        &["plugin", "add", zip_path.to_str().unwrap(), "--yes"],
    );
    assert!(home.join("plugins/goal-bar/main.js").is_file());

    // A plugin for a newer API is refused, and nothing is written for it.
    let newer = tmp.path().join("newer");
    fs::create_dir_all(&newer).unwrap();
    fs::write(newer.join("index.html"), "<html></html>").unwrap();
    fs::write(
        newer.join("overlay.toml"),
        "[overlay]\nid='newer'\nname='N'\napi=9\n",
    )
    .unwrap();
    let (good, text) = run(&home, &["plugin", "add", newer.to_str().unwrap(), "--yes"]);
    assert!(!good && text.contains("API 9"), "{text}");
    assert!(!home.join("plugins/newer").exists());

    // Removed with its settings.
    ok(&home, &["plugin", "remove", "goal-bar", "--yes"]);
    assert!(!home.join("plugins/goal-bar").exists());
    let config = fs::read_to_string(home.join("aftercast.toml")).unwrap();
    assert!(!config.contains("goal-bar"), "{config}");
}

/// Removing a plugin that replaced a system overlay brings the system one back, and the
/// overlay keeps its place in `[overlay] order`.
#[test]
fn removing_a_replacement_keeps_the_system_overlay_in_the_order() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    ok(&home, &["init"]);
    let config = home.join("aftercast.toml");
    let text = fs::read_to_string(&config).unwrap();
    let text = text.replacen(
        "
[overlay]
",
        "
[overlay]
order = [\"now\", \"keys\"]
",
        1,
    );
    fs::write(&config, text).unwrap();
    let project = tmp.path().join("now");
    ok(
        &home,
        &["overlay", "new", "now", "--dir", project.to_str().unwrap()],
    );
    ok(
        &home,
        &["plugin", "add", project.to_str().unwrap(), "--yes"],
    );
    assert!(ok(&home, &["plugin", "list"]).contains("replaces the system one"));
    ok(&home, &["plugin", "remove", "now", "--yes"]);
    let after = fs::read_to_string(&config).unwrap();
    assert!(after.contains(r#"order = ["now", "keys"]"#), "{after}");
    assert!(ok(&home, &["plugin", "list"]).contains("built in"));
}

#[test]
fn a_linked_plugin_is_used_in_place() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    ok(&home, &["init"]);
    let project = tmp.path().join("proj");
    ok(
        &home,
        &[
            "overlay",
            "new",
            "live-bar",
            "--dir",
            project.to_str().unwrap(),
        ],
    );
    ok(
        &home,
        &[
            "plugin",
            "add",
            project.to_str().unwrap(),
            "--link",
            "--yes",
        ],
    );
    assert!(home.join("plugins/live-bar.link").is_file());
    assert!(!home.join("plugins/live-bar").exists());
    assert!(ok(&home, &["plugin", "list"]).contains("live-bar"));
    ok(&home, &["plugin", "remove", "live-bar", "--yes"]);
    assert!(!home.join("plugins/live-bar.link").exists());
    assert!(
        project.join("overlay.toml").is_file(),
        "the project itself is untouched"
    );
}

#[test]
fn signed_packages_registries_and_trust() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    ok(&home, &["init"]);
    let project = tmp.path().join("goal-bar");
    ok(
        &home,
        &[
            "overlay",
            "new",
            "goal-bar",
            "--dir",
            project.to_str().unwrap(),
        ],
    );

    // A publisher makes a key and signs a package.
    let public = ok(&home, &["plugin", "keygen", "maker"]);
    let line = public
        .lines()
        .find(|l| l.trim().starts_with("maker:"))
        .expect("the public line")
        .trim()
        .to_owned();
    let zip = tmp.path().join("goal-bar-0.1.0.zip");
    let packed = ok(
        &home,
        &[
            "plugin",
            "pack",
            project.to_str().unwrap(),
            "--sign",
            "maker",
            "--out",
            zip.to_str().unwrap(),
        ],
    );
    let sha = packed
        .lines()
        .find_map(|l| l.strip_prefix("sha256 "))
        .expect("a digest")
        .trim()
        .to_owned();
    let sig_path = format!("{}.sig.json", zip.display());
    assert!(Path::new(&sig_path).is_file());

    // Someone else (another home) who does not trust the key: an unknown signer is shown, and
    // refused with --require-signed; once the key is trusted it installs.
    let other = tmp.path().join("other");
    ok(&other, &["init"]);
    let text = ok(&other, &["plugin", "add", zip.to_str().unwrap(), "--yes"]);
    assert!(text.contains("not in your trusted_keys"), "{text}");
    ok(&other, &["plugin", "remove", "goal-bar", "--yes"]);
    let (good, text) = run(
        &other,
        &[
            "plugin",
            "add",
            zip.to_str().unwrap(),
            "--yes",
            "--require-signed",
        ],
    );
    assert!(!good && text.contains("not in your trusted_keys"), "{text}");
    let cfg = other.join("aftercast.toml");
    let mut config = fs::read_to_string(&cfg).unwrap();
    config.push_str(&format!(
        "\n[plugin_registry]\ntrusted_keys = [\"{line}\"]\n"
    ));
    fs::write(&cfg, config).unwrap();
    let text = ok(
        &other,
        &[
            "plugin",
            "add",
            zip.to_str().unwrap(),
            "--yes",
            "--require-signed",
        ],
    );
    assert!(text.contains("signed by maker, whom you trust"), "{text}");
    ok(&other, &["plugin", "remove", "goal-bar", "--yes"]);

    // A package changed after it was signed is refused.
    let tampered = tmp.path().join("tampered.zip");
    let mut bytes = fs::read(&zip).unwrap();
    let last = bytes.len() / 2;
    bytes[last] ^= 0xFF;
    fs::write(&tampered, bytes).unwrap();
    fs::copy(&sig_path, format!("{}.sig.json", tampered.display())).unwrap();
    let (good, text) = run(
        &other,
        &["plugin", "add", tampered.to_str().unwrap(), "--yes"],
    );
    assert!(
        !good
            && (text.contains("different package")
                || text.contains("zip")
                || text.contains("signature")),
        "{text}"
    );

    // A registry: a file listing the package with its digest and signature.
    let sig: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&sig_path).unwrap()).unwrap();
    let index = serde_json::json!({ "plugins": [{
        "id": "goal-bar", "name": "Goal bar", "version": "0.1.0", "description": "A donation goal.",
        "source": "goal-bar-0.1.0.zip", "sha256": sha, "key": sig["key"], "signer": "maker", "signature": sig["signature"],
    }]});
    fs::write(tmp.path().join("index.json"), index.to_string()).unwrap();
    let reg = tmp
        .path()
        .join("index.json")
        .display()
        .to_string()
        .replace('\\', "/");
    let mut config = fs::read_to_string(&cfg).unwrap();
    config = config.replace(
        "[plugin_registry]\n",
        &format!("[plugin_registry]\nurls = [\"{reg}\"]\n"),
    );
    fs::write(&cfg, config).unwrap();
    let found = ok(&other, &["plugin", "search", "goal"]);
    assert!(
        found.contains("goal-bar") && found.contains("[signed, trusted]"),
        "{found}"
    );
    let text = ok(&other, &["plugin", "add", "goal-bar", "--yes"]);
    assert!(text.contains("signed by maker"), "{text}");
    assert!(other.join("plugins/goal-bar/overlay.toml").is_file());
    // A wrong digest in the registry is refused.
    ok(&other, &["plugin", "remove", "goal-bar", "--yes"]);
    let bad = index.to_string().replace(&sha, &"0".repeat(64));
    fs::write(tmp.path().join("index.json"), bad).unwrap();
    let (good, text) = run(&other, &["plugin", "add", "goal-bar", "--yes"]);
    assert!(!good && text.contains("digest"), "{text}");
}
