use clap::Parser;
use disk_organizer::aggregate::aggregate;
use disk_organizer::format::human;
use disk_organizer::index::build_index;
use disk_organizer::paths::path_for;
use disk_organizer::tree::{top_n_dirs, top_n_files};
use std::collections::HashMap;

#[derive(Parser)]
#[command(
    name = "disk_organizer",
    about = "Find the largest dirs/files on an NTFS volume via the MFT"
)]
struct Args {
    /// Drive letter to scan, e.g. C
    drive: String,
    #[arg(long, default_value_t = 30)]
    top: usize,
    #[arg(long, default_value_t = 100)]
    min_size_mb: u64,
}

fn main() -> std::io::Result<()> {
    let args = Args::parse();
    let min = args.min_size_mb.saturating_mul(1024 * 1024);

    eprintln!(
        "Reading MFT for {}: (requires Administrator) ...",
        args.drive
    );
    let image = disk_organizer::volume::read_mft(&args.drive)?;
    let records = disk_organizer::mft_scan::parse_records(image.bytes);
    eprintln!("Parsed {} records.", records.len());

    let index = build_index(records);
    let totals = aggregate(&index);
    let mut cache = HashMap::new();

    println!(
        "\n== Top {} directories (on-disk, hardlink-deduped) ==",
        args.top
    );
    for (frn, agg) in top_n_dirs(&totals, min, args.top) {
        println!(
            "{:>10}  {:>8} files  {}",
            human(agg.physical_size),
            agg.file_count,
            path_for(frn, &index, &mut cache).display()
        );
    }

    println!("\n== Top {} files ==", args.top);
    for (frn, rec) in top_n_files(&index, min, args.top) {
        println!(
            "{:>10}  {}",
            human(rec.physical_size),
            path_for(frn, &index, &mut cache).display()
        );
    }
    Ok(())
}
