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
