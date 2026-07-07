//! Chart export: walk the solved tree top-down, prune by equilibrium reach,
//! and write the `src/preflop.rs` format (header.json + starter.jsonl +
//! charts.jsonl, design 07).

use crate::game::{Ruleset, State};
use crate::mccfr::Solver;
use poker_trainer::preflop::{
    class_combos, PreflopGenInfo, PreflopHeader, PreflopNode, CLASSES, FORMAT_VERSION,
};
use std::io::{self, Write};
use std::path::Path;

/// Stable FNV-1a hash of the ruleset's canonical JSON echo — the `gen`
/// skip-if-unchanged key (same idiom as `SpotConfig::hash8`).
pub fn config_hash(config: &serde_json::Value) -> String {
    let json = serde_json::to_string(config).expect("config serializes");
    let mut h: u64 = 0xcbf29ce484222325;
    for b in json.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:08x}", (h >> 32) as u32)
}

/// Read a ruleset TOML into the JSON echo the header carries.
pub fn config_echo(toml_path: impl AsRef<Path>) -> io::Result<serde_json::Value> {
    let text = std::fs::read_to_string(toml_path)?;
    let value: toml::Value =
        toml::from_str(&text).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
    serde_json::to_value(value).map_err(io::Error::other)
}

/// One exported node before serialization, with its computed reach.
struct Row {
    reach: f64,
    node: PreflopNode,
}

/// Walk the solved tree and export every decision node with equilibrium
/// reach ≥ `export_reach`; rows come back sorted by path for stable diffs.
fn collect(solver: &Solver, rs: &Ruleset) -> Vec<Row> {
    let mut rows = Vec::new();
    // Per-seat, per-class product of that seat's own past action frequencies.
    let own = vec![vec![1.0f64; CLASSES]; rs.n()];
    walk(solver, rs, State::root(rs), String::new(), own, &mut rows);
    rows.sort_by(|a, b| a.node.path.cmp(&b.node.path));
    rows
}

fn walk(
    solver: &Solver,
    rs: &Ruleset,
    st: State,
    path: String,
    own: Vec<Vec<f64>>,
    rows: &mut Vec<Row>,
) {
    let Some(actor) = st.to_act() else { return };
    let actor = actor as usize;
    let Some(node) = solver.infosets.get(&st.key()) else {
        return; // never visited ⇒ effectively unreachable
    };

    // Marginal (combo-weighted) reach of every *other* seat, times the
    // actor's own class-conditional reach; the node's headline reach is the
    // actor's best case.
    let marginal = |seat: usize| -> f64 {
        (0..CLASSES)
            .map(|c| own[seat][c] * f64::from(class_combos(c)) / 1326.0)
            .sum()
    };
    let others: f64 = (0..rs.n()).filter(|&s| s != actor).map(marginal).product();
    let reach = (0..CLASSES)
        .map(|c| own[actor][c] * others)
        .fold(0.0f64, f64::max);
    if reach < f64::from(rs.solver.export_reach) {
        return; // children only get smaller
    }

    let mut acts = Vec::new();
    st.legal(rs, &mut acts);
    let averages: Vec<Vec<f32>> = (0..CLASSES).map(|c| node.average(c)).collect();
    let round3 = |x: f32| (x * 1000.0).round() / 1000.0;
    let round2 = |x: f32| (x * 100.0).round() / 100.0; // centi-bb EVs read fine

    let freqs: Vec<Vec<f32>> = (0..acts.len())
        .map(|a| (0..CLASSES).map(|c| round3(averages[c][a])).collect())
        .collect();
    let evs: Vec<Vec<f32>> = (0..acts.len())
        .map(|a| {
            (0..CLASSES)
                .map(|c| round2(node.action_ev(c).map_or(0.0, |ev| ev[a])))
                .collect()
        })
        .collect();

    rows.push(Row {
        reach,
        node: PreflopNode {
            path: path.clone(),
            seat: rs.seats[actor].clone(),
            pot_bb: st.pot(rs) as f32 / 100.0,
            to_call_bb: (st.cur_bet - st.committed[actor]) as f32 / 100.0,
            reach: (reach * 1e6).round() as f32 / 1e6,
            actions: acts.iter().map(|a| a.label()).collect(),
            freqs,
            evs: Some(evs),
        },
    });

    for (ai, a) in acts.iter().enumerate() {
        let mut child_own = own.clone();
        for c in 0..CLASSES {
            child_own[actor][c] *= f64::from(averages[c][ai]);
        }
        let child_path = if path.is_empty() {
            a.token()
        } else {
            format!("{path}-{}", a.token())
        };
        walk(solver, rs, st.apply(rs, *a), child_path, child_own, rows);
    }
}

/// Write `header.json`, `starter.jsonl` (reach ≥ starter_reach), and
/// `charts.jsonl` (everything collected) into `dir`.
pub fn write_ruleset(
    solver: &Solver,
    rs: &Ruleset,
    config: serde_json::Value,
    strategy_drift: Option<f32>,
    dir: impl AsRef<Path>,
) -> io::Result<()> {
    let dir = dir.as_ref();
    std::fs::create_dir_all(dir)?;

    let header = PreflopHeader {
        version: FORMAT_VERSION,
        ruleset: rs.id.clone(),
        label: rs.label.clone(),
        config_hash: config_hash(&config),
        config,
        ev_unit: if rs.icm_payouts.is_some() {
            "payout"
        } else {
            "bb"
        }
        .into(),
        generator: PreflopGenInfo {
            version: env!("CARGO_PKG_VERSION").into(),
            traversals: solver.hands_dealt(),
            seed: rs.solver.seed,
            strategy_drift,
        },
    };
    std::fs::write(
        dir.join("header.json"),
        serde_json::to_string_pretty(&header)?,
    )?;

    let rows = collect(solver, rs);
    let mut starter = std::io::BufWriter::new(std::fs::File::create(dir.join("starter.jsonl"))?);
    let mut full = std::io::BufWriter::new(std::fs::File::create(dir.join("charts.jsonl"))?);
    let (mut n_starter, mut n_full) = (0u32, 0u32);
    for row in &rows {
        let line = serde_json::to_string(&row.node)?;
        writeln!(full, "{line}")?;
        n_full += 1;
        if row.reach >= f64::from(rs.solver.starter_reach) {
            writeln!(starter, "{line}")?;
            n_starter += 1;
        }
    }
    starter.flush()?;
    full.flush()?;
    eprintln!(
        "{}: exported {n_full} nodes (starter tier {n_starter}) to {}",
        rs.id,
        dir.display()
    );
    Ok(())
}

/// Refresh `data/preflop/index.json`: every subdirectory with a header.
pub fn write_index(data_dir: impl AsRef<Path>) -> io::Result<()> {
    let data_dir = data_dir.as_ref();
    let mut ids: Vec<String> = std::fs::read_dir(data_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join("header.json").exists())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort();
    std::fs::write(data_dir.join("index.json"), serde_json::to_string(&ids)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::equity::EquityCache;
    use poker_trainer::preflop::PreflopCharts;

    #[test]
    fn export_round_trips_through_the_trainer_loader() {
        // Tiny HU push/fold solve, exported and read back via src/preflop.rs
        // — the cross-crate format contract, exercised end to end.
        let rs: Ruleset = toml::from_str(
            r#"
            id = "hu-rt"
            label = "round trip"
            seats = ["SB", "BB"]
            stack_bb = 10.0
            sb = 0.5
            bb = 1.0
            open_to_bb = []
            threebet_mult = [3.0]
            squeeze_mult = [3.0]
            fourbet_mult = [2.3]
            jam_from_level = 0
            [solver]
            traversals = 3000
            seed = 5
            export_reach = 0.0001
            starter_reach = 0.05
            "#,
        )
        .unwrap();
        let mut solver = Solver::new(&rs, EquityCache::new(vec![0.5; CLASSES * CLASSES]));
        solver.run(3_000);

        let dir = std::env::temp_dir().join(format!("pt-export-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let config = serde_json::json!({"id": "hu-rt"});
        write_ruleset(&solver, &rs, config, Some(0.001), &dir).unwrap();

        let charts = PreflopCharts::load(&dir).unwrap();
        assert_eq!(charts.header.ruleset, "hu-rt");
        assert_eq!(charts.header.ev_unit, "bb");
        assert_eq!(charts.header.config_hash.len(), 8);

        // Root (SB fold/limp/jam) and the jam-facing node both export; the
        // root has full reach.
        let root = charts.node("").unwrap();
        assert_eq!(root.seat, "SB");
        assert_eq!(root.actions, vec!["Fold", "Call", "All-in"]);
        assert!((root.reach - 1.0).abs() < 1e-6);
        assert_eq!(root.pot_bb, 1.5);
        let vs_jam = charts.node("ai").unwrap();
        assert_eq!(vs_jam.seat, "BB");
        assert_eq!(vs_jam.to_call_bb, 9.0);
        // Frequencies are per-class normalized; EVs present.
        let f = vs_jam.freqs_for(0);
        assert!((f.iter().sum::<f32>() - 1.0).abs() < 0.01);
        assert!(vs_jam.strategy_for(0).is_some());

        // Index generation sees the directory.
        write_index(dir.parent().unwrap()).ok(); // parent is temp_dir: not asserted
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn config_hash_is_stable_and_sensitive() {
        let a = serde_json::json!({"id": "x", "stack_bb": 100.0});
        let b = serde_json::json!({"id": "x", "stack_bb": 40.0});
        assert_eq!(config_hash(&a), config_hash(&a));
        assert_eq!(config_hash(&a).len(), 8);
        assert_ne!(config_hash(&a), config_hash(&b));
    }

    /// The committed starter tiers must keep their poker shapes (design 07
    /// M4/M5): monotone entry toward the button, wide BB defense, correct EV
    /// units, and a wide open/jam range at the push/fold rungs of the cash
    /// depth ladder. Guards accidental regens as much as solver regressions.
    #[test]
    fn shipped_charts_have_sane_shapes() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../data/preflop");
        let load = |id: &str| PreflopCharts::load(format!("{dir}/{id}")).unwrap();
        // Combo-weighted raise-first-in: 1 − fold frequency at a node.
        let continue_freq = |charts: &PreflopCharts, path: &str| -> f64 {
            let node = charts.node(path).unwrap_or_else(|| panic!("node {path:?}"));
            (0..CLASSES)
                .map(|c| {
                    f64::from(1.0 - node.freqs[0][c])
                        * f64::from(poker_trainer::preflop::class_combos(c))
                })
                .sum::<f64>()
                / 1326.0
        };

        let cash = load("cash100");
        assert_eq!(cash.header.ev_unit, "bb");
        // Entry freq (1 − fold: limp + raise) rises toward the button; limps
        // are near-zero deep, so this still tracks RFI.
        let entry: Vec<f64> = ["", "f", "f-f", "f-f-f"]
            .iter()
            .map(|p| continue_freq(&cash, p))
            .collect();
        for w in entry.windows(2) {
            assert!(
                w[0] < w[1],
                "entry not monotone toward the button: {entry:?}"
            );
        }
        assert!((0.08..0.30).contains(&entry[0]), "UTG entry {entry:?}");
        assert!((0.28..0.65).contains(&entry[3]), "BTN entry {entry:?}");
        assert!(
            continue_freq(&cash, "f-f-f-r2.5-f") > 0.40,
            "BB defense vs BTN 2.5x"
        );

        // Cash depth ladder: every rung is chip-EV. Even the 10bb push/fold
        // rung keeps a real, position-monotone range — rake makes short-stack
        // cash tighter than deep (no postflop edge, taxed marginal opens), so
        // this is a sanity floor, not a "wider than deep" claim.
        let (c50, c20, c10) = (load("cash50"), load("cash20"), load("cash10"));
        for c in [&cash, &c50, &c20, &c10] {
            assert_eq!(c.header.ev_unit, "bb");
        }
        let (utg10, btn10) = (continue_freq(&c10, ""), continue_freq(&c10, "f-f-f"));
        assert!(
            utg10 < btn10,
            "10bb entry not monotone: UTG {utg10} vs BTN {btn10}"
        );
        assert!((0.20..0.55).contains(&btn10), "10bb BTN open/jam {btn10}");
    }
}
