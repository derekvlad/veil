//! `veilctl` — control client for `veild`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;

fn usage() {
    println!(
        "veilctl {} — control the veil wallpaper daemon\n\
         \n\
         Usage:\n\
         \x20 veilctl status                     show monitors, decoder, pause state\n\
         \x20 veilctl pause | resume | toggle    pause or resume playback\n\
         \x20 veilctl set <file> [--mode M]      set the wallpaper\n\
         \x20 veilctl set <file> --output DP-1   per-monitor wallpaper\n\
         \x20 veilctl reload                     re-read the config file\n\
         \x20 veilctl quit                       stop the daemon\n\
         \n\
         Modes: fill, fit, stretch, center",
        env!("CARGO_PKG_VERSION")
    );
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first() else {
        usage();
        return;
    };

    let request = match command.as_str() {
        "status" => serde_json::json!({ "cmd": "status" }),
        "pause" => serde_json::json!({ "cmd": "pause" }),
        "resume" => serde_json::json!({ "cmd": "resume" }),
        "toggle" => serde_json::json!({ "cmd": "toggle" }),
        "reload" => serde_json::json!({ "cmd": "reload" }),
        "quit" => serde_json::json!({ "cmd": "quit" }),
        "set" => {
            let Some(path) = args.get(1) else {
                eprintln!("veilctl: `set` needs a file path");
                std::process::exit(2);
            };
            let mut mode: Option<String> = None;
            let mut output: Option<String> = None;
            let mut index = 2;
            while index < args.len() {
                match args[index].as_str() {
                    "--mode" | "-m" => {
                        mode = args.get(index + 1).cloned();
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
            serde_json::json!({
                "cmd": "set",
                "path": path,
                "mode": mode,
                "output": output,
            })
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

    match send(&request.to_string()) {
        Ok(response) => pretty(&response),
        Err(error) => {
            eprintln!("veilctl: {error}");
            std::process::exit(1);
        }
    }
}

fn send(request: &str) -> Result<String, String> {
    let path = veil::ipc::socket_path();
    let mut stream = UnixStream::connect(&path)
        .map_err(|error| format!("{}: {error} (is veild running?)", path.display()))?;
    writeln!(stream, "{request}").map_err(|error| error.to_string())?;
    let mut line = String::new();
    BufReader::new(stream)
        .read_line(&mut line)
        .map_err(|error| error.to_string())?;
    if line.trim().is_empty() {
        return Err("daemon closed the connection".into());
    }
    Ok(line.trim().to_string())
}

fn pretty(response: &str) {
    match serde_json::from_str::<serde_json::Value>(response) {
        Ok(value) => match serde_json::to_string_pretty(&value) {
            Ok(text) => println!("{text}"),
            Err(_) => println!("{response}"),
        },
        Err(_) => println!("{response}"),
    }
}
