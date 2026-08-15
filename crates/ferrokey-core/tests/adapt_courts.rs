//! ADAPT.* — the adaptive-geometry courts (Phase 4 WS4, §4.15).
//!
//! These run as a normal `cargo test` (inside the builder container — never
//! on the host), printing one machine-readable gate line per assertion:
//!
//! ```text
//! ADAPT.CENTERED.001  <label> ... PASS
//! ```
//!
//! `testing/scripts/adaptive-court.sh` parses those lines, writes the court
//! receipt, and fails the suite when any gate fails. The gates:
//!
//! ```text
//! ADAPT.CENTERED.001  remains near baseline
//! ADAPT.LEFT_BIAS.001 shifts effective region correctly
//! ADAPT.RIGHT_BIAS.001
//! ADAPT.OUTLIERS.001  robust to extreme samples
//! ADAPT.NEIGHBOR.001  deterministic competition
//! ADAPT.BOUNDS.001    all geometry constraints preserved
//! ADAPT.FREEZE.001    frozen geometry immutable
//! ADAPT.RESET.001     exact baseline restoration
//! ADAPT.REPLAY.001    deterministic reproduction
//! ADAPT.METRIC.001    measurable improvement
//! ```

use ferrokey_core::geometry::{
    synthetic_dataset, AdaptiveConfig, AdaptiveGeometry, Ellipse, GeometryConstraints, Point,
    PopulationKind, Rect, Sample,
};

// ── a realistic compact-keyboard grid ───────────────────────────────────────

const KEY_H: f64 = 52.0;
const KEY_BASE: f64 = 90.0;
const SPACING: f64 = 6.0;
const PAD: f64 = 6.0;

/// Build a 5-row compact layout: rects + neighbor adjacency (left/right in
/// a row; any key in an adjacent row whose horizontal interval overlaps).
fn compact_geometry() -> (Vec<Rect>, Vec<Vec<usize>>) {
    let rows: [&[f32]; 5] = [
        &[1.0; 10], // number row
        &[1.0; 10], // qwerty
        &[1.5, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.5],
        &[1.75, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.75],
        &[1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0],
    ];
    let mut visual = Vec::new();
    let mut row_of = Vec::new();
    for (r, row) in rows.iter().enumerate() {
        let mut x = PAD;
        for &f in *row {
            let w = f64::from(f) * KEY_BASE;
            let y = PAD + r as f64 * (KEY_H + SPACING);
            visual.push(Rect::new(x, y, w, KEY_H));
            row_of.push(r);
            x += w + SPACING;
        }
    }
    let n = visual.len();
    let mut neighbors = vec![Vec::new(); n];
    for i in 0..n {
        // horizontal: previous / next index in the same row
        if i > 0 && row_of[i - 1] == row_of[i] {
            neighbors[i].push(i - 1);
        }
        if i + 1 < n && row_of[i + 1] == row_of[i] {
            neighbors[i].push(i + 1);
        }
        // vertical: any key in an adjacent row with interval overlap
        let (x0, x1) = (visual[i].x, visual[i].x + visual[i].w);
        for j in 0..n {
            if i == j {
                continue;
            }
            let dr = row_of[i].abs_diff(row_of[j]);
            if dr != 1 {
                continue;
            }
            let (jx0, jx1) = (visual[j].x, visual[j].x + visual[j].w);
            if x0 < jx1 && jx0 < x1 {
                neighbors[i].push(j);
            }
        }
        neighbors[i].sort_unstable();
        neighbors[i].dedup();
    }
    (visual, neighbors)
}

fn fresh(visual: &[Rect], neighbors: &[Vec<usize>]) -> AdaptiveGeometry {
    AdaptiveGeometry::new(
        AdaptiveConfig::default(),
        GeometryConstraints::default(),
        visual,
        neighbors,
    )
}

// ── gate plumbing ───────────────────────────────────────────────────────────

fn gate(id: &str, label: &str, pass: bool, detail: &str) {
    println!(
        "ADAPT.{}  {} ... {}",
        id,
        label,
        if pass { "PASS" } else { "FAIL" }
    );
    if !pass {
        println!("  detail: {detail}");
        assert!(pass, "{id} {label}: {detail}");
    }
}

/// Maximum center displacement across all keys.
fn max_center_displacement(ag: &AdaptiveGeometry) -> f64 {
    let mut m = 0.0f64;
    for i in 0..ag.len() {
        let c = ag.visual(i).center();
        let h = ag.hit(i);
        m = m.max((h.cx - c.x).abs()).max((h.cy - c.y).abs());
    }
    m
}

fn count_constraint_violations(ag: &AdaptiveGeometry) -> usize {
    let mut bad = 0;
    for i in 0..ag.len() {
        let nbs: Vec<Ellipse> = (0..ag.len())
            .filter(|&j| i != j && adjacent(ag, i, j))
            .map(|j| ag.hit(j))
            .collect();
        if ag
            .constraints
            .violated_by(ag.visual(i), ag.hit(i), &nbs)
            .is_some()
        {
            bad += 1;
        }
    }
    bad
}

/// A cheap adjacency re-check (mirrors the neighbor graph used at build
/// time; the model itself owns the authoritative graph).
fn adjacent(ag: &AdaptiveGeometry, i: usize, j: usize) -> bool {
    let a = ag.visual(i);
    let b = ag.visual(j);
    let x_overlap = a.x < b.x + b.w && b.x < a.x + a.w;
    let y_overlap = a.y < b.y + b.h && b.y < a.y + a.h;
    x_overlap && y_overlap
}

fn run_population(
    kind: PopulationKind,
    visual: &[Rect],
    neighbors: &[Vec<usize>],
    samples_per_key: u32,
) -> (
    AdaptiveGeometry,
    Vec<Sample>,
    ferrokey_core::geometry::Evaluation,
) {
    let dataset = synthetic_dataset(kind, visual, samples_per_key, 12345);
    let mut ag = fresh(visual, neighbors);
    let eval = ag.evaluate(kind.name(), &dataset);
    (ag, dataset, eval)
}

// ── the courts ─────────────────────────────────────────────────────────────

#[test]
fn adapt_courts() {
    let (visual, neighbors) = compact_geometry();
    assert!(visual.len() >= 40, "court grid too small: {}", visual.len());

    // ── ADAPT.CENTERED.001: centered population stays near baseline ────────
    {
        let dataset = synthetic_dataset(PopulationKind::Centered, &visual, 30, 1);
        let mut ag = fresh(&visual, &neighbors);
        let eval = ag.evaluate("centered", &dataset);
        let disp = max_center_displacement(&ag);
        gate(
            "CENTERED.001",
            "remains near baseline",
            eval.constraints_violated == 0
                && disp < 1.0
                && eval.adaptive_error_rate <= eval.baseline_error_rate + 0.01,
            &format!(
                "disp={disp:.2} baseline={:.3} adaptive={:.3}",
                eval.baseline_error_rate, eval.adaptive_error_rate
            ),
        );
    }

    // ── ADAPT.LEFT_BIAS.001 / ADAPT.RIGHT_BIAS.001 ─────────────────────────
    for (kind, id, sign) in [
        (PopulationKind::LeftBias, "LEFT_BIAS.001", -1.0),
        (PopulationKind::RightBias, "RIGHT_BIAS.001", 1.0),
    ] {
        let (ag, _, eval) = run_population(kind, &visual, &neighbors, 40);
        // The effective centers moved in the bias direction on average.
        let mut total_dx = 0.0;
        let mut keys_used = 0;
        for i in 0..ag.len() {
            let c = ag.visual(i).center();
            let h = ag.hit(i);
            if ag.stats(i).samples > 0 {
                total_dx += (h.cx - c.x) * sign;
                keys_used += 1;
            }
        }
        let mean_dx = total_dx / f64::from(keys_used.max(1));
        let improve = if eval.baseline_error_rate > 0.02 {
            eval.adaptive_error_rate < eval.baseline_error_rate
        } else {
            eval.adaptive_error_rate <= eval.baseline_error_rate + 0.01
        };
        gate(
            id,
            "shifts effective region correctly",
            mean_dx > 0.5 && improve && eval.constraints_violated == 0,
            &format!(
                "mean_signed_dx={mean_dx:.2} baseline={:.3} adaptive={:.3} violated={}",
                eval.baseline_error_rate, eval.adaptive_error_rate, eval.constraints_violated
            ),
        );
    }

    // ── ADAPT.OUTLIERS.001: robust to extreme samples ───────────────────────
    {
        let (ag, _, eval) = run_population(PopulationKind::OutlierHeavy, &visual, &neighbors, 40);
        let disp = max_center_displacement(&ag);
        gate(
            "OUTLIERS.001",
            "robust to extreme samples",
            eval.constraints_violated == 0
                && eval.adaptive_error_rate <= eval.baseline_error_rate + 0.01
                && disp <= 12.0,
            &format!(
                "disp={disp:.2} baseline={:.3} adaptive={:.3} violated={}",
                eval.baseline_error_rate, eval.adaptive_error_rate, eval.constraints_violated
            ),
        );
    }

    // ── ADAPT.NEIGHBOR.001: deterministic competition ───────────────────────
    {
        let dataset = synthetic_dataset(PopulationKind::Bimodal, &visual, 40, 7);
        let mut ag = fresh(&visual, &neighbors);
        ag.feed(&dataset);
        let violations = count_constraint_violations(&ag);
        // Deterministic: re-running the identical pipeline reproduces the
        // exact geometry.
        let mut ag2 = fresh(&visual, &neighbors);
        ag2.feed(&dataset);
        let identical = (0..ag.len()).all(|i| ag.hit(i) == ag2.hit(i));
        gate(
            "NEIGHBOR.001",
            "deterministic competition",
            violations == 0 && identical,
            &format!("violations={violations} deterministic={identical}"),
        );
    }

    // ── ADAPT.BOUNDS.001: constraints preserved on every population ────────
    {
        let mut bad = 0;
        let mut worst = String::new();
        for kind in PopulationKind::ALL {
            let (ag, _, eval) = run_population(kind, &visual, &neighbors, 30);
            let v = count_constraint_violations(&ag);
            if v > 0 {
                bad += v;
                worst = format!("{}:{} violations", kind.name(), v);
            }
            // A strict per-key re-check against the model's own neighbor
            // graph (the definitive one).
            for (i, nb) in neighbors.iter().enumerate() {
                let nbs: Vec<Ellipse> = nb.iter().map(|&j| ag.hit(j)).collect();
                if let Some(viol) = ag.constraints.violated_by(ag.visual(i), ag.hit(i), &nbs) {
                    bad += 1;
                    worst = format!("{}:{:?}", kind.name(), viol);
                }
            }
            assert_eq!(eval.constraints_violated, 0, "{worst}");
        }
        gate(
            "BOUNDS.001",
            "all geometry constraints preserved",
            bad == 0,
            &worst,
        );
    }

    // ── ADAPT.FREEZE.001: frozen geometry immutable ─────────────────────────
    {
        let dataset = synthetic_dataset(PopulationKind::RightBias, &visual, 40, 8);
        let mut ag = fresh(&visual, &neighbors);
        ag.set_frozen(true);
        let before: Vec<Ellipse> = (0..ag.len()).map(|i| ag.hit(i)).collect();
        for s in &dataset {
            ag.record_hit(s.key, Point::new(s.x, s.y));
            if ag.optimize_due() {
                ag.optimize();
            }
        }
        ag.optimize();
        let after: Vec<Ellipse> = (0..ag.len()).map(|i| ag.hit(i)).collect();
        let changed = before.iter().zip(&after).filter(|(a, b)| a != b).count();
        gate(
            "FREEZE.001",
            "frozen geometry immutable",
            before == after,
            &format!("changed={changed}"),
        );
        // Unfreeze resumes adaptation (re-capture after the unfrozen pass).
        ag.set_frozen(false);
        ag.optimize();
        let resumed: Vec<Ellipse> = (0..ag.len()).map(|i| ag.hit(i)).collect();
        gate(
            "FREEZE.001b",
            "unfreeze resumes learning",
            resumed != before,
            &format!(
                "changed_after_unfreeze={}",
                resumed.iter().zip(&before).filter(|(a, b)| a != b).count()
            ),
        );
    }

    // ── ADAPT.RESET.001: exact baseline restoration ─────────────────────────
    {
        let dataset = synthetic_dataset(PopulationKind::LeftBias, &visual, 40, 9);
        let mut ag = fresh(&visual, &neighbors);
        ag.feed(&dataset);
        assert!(max_center_displacement(&ag) > 1e-9);
        ag.reset();
        let mut exact = true;
        for i in 0..ag.len() {
            let c = ag.visual(i).center();
            let h = ag.hit(i);
            if (h.cx - c.x).abs() > 1e-9
                || (h.cy - c.y).abs() > 1e-9
                || (h.rx - ag.visual(i).w / 2.0).abs() > 1e-9
                || (h.ry - ag.visual(i).h / 2.0).abs() > 1e-9
                || ag.stats(i).samples != 0
                || ag.frequency(i) != 0
            {
                exact = false;
            }
        }
        gate(
            "RESET.001",
            "exact baseline restoration",
            exact,
            "geometry/stats not fully cleared",
        );
    }

    // ── ADAPT.REPLAY.001: deterministic reproduction ────────────────────────
    {
        let dataset = synthetic_dataset(PopulationKind::UpwardThumbArc, &visual, 33, 99);
        let mut a = fresh(&visual, &neighbors);
        let mut b = fresh(&visual, &neighbors);
        a.feed(&dataset);
        b.feed(&dataset);
        let identical = (0..a.len()).all(|i| a.hit(i) == b.hit(i));
        gate(
            "REPLAY.001",
            "deterministic reproduction",
            identical,
            "geometry diverged across runs",
        );
    }

    // ── ADAPT.METRIC.001: measurable improvement (4.14) ─────────────────────
    {
        let mut rows = Vec::new();
        let mut improved = 0usize;
        let mut possible = 0usize;
        let mut any_worse = false;
        for kind in PopulationKind::ALL {
            let (_, _, eval) = run_population(kind, &visual, &neighbors, 40);
            let base = eval.baseline_error_rate;
            let adapt = eval.adaptive_error_rate;
            if base > 0.02 {
                possible += 1;
                if adapt < base {
                    improved += 1;
                } else {
                    any_worse = true;
                }
            } else if adapt > base + 0.01 {
                any_worse = true;
            }
            rows.push(format!(
                "{{\"dataset\": \"{}\", \"baseline_error_rate\": {:.4}, \"adaptive_error_rate\": {:.4}, \"relative_improvement\": {:.4}, \"constraints_violated\": {}}}",
                kind.name(),
                base,
                adapt,
                if base > 0.0 { (base - adapt) / base } else { 0.0 },
                eval.constraints_violated
            ));
        }
        println!("ADAPT.METRIC.REPORT [{}]", rows.join(","));
        gate(
            "METRIC.001",
            "measurable improvement",
            possible > 0 && improved == possible && !any_worse,
            &format!("possible={possible} improved={improved} any_worse={any_worse}"),
        );
    }
}
