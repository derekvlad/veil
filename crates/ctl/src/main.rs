//! `veilctl` — control client for `veild`.

use veil::config::Mode;
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
        "status" => Request::Status,
        "pause" => Request::Pause,
        "resume" => Request::Resume,
        "toggle" => Request::Toggle,
        "reload" => Request::Reload,
        "quit" => Request::Quit,
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
