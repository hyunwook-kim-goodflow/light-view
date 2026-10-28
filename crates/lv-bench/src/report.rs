//! Results as JSON lines, and the regression check against a baseline file.

/// One case's result: the medians over its runs, as named values in their order. A name
/// ending in `_ms` is a time, one ending in `_mb` a memory figure.
#[derive(Clone, Debug, PartialEq)]
pub struct CaseResult {
    pub case: String,
    pub values: Vec<(String, f64)>,
}

/// The values a case can measure; a baseline line with any other key is refused, so a
/// misspelt key never silently drops its check.
pub const KNOWN_VALUES: [&str; 6] = [
    "first_pixel_ms",
    "decode_ms",
    "private_mb",
    "estimates_ms",
    "first_estimate_ms",
    "peak_private_mb",
];

/// A result fails the check when a value is over this many times its baseline.
pub const CHECK_FACTOR: f64 = 1.5;

impl CaseResult {
    pub fn new(case: &str, values: &[(&str, f64)]) -> CaseResult {
        CaseResult {
            case: case.to_string(),
            values: values.iter().map(|(n, v)| (n.to_string(), *v)).collect(),
        }
    }

    pub fn value(&self, name: &str) -> Option<f64> {
        self.values.iter().find(|(n, _)| n == name).map(|v| v.1)
    }

    /// `{"case": "jpeg12mp", "first_pixel_ms": 512, "decode_ms": 230, "private_mb": 180}`
    pub fn to_json(&self) -> String {
        let mut line = format!(r#"{{"case": "{}""#, self.case);
        for (name, value) in &self.values {
            line.push_str(&format!(r#", "{name}": {value:.0}"#));
        }
        line.push('}');
        line
    }

    /// Parses a line written by `to_json` (spacing and key order may differ). Only this
    /// flat shape is read: string `case`, numeric values, no nesting or escapes. `None`
    /// for a key not in `KNOWN_VALUES`, a key given twice, or no value at all.
    pub fn from_json(line: &str) -> Option<CaseResult> {
        let body = line.trim().strip_prefix('{')?.strip_suffix('}')?;
        let mut case = None;
        let mut values = Vec::new();
        for pair in body.split(',') {
            let (key, value) = pair.split_once(':')?;
            let value = value.trim();
            match key.trim().trim_matches('"') {
                "case" => {
                    let s = value.strip_prefix('"')?.strip_suffix('"')?;
                    if case.replace(s.to_string()).is_some() {
                        return None;
                    }
                }
                name => {
                    let known = KNOWN_VALUES.contains(&name);
                    if !known || values.iter().any(|(n, _): &(String, f64)| n == name) {
                        return None;
                    }
                    values.push((name.to_string(), value.parse().ok()?));
                }
            }
        }
        if values.is_empty() {
            return None;
        }
        Some(CaseResult {
            case: case?,
            values,
        })
    }
}

/// The middle value (the mean of the two middle ones for an even count); 0 when empty.
pub fn median(values: &mut [f64]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f64::total_cmp);
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    }
}

/// Reads a baseline: one JSON object per line; blank lines are skipped.
pub fn parse_baseline(text: &str) -> Result<Vec<CaseResult>, String> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            CaseResult::from_json(l).ok_or_else(|| format!("baseline line not understood: {l}"))
        })
        .collect()
}

/// Allowed on top of a small baseline: times may grow by this many ms, …
pub const SLACK_MS: f64 = 50.0;
/// … and memory by this many MB, even where that is more than `CHECK_FACTOR`.
pub const SLACK_MB: f64 = 30.0;

/// What in `current` is over its limit, one message per value: the limit is the larger
/// of `CHECK_FACTOR` times the baseline and the baseline plus the slack (`SLACK_MS` for a
/// `_ms` value, `SLACK_MB` otherwise). A baseline case with no result fails unless it is in
/// `skipped` (it could not run here, e.g. no HEIC sample), and so does a baseline value the
/// result lacks; a result with no baseline case is not compared.
pub fn check(baseline: &[CaseResult], current: &[CaseResult], skipped: &[String]) -> Vec<String> {
    let mut failures = Vec::new();
    for base in baseline {
        let Some(now) = current.iter().find(|r| r.case == base.case) else {
            if !skipped.contains(&base.case) {
                failures.push(format!("{} has a baseline but no result", base.case));
            }
            continue;
        };
        for (name, base_value) in &base.values {
            let Some(now_value) = now.value(name) else {
                failures.push(format!("{} has no {name}", now.case));
                continue;
            };
            let slack = if name.ends_with("_ms") {
                SLACK_MS
            } else {
                SLACK_MB
            };
            let limit = (base_value * CHECK_FACTOR).max(base_value + slack);
            if now_value > limit {
                failures.push(format!(
                    "{} {name} {now_value:.0} is over its limit {limit:.0} (baseline {base_value:.0})",
                    now.case
                ));
            }
        }
    }
    failures
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(case: &str, first: f64, decode: f64, mb: f64) -> CaseResult {
        CaseResult::new(
            case,
            &[
                ("first_pixel_ms", first),
                ("decode_ms", decode),
                ("private_mb", mb),
            ],
        )
    }

    #[test]
    fn json_line_has_the_agreed_shape_and_reads_back() {
        let r = result("jpeg12mp", 512.0, 230.4, 180.6);
        let line = r.to_json();
        assert_eq!(
            line,
            r#"{"case": "jpeg12mp", "first_pixel_ms": 512, "decode_ms": 230, "private_mb": 181}"#
        );
        let back = CaseResult::from_json(&line).unwrap();
        assert_eq!(back, result("jpeg12mp", 512.0, 230.0, 181.0));
        let reordered = r#"{"private_mb":3,"case":"x","decode_ms":2.5,"first_pixel_ms":1}"#;
        let back = CaseResult::from_json(reordered).unwrap();
        assert_eq!(back.case, "x");
        assert_eq!(
            ["first_pixel_ms", "decode_ms", "private_mb"].map(|n| back.value(n)),
            [Some(1.0), Some(2.5), Some(3.0)]
        );
        assert_eq!(CaseResult::from_json(r#"{"case": "x"}"#), None);
        assert_eq!(
            CaseResult::from_json(r#"{"case": "x", "decode_ms": y}"#),
            None
        );
        // A misspelt or unknown key, or a key given twice, is refused.
        assert_eq!(CaseResult::from_json(r#"{"case": "x", "foo": 1}"#), None);
        assert_eq!(
            CaseResult::from_json(r#"{"case": "x", "decode_ms": 1, "decod_ms": 2}"#),
            None
        );
        assert_eq!(
            CaseResult::from_json(r#"{"case": "x", "decode_ms": 1, "decode_ms": 2}"#),
            None
        );
        assert_eq!(
            CaseResult::from_json(r#"{"case": "x", "case": "y", "decode_ms": 1}"#),
            None
        );
        assert_eq!(CaseResult::from_json("not json"), None);
    }

    #[test]
    fn median_takes_the_middle() {
        assert_eq!(median(&mut [5.0, 1.0, 3.0]), 3.0);
        assert_eq!(median(&mut [4.0, 1.0, 3.0, 2.0]), 2.5);
        assert_eq!(median(&mut []), 0.0);
    }

    #[test]
    fn check_fails_only_past_one_and_a_half_times_the_baseline() {
        let baseline = vec![
            result("jpeg12mp", 200.0, 120.0, 300.0),
            result("png_large", 1000.0, 800.0, 900.0),
        ];
        let all = |r: Vec<CaseResult>| {
            let mut r = r;
            r.push(result("png_large", 1000.0, 800.0, 900.0));
            r
        };
        // Exactly 1.5x passes; anything over fails, per value.
        let ok = all(vec![result("jpeg12mp", 300.0, 180.0, 450.0)]);
        assert!(check(&baseline, &ok, &[]).is_empty());
        let slow = all(vec![
            result("jpeg12mp", 301.0, 120.0, 451.0),
            result("heic12mp", 9999.0, 9999.0, 9999.0), // not in the baseline
        ]);
        let failures = check(&baseline, &slow, &[]);
        assert_eq!(failures.len(), 2, "{failures:?}");
        assert!(failures[0].contains("jpeg12mp") && failures[0].contains("first_pixel_ms"));
        assert!(failures[1].contains("private_mb"));
    }

    #[test]
    fn small_baselines_get_a_fixed_slack() {
        // 1.5x of 20 ms is 30 ms, but +50 ms is allowed; memory gets +30 MB.
        let baseline = vec![result("tiny", 20.0, 10.0, 40.0)];
        assert!(check(&baseline, &[result("tiny", 70.0, 60.0, 70.0)], &[]).is_empty());
        let failures = check(&baseline, &[result("tiny", 71.0, 61.0, 71.0)], &[]);
        assert_eq!(failures.len(), 3, "{failures:?}");
    }

    #[test]
    fn a_baseline_case_without_a_result_fails_unless_skipped() {
        let baseline = vec![
            result("jpeg12mp", 200.0, 100.0, 300.0),
            result("heic12mp", 300.0, 200.0, 300.0),
        ];
        let current = vec![result("jpeg12mp", 200.0, 100.0, 300.0)];
        let failures = check(&baseline, &current, &[]);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].contains("heic12mp") && failures[0].contains("no result"));
        assert!(check(&baseline, &current, &["heic12mp".to_string()]).is_empty());
    }

    #[test]
    fn resize_case_has_its_own_values() {
        let r = CaseResult::new(
            "resize10x12mp",
            &[("estimates_ms", 2400.4), ("peak_private_mb", 610.0)],
        );
        let line = r.to_json();
        assert_eq!(
            line,
            r#"{"case": "resize10x12mp", "estimates_ms": 2400, "peak_private_mb": 610}"#
        );
        let base = CaseResult::from_json(&line).unwrap();
        // Times get +50 ms (or 1.5x), memory +30 MB (or 1.5x).
        let ok = CaseResult::new(
            "resize10x12mp",
            &[("estimates_ms", 3600.0), ("peak_private_mb", 915.0)],
        );
        assert!(check(std::slice::from_ref(&base), &[ok], &[]).is_empty());
        let over = CaseResult::new(
            "resize10x12mp",
            &[("estimates_ms", 3601.0), ("peak_private_mb", 916.0)],
        );
        assert_eq!(check(std::slice::from_ref(&base), &[over], &[]).len(), 2);
        // A value the baseline has but the result lacks fails.
        let partial = CaseResult::new("resize10x12mp", &[("estimates_ms", 1.0)]);
        let failures = check(&[base], &[partial], &[]);
        assert_eq!(failures.len(), 1, "{failures:?}");
        assert!(failures[0].contains("no peak_private_mb"));
    }

    #[test]
    fn baseline_reads_lines_and_rejects_garbage() {
        let text =
            "\n{\"case\": \"a\", \"first_pixel_ms\": 1, \"decode_ms\": 2, \"private_mb\": 3}\r\n\n";
        assert_eq!(
            parse_baseline(text).unwrap(),
            vec![result("a", 1.0, 2.0, 3.0)]
        );
        assert!(parse_baseline("{\"case\": \"a\"}").is_err());
    }
}
