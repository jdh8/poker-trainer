//! Persistent drill history and the leak-report aggregator (design doc 04, P5).
//!
//! Every scored decision in a GTO-scored drill (`gto`, `range`, `hand`)
//! appends one JSONL record to `$XDG_DATA_HOME/poker-trainer/history.jsonl`;
//! `poker-trainer stats` folds the file into per-group leak stats. The
//! aggregator is a pure function over records, so P9's analyze can feed it
//! imported hands and get the same report.

use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Severity bands in bb of EV lost, used everywhere a decision is judged:
/// blunder > 0.30, error 0.05–0.30, ok < 0.05.
pub const BLUNDER_BB: f32 = 0.30;
pub const ERROR_BB: f32 = 0.05;
/// The existing convention: a pick is "accurate" if GTO plays it >= 5%.
pub const GTO_ACTION_FREQ: f32 = 0.05;

/// One scored decision. `#[serde(default)]` keeps old (and future) records
/// parsing — absent fields just come back empty.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct StatRecord {
    pub v: u32,
    /// Unix seconds.
    pub ts: u64,
    /// Which drill scored it: `"gto"`, `"range"`, `"hand"`, …
    pub drill: String,
    pub formation: String,
    /// Packed lowercase flop, e.g. `"td9d6h"`.
    pub flop: String,
    /// Objective flop texture, e.g. `"two-tone"` / `"paired"`.
    pub texture: String,
    /// `"flop"`, `"turn"`, or `"river"`.
    pub street: String,
    /// Hero's combo (`"AsKh"`); empty for whole-bucket decisions.
    pub hand: String,
    /// Flop made-hand bucket, e.g. `"TopPair"`.
    pub bucket: String,
    /// Action labels from the root to the decision.
    pub line: Vec<String>,
    pub chosen: String,
    pub best: String,
    /// bb given up vs. the best action; `None` when the drill can't measure EV.
    pub ev_loss: Option<f32>,
    /// GTO frequency of the chosen action.
    pub gto_freq: Option<f32>,
}

impl StatRecord {
    /// A v1 record stamped now; callers fill in the spot fields.
    pub fn new(drill: &str) -> Self {
        Self {
            v: 1,
            ts: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
            drill: drill.into(),
            ..Default::default()
        }
    }
}

/// `$XDG_DATA_HOME/poker-trainer/history.jsonl`, with the spec's
/// `~/.local/share` fallback (a relative `XDG_DATA_HOME` is ignored).
pub fn history_path() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".local/share")
        })
        .join("poker-trainer/history.jsonl")
}

/// Append one record to the history. Failures warn and never block a drill.
pub fn record(rec: &StatRecord) {
    if let Err(e) = append(&history_path(), rec) {
        eprintln!("(history not recorded: {e})");
    }
}

fn append(path: &Path, rec: &StatRecord) -> io::Result<()> {
    fs::create_dir_all(path.parent().expect("history path has a parent"))?;
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{}", serde_json::to_string(rec)?)
}

/// Read every parseable record; unparseable lines are skipped so a version
/// bump never bricks the whole history. A missing file is just empty history.
pub fn load(path: &Path) -> io::Result<Vec<StatRecord>> {
    let f = match fs::File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    Ok(BufReader::new(f)
        .lines()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str(&l).ok())
        .collect())
}

/// The grouping dimensions of `stats --by`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum GroupBy {
    Formation,
    Street,
    Texture,
    Bucket,
}

impl GroupBy {
    fn key<'a>(&self, r: &'a StatRecord) -> &'a str {
        match self {
            GroupBy::Formation => &r.formation,
            GroupBy::Street => &r.street,
            GroupBy::Texture => &r.texture,
            GroupBy::Bucket => &r.bucket,
        }
    }
}

/// One group's aggregated leak stats.
#[derive(Debug, PartialEq)]
pub struct GroupStats {
    pub key: String,
    pub count: usize,
    /// Mean over the records that measured EV.
    pub avg_ev_loss: f32,
    /// Share of decisions on an action GTO plays >= 5%.
    pub accuracy: f64,
    /// Decisions losing more than [`BLUNDER_BB`].
    pub blunders: usize,
}

/// Fold records into per-group stats, worst average EV loss first. Pure.
pub fn aggregate(records: &[StatRecord], by: GroupBy) -> Vec<GroupStats> {
    let mut groups: BTreeMap<&str, Vec<&StatRecord>> = BTreeMap::new();
    for r in records {
        groups.entry(by.key(r)).or_default().push(r);
    }
    let mut out: Vec<GroupStats> = groups
        .into_iter()
        .map(|(key, rs)| summarize(key, &rs))
        .collect();
    out.sort_by(|a, b| b.avg_ev_loss.total_cmp(&a.avg_ev_loss));
    out
}

fn summarize(key: &str, rs: &[&StatRecord]) -> GroupStats {
    let evs: Vec<f32> = rs.iter().filter_map(|r| r.ev_loss).collect();
    GroupStats {
        key: if key.is_empty() { "(none)" } else { key }.into(),
        count: rs.len(),
        avg_ev_loss: if evs.is_empty() {
            0.0
        } else {
            evs.iter().sum::<f32>() / evs.len() as f32
        },
        accuracy: rs
            .iter()
            .filter(|r| r.gto_freq.is_some_and(|f| f >= GTO_ACTION_FREQ))
            .count() as f64
            / rs.len().max(1) as f64,
        blunders: evs.iter().filter(|&&e| e > BLUNDER_BB).count(),
    }
}

/// Severity band of an EV loss.
pub fn band(ev_loss: f32) -> &'static str {
    if ev_loss > BLUNDER_BB {
        "blunder"
    } else if ev_loss >= ERROR_BB {
        "error"
    } else {
        "ok"
    }
}

const TREND_WINDOW: usize = 200;

/// `(prior, recent)` average EV loss over the last two windows of up to
/// [`TREND_WINDOW`] EV-measured decisions; `None` until both windows have at
/// least 20 (any less is noise, not a trend).
pub fn trend(records: &[StatRecord]) -> Option<(f32, f32)> {
    let evs: Vec<f32> = records.iter().filter_map(|r| r.ev_loss).collect();
    let w = TREND_WINDOW.min(evs.len() / 2);
    if w < 20 {
        return None;
    }
    let avg = |s: &[f32]| s.iter().sum::<f32>() / s.len() as f32;
    Some((
        avg(&evs[evs.len() - 2 * w..evs.len() - w]),
        avg(&evs[evs.len() - w..]),
    ))
}

/// Entry point for `poker-trainer stats`.
pub fn run(by: GroupBy, last: Option<usize>) {
    let path = history_path();
    let mut records = match load(&path) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Couldn't read {}: {e}", path.display());
            return;
        }
    };
    if let Some(n) = last {
        let skip = records.len().saturating_sub(n);
        records.drain(..skip);
    }
    if records.is_empty() {
        println!(
            "No history yet — play some drills first ({}).",
            path.display()
        );
        return;
    }

    let refs: Vec<&StatRecord> = records.iter().collect();
    let all = summarize("all", &refs);
    println!("{} decisions ({}).", all.count, path.display());
    println!(
        "  Avg EV loss {:.3}bb ({}) | accuracy {:.0}% | blunders {} ({:.0}%)\n",
        all.avg_ev_loss,
        band(all.avg_ev_loss),
        100.0 * all.accuracy,
        all.blunders,
        100.0 * all.blunders as f64 / all.count as f64
    );

    let header = format!("{by:?}").to_lowercase();
    println!(
        "  {:<14} {:>6}  {:>9}  {:>9}  {:>9}  band",
        header, "count", "avg loss", "accuracy", "blunders"
    );
    for g in aggregate(&records, by) {
        println!(
            "  {:<14} {:>6}  {:>7.3}bb  {:>8.0}%  {:>8.0}%  {}",
            g.key,
            g.count,
            g.avg_ev_loss,
            100.0 * g.accuracy,
            100.0 * g.blunders as f64 / g.count as f64,
            band(g.avg_ev_loss)
        );
    }

    match trend(&records) {
        Some((prior, recent)) => println!(
            "\n  Trend: {prior:.3}bb -> {recent:.3}bb avg EV loss ({}).",
            if recent < prior {
                "improving"
            } else if recent > prior {
                "worsening"
            } else {
                "flat"
            }
        ),
        None => println!("\n  (not enough history for a trend yet)"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(bucket: &str, street: &str, ev_loss: f32, gto_freq: f32) -> StatRecord {
        StatRecord {
            bucket: bucket.into(),
            street: street.into(),
            ev_loss: Some(ev_loss),
            gto_freq: Some(gto_freq),
            ..StatRecord::new("gto")
        }
    }

    #[test]
    fn bands_have_the_designed_boundaries() {
        assert_eq!(band(0.04), "ok");
        assert_eq!(band(0.05), "error");
        assert_eq!(band(0.30), "error");
        assert_eq!(band(0.31), "blunder");
    }

    #[test]
    fn aggregate_groups_and_sorts_worst_first() {
        let records = vec![
            rec("Air", "flop", 0.50, 0.01),   // blunder, inaccurate
            rec("Air", "flop", 0.10, 0.40),   // error, accurate
            rec("Value", "turn", 0.02, 0.90), // ok, accurate
        ];
        let groups = aggregate(&records, GroupBy::Bucket);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].key, "Air"); // worst avg loss first
        assert_eq!(groups[0].count, 2);
        assert!((groups[0].avg_ev_loss - 0.30).abs() < 1e-6);
        assert_eq!(groups[0].blunders, 1);
        assert!((groups[0].accuracy - 0.5).abs() < 1e-12);
        assert_eq!(groups[1].key, "Value");

        let by_street = aggregate(&records, GroupBy::Street);
        assert_eq!(by_street.len(), 2); // same records, different keys
    }

    #[test]
    fn aggregate_handles_missing_ev_and_empty_keys() {
        let mut r = rec("", "flop", 0.0, 0.0);
        r.ev_loss = None; // a preflop-style accuracy-only record
        r.gto_freq = None;
        let groups = aggregate(&[r], GroupBy::Bucket);
        assert_eq!(groups[0].key, "(none)");
        assert_eq!(groups[0].avg_ev_loss, 0.0);
        assert_eq!(groups[0].accuracy, 0.0);
    }

    #[test]
    fn trend_needs_enough_history_then_compares_windows() {
        let bad: Vec<StatRecord> = (0..20).map(|_| rec("Air", "flop", 0.4, 0.0)).collect();
        let good: Vec<StatRecord> = (0..20).map(|_| rec("Air", "flop", 0.1, 0.5)).collect();
        assert_eq!(trend(&bad), None); // one window's worth only

        let both: Vec<StatRecord> = bad.iter().chain(&good).cloned().collect();
        let (prior, recent) = trend(&both).unwrap();
        assert!((prior - 0.4).abs() < 1e-6);
        assert!((recent - 0.1).abs() < 1e-6);
    }

    #[test]
    fn records_round_trip_and_tolerate_missing_fields() {
        let r = rec("TopPair", "turn", 0.31, 0.12);
        let json = serde_json::to_string(&r).unwrap();
        let back: StatRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(back.bucket, "TopPair");
        assert_eq!(back.ev_loss, Some(0.31));

        // Sparse/old records still parse; absent fields default.
        let sparse: StatRecord = serde_json::from_str(r#"{"drill":"gto"}"#).unwrap();
        assert_eq!(sparse.drill, "gto");
        assert_eq!(sparse.ev_loss, None);
        assert!(sparse.line.is_empty());
    }

    #[test]
    fn load_skips_garbage_and_missing_file_is_empty() {
        let dir = std::env::temp_dir().join(format!("pt-stats-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("history.jsonl");

        append(&path, &rec("Air", "flop", 0.1, 0.2)).unwrap();
        fs::write(
            &path,
            format!(
                "{}not json\n",
                fs::read_to_string(&path).unwrap() // keep the good line first
            ),
        )
        .unwrap();
        append(&path, &rec("Value", "turn", 0.0, 0.9)).unwrap();

        let records = load(&path).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[1].bucket, "Value");

        assert!(load(&dir.join("nope.jsonl")).unwrap().is_empty());
        fs::remove_dir_all(&dir).unwrap();
    }
}
