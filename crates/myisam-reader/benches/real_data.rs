//! Decode-only benchmark: fixture bytes in RAM, no FTP/RAR/staging/index overhead.
use myisam_reader::{FrmSchema, MyisamInfo, walk_records};
use std::{fs, hint::black_box, path::PathBuf, time::Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Cargo test --all-targets executes custom harnesses with --test. Keep that a cheap smoke run.
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    let iterations: u64 = args
        .windows(2)
        .find(|p| p[0] == "--iterations")
        .map(|p| p[1].parse())
        .transpose()?
        .unwrap_or(
            if cfg!(debug_assertions) || args.iter().any(|a| a == "--test") {
                1
            } else {
                200
            },
        );
    if iterations == 0 {
        return Err("iterations must be positive".into());
    }
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/libgen-2026-09-06");
    let mut results = Vec::new();
    for table in [
        "editions",
        "editions_add_descr",
        "editions_to_files",
        "files",
        "elem_descr",
    ] {
        let schema = FrmSchema::parse(&fs::read(root.join(format!("{table}.frm")))?)?;
        let info = MyisamInfo::parse_myi(&fs::read(root.join(format!("{table}.MYI.header")))?)?;
        let data = fs::read(root.join(format!("{table}.MYD")))?;
        let decoder = schema.decoder(&info)?;
        let start = Instant::now();
        let mut rows = 0u64;
        for _ in 0..iterations {
            let stats = walk_records(
                black_box(data.as_slice()),
                data.len() as u64,
                &info,
                None,
                |packed| {
                    black_box(decoder.unpack(black_box(packed))?);
                    Ok(())
                },
            )?;
            rows += stats.records;
        }
        let seconds = start.elapsed().as_secs_f64();
        results.push(
            serde_json::json!({ "table":table, "iterations":iterations, "rows":rows,
            "seconds":seconds, "rows_per_second":rows as f64 / seconds,
            "decoded_mib_per_second":data.len() as f64 * iterations as f64 / 1048576.0 / seconds }),
        );
    }
    println!("{}", serde_json::to_string_pretty(&results)?);
    Ok(())
}
