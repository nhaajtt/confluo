//! `confluo`: the download engine without a window. Used by scripts, tests and benchmarks.

use std::io::Write;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::{Duration, Instant};

use confluo_engine::limit::Limiter;
use confluo_engine::routes::{RouteBook, Selection, watch_routes};
use confluo_engine::{Checksum, Command, Config, Outcome, Route, Snapshot, Spec, download};
use tokio::sync::watch;
use url::Url;

const USAGE: &str = "\
confluo - multi-network download engine

USAGE:
  confluo get <url> [mirror-url...] [options]
  confluo interfaces

OPTIONS (get):
  -o, --dir <path>        folder to save into (default: current folder)
  -n, --name <file>       file name (default: what the server suggests)
  -i, --interface <name>  use this network interface; repeat for several
      --system            let the operating system choose the route
      --emulate-links <r,r>  pretend to have networks of these speeds (e.g. 4m,1m); for testing
  -c, --connections <n>   most connections per network (default 32)
  -l, --limit <rate>      speed limit, e.g. 500k, 2m (bytes per second)
      --checksum <hash>   expected sha256 / sha1 / md5, optionally prefixed (sha256:...)
  -H, --header <k: v>     extra request header; repeat for several
      --give-up <secs>    fail after this long without progress (default: keep trying)
      --json              print one JSON line per progress update and a final result line
  -q, --quiet             no progress output
";

struct Args {
    urls: Vec<Url>,
    dir: PathBuf,
    name: Option<String>,
    interfaces: Vec<String>,
    system: bool,
    /// Speeds of pretend networks, for testing the scheduler on one machine.
    emulate: Vec<u64>,
    connections: Option<usize>,
    limit: u64,
    checksum: Option<Checksum>,
    headers: Vec<(String, String)>,
    give_up: Option<Duration>,
    json: bool,
    quiet: bool,
}

fn parse_rate(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_lowercase();
    let (num, mul) = match s.chars().last()? {
        'k' => (&s[..s.len() - 1], 1024.0),
        'm' => (&s[..s.len() - 1], 1024.0 * 1024.0),
        'g' => (&s[..s.len() - 1], 1024.0 * 1024.0 * 1024.0),
        _ => (s.as_str(), 1.0),
    };
    let v: f64 = num.parse().ok()?;
    (v >= 0.0).then_some((v * mul) as u64)
}

fn parse(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let mut a = Args {
        urls: Vec::new(),
        dir: PathBuf::from("."),
        name: None,
        interfaces: Vec::new(),
        system: false,
        emulate: Vec::new(),
        connections: None,
        limit: 0,
        checksum: None,
        headers: Vec::new(),
        give_up: None,
        json: false,
        quiet: false,
    };
    while let Some(arg) = it.next() {
        let mut value = |what: &str| it.next().ok_or_else(|| format!("{what} needs a value"));
        match arg.as_str() {
            "-o" | "--dir" => a.dir = PathBuf::from(value("--dir")?),
            "-n" | "--name" => a.name = Some(value("--name")?),
            "-i" | "--interface" => a.interfaces.push(value("--interface")?),
            "--system" => a.system = true,
            "--emulate-links" => {
                for part in value("--emulate-links")?.split(',') {
                    a.emulate.push(parse_rate(part).filter(|r| *r > 0).ok_or("bad --emulate-links")?);
                }
            }
            "-c" | "--connections" => {
                a.connections = Some(value("--connections")?.parse().map_err(|_| "bad --connections")?);
            }
            "-l" | "--limit" => a.limit = parse_rate(&value("--limit")?).ok_or("bad --limit")?,
            "--checksum" => {
                a.checksum = Some(Checksum::parse(&value("--checksum")?).ok_or("bad --checksum")?)
            }
            "-H" | "--header" => {
                let h = value("--header")?;
                let (k, v) = h.split_once(':').ok_or("header must look like `Name: value`")?;
                a.headers.push((k.trim().to_string(), v.trim().to_string()));
            }
            "--give-up" => {
                a.give_up =
                    Some(Duration::from_secs(value("--give-up")?.parse().map_err(|_| "bad --give-up")?));
            }
            "--json" => a.json = true,
            "-q" | "--quiet" => a.quiet = true,
            s if s.starts_with('-') => return Err(format!("unknown option {s}")),
            s => a.urls.push(Url::parse(s).map_err(|e| format!("{s}: {e}"))?),
        }
    }
    if a.urls.is_empty() {
        return Err("no url given".into());
    }
    Ok(a)
}

fn human(bytes: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{v:.0} {}", UNITS[u]) } else { format!("{v:.1} {}", UNITS[u]) }
}

fn progress_line(s: &Snapshot) -> String {
    let pct = s
        .size
        .filter(|t| *t > 0)
        .map_or_else(|| "  ?".to_string(), |t| format!("{:3.0}", s.done as f64 * 100.0 / t as f64));
    let routes: Vec<String> =
        s.routes.iter().map(|r| format!("{} {}/s x{}", r.label, human(r.rate), r.connections)).collect();
    format!(
        "{pct}%  {} / {}  {}/s  {:?}  [{}]",
        human(s.done as f64),
        s.size.map_or_else(|| "?".into(), |t| human(t as f64)),
        human(s.rate),
        s.verdict,
        routes.join(" | ")
    )
}

fn interfaces() -> ExitCode {
    match confluo_net::list_interfaces() {
        Ok(list) => {
            for i in list {
                println!(
                    "{:<3} {:<34} {:?}{}{}  {}",
                    i.index,
                    i.label,
                    i.medium,
                    if i.up { " up" } else { " down" },
                    if i.has_gateway { " gateway" } else { "" },
                    i.addrs.iter().map(ToString::to_string).collect::<Vec<_>>().join(" ")
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn get(args: Args) -> ExitCode {
    let selection = if args.system {
        Selection::System
    } else if args.interfaces.is_empty() {
        Selection::Auto
    } else {
        Selection::Only(args.interfaces.clone())
    };
    let (_sel_tx, sel_rx) = watch::channel(selection);
    let (_emu_tx, emu_rx) = watch::channel(
        args.emulate
            .iter()
            .enumerate()
            .map(|(i, rate)| Route {
                id: format!("link{}", i + 1),
                label: format!("link {} ({}/s)", i + 1, human(*rate as f64)),
                bind: None,
                limiter: Arc::new(Limiter::new(*rate)),
                uplink: None,
            })
            .collect::<Vec<_>>(),
    );
    let routes =
        if args.emulate.is_empty() { watch_routes(Arc::new(RouteBook::default()), sel_rx) } else { emu_rx };
    let (cmd_tx, cmd_rx) = watch::channel(Command::Run);
    tokio::spawn(async move {
        // Ctrl-C pauses: the partial file and its journal stay for the next run.
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = cmd_tx.send(Command::Pause);
        }
        std::future::pending::<()>().await;
    });
    let mut cfg = Config {
        give_up_after: args.give_up,
        limiter: Arc::new(Limiter::new(args.limit)),
        ..Config::default()
    };
    if let Some(n) = args.connections {
        cfg.conns_max = n.max(1);
        cfg.conns_initial = cfg.conns_initial.min(cfg.conns_max);
        cfg.conns_total = cfg.conns_total.max(cfg.conns_max);
    }
    let spec = Spec {
        urls: args.urls,
        dir: args.dir,
        file_name: args.name,
        headers: args.headers,
        checksum: args.checksum,
    };
    let started = Instant::now();
    let (json, quiet) = (args.json, args.quiet);
    let mut last_print = Instant::now();
    let mut peak_conns = 0usize;
    let mut on_snapshot = |s: Snapshot| {
        peak_conns = peak_conns.max(s.connections);
        if quiet {
            return;
        }
        if json {
            println!("{}", serde_json::to_string(&s).unwrap_or_default());
        } else if last_print.elapsed() >= Duration::from_millis(500) {
            last_print = Instant::now();
            eprint!("\r\x1b[2K{}", progress_line(&s));
            let _ = std::io::stderr().flush();
        }
    };
    let result = download(spec, cfg, routes, cmd_rx, &mut on_snapshot).await;
    let secs = started.elapsed().as_secs_f64();
    if !quiet && !json {
        eprintln!();
    }
    match result {
        Ok(Outcome::Completed { path, size }) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"result": "completed", "path": path, "size": size, "seconds": secs,
                        "bytes_per_sec": size as f64 / secs.max(1e-9), "peak_connections": peak_conns})
                );
            } else {
                println!(
                    "{}  {} in {secs:.2}s ({}/s)",
                    path.display(),
                    human(size as f64),
                    human(size as f64 / secs.max(1e-9))
                );
            }
            ExitCode::SUCCESS
        }
        Ok(Outcome::Paused) => {
            if json {
                println!("{}", serde_json::json!({"result": "paused", "seconds": secs}));
            } else {
                println!("paused; run the same command again to continue");
            }
            ExitCode::from(3)
        }
        Ok(Outcome::Cancelled) => ExitCode::from(4),
        Err(e) => {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"result": "error", "error": e.to_string(), "seconds": secs})
                );
            } else {
                eprintln!("error: {e}");
            }
            ExitCode::FAILURE
        }
    }
}

fn main() -> ExitCode {
    let mut argv = std::env::args().skip(1);
    match argv.next().as_deref() {
        Some("interfaces") => interfaces(),
        Some("get") => match parse(argv) {
            Ok(args) => {
                let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("runtime");
                rt.block_on(get(args))
            }
            Err(e) => {
                eprintln!("error: {e}\n\n{USAGE}");
                ExitCode::from(2)
            }
        },
        _ => {
            print!("{USAGE}");
            ExitCode::from(2)
        }
    }
}
