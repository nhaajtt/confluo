//! Runs the fault-injecting file server from the command line (for benchmarks).

use std::net::SocketAddr;
use std::time::Duration;

use confluo_testsrv::{Behavior, Server};

const USAGE: &str = "\
confluo-testsrv - a file server with configurable limits

  --bind <addr:port>     address to listen on (default 127.0.0.1:8700); repeat for several
  --size <bytes>         file size, accepts k/m/g suffix (default 256m)
  --seed <n>             content seed (default 1)
  --rate-conn <bytes/s>  cap per connection
  --rate-ip <bytes/s>    cap per client address
  --rate-total <bytes/s> cap for the whole server
  --max-conns <n>        concurrent responses allowed, others get 503
  --latency <ms>         delay before each response
  --no-ranges            ignore Range requests
  --no-length            do not send Content-Length
";

fn num(s: &str) -> Option<u64> {
    let s = s.to_ascii_lowercase();
    let (n, mul) = match s.chars().last()? {
        'k' => (&s[..s.len() - 1], 1u64 << 10),
        'm' => (&s[..s.len() - 1], 1 << 20),
        'g' => (&s[..s.len() - 1], 1 << 30),
        _ => (s.as_str(), 1),
    };
    n.parse::<f64>().ok().map(|v| (v * mul as f64) as u64)
}

#[tokio::main]
async fn main() {
    let mut b = Behavior { size: 256 << 20, ..Behavior::default() };
    let mut binds: Vec<SocketAddr> = Vec::new();
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        let mut val = || {
            it.next().unwrap_or_else(|| {
                eprintln!("{arg} needs a value\n\n{USAGE}");
                std::process::exit(2)
            })
        };
        let bad = |what: &str| -> ! {
            eprintln!("bad value for {what}\n\n{USAGE}");
            std::process::exit(2)
        };
        match arg.as_str() {
            "--bind" => binds.push(val().parse().unwrap_or_else(|_| bad("--bind"))),
            "--size" => b.size = num(&val()).unwrap_or_else(|| bad("--size")),
            "--seed" => b.seed = val().parse().unwrap_or_else(|_| bad("--seed")),
            "--rate-conn" => b.rate_per_conn = Some(num(&val()).unwrap_or_else(|| bad("--rate-conn"))),
            "--rate-ip" => b.rate_per_ip = Some(num(&val()).unwrap_or_else(|| bad("--rate-ip"))),
            "--rate-total" => b.rate_total = Some(num(&val()).unwrap_or_else(|| bad("--rate-total"))),
            "--max-conns" => b.max_conns = Some(val().parse().unwrap_or_else(|_| bad("--max-conns"))),
            "--latency" => {
                b.latency = Duration::from_millis(val().parse().unwrap_or_else(|_| bad("--latency")))
            }
            "--no-ranges" => b.ranges = false,
            "--no-length" => b.content_length = false,
            _ => {
                print!("{USAGE}");
                std::process::exit(2);
            }
        }
    }
    if binds.is_empty() {
        binds.push(([127, 0, 0, 1], 8700).into());
    }
    let mut servers = Vec::new();
    for bind in binds {
        match Server::start(bind, b.clone()).await {
            Ok(s) => {
                println!("serving {} bytes at {}", b.size, s.url());
                servers.push(s);
            }
            Err(e) => {
                eprintln!("cannot listen on {bind}: {e}");
                std::process::exit(1);
            }
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}
