//! `veilctl` — control client for `veild`.

use veil::catalog::{self, Catalog};
use veil::config::{Config, Mode};
use veil::ipc::{self, Request};

fn usage() {
    println!(
        "veilctl {} — control the veil wallpaper daemon\n\
         \n\
         Usage:\n\
         \x20 veilctl status                     show monitors, decoder, pause state\n\
         \x20 veilctl pause | resume | toggle    pause or resume playback\n\
         \x20 veilctl set <file> [--mode M]      set the wallpaper\n\
         \x20 veilctl set <file> --output DP-1   per-monitor wallpaper\n\
         \x20 veilctl next [--output DP-1]       next playlist entry\n\
         \x20 veilctl random [--output DP-1]     random playlist entry\n\
         \x20 veilctl clear [--output DP-1]      drop per-monitor overrides\n\
         \x20 veilctl span on|off|toggle         spread one picture over all monitors\n\
         \x20 veilctl playlist on|off|toggle     start or stop the rotation\n\
         \x20 veilctl install <id|url> [--sha256 HEX] [--no-apply]\n\
         \x20                                    download from the workshop\n\
         \x20 veilctl catalog [query] [--url URL]\n\
         \x20 veilctl engine list|status             Wallpaper Engine content\n\
         \x20 veilctl engine apply <id> [--output DP-1]\n\
         \x20 veilctl engine props <id>              scene settings\n\
         \x20 veilctl engine set <id> <key> <value>  change a scene setting\n\
         \x20 veilctl engine unset <id> [key]        drop scene overrides\n\
         \x20 veilctl widgets [list]                desktop widgets from [[widgets]]\n\
         \x20                                    list the workshop catalogue\n\
         \x20 veilctl reload                     re-read the config file\n\
         \x20 veilctl quit                       stop the daemon\n\
         \n\
         Modes: fill, fit, stretch, center",
        env!("CARGO_PKG_VERSION")
    );
}

/// Read an optional `--output NAME` pair and return it with the next index.
fn output_arg(args: &[String], mut index: usize) -> (Option<String>, usize) {
    match args.get(index).map(|arg| arg.as_str()) {
        Some("--output" | "-o") => {
            index += 1;
            let output = args.get(index).cloned();
            if output.is_none() {
                eprintln!("veilctl: `--output` needs a monitor name");
                std::process::exit(2);
            }
            (output, index + 1)
        }
        _ => (None, index),
    }
}

fn switch_flag(value: Option<&String>, current: bool) -> bool {
    match value.map(|value| value.as_str()) {
        Some("on" | "true" | "yes" | "1") => true,
        Some("off" | "false" | "no" | "0") => false,
        Some("toggle") => !current,
        Some(other) => {
            eprintln!("veilctl: expected on, off or toggle, got `{other}`");
            std::process::exit(2);
        }
        None => {
            eprintln!("veilctl: this command needs on, off or toggle");
            std::process::exit(2);
        }
    }
}

/// Edit the config file, then ask the daemon to re-read it.
fn update_config(change: impl FnOnce(&mut Config)) {
    let (mut cfg, path) = match Config::load() {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!("veilctl: {error}");
            std::process::exit(1);
        }
    };
    change(&mut cfg);
    if let Err(error) = cfg.save_to(&path) {
        eprintln!("veilctl: {error}");
        std::process::exit(1);
    }
    match ipc::call(&Request::Reload) {
        Ok(_) => println!("{}", path.display()),
        Err(_) => println!("{} (saved; the daemon is not running)", path.display()),
    }
}

fn human_size(bytes: u64) -> String {
    if bytes == 0 {
        return "—".into();
    }
    let mb = bytes as f64 / (1024.0 * 1024.0);
    if mb >= 1024.0 {
        format!("{:.1} GiB", mb / 1024.0)
    } else {
        format!("{mb:.1} MiB")
    }
}

fn install(args: &[String]) {
    let Some(reference) = args.get(1) else {
        eprintln!("veilctl: `install` needs an id from the catalogue or an http(s) URL");
        std::process::exit(2);
    };
    let mut sha256: Option<String> = None;
    let mut apply = true;
    let mut output: Option<String> = None;
    let mut index = 2;
    while index < args.len() {
        match args[index].as_str() {
            "--sha256" => {
                sha256 = args.get(index + 1).cloned();
                if sha256.is_none() {
                    eprintln!("veilctl: `--sha256` needs a hex digest");
                    std::process::exit(2);
                }
                index += 2;
            }
            "--no-apply" => {
                apply = false;
                index += 1;
            }
            "--output" | "-o" => {
                output = args.get(index + 1).cloned();
                index += 2;
            }
            other => {
                eprintln!("veilctl: unknown option `{other}`");
                std::process::exit(2);
            }
        }
    }

    let (cfg, _) = match Config::load() {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!("veilctl: {error}");
            std::process::exit(1);
        }
    };
    let dir = cfg.library_path();
    let direct = reference.starts_with("https://") || reference.starts_with("http://");
    println!("→ {reference}");

    if direct && sha256.is_none() {
        eprintln!("warning: no --sha256 given, the download is not verified");
    }
    let result = if direct {
        catalog::install(reference, sha256.as_deref(), &dir)
    } else {
        catalog::install_reference(reference, &cfg.catalog.url, &dir)
    };

    let path = match result {
        Ok(path) => path,
        Err(error) => {
            eprintln!("veilctl: {error}");
            std::process::exit(1);
        }
    };
    println!("{}", path.display());

    if apply {
        let request = Request::Set {
            path: Some(path.to_string_lossy().into_owned()),
            mode: None,
            output: output.clone(),
            clear_outputs: false,
        };
        match ipc::call(&request) {
            Ok(_) => println!("applied"),
            Err(_) => println!("saved; the daemon is not running"),
        }
    }
}

fn catalog_list(args: &[String]) {
    let mut query = String::new();
    let mut url: Option<String> = None;
    let mut index = 1;
    while index < args.len() {
        match args[index].as_str() {
            "--url" => {
                url = args.get(index + 1).cloned();
                if url.is_none() {
                    eprintln!("veilctl: `--url` needs a URL");
                    std::process::exit(2);
                }
                index += 2;
            }
            other if other.starts_with('-') => {
                eprintln!("veilctl: unknown option `{other}`");
                std::process::exit(2);
            }
            other => {
                query = other.to_string();
                index += 1;
            }
        }
    }

    let (mut cfg, path) = match Config::load() {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!("veilctl: {error}");
            std::process::exit(1);
        }
    };
    if let Some(url) = url {
        cfg.catalog.url = url;
        if let Err(error) = cfg.save_to(&path) {
            eprintln!("veilctl: {error}");
            std::process::exit(1);
        }
        println!("{}", path.display());
    }
    if cfg.catalog.url.is_empty() {
        eprintln!("veilctl: no catalogue URL; pass `--url <manifest.json>` once");
        std::process::exit(2);
    }

    let text = match catalog::fetch_text(&cfg.catalog.url) {
        Ok(text) => text,
        Err(error) => {
            eprintln!("veilctl: {error}");
            std::process::exit(1);
        }
    };
    let catalog = match Catalog::parse(&text) {
        Ok(catalog) => catalog,
        Err(error) => {
            eprintln!("veilctl: {error}");
            std::process::exit(1);
        }
    };

    let entries = catalog.search(&query);
    if entries.is_empty() {
        println!("nothing matches `{query}`");
        return;
    }
    for entry in entries {
        let tags = if entry.tags.is_empty() {
            String::new()
        } else {
            format!("  [{}]", entry.tags.join(", "))
        };
        println!(
            "{:<24} {:<32} {:<16} {}{}",
            entry.id,
            entry.title,
            entry.author,
            human_size(entry.size),
            tags
        );
    }
    println!("\n{} entries in {}", catalog.entries.len(), cfg.catalog.url);
}

fn engine_command(args: &[String]) {
    use veil::engine;

    match args.get(1).map(String::as_str) {
        Some("apply") => {
            let Some(reference) = args.get(2) else {
                eprintln!("veilctl: `engine apply` needs a workshop id");
                std::process::exit(2);
            };
            let id = engine::marker_id(reference)
                .unwrap_or(reference)
                .to_string();
            let mut output: Option<String> = None;
            let mut index = 3;
            while index < args.len() {
                match args[index].as_str() {
                    "--output" | "-o" => {
                        output = args.get(index + 1).cloned();
                        index += 2;
                    }
                    other => {
                        eprintln!("veilctl: unknown option `{other}`");
                        std::process::exit(2);
                    }
                }
            }
            match engine::find(&id) {
                Some(item) if item.supported => {}
                Some(item) => {
                    eprintln!(
                        "veilctl: `{}` is a `{}` wallpaper; linux-wallpaperengine cannot draw it",
                        item.title, item.kind
                    );
                    std::process::exit(1);
                }
                None => {
                    eprintln!(
                        "veilctl: no wallpaper {id} in the Steam workshop folder; subscribe to it in Wallpaper Engine first"
                    );
                    std::process::exit(1);
                }
            }
            let request = Request::Set {
                path: Some(format!("{}{id}", engine::MARKER)),
                mode: None,
                output,
                clear_outputs: false,
            };
            match ipc::call(&request) {
                Ok(_) => println!("{}{id}", engine::MARKER),
                Err(error) => {
                    eprintln!("veilctl: {error}");
                    std::process::exit(1);
                }
            }
        }
        Some("props") => {
            let (cfg, item) = scene_from_args(args, 2, "props");
            let properties = match scene_properties(&cfg, &item) {
                Ok(properties) => properties,
                Err(error) => {
                    eprintln!("veilctl: {error}");
                    std::process::exit(1);
                }
            };
            println!("{} ({})", item.title, item.id);
            for property in &properties {
                let override_value = cfg.engine.properties.get(&property.key);
                let value = override_value.unwrap_or(&property.value);
                println!(
                    "  {:<24} {:<8} {}{}",
                    property.key,
                    property_kind_name(property.kind),
                    value,
                    if override_value.is_some() {
                        "  (set by you)"
                    } else {
                        ""
                    }
                );
                if !property.label.trim().is_empty() && property.title() != property.key {
                    println!("  {:<24} {}", "", property.title());
                }
                for (key, label) in &property.options {
                    println!("  {:<24} {key} = {label}", "");
                }
            }
            if !cfg.engine.properties.is_empty() {
                println!();
                println!("overrides in the config (they go to every scene):");
                for (key, value) in &cfg.engine.properties {
                    let known = properties.iter().any(|property| &property.key == key);
                    println!(
                        "  {key} = {value}{}",
                        if known {
                            ""
                        } else {
                            "   (this scene has no such setting)"
                        }
                    );
                }
            }
            println!();
            println!("change one:  veilctl engine set {} <key> <value>", item.id);
        }
        Some("set") => {
            let (mut cfg, item) = scene_from_args(args, 2, "set");
            let Some(key) = args.get(3) else {
                eprintln!("veilctl: `engine set` needs a key and a value");
                std::process::exit(2);
            };
            let value: String = args[4..].join(" ");
            if value.trim().is_empty() {
                eprintln!("veilctl: `engine set` needs a value after `{key}`");
                std::process::exit(2);
            }
            let properties = match scene_properties(&cfg, &item) {
                Ok(properties) => properties,
                Err(error) => {
                    eprintln!("veilctl: {error}");
                    std::process::exit(1);
                }
            };
            let Some(property) = properties.iter().find(|item| &item.key == key) else {
                eprintln!("veilctl: this scene has no setting `{key}`");
                std::process::exit(1);
            };
            let value = match property.normalize(&value) {
                Ok(value) => value,
                Err(error) => {
                    eprintln!("veilctl: {error}");
                    std::process::exit(2);
                }
            };
            if value == property.default_value() {
                cfg.engine.properties.remove(key);
            } else {
                cfg.engine.properties.insert(key.clone(), value.clone());
            }
            let (_, path) = match Config::load() {
                Ok(loaded) => loaded,
                Err(error) => {
                    eprintln!("veilctl: {error}");
                    std::process::exit(1);
                }
            };
            if let Err(error) = cfg.save_to(&path) {
                eprintln!("veilctl: {error}");
                std::process::exit(1);
            }
            println!("{key} = {value}");
            reload_daemon(key);
        }
        Some("unset") => {
            let (mut cfg, item) = scene_from_args(args, 2, "unset");
            let (_, path) = match Config::load() {
                Ok(loaded) => loaded,
                Err(error) => {
                    eprintln!("veilctl: {error}");
                    std::process::exit(1);
                }
            };
            match args.get(3) {
                Some(key) => {
                    cfg.engine.properties.remove(key);
                    println!("dropped override `{key}`");
                }
                None => {
                    let properties = scene_properties(&cfg, &item).unwrap_or_default();
                    for property in &properties {
                        cfg.engine.properties.remove(&property.key);
                    }
                    println!("dropped every override of scene {}", item.id);
                }
            }
            if let Err(error) = cfg.save_to(&path) {
                eprintln!("veilctl: {error}");
                std::process::exit(1);
            }
            reload_daemon("");
        }
        Some("status") => engine_status(),
        Some("list") | None => {
            let query = args.get(2).map(|s| s.to_lowercase()).unwrap_or_default();
            let items = engine::scan_workshop();
            if items.is_empty() {
                println!(
                    "no Wallpaper Engine wallpapers found; install Wallpaper Engine in Steam and subscribe to some"
                );
                return;
            }
            for item in items
                .iter()
                .filter(|item| query.is_empty() || item.title.to_lowercase().contains(&query))
            {
                let status = if item.supported {
                    ""
                } else {
                    "  (unsupported)"
                };
                println!(
                    "{:<12} {:<40} {:<8}{status}",
                    item.id, item.title, item.kind
                );
            }
            println!("\n{} wallpapers", items.len());
        }
        Some(other) => {
            eprintln!("veilctl: unknown `engine` command `{other}`");
            std::process::exit(2);
        }
    }
}

/// Resolve `<id>` and load the config for one scene command.
fn scene_from_args(args: &[String], index: usize, command: &str) -> (Config, veil::engine::Item) {
    let Some(reference) = args.get(index) else {
        eprintln!("veilctl: `engine {command}` needs a workshop id");
        std::process::exit(2);
    };
    let id = veil::engine::marker_id(reference).unwrap_or(reference);
    let Some(item) = veil::engine::find(id) else {
        eprintln!("veilctl: no wallpaper {id} in the Steam workshop folder");
        std::process::exit(1);
    };
    let (cfg, _) = match Config::load() {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!("veilctl: {error}");
            std::process::exit(1);
        }
    };
    (cfg, item)
}

/// Ask the engine which settings the scene declares.
fn scene_properties(
    cfg: &Config,
    item: &veil::engine::Item,
) -> Result<Vec<veil::engine::Property>, String> {
    let Some(binary) = veil::engine::binary(&cfg.engine.binary) else {
        return Err("linux-wallpaperengine is not installed".into());
    };
    let assets = if cfg.engine.assets.trim().is_empty() {
        veil::engine::assets_dir()
    } else {
        Some(std::path::PathBuf::from(veil::config::expand(
            &cfg.engine.assets,
        )))
    };
    let mut command = std::process::Command::new(binary.clone());
    if let Some(assets) = assets {
        command.arg("--assets-dir").arg(assets);
    }
    // A web wallpaper starts Chromium inside the engine; without libcef ahead
    // of libc it dies with "close symbol missing" (upstream issue #628).
    if item.kind == "web" {
        if let Some(libcef) = veil::engine::libcef(&binary, &cfg.engine.preload) {
            command.env("LD_PRELOAD", libcef);
        }
    }
    command.arg("--list-properties").arg(&item.dir);
    let output = command
        .output()
        .map_err(|error| format!("{}: {error}", item.dir.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let last = stderr.lines().last().unwrap_or_default();
        return Err(format!("{}: {last}", item.dir.display()));
    }
    Ok(veil::engine::parse_properties(&String::from_utf8_lossy(
        &output.stdout,
    )))
}

fn property_kind_name(kind: veil::engine::PropertyKind) -> &'static str {
    use veil::engine::PropertyKind;
    match kind {
        PropertyKind::Boolean => "boolean",
        PropertyKind::Combo => "combo",
        PropertyKind::Slider => "slider",
        PropertyKind::Color => "color",
        PropertyKind::Text => "text",
        PropertyKind::File => "file",
        PropertyKind::SceneTexture => "texture",
        PropertyKind::Unknown => "other",
    }
}

fn reload_daemon(what: &str) {
    match ipc::call(&Request::Reload) {
        Ok(_) => {
            if !what.is_empty() {
                println!("applied to the running daemon");
            }
        }
        Err(_) => println!("saved; the daemon is not running, it reads the file on start"),
    }
}

fn engine_status() {
    use veil::engine;

    let (cfg, _) = match Config::load() {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!("veilctl: {error}");
            std::process::exit(1);
        }
    };
    let binary = engine::binary(&cfg.engine.binary);
    let assets = if cfg.engine.assets.trim().is_empty() {
        engine::assets_dir()
    } else {
        Some(std::path::PathBuf::from(veil::config::expand(
            &cfg.engine.assets,
        )))
    };
    let items = engine::scan_workshop();
    let supported = items.iter().filter(|item| item.supported).count();
    let pid = ipc::call(&Request::Status)
        .ok()
        .and_then(|value| {
            value
                .get("engine")
                .and_then(|engine| engine.get("pid"))
                .cloned()
        })
        .and_then(|value| value.as_u64());

    println!(
        "binary:  {}",
        binary
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "not found (install linux-wallpaperengine-git)".into())
    );
    println!(
        "assets:  {}",
        assets
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "not found (install Wallpaper Engine in Steam)".into())
    );
    println!("items:   {} ({supported} supported)", items.len());
    println!(
        "running: {}",
        pid.map(|pid| format!("pid {pid}"))
            .unwrap_or_else(|| "no".into())
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first() else {
        usage();
        return;
    };

    let request = match command.as_str() {
        "status" => Request::Status,
        "pause" => Request::Pause,
        "resume" => Request::Resume,
        "toggle" => Request::Toggle,
        "reload" => Request::Reload,
        "quit" => Request::Quit,
        "next" => {
            let (output, _) = output_arg(&args, 1);
            Request::Next { output }
        }
        "random" => {
            let (output, _) = output_arg(&args, 1);
            Request::Random { output }
        }
        "clear" => {
            let (output, _) = output_arg(&args, 1);
            Request::Clear { output }
        }
        "span" => {
            let enabled = switch_flag(
                args.get(1),
                Config::load()
                    .map(|(cfg, _)| cfg.wallpaper.span)
                    .unwrap_or(false),
            );
            update_config(|cfg| cfg.wallpaper.span = enabled);
            return;
        }
        "playlist" => {
            let enabled = switch_flag(
                args.get(1),
                Config::load()
                    .map(|(cfg, _)| cfg.playlist.enabled)
                    .unwrap_or(false),
            );
            update_config(|cfg| cfg.playlist.enabled = enabled);
            return;
        }
        "install" => {
            install(&args);
            return;
        }
        "catalog" => {
            catalog_list(&args);
            return;
        }
        "engine" => {
            engine_command(&args);
            return;
        }
        "widgets" => {
            widgets_command();
            return;
        }
        "set" => {
            let Some(path) = args.get(1) else {
                eprintln!("veilctl: `set` needs a file path");
                std::process::exit(2);
            };
            let mut mode: Option<Mode> = None;
            let mut output: Option<String> = None;
            let mut index = 2;
            while index < args.len() {
                match args[index].as_str() {
                    "--mode" | "-m" => {
                        let Some(value) = args.get(index + 1) else {
                            eprintln!("veilctl: `--mode` needs a value");
                            std::process::exit(2);
                        };
                        mode = match value.parse() {
                            Ok(mode) => Some(mode),
                            Err(error) => {
                                eprintln!("veilctl: {error}");
                                std::process::exit(2);
                            }
                        };
                        index += 2;
                    }
                    "--output" | "-o" => {
                        output = args.get(index + 1).cloned();
                        index += 2;
                    }
                    other => {
                        eprintln!("veilctl: unknown option `{other}`");
                        std::process::exit(2);
                    }
                }
            }
            Request::Set {
                path: Some(path.clone()),
                mode,
                output,
                clear_outputs: false,
            }
        }
        "-h" | "--help" | "help" => {
            usage();
            return;
        }
        other => {
            eprintln!("veilctl: unknown command `{other}` (try --help)");
            std::process::exit(2);
        }
    };

    match ipc::call(&request) {
        Ok(response) => pretty(&response),
        Err(error) => {
            eprintln!("veilctl: {error}");
            std::process::exit(1);
        }
    }
}

fn pretty(response: &serde_json::Value) {
    match serde_json::to_string_pretty(response) {
        Ok(text) => println!("{text}"),
        Err(_) => println!("{response}"),
    }
}

/// `veilctl widgets`: what the config asks for and whether a renderer is up.
fn widgets_command() {
    let (cfg, path) = match Config::load() {
        Ok(loaded) => loaded,
        Err(error) => {
            eprintln!("veilctl: {error}");
            std::process::exit(1);
        }
    };
    let enabled = cfg.widgets.iter().filter(|widget| widget.enabled).count();
    println!("config:  {}", path.display());
    println!("widgets: {} ({enabled} enabled)", cfg.widgets.len());
    for widget in &cfg.widgets {
        let state = if widget.enabled { "" } else { " (off)" };
        println!("  {}{state}", widget.summary());
    }
    let running = std::process::Command::new("pgrep")
        .args(["-f", "veil-widgets"])
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false);
    println!("running: {}", if running { "yes" } else { "no" });
    if enabled > 0 && !running {
        println!("hint:   start it with `veil-widgets &` or add it to the compositor autostart");
    }
}
