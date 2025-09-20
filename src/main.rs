use anyhow::{Context, Result, bail};
use clap::Parser;
use std::collections::VecDeque;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;
use std::process::Command;

use libc;

/// swap-manager — tiny Rust tool to show / set / empty swap
///
/// This program accepts a sequence of commands, for example:
///   swap-manager set 5G show
///   swap-manager show
///   swap-manager set 512M --replace --persist
///   swap-manager empty
#[derive(Parser, Debug)]
#[command(
    name = "swap-manager",
    version,
    about = "Manage swap: show, set, empty. Commands may be chained.",
    long_about = "Maintained by Mai Bloom Tech Studio

We value transparency and open-source collaboration. With that freedom comes responsibility: please test our tools in safe environments before production use. This product is provided as-is, without warranty of any kind.

Manage swap: show, set, empty. Commands may be chained.

USAGE EXAMPLES:
  swap-manager show
  swap-manager set 5G --replace --persist
  swap-manager set 512M show
  swap-manager set 1G --replace show empty

NOTES:
- This tool manipulates swap devices and files: run as root (sudo).
- Test in a VM/container before using on production systems.
- 'set' creates a swapfile at /swap-manager.swap by default.

FLAGS:
  -h, --help       Print help information
  -V, --version    Print version information

For more usage and examples, run with `--help`."
)]
struct Cli {
    /// Actions and their args (positional; you can chain: set 5G show)
    #[arg(num_args = 1.., trailing_var_arg = true)]
    actions: Vec<String>,
}

fn main() -> Result<()> {
    // Normalize behavior: clap prints a short help for `-h` and a long help for `--help` by default.
    // To make `-h` and `--help` show the same complete guideline, detect `-h` early and print the
    // long help explicitly, then exit. For `--help` clap will handle long help itself.
    use clap::CommandFactory;
    use std::env;

    let raw_args: Vec<String> = env::args().collect();
    if raw_args.iter().any(|a| a == "-h") {
        let mut cmd = Cli::command();
        cmd.print_long_help()?;
        println!();
        return Ok(());
    }

    let cli = Cli::parse();
    if cli.actions.is_empty() {
        println!("No actions provided. Try: swap-manager --help");
        return Ok(());
    }

    // process actions sequentially
    let mut q: VecDeque<String> = cli.actions.into_iter().collect();

    while let Some(tok) = q.pop_front() {
        match tok.as_str() {
            "show" => {
                show_swaps()?;
            }
            "empty" => {
                empty_swap()?;
            }
            "set" => {
                // next token must be a size like 5G or 512M
                let size_tok = q
                    .pop_front()
                    .ok_or_else(|| anyhow::anyhow!("'set' requires a size argument, e.g. 5G"))?;
                // collect optional flags for this set command: --replace, --persist
                let mut replace = false;
                let mut persist = false;
                // peek next tokens that start with "--" and belong to set
                while matches!(q.front().map(String::as_str), Some(s) if s.starts_with("--")) {
                    let flag = q.pop_front().unwrap();
                    match flag.as_str() {
                        "--replace" => replace = true,
                        "--persist" => persist = true,
                        other => bail!("Unknown flag for 'set': {}", other),
                    }
                }
                set_swap(&size_tok, replace, persist)?;
            }
            other if other.starts_with('-') => {
                bail!("Unexpected global flag or misplaced flag: {}", other)
            }
            other => bail!("Unknown command: {} (expected set/show/empty)", other),
        }
    }

    Ok(())
}

// ---------------------- Implementation helpers ----------------------

fn require_root() -> Result<()> {
    // safe, tiny check
    let euid = unsafe { libc::geteuid() };
    if euid != 0 {
        bail!("This operation requires root. Run with sudo or as root.");
    }
    Ok(())
}

fn parse_human_size(s: &str) -> Result<u64> {
    // Accept integer sizes with optional suffix K, M, G, T (powers of 1024)
    // Examples: 512M, 5G, 1024
    if s.is_empty() {
        bail!("empty size");
    }
    let ls = s.to_uppercase();
    let last = ls.chars().last().unwrap();
    let (num_str, multiplier) = match last {
        'K' => (&ls[..ls.len() - 1], 1024u64),
        'M' => (&ls[..ls.len() - 1], 1024u64.pow(2)),
        'G' => (&ls[..ls.len() - 1], 1024u64.pow(3)),
        'T' => (&ls[..ls.len() - 1], 1024u64.pow(4)),
        c if c.is_ascii_digit() => (ls.as_str(), 1u64),
        _ => bail!(
            "Unknown size suffix in '{}'. Use K/M/G/T or plain bytes.",
            s
        ),
    };
    if num_str.is_empty() {
        bail!("Malformed size: {}", s);
    }
    // parse integer part only (no decimals allowed)
    let n: u64 = num_str.parse().context("parsing numeric part of size")?;
    Ok(n.checked_mul(multiplier)
        .ok_or_else(|| anyhow::anyhow!("size overflow"))?)
}

fn show_swaps() -> Result<()> {
    let text = fs::read_to_string("/proc/swaps").context("reading /proc/swaps")?;
    let mut lines = text.lines();
    let header = lines.next().unwrap_or("");
    println!("{}", header);
    let mut total_size: u64 = 0;
    let mut total_used: u64 = 0;
    for line in lines {
        // /proc/swaps columns: Filename Type Size Used Priority
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 5 {
            let size_kb: u64 = parts[2].parse().unwrap_or(0);
            let used_kb: u64 = parts[3].parse().unwrap_or(0);
            total_size += size_kb * 1024;
            total_used += used_kb * 1024;
            println!("{}", line);
        }
    }
    println!(
        "
Total: {} used / {} total",
        human_readable_bytes(total_used),
        human_readable_bytes(total_size)
    );
    Ok(())
}

fn empty_swap() -> Result<()> {
    require_root()?;
    println!("Disabling all swap (this will move pages back into RAM)...");
    let s = Command::new("swapoff")
        .arg("-a")
        .status()
        .context("running swapoff -a")?;
    if !s.success() {
        bail!("swapoff failed: exit {}", s.code().unwrap_or(-1));
    }
    println!("Re-enabling swap (swapon -a)...");
    let s2 = Command::new("swapon")
        .arg("-a")
        .status()
        .context("running swapon -a")?;
    if !s2.success() {
        bail!("swapon failed: exit {}", s2.code().unwrap_or(-1));
    }
    println!("Swap emptied (swapoff -> swapon cycle completed).");
    Ok(())
}

fn set_swap(size_token: &str, replace: bool, persist: bool) -> Result<()> {
    require_root()?;
    println!(
        "Requested set {} (replace={} persist={})",
        size_token, replace, persist
    );
    let size_bytes = parse_human_size(size_token)?;

    if replace {
        println!("Replacing existing swap (running swapoff -a)...");
        let s = Command::new("swapoff")
            .arg("-a")
            .status()
            .context("swapoff -a")?;
        if !s.success() {
            bail!("swapoff -a failed: exit {}", s.code().unwrap_or(-1));
        }
    }

    let path = Path::new("/swap-manager.swap");
    if path.exists() {
        // if it's active we should turn it off before overwriting
        println!("Existing {} found — disabling it first...", path.display());
        let _ = Command::new("swapoff").arg(path).status();
        fs::remove_file(path).context("removing existing swapfile")?;
    }

    // Try fallocate (accepts human suffix like 5G), otherwise fall back to dd.
    // We'll attempt `fallocate -l <size_token> path` first.
    let mut created = false;
    if Command::new("fallocate")
        .arg("-l")
        .arg(size_token)
        .arg(path)
        .status()
        .is_ok()
    {
        let s = Command::new("fallocate")
            .arg("-l")
            .arg(size_token)
            .arg(path)
            .status()?;
        if s.success() {
            created = true;
        }
    }

    if !created {
        // fall back to dd. Use MB blocksize; compute count (round up)
        let mb = (size_bytes + 1024 * 1024 - 1) / (1024 * 1024);
        println!(
            "fallocate unavailable or failed, falling back to dd ({} MiB)...",
            mb
        );
        let s = Command::new("dd")
            .arg("if=/dev/zero")
            .arg(format!("of={}", path.display()))
            .arg("bs=1M")
            .arg(format!("count={}", mb))
            .status()
            .context("running dd to create swapfile")?;
        if !s.success() {
            bail!(
                "dd failed to create swapfile: exit {}",
                s.code().unwrap_or(-1)
            );
        }
    }

    // chmod 600
    let _ = Command::new("chmod").arg("600").arg(path).status();
    // mkswap
    let s = Command::new("mkswap")
        .arg(path)
        .status()
        .context("mkswap")?;
    if !s.success() {
        bail!("mkswap failed: exit {}", s.code().unwrap_or(-1));
    }
    // swapon
    let s2 = Command::new("swapon")
        .arg(path)
        .status()
        .context("swapon")?;
    if !s2.success() {
        bail!("swapon failed: exit {}", s2.code().unwrap_or(-1));
    }

    println!(
        "Activated swapfile {} (size {}).",
        path.display(),
        human_readable_bytes(size_bytes)
    );

    if persist {
        println!("Adding entry to /etc/fstab to make swap persistent...");
        let fstab_line = format!(
            "{} none swap sw 0 0
",
            path.display()
        );
        let fstab = "/etc/fstab";
        // Heuristic: don't add duplicate lines
        let existing = fs::read_to_string(fstab).unwrap_or_default();
        if existing.contains(&fstab_line) {
            println!("/etc/fstab already contains the same entry — skipping append.");
        } else {
            let mut f = OpenOptions::new()
                .append(true)
                .open(fstab)
                .context("open /etc/fstab for appending")?;
            f.write_all(fstab_line.as_bytes())
                .context("writing to /etc/fstab")?;
            println!("Appended to /etc/fstab: {}", fstab_line.trim());
        }
    }

    Ok(())
}

fn human_readable_bytes(n: u64) -> String {
    // simple 1024-based representation
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut val = n as f64;
    let mut idx = 0usize;
    while val >= 1024.0 && idx + 1 < units.len() {
        val /= 1024.0;
        idx += 1;
    }
    if idx == 0 {
        format!("{} {}", n, units[0])
    } else {
        format!("{:.2} {}", val, units[idx])
    }
}
