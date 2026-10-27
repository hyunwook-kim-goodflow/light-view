//! lv-bench: first-pixel time and memory of light-view, per case, as JSON lines.
//!
//! ```text
//! lv-bench [--runs N] [--case NAME]... [--app PATH] [--heic FILE] [--check BASELINE] [-v]
//! ```
//!
//! Cases: `jpeg12mp` (4032×3024 JPEG), `jpeg24mp` (6000×4000 JPEG, tiled and mipmapped),
//! `png_large` (8000×6000 PNG), `heic12mp` (needs `--heic FILE`, a 12 MP HEIC, and
//! `lv-worker.exe` next to the app). Each case runs `--runs` times (default 5) and
//! prints the medians:
//!
//! `{"case": "jpeg12mp", "first_pixel_ms": 512, "decode_ms": 230, "private_mb": 300}`
//!
//! `first_pixel_ms` is the app's `--timing` value (process start to the first image
//! presented); `decode_ms` is the decode of that image on the prefetch thread;
//! `private_mb` is the private bytes of the app plus its HEIF workers once memory
//! settles (the open photo and its prefetched neighbours). With `--check BASELINE` (a
//! file of such lines) the exit code is 1 when any value is over the larger of 1.5× its
//! baseline and the baseline + 50 ms (times) or + 30 MB (memory), or a baseline case
//! that was asked for has no result (a case that could not run here, like `heic12mp`
//! without `--heic`, is listed as skipped instead).

mod images;
mod report;
mod run;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use images::Picture;
use report::CaseResult;

const CASES: [&str; 4] = ["jpeg12mp", "jpeg24mp", "png_large", "heic12mp"];

struct Args {
    runs: usize,
    cases: Vec<String>,
    app: PathBuf,
    heic: Option<PathBuf>,
    check: Option<PathBuf>,
    verbose: bool,
}

fn main() -> ExitCode {
    match real_main() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("lv-bench: {e}");
            ExitCode::from(2)
        }
    }
}

fn real_main() -> Result<ExitCode, String> {
    let args = parse_args(std::env::args().skip(1))?;
    if !args.app.is_file() {
        return Err(format!("{} not found (use --app)", args.app.display()));
    }
    let dir = std::env::temp_dir().join(format!("lv-bench-{}", std::process::id()));
    let results = run_cases(&args, &dir);
    let _ = std::fs::remove_dir_all(&dir);
    let (results, mut skipped) = results?;
    for r in &results {
        println!("{}", r.to_json());
    }
    let Some(path) = &args.check else {
        return Ok(ExitCode::SUCCESS);
    };
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let baseline = report::parse_baseline(&text)?;
    // Baseline cases not asked for with --case are not compared either.
    for base in &baseline {
        if !args.cases.contains(&base.case) && !skipped.contains(&base.case) {
            eprintln!(
                "lv-bench: {} not run (not asked for); not compared",
                base.case
            );
            skipped.push(base.case.clone());
        }
    }
    let failures = report::check(&baseline, &results, &skipped);
    for f in &failures {
        eprintln!("lv-bench: regression: {f}");
    }
    Ok(if failures.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// The results of the cases that ran, and the cases that could not run here.
fn run_cases(args: &Args, dir: &Path) -> Result<(Vec<CaseResult>, Vec<String>), String> {
    let mut results = Vec::new();
    let mut skipped = Vec::new();
    for case in &args.cases {
        let folder = dir.join(case);
        let file = match case_file(case, args, &folder)? {
            Some(file) => file,
            None => {
                skipped.push(case.clone());
                continue;
            }
        };
        eprintln!("lv-bench: {case}: {} runs", args.runs);
        let mut samples = Vec::new();
        for i in 0..args.runs {
            let sample = run::measure(&args.app, &file)?;
            if args.verbose {
                eprintln!(
                    "  run {}: spawn-to-line {:.0} ms, app {:.0} MB + workers {:.0} MB settled, app peak {:.0} MB; {}",
                    i + 1,
                    sample.spawn_to_line_ms,
                    sample.app_mb,
                    sample.workers_mb,
                    sample.app_peak_mb,
                    sample
                        .steps
                        .iter()
                        .map(|(n, v)| format!("{n}={v}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            }
            samples.push(sample);
        }
        results.push(summarize(case, &samples)?);
    }
    Ok((results, skipped))
}

/// The picture a case opens, written into `folder`; `None` (with a note) for a case
/// that cannot run here.
fn case_file(case: &str, args: &Args, folder: &Path) -> Result<Option<PathBuf>, String> {
    let picture = match case {
        "jpeg12mp" => Picture::Jpeg {
            width: 4032,
            height: 3024,
        },
        "jpeg24mp" => Picture::Jpeg {
            width: 6000,
            height: 4000,
        },
        "png_large" => Picture::Png {
            width: 8000,
            height: 6000,
        },
        "heic12mp" => {
            let Some(heic) = &args.heic else {
                eprintln!("lv-bench: heic12mp skipped: give a 12 MP HEIC with --heic FILE");
                return Ok(None);
            };
            let worker = args.app.with_file_name("lv-worker.exe");
            if !worker.is_file() {
                return Err(format!("heic12mp needs {}", worker.display()));
            }
            return images::prepare_copies(folder, heic)
                .map(Some)
                .map_err(|e| format!("{}: {e}", heic.display()));
        }
        other => return Err(format!("unknown case {other}")),
    };
    images::prepare(folder, picture)
        .map(Some)
        .map_err(|e| format!("writing the {case} pictures: {e}"))
}

fn summarize(case: &str, samples: &[run::Sample]) -> Result<CaseResult, String> {
    let step = |name: &str| -> Result<Vec<f64>, String> {
        samples
            .iter()
            .map(|s| {
                s.step(name)
                    .ok_or_else(|| format!("{case}: a run's timing line has no {name}"))
            })
            .collect()
    };
    let mut memory: Vec<f64> = samples.iter().map(|s| s.app_mb + s.workers_mb).collect();
    Ok(CaseResult {
        case: case.to_string(),
        first_pixel_ms: report::median(&mut step("first-present-ms")?),
        decode_ms: report::median(&mut step("decode-ms")?),
        private_mb: report::median(&mut memory),
    })
}

fn parse_args(mut it: impl Iterator<Item = String>) -> Result<Args, String> {
    let app_default = std::env::current_exe()
        .map_err(|e| e.to_string())?
        .with_file_name("lv-app.exe");
    let mut args = Args {
        runs: 5,
        cases: Vec::new(),
        app: app_default,
        heic: None,
        check: None,
        verbose: false,
    };
    while let Some(a) = it.next() {
        let mut value = || it.next().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--runs" => {
                args.runs = value()?
                    .parse()
                    .ok()
                    .filter(|&n| n > 0)
                    .ok_or("--runs needs a number above 0")?;
            }
            "--case" => {
                let case = value()?;
                if !CASES.contains(&case.as_str()) {
                    return Err(format!("unknown case {case}; cases: {}", CASES.join(", ")));
                }
                args.cases.push(case);
            }
            "--app" => args.app = PathBuf::from(value()?),
            "--heic" => args.heic = Some(PathBuf::from(value()?)),
            "--check" => args.check = Some(PathBuf::from(value()?)),
            "-v" | "--verbose" => args.verbose = true,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    if args.cases.is_empty() {
        args.cases = CASES.iter().map(|c| c.to_string()).collect();
    }
    Ok(args)
}
