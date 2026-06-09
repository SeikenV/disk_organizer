use clap::Parser;
use disk_organizer::aggregate::aggregate;
use disk_organizer::cut::cut;
use disk_organizer::delete::{delete_to_recycle_bin, full_path, plan};
use disk_organizer::format::human;
use disk_organizer::index::build_index;
use disk_organizer::model::{Item, RawRecord, Risk};
use disk_organizer::select::parse_selection;
use disk_organizer::snapshot;
use std::io::Write;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "disk_organizer", about = "Classify and clean up disk usage via the NTFS MFT")]
struct Args {
    /// Drive letter to scan, e.g. C (omit when using --from-snapshot)
    drive: Option<String>,
    #[arg(long, default_value_t = 40)]
    top: usize,
    #[arg(long, default_value_t = 200)]
    min_size_mb: u64,
    /// Save the raw scan to a JSON snapshot
    #[arg(long)]
    save_snapshot: Option<PathBuf>,
    /// Analyze a saved snapshot instead of reading the MFT (no admin needed)
    #[arg(long)]
    from_snapshot: Option<PathBuf>,
    /// Print what would be deleted without deleting
    #[arg(long)]
    dry_run: bool,
}

fn risk_tag(r: Risk) -> &'static str {
    match r {
        Risk::Safe => "[SAFE]   ",
        Risk::Caution => "[CAUTION]",
        Risk::System => "[SYSTEM] ",
        Risk::Unknown => "[UNKNOWN]",
    }
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();
    let min = args.min_size_mb.saturating_mul(1024 * 1024);

    // 1. Obtain records: from snapshot, or by reading the MFT.
    let (drive, records): (String, Vec<RawRecord>) = match &args.from_snapshot {
        Some(path) => {
            eprintln!("Loading snapshot {} ...", path.display());
            let snap = snapshot::load(path)?;
            (snap.drive, snap.records)
        }
        None => {
            let drive = args.drive.clone().unwrap_or_else(|| {
                eprintln!("error: provide a drive letter or --from-snapshot");
                std::process::exit(2);
            });
            eprintln!("Reading MFT for {drive}: (requires Administrator) ...");
            let image = disk_organizer::volume::read_mft(&drive)?;
            (drive, disk_organizer::mft_scan::parse_records(image.bytes))
        }
    };
    // Normalize to the bare drive letter ("C:" / "C:\" -> "C") so snapshots and
    // full_path() are consistent regardless of how the user typed the drive.
    let drive = drive.trim_end_matches([':', '\\', '/']).to_ascii_uppercase();
    eprintln!("{} records.", records.len());

    if let Some(path) = &args.save_snapshot {
        snapshot::save(path, &drive, &records)?;
        eprintln!("Saved snapshot to {}", path.display());
    }

    // 2. Classify.
    let index = build_index(records);
    let totals = aggregate(&index);
    let mut items = cut(&index, &totals, min);
    items.truncate(args.top);

    // 3. Print the numbered, risk-annotated list.
    println!("\n#   Risk       Size        Category — path");
    for (i, it) in items.iter().enumerate() {
        println!(
            "{:>3} {} {:>10}  {} — {}",
            i + 1, risk_tag(it.risk), human(it.physical_size), it.category, it.path.display()
        );
    }
    println!("\nLegend: SAFE=cache/regenerable, CAUTION=review first, SYSTEM=never deleted, UNKNOWN=unclassified");

    // 4. Prompt for selection.
    print!("\nEnter numbers to delete (e.g. 1 3 5), or just Enter to quit: ");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let input = line.trim().trim_start_matches('\u{feff}').trim();
    let chosen = match parse_selection(input, items.len()) {
        Ok(v) if v.is_empty() => { eprintln!("Nothing selected. Bye."); return Ok(()); }
        Ok(v) => v,
        Err(e) => { eprintln!("Invalid selection: {e}"); return Ok(()); }
    };

    let selected: Vec<Item> = chosen.iter().map(|&i| items[i].clone()).collect();
    let p = plan(&selected);

    // 5. Summarize + confirm.
    if !p.skipped_system.is_empty() {
        eprintln!("\nSkipping {} SYSTEM item(s) (never auto-deleted):", p.skipped_system.len());
        for it in &p.skipped_system {
            eprintln!("  - {}", it.path.display());
        }
    }
    if p.deletable.is_empty() {
        eprintln!("Nothing deletable selected. Bye.");
        return Ok(());
    }
    println!(
        "\nAbout to send {} item(s) to the Recycle Bin: {} (SAFE {}, CAUTION {}).",
        p.deletable.len(), human(p.total_bytes), p.safe, p.caution
    );

    let full: Vec<PathBuf> = p.deletable.iter().map(|it| full_path(&drive, &it.path)).collect();

    if args.dry_run {
        println!("[dry-run] would delete:");
        for path in &full {
            println!("  {}", path.display());
        }
        return Ok(());
    }

    print!("Type 'yes' to confirm: ");
    std::io::stdout().flush()?;
    let mut confirm = String::new();
    std::io::stdin().read_line(&mut confirm)?;
    if confirm.trim() != "yes" {
        eprintln!("Cancelled.");
        return Ok(());
    }

    // 6. Delete to Recycle Bin.
    let results = delete_to_recycle_bin(&full);
    let mut ok = 0;
    for (path, res) in &results {
        match res {
            Ok(()) => ok += 1,
            Err(e) => eprintln!("  FAILED {}: {e}", path.display()),
        }
    }
    println!("Moved {ok}/{} item(s) to the Recycle Bin.", results.len());
    Ok(())
}
