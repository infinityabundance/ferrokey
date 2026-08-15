//! Adaptive key geometry (Phase 4 WS4).
//!
//! Ferrokey learns how the user actually touches the OSK and improves the
//! **effective** hit target of every key, while the **visible** keyboard
//! stays stable:
//!
//! ```text
//! VisualGeometry  (what is drawn — fixed rects)
//! HitGeometry     (what is hit-tested — adaptive ellipses)
//! ```
//!
//! Same keyboard semantics, better interpretation of touch intent → lower
//! miss/correction rate. The module is:
//!
//! * **pure** — no I/O, no clocks, no randomness in the pipeline (the
//!   synthetic populations use a fixed-seed generator for replay);
//! * **bounded** — per-key online statistics (Welford) with constant
//!   memory, never a chronological tap database;
//! * **deterministic** — same baseline + same dataset + same version ⇒
//!   identical output (exact regression testing);
//! * **constrained** — the optimizer can never violate the hard geometry
//!   invariants ([`GeometryConstraints`]); freeze/reset are exact.
//!
//! The touch hot path calls [`AdaptiveGeometry::record_hit`] and
//! [`AdaptiveGeometry::hit_test`] — both constant-time scans. The optimizer
//! ([`AdaptiveGeometry::optimize`]) runs only after enough accumulated
//! evidence, never in the hot path.

use std::ops::{Add, Sub};

// ── primitives ─────────────────────────────────────────────────────────────

/// A 2-D point (logical px, the same space as the view geometry).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Point {
    pub const fn new(x: f64, y: f64) -> Self {
        Point { x, y }
    }
}

impl Add for Point {
    type Output = Point;
    fn add(self, o: Point) -> Point {
        Point::new(self.x + o.x, self.y + o.y)
    }
}

impl Sub for Point {
    type Output = Point;
    fn sub(self, o: Point) -> Point {
        Point::new(self.x - o.x, self.y - o.y)
    }
}

/// An axis-aligned rectangle — the **visual** geometry of one key.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub const fn new(x: f64, y: f64, w: f64, h: f64) -> Self {
        Rect { x, y, w, h }
    }

    pub fn center(&self) -> Point {
        Point::new(self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    pub fn contains(&self, p: Point) -> bool {
        p.x >= self.x && p.x <= self.x + self.w && p.y >= self.y && p.y <= self.y + self.h
    }

    pub fn width(&self) -> f64 {
        self.w
    }

    pub fn height(&self) -> f64 {
        self.h
    }
}

/// An ellipse — the **hit** (effective touch target) geometry of one key.
///
/// `(cx, cy)` is the center, `rx`/`ry` the semi-axes. A point is inside when
/// its *normalized distance* `d = sqrt(((x-cx)/rx)² + ((y-cy)/ry)²) ≤ 1`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ellipse {
    pub cx: f64,
    pub cy: f64,
    pub rx: f64,
    pub ry: f64,
}

impl Ellipse {
    pub const fn new(cx: f64, cy: f64, rx: f64, ry: f64) -> Self {
        Ellipse { cx, cy, rx, ry }
    }

    /// The normalized (Mahalanobis-style) distance of `p` from the center,
    /// in units of the semi-axes. `≤ 1` means inside the ellipse; the value
    /// doubles as the *confidence* measure (0 = dead center).
    pub fn distance(&self, p: Point) -> f64 {
        let dx = (p.x - self.cx) / self.rx.max(f64::EPSILON);
        let dy = (p.y - self.cy) / self.ry.max(f64::EPSILON);
        (dx * dx + dy * dy).sqrt()
    }

    pub fn contains(&self, p: Point) -> bool {
        self.distance(p) <= 1.0
    }

    pub fn center(&self) -> Point {
        Point::new(self.cx, self.cy)
    }

    pub fn area(&self) -> f64 {
        std::f64::consts::PI * self.rx * self.ry
    }

    /// The axis-aligned bounding box.
    pub fn bbox(&self) -> Rect {
        Rect::new(
            self.cx - self.rx,
            self.cy - self.ry,
            2.0 * self.rx,
            2.0 * self.ry,
        )
    }
}

/// The intersection area of two axis-aligned rectangles (exact).
pub fn bbox_intersection_area(a: Rect, b: Rect) -> f64 {
    let w = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
    let h = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
    if w > 0.0 && h > 0.0 {
        w * h
    } else {
        0.0
    }
}

// ── bounded online statistics ──────────────────────────────────────────────

/// Per-key bounded online statistics (Welford's algorithm).
///
/// Constant memory (no tap history): mean, variance and covariance of the
/// touch positions are sufficient statistics for the optimizer's expected
/// cost (see [`AdaptiveGeometry::cost`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyTouchStats {
    /// Number of recorded samples.
    pub samples: u32,
    pub mean_x: f64,
    pub mean_y: f64,
    /// M2 accumulator for x (sum of squared deviations).
    pub m2_x: f64,
    /// M2 accumulator for y.
    pub m2_y: f64,
    /// Covariance accumulator (xy).
    pub cov_xy: f64,
}

impl Default for KeyTouchStats {
    fn default() -> Self {
        KeyTouchStats {
            samples: 0,
            mean_x: 0.0,
            mean_y: 0.0,
            m2_x: 0.0,
            m2_y: 0.0,
            cov_xy: 0.0,
        }
    }
}

impl KeyTouchStats {
    pub const fn new() -> Self {
        KeyTouchStats {
            samples: 0,
            mean_x: 0.0,
            mean_y: 0.0,
            m2_x: 0.0,
            m2_y: 0.0,
            cov_xy: 0.0,
        }
    }

    /// Welford online update (numerically stable; O(1) per sample).
    pub fn add(&mut self, p: Point) {
        self.samples += 1;
        let n = f64::from(self.samples);
        let dx = p.x - self.mean_x;
        let dy = p.y - self.mean_y;
        self.mean_x += dx / n;
        self.mean_y += dy / n;
        let dx2 = p.x - self.mean_x;
        let dy2 = p.y - self.mean_y;
        self.m2_x += dx * dx2;
        self.m2_y += dy * dy2;
        self.cov_xy += dx * dy2;
    }

    /// Population variance of x (0 when fewer than 1 sample).
    pub fn variance_x(&self) -> f64 {
        if self.samples == 0 {
            0.0
        } else {
            self.m2_x / f64::from(self.samples)
        }
    }

    /// Population variance of y.
    pub fn variance_y(&self) -> f64 {
        if self.samples == 0 {
            0.0
        } else {
            self.m2_y / f64::from(self.samples)
        }
    }

    pub fn std_x(&self) -> f64 {
        self.variance_x().sqrt()
    }

    pub fn std_y(&self) -> f64 {
        self.variance_y().sqrt()
    }

    /// The mean position (0,0) when no samples have been recorded.
    pub fn mean(&self) -> Point {
        Point::new(self.mean_x, self.mean_y)
    }
}

// ── constraints and configuration ──────────────────────────────────────────

/// Hard geometry invariants. **No optimizer output may violate them** (the
/// ADAPT.BOUNDS.001 court checks this on every population).
///
/// Overlap is measured on the ellipses' axis-aligned bounding boxes (exact,
/// cheap, conservative — it bounds the true geometric overlap).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GeometryConstraints {
    /// Maximum |center displacement| per axis (px).
    pub max_center_dx: f64,
    pub max_center_dy: f64,
    /// Maximum semi-axis growth over the visual baseline (px).
    pub max_expansion_x: f64,
    pub max_expansion_y: f64,
    /// Minimum semi-axes (accessibility: a target can never shrink below this).
    pub min_radius_x: f64,
    pub min_radius_y: f64,
    /// Minimum hit-region area per key (px²).
    pub min_accessible_area: f64,
    /// Maximum permitted bounding-box overlap between neighbor ellipses,
    /// as a fraction of the smaller bounding box.
    pub max_bbox_overlap: f64,
}

impl Default for GeometryConstraints {
    fn default() -> Self {
        GeometryConstraints {
            max_center_dx: 12.0,
            max_center_dy: 8.0,
            max_expansion_x: 10.0,
            max_expansion_y: 6.0,
            min_radius_x: 10.0,
            min_radius_y: 12.0,
            min_accessible_area: 400.0,
            max_bbox_overlap: 0.25,
        }
    }
}

impl GeometryConstraints {
    /// Whether an ellipse violates any constraint relative to its visual
    /// baseline and neighbor set. Deterministic and exact.
    pub fn violated_by(
        &self,
        visual: Rect,
        hit: Ellipse,
        neighbors: &[Ellipse],
    ) -> Option<ConstraintViolation> {
        let c = visual.center();
        if (hit.cx - c.x).abs() > self.max_center_dx + 1e-9 {
            return Some(ConstraintViolation::CenterDisplacement);
        }
        if (hit.cy - c.y).abs() > self.max_center_dy + 1e-9 {
            return Some(ConstraintViolation::CenterDisplacement);
        }
        let rx0 = visual.w / 2.0;
        let ry0 = visual.h / 2.0;
        if hit.rx > rx0 + self.max_expansion_x + 1e-9 {
            return Some(ConstraintViolation::Expansion);
        }
        if hit.ry > ry0 + self.max_expansion_y + 1e-9 {
            return Some(ConstraintViolation::Expansion);
        }
        if hit.rx < self.min_radius_x - 1e-9 || hit.ry < self.min_radius_y - 1e-9 {
            return Some(ConstraintViolation::MinimumSize);
        }
        if hit.area() < self.min_accessible_area - 1e-9 {
            return Some(ConstraintViolation::MinimumSize);
        }
        let b = hit.bbox();
        for nb in neighbors {
            let bn = nb.bbox();
            let inter = bbox_intersection_area(b, bn);
            let smaller = b.width().min(bn.width()) * b.height().min(bn.height());
            if smaller > 0.0 && inter > self.max_bbox_overlap * smaller + 1e-9 {
                return Some(ConstraintViolation::Overlap);
            }
        }
        None
    }
}

/// The first constraint an ellipse violates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConstraintViolation {
    CenterDisplacement,
    Expansion,
    MinimumSize,
    Overlap,
}

/// Convergence / user-control configuration.
///
/// Convergence is deliberately conservative: a small amount of noisy new
/// evidence must not continuously reshape hit regions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AdaptiveConfig {
    /// Master switch (user control: Adaptive Geometry On/Off).
    pub enabled: bool,
    /// When frozen, no learning pass may mutate the effective geometry
    /// (user control: Freeze Current Geometry). Stats may still accumulate;
    /// they simply cannot move the hit regions until unfrozen.
    pub frozen: bool,
    /// Minimum samples per key before its geometry may adapt.
    pub min_samples: u32,
    /// The mean bias must exceed this fraction of the baseline radius
    /// before a center shift is even considered (confidence threshold).
    pub confidence: f64,
    /// Maximum fraction of the desired adjustment applied per optimization
    /// pass (0 < max_update ≤ 1) — prevents jitter from noisy evidence.
    pub max_update: f64,
    /// Minimum relative cost improvement required to apply a change
    /// (hysteresis; 0 = apply any improvement).
    pub hysteresis: f64,
    /// Run an optimization pass only after this many new samples have been
    /// recorded since the last pass (the optimizer never runs on the touch
    /// hot path).
    pub optimize_every: u32,
}

impl Default for AdaptiveConfig {
    fn default() -> Self {
        AdaptiveConfig {
            enabled: true,
            frozen: false,
            min_samples: 8,
            confidence: 0.12,
            max_update: 0.35,
            hysteresis: 0.005,
            optimize_every: 16,
        }
    }
}

// ── datasets and evaluation ────────────────────────────────────────────────

/// One touch sample for replay/evaluation: the intended key index and the
/// touch position. This is the deterministic replay dataset format
/// (same baseline + dataset + optimizer version ⇒ identical output).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub key: usize,
    pub x: f64,
    pub y: f64,
}

/// The measurable result of an evaluation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Evaluation {
    pub dataset: &'static str,
    /// Fraction of samples mis-assigned by the **visual** (rect) hit test.
    pub baseline_error_rate: f64,
    /// Fraction mis-assigned by the **adaptive** (ellipse) hit test.
    pub adaptive_error_rate: f64,
    /// `1 - adaptive/baseline` (negative = adaptive made it worse).
    pub relative_improvement: f64,
    /// Number of constraint violations across all adapted keys (must be 0).
    pub constraints_violated: usize,
}

// ── per-key diagnostics (explainability) ───────────────────────────────────

/// Why a key's effective target is what it is (user control: Inspect /
/// Preview Adaptation).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KeyDiagnostics {
    pub key: usize,
    pub sample_count: u32,
    /// Observed mean touch bias relative to the visual center.
    pub mean_bias: Point,
    pub variance: Point,
    /// The visual (baseline) hit region.
    pub baseline: Ellipse,
    /// The current effective hit region.
    pub current: Ellipse,
    /// The proposed adjustment from the last optimization pass (before the
    /// update gate), if any.
    pub proposed: Option<Ellipse>,
    /// This key's contribution to the total objective (expected cost).
    pub objective_contribution: f64,
    /// The constraint that limited the adjustment, if any.
    pub limiting_constraint: Option<ConstraintViolation>,
}

// ── the adaptive controller ────────────────────────────────────────────────

/// The adaptive geometry controller: per-key visual baseline, current hit
/// ellipse, bounded statistics, frequencies, and the neighbor graph.
#[derive(Debug, Clone)]
pub struct AdaptiveGeometry {
    pub config: AdaptiveConfig,
    pub constraints: GeometryConstraints,
    keys: Vec<KeyGeometry>,
    /// Adjacency: `neighbors[i]` lists the indices of i's neighboring keys
    /// (symmetric, no self-edges).
    neighbors: Vec<Vec<usize>>,
    samples_since_optimize: u32,
    last_proposals: Vec<Option<Ellipse>>,
}

#[derive(Debug, Clone)]
struct KeyGeometry {
    visual: Rect,
    hit: Ellipse,
    stats: KeyTouchStats,
    /// Press frequency (evidence weight + deterministic tie-break).
    frequency: u32,
    /// The constraint that limited the last adjustment (explainability).
    last_limit: Option<ConstraintViolation>,
}

impl AdaptiveGeometry {
    /// Build the controller from the visual geometry and the neighbor graph.
    /// `visual[i]` is key i's rectangle; `neighbors[i]` its adjacency.
    /// Initial hit geometry == the visual baseline (ellipse inscribed in the
    /// rect), so with no learning the behavior is identical to the old
    /// rect hit-testing.
    pub fn new(
        config: AdaptiveConfig,
        constraints: GeometryConstraints,
        visual: &[Rect],
        neighbors: &[Vec<usize>],
    ) -> Self {
        let keys = visual
            .iter()
            .map(|r| {
                let c = r.center();
                KeyGeometry {
                    visual: *r,
                    hit: Ellipse::new(c.x, c.y, r.w / 2.0, r.h / 2.0),
                    stats: KeyTouchStats::new(),
                    frequency: 0,
                    last_limit: None,
                }
            })
            .collect();
        AdaptiveGeometry {
            config,
            constraints,
            keys,
            neighbors: neighbors.to_vec(),
            samples_since_optimize: 0,
            last_proposals: vec![None; visual.len()],
        }
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// The current effective hit ellipse for key `i`.
    pub fn hit(&self, i: usize) -> Ellipse {
        self.keys[i].hit
    }

    /// The visual baseline rect for key `i`.
    pub fn visual(&self, i: usize) -> Rect {
        self.keys[i].visual
    }

    pub fn stats(&self, i: usize) -> KeyTouchStats {
        self.keys[i].stats
    }

    pub fn frequency(&self, i: usize) -> u32 {
        self.keys[i].frequency
    }

    // ── user controls (4.9) ────────────────────────────────────────────────

    pub fn set_enabled(&mut self, enabled: bool) {
        self.config.enabled = enabled;
    }

    pub fn set_frozen(&mut self, frozen: bool) {
        self.config.frozen = frozen;
    }

    pub fn frozen(&self) -> bool {
        self.config.frozen
    }

    /// Reset learning: exact baseline restoration (hit == visual, stats and
    /// frequencies cleared). Deterministic.
    pub fn reset(&mut self) {
        for g in &mut self.keys {
            let c = g.visual.center();
            g.hit = Ellipse::new(c.x, c.y, g.visual.w / 2.0, g.visual.h / 2.0);
            g.stats = KeyTouchStats::new();
            g.frequency = 0;
            g.last_limit = None;
        }
        self.samples_since_optimize = 0;
        self.last_proposals = vec![None; self.keys.len()];
    }

    // ── touch hot path (4.7): constant-time, no optimizer here ─────────────

    /// Record a touch on `key` at `p`. **Evidence rule:** the caller must
    /// only record samples with a meaningful intended-key signal (e.g. an
    /// unambiguous hit — see [`AdaptiveGeometry::hit_test_confidence`]).
    /// Bounded O(1) Welford update + frequency counter.
    pub fn record_hit(&mut self, key: usize, p: Point) {
        if key >= self.keys.len() {
            return;
        }
        self.keys[key].stats.add(p);
        self.keys[key].frequency = self.keys[key].frequency.saturating_add(1);
        self.samples_since_optimize = self.samples_since_optimize.saturating_add(1);
    }

    /// The key under `p`, assigned by **normalized distance** to the hit
    /// ellipses (the deterministic neighbor-competition boundary): the key
    /// with the smallest `((x-cx)/rx)² + ((y-cy)/ry)²` wins; ties break by
    /// higher frequency, then lower index. Returns `None` when the point is
    /// outside every ellipse's *effective* region (distance > 1).
    pub fn hit_test(&self, p: Point) -> Option<usize> {
        self.hit_test_confidence(p).0
    }

    /// [`AdaptiveGeometry::hit_test`] plus the confidence of the assignment
    /// (the normalized distance of the *runner-up* boundary, 0 = dead
    /// center). Confidence drives the intended-key evidence rule: only
    /// unambiguous hits (confidence below a threshold) are recorded as
    /// training data.
    pub fn hit_test_confidence(&self, p: Point) -> (Option<usize>, f64) {
        let mut best: Option<(f64, u32, usize)> = None; // (distance, -freq, index)
        for (i, g) in self.keys.iter().enumerate() {
            let d = g.hit.distance(p);
            if d <= 1.0 {
                let cand = (d, u32::MAX - g.frequency, i);
                if best.is_none_or(|b| cand < b) {
                    best = Some(cand);
                }
            }
        }
        match best {
            Some((d, _, i)) => (Some(i), d),
            None => (None, f64::INFINITY),
        }
    }

    // ── optimization (NOT on the touch hot path) ───────────────────────────

    /// Run a full deterministic optimization pass: propose a candidate
    /// ellipse per key from its bounded statistics, apply the neighbor
    /// competition rule, enforce the hard constraints, and accept the
    /// candidates only when the objective improves beyond the hysteresis and
    /// by at most `max_update` per pass (convergence). Frozen geometry is
    /// never mutated.
    pub fn optimize(&mut self) {
        if !self.config.enabled || self.config.frozen {
            // Still evaluate the objective so `explain()` stays truthful,
            // but do not mutate the effective geometry.
            self.evaluate_and_propose();
            return;
        }
        let mut proposals = self.evaluate_and_propose();
        // Hard-constraint enforcement on the proposals (belt and braces —
        // the candidate construction is already constrained).
        for (i, prop) in proposals.iter_mut().enumerate() {
            if let Some(e) = *prop {
                *prop = Some(self.clamp_to_constraints(i, e));
            }
        }
        // Apply the gated proposals.
        for (i, prop) in proposals.iter().enumerate() {
            if let Some(e) = *prop {
                self.keys[i].hit = e;
            }
        }
        self.last_proposals = proposals;
        self.samples_since_optimize = 0;
    }

    /// The number of new samples since the last optimization pass.
    pub fn samples_since_optimize(&self) -> u32 {
        self.samples_since_optimize
    }

    /// Whether an optimization pass is due (enough new evidence accumulated).
    pub fn optimize_due(&self) -> bool {
        self.samples_since_optimize >= self.config.optimize_every
    }

    /// Compute and gate per-key candidate ellipses; returns the gated
    /// proposals without applying them (pure).
    fn evaluate_and_propose(&mut self) -> Vec<Option<Ellipse>> {
        // 1. propose per key (bias + spread, clamped).
        let mut cand: Vec<Ellipse> = (0..self.keys.len()).map(|i| self.candidate(i)).collect();
        // 2. neighbor competition: resolve both-way expansions deterministically.
        self.competition(&mut cand);
        // 3. hard-constraint clamp.
        for (i, c) in cand.iter_mut().enumerate() {
            *c = self.clamp_to_constraints(i, *c);
        }
        // 4. objective gate (hysteresis + max_update) against the current hit.
        let mut out = vec![None; self.keys.len()];
        for i in 0..self.keys.len() {
            let cur = self.keys[i].hit;
            if cand[i] == cur {
                continue;
            }
            // Cost must improve (not merely change): candidate cost <= current.
            let cur_cost = self.cost(i, cur);
            let cand_cost = self.cost(i, cand[i]);
            if cur_cost == 0.0 {
                out[i] = Some(cand[i]);
                continue;
            }
            let rel_improvement = (cur_cost - cand_cost) / cur_cost;
            if rel_improvement > self.config.hysteresis {
                // move only a fraction of the way (convergence).
                out[i] = Some(self.lerp(cur, cand[i], self.config.max_update));
            }
        }
        out
    }

    /// The per-key candidate from bounded statistics:
    /// center = visual center + clamped mean bias (only when the sample
    /// count and confidence thresholds are met), radii = baseline ± spread.
    fn candidate(&self, i: usize) -> Ellipse {
        let g = &self.keys[i];
        let c = g.visual.center();
        let rx0 = g.visual.w / 2.0;
        let ry0 = g.visual.h / 2.0;
        let st = &g.stats;
        // Confidence gate: enough samples AND a mean bias that exceeds the
        // confidence fraction of the baseline radius.
        let eligible = st.samples >= self.config.min_samples;
        let bias_x = if eligible && (st.mean_x - c.x).abs() >= self.config.confidence * rx0 {
            st.mean_x - c.x
        } else {
            0.0
        };
        let bias_y = if eligible && (st.mean_y - c.y).abs() >= self.config.confidence * ry0 {
            st.mean_y - c.y
        } else {
            0.0
        };
        let cx = (c.x + bias_x).clamp(
            c.x - self.constraints.max_center_dx,
            c.x + self.constraints.max_center_dx,
        );
        let cy = (c.y + bias_y).clamp(
            c.y - self.constraints.max_center_dy,
            c.y + self.constraints.max_center_dy,
        );
        // Radii: baseline + observed spread, bounded by the constraints.
        let rx = (rx0 + st.std_x() * 0.5).clamp(
            self.constraints.min_radius_x,
            rx0 + self.constraints.max_expansion_x,
        );
        let ry = (ry0 + st.std_y() * 0.5).clamp(
            self.constraints.min_radius_y,
            ry0 + self.constraints.max_expansion_y,
        );
        Ellipse::new(cx, cy, rx, ry)
    }

    /// Neighbor competition (4.13): when two neighbors' candidates both
    /// expand toward each other, their bounding boxes may overlap no more
    /// than `max_bbox_overlap` × the smaller box. Both candidates are
    /// clamped symmetrically from the shared midpoint — deterministic.
    fn competition(&self, cand: &mut [Ellipse]) {
        for i in 0..self.keys.len() {
            for &j in &self.neighbors[i] {
                if j <= i {
                    continue; // each pair resolved once
                }
                let bi = cand[i].bbox();
                let bj = cand[j].bbox();
                let inter = bbox_intersection_area(bi, bj);
                let smaller = bi.width().min(bj.width()) * bi.height().min(bj.height());
                if smaller <= 0.0 || inter <= self.constraints.max_bbox_overlap * smaller {
                    continue;
                }
                // Both boxes shrink toward their own centers by the excess
                // overlap (proportional to each box's contribution).
                let excess = inter - self.constraints.max_bbox_overlap * smaller;
                let diag_i = bi.width().min(bi.height());
                let diag_j = bj.width().min(bj.height());
                let total = diag_i + diag_j;
                if total <= 0.0 {
                    continue;
                }
                let shrink_i = excess * (diag_i / total);
                let shrink_j = excess * (diag_j / total);
                cand[i] = shrink_bbox(cand[i], shrink_i);
                cand[j] = shrink_bbox(cand[j], shrink_j);
            }
        }
    }

    /// Clamp an ellipse so it violates no hard constraint (relative to its
    /// own baseline and its neighbors' CURRENT hit ellipses). Records the
    /// limiting constraint for explainability.
    fn clamp_to_constraints(&mut self, i: usize, e: Ellipse) -> Ellipse {
        let g = self.keys[i].visual;
        let c = g.center();
        let rx0 = g.w / 2.0;
        let ry0 = g.h / 2.0;
        let mut out = e;
        out.cx = out.cx.clamp(
            c.x - self.constraints.max_center_dx,
            c.x + self.constraints.max_center_dx,
        );
        out.cy = out.cy.clamp(
            c.y - self.constraints.max_center_dy,
            c.y + self.constraints.max_center_dy,
        );
        out.rx = out.rx.clamp(
            self.constraints.min_radius_x,
            rx0 + self.constraints.max_expansion_x,
        );
        out.ry = out.ry.clamp(
            self.constraints.min_radius_y,
            ry0 + self.constraints.max_expansion_y,
        );
        if out.area() < self.constraints.min_accessible_area {
            let scale =
                (self.constraints.min_accessible_area / out.area().max(f64::EPSILON)).sqrt();
            out.rx = (out.rx * scale).max(self.constraints.min_radius_x);
            out.ry = (out.ry * scale).max(self.constraints.min_radius_y);
        }
        let nbs = self.neighbor_ellipses(i);
        let limit = self.constraints.violated_by(g, out, &nbs);
        if let Some(ConstraintViolation::Overlap) = limit {
            // Conservative: pull the box back toward the visual box.
            let cur = self.keys[i].hit;
            out = self.lerp(cur, out, 0.5);
        }
        // Record the limiting constraint for explainability (re-check after
        // the conservative pull so the reported limit is the final one).
        self.keys[i].last_limit = self.constraints.violated_by(g, out, &nbs);
        out
    }

    /// The hit ellipses of key `i`'s neighbors.
    fn neighbor_ellipses(&self, i: usize) -> Vec<Ellipse> {
        self.neighbors[i]
            .iter()
            .map(|&j| self.keys[j].hit)
            .collect()
    }

    /// Expected input cost of key `i` under ellipse `e`:
    /// `freq × (E[(x-cx)²]/rx² + E[(y-cy)²]/ry²)` computed exactly from the
    /// Welford statistics (variance + squared mean bias) — no tap history.
    fn cost(&self, i: usize, e: Ellipse) -> f64 {
        let st = &self.keys[i].stats;
        if st.samples == 0 {
            return 0.0;
        }
        let ex2 = st.variance_x() + (st.mean_x - e.cx) * (st.mean_x - e.cx);
        let ey2 = st.variance_y() + (st.mean_y - e.cy) * (st.mean_y - e.cy);
        let d = ex2 / (e.rx * e.rx).max(f64::EPSILON) + ey2 / (e.ry * e.ry).max(f64::EPSILON);
        f64::from(self.keys[i].frequency.max(1)) * d
    }

    fn lerp(&self, from: Ellipse, to: Ellipse, t: f64) -> Ellipse {
        let t = t.clamp(0.0, 1.0);
        Ellipse::new(
            from.cx + (to.cx - from.cx) * t,
            from.cy + (to.cy - from.cy) * t,
            from.rx + (to.rx - from.rx) * t,
            from.ry + (to.ry - from.ry) * t,
        )
    }

    // ── explainability (4.10) ──────────────────────────────────────────────

    /// Per-key diagnostics: why is this key's effective target what it is?
    pub fn explain(&self, i: usize) -> KeyDiagnostics {
        let g = &self.keys[i];
        let c = g.visual.center();
        let baseline = Ellipse::new(c.x, c.y, g.visual.w / 2.0, g.visual.h / 2.0);
        let proposal = self.last_proposals[i].unwrap_or(g.hit);
        let proposed = if proposal == g.hit {
            None
        } else {
            Some(proposal)
        };
        KeyDiagnostics {
            key: i,
            sample_count: g.stats.samples,
            mean_bias: Point::new(g.stats.mean_x - c.x, g.stats.mean_y - c.y),
            variance: Point::new(g.stats.variance_x(), g.stats.variance_y()),
            baseline,
            current: g.hit,
            proposed,
            objective_contribution: self.cost(i, g.hit),
            limiting_constraint: g.last_limit,
        }
    }

    // ── evaluation / deterministic replay (4.11, 4.14) ─────────────────────

    /// Feed a dataset through the adaptive pipeline (record + periodic
    /// optimize) — used by replay and by the synthetic populations.
    pub fn feed(&mut self, dataset: &[Sample]) {
        for s in dataset {
            self.record_hit(s.key, Point::new(s.x, s.y));
            if self.optimize_due() {
                self.optimize();
            }
        }
        if self.samples_since_optimize() > 0 {
            self.optimize();
        }
    }

    /// Measure the error rate of a hit-test function against the dataset
    /// (a sample counts as an error when the assigned key ≠ the intended
    /// key).
    fn error_rate<F: Fn(Point) -> Option<usize>>(dataset: &[Sample], hit: F) -> f64 {
        if dataset.is_empty() {
            return 0.0;
        }
        let mut err = 0usize;
        for s in dataset {
            if hit(Point::new(s.x, s.y)) != Some(s.key) {
                err += 1;
            }
        }
        err as f64 / dataset.len() as f64
    }

    /// The visual-rect baseline error rate over `dataset` (uses the
    /// baseline rects, no learning).
    pub fn baseline_error_rate(&self, dataset: &[Sample]) -> f64 {
        Self::error_rate(dataset, |p| {
            self.keys.iter().position(|g| g.visual.contains(p))
        })
    }

    /// The full before/after report (4.14): baseline error, adaptive error,
    /// relative improvement, and constraint violations. Deterministic.
    pub fn evaluate(&mut self, dataset_name: &'static str, dataset: &[Sample]) -> Evaluation {
        let baseline = self.baseline_error_rate(dataset);
        self.feed(dataset);
        let adaptive = Self::error_rate(dataset, |p| self.hit_test(p));
        let violated = (0..self.keys.len())
            .filter(|&i| {
                self.constraints
                    .violated_by(
                        self.keys[i].visual,
                        self.keys[i].hit,
                        &self.neighbor_ellipses(i),
                    )
                    .is_some()
            })
            .count();
        let rel = if baseline > 0.0 {
            (baseline - adaptive) / baseline
        } else {
            0.0
        };
        Evaluation {
            dataset: dataset_name,
            baseline_error_rate: baseline,
            adaptive_error_rate: adaptive,
            relative_improvement: rel,
            constraints_violated: violated,
        }
    }
}

fn shrink_bbox(e: Ellipse, amount: f64) -> Ellipse {
    // Shrink both semi-axes by `amount` (conservative, keeps the center).
    let rx = (e.rx - amount / 2.0).max(1e-6);
    let ry = (e.ry - amount / 2.0).max(1e-6);
    Ellipse::new(e.cx, e.cy, rx, ry)
}

// ── deterministic synthetic populations (4.12) ─────────────────────────────

/// A fixed-seed xorshift64* generator: the synthetic populations are fully
/// deterministic (replayable), with no external dependency.
pub struct SeededRng(u64);

impl SeededRng {
    pub const fn new(seed: u64) -> Self {
        SeededRng(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    /// Next u64 in [0, 2^64).
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Next f64 in [0, 1).
    pub fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Standard-normal-ish sample via the Box–Muller transform.
    pub fn next_gauss(&mut self) -> f64 {
        let u1 = self.next_f64().max(1e-12);
        let u2 = self.next_f64();
        (-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()
    }
}

/// The synthetic user populations. Each is a deterministic generator
/// producing `samples_per_key` samples per key around the visual centers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopulationKind {
    /// Every touch dead center.
    Centered,
    /// Consistent left bias (thumb or hand offset).
    LeftBias,
    /// Consistent right bias.
    RightBias,
    /// Upward thumb arc: y bias grows with x position.
    UpwardThumbArc,
    /// High variance around the centers.
    HighVariance,
    /// Bias grows toward the screen edge (edge-screen reach).
    EdgeScreenBias,
    /// One rarely-used key (sparse evidence must not distort it).
    RareKey,
    /// One very frequently used key (evidence weight).
    FrequentKey,
    /// Outlier-heavy: 10% of samples at ~5× the normal spread.
    OutlierHeavy,
    /// Bimodal: two clusters per key (left- and right-thumb peaks).
    Bimodal,
}

impl PopulationKind {
    pub const ALL: [PopulationKind; 10] = [
        PopulationKind::Centered,
        PopulationKind::LeftBias,
        PopulationKind::RightBias,
        PopulationKind::UpwardThumbArc,
        PopulationKind::HighVariance,
        PopulationKind::EdgeScreenBias,
        PopulationKind::RareKey,
        PopulationKind::FrequentKey,
        PopulationKind::OutlierHeavy,
        PopulationKind::Bimodal,
    ];

    pub fn name(self) -> &'static str {
        match self {
            PopulationKind::Centered => "centered",
            PopulationKind::LeftBias => "left_bias",
            PopulationKind::RightBias => "right_bias",
            PopulationKind::UpwardThumbArc => "upward_thumb_arc",
            PopulationKind::HighVariance => "high_variance",
            PopulationKind::EdgeScreenBias => "edge_screen_bias",
            PopulationKind::RareKey => "rare_key",
            PopulationKind::FrequentKey => "frequent_key",
            PopulationKind::OutlierHeavy => "outlier_heavy",
            PopulationKind::Bimodal => "bimodal",
        }
    }
}

/// Generate a deterministic dataset for `kind` over `visual` geometry
/// (`samples_per_key` per key; the rare-key population uses a fraction).
pub fn synthetic_dataset(
    kind: PopulationKind,
    visual: &[Rect],
    samples_per_key: u32,
    seed: u64,
) -> Vec<Sample> {
    let mut rng = SeededRng::new(seed ^ (visual.len() as u64 * 2_654_435_761));
    // The layout extent (for edge / arc biases that depend on position).
    let max_x = visual
        .iter()
        .map(|r| r.x + r.w)
        .fold(0.0, f64::max)
        .max(1.0);
    let mut out = Vec::new();
    for (i, r) in visual.iter().enumerate() {
        let c = r.center();
        let xf = (c.x / max_x).clamp(0.0, 1.0);
        let (bias, spread, rare) = match kind {
            PopulationKind::Centered => ((0.0, 0.0), (0.05, 0.05), false),
            PopulationKind::LeftBias => ((-0.38, 0.0), (0.10, 0.08), false),
            PopulationKind::RightBias => ((0.38, 0.0), (0.10, 0.08), false),
            PopulationKind::UpwardThumbArc => {
                // y bias grows with x (thumb arc): at the left edge no bias,
                // at the right edge up to -0.25 of the key height.
                ((-0.10, -0.25 * xf), (0.08, 0.08), false)
            }
            PopulationKind::HighVariance => ((0.0, 0.0), (0.35, 0.25), false),
            PopulationKind::EdgeScreenBias => {
                // Outward from the screen center: keys near the left edge
                // are pushed left, near the right edge pushed right.
                let bx = (xf - 0.5) * 0.8;
                ((bx, 0.0), (0.08, 0.08), false)
            }
            PopulationKind::RareKey => ((0.0, 0.0), (0.08, 0.08), i % 7 == 0),
            PopulationKind::FrequentKey
            | PopulationKind::OutlierHeavy
            | PopulationKind::Bimodal => ((0.0, 0.0), (0.08, 0.08), false),
        };
        let n = if rare {
            (samples_per_key / 8).max(1)
        } else if kind == PopulationKind::FrequentKey && i % 5 == 0 {
            samples_per_key * 4
        } else {
            samples_per_key
        };
        let bx = bias.0 * r.w;
        let by = bias.1 * r.h;
        let sx = spread.0 * r.w;
        let sy = spread.1 * r.h;
        for _ in 0..n {
            let (dx, dy) = match kind {
                PopulationKind::Bimodal => {
                    // Two clusters: 55% at -0.25w, 45% at +0.25w (both axes).
                    let m = if rng.next_f64() < 0.55 { -0.25 } else { 0.25 };
                    (
                        m * r.w + rng.next_gauss() * sx,
                        m * r.h + rng.next_gauss() * sy,
                    )
                }
                PopulationKind::OutlierHeavy => {
                    if rng.next_f64() < 0.10 {
                        (rng.next_gauss() * sx * 5.0, rng.next_gauss() * sy * 5.0)
                    } else {
                        (rng.next_gauss() * sx, rng.next_gauss() * sy)
                    }
                }
                _ => (rng.next_gauss() * sx, rng.next_gauss() * sy),
            };
            out.push(Sample {
                key: i,
                x: c.x + bx + dx,
                y: c.y + by + dy,
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn three_key_geometry() -> (AdaptiveGeometry, Vec<Rect>, Vec<Vec<usize>>) {
        // Three horizontal keys: A B C.
        let visual = vec![
            Rect::new(0.0, 0.0, 100.0, 52.0),
            Rect::new(106.0, 0.0, 100.0, 52.0),
            Rect::new(212.0, 0.0, 100.0, 52.0),
        ];
        let neighbors = vec![vec![1], vec![0, 2], vec![1]];
        let ag = AdaptiveGeometry::new(
            AdaptiveConfig::default(),
            GeometryConstraints::default(),
            &visual,
            &neighbors,
        );
        (ag, visual, neighbors)
    }

    #[test]
    fn welford_matches_two_pass_statistics() {
        let mut st = KeyTouchStats::new();
        for p in [
            Point::new(1.0, 2.0),
            Point::new(3.0, 4.0),
            Point::new(5.0, 6.0),
        ] {
            st.add(p);
        }
        assert_eq!(st.samples, 3);
        assert!((st.mean_x - 3.0).abs() < 1e-9);
        assert!((st.mean_y - 4.0).abs() < 1e-9);
        // population variance of {1,3,5} = 8/3
        assert!((st.variance_x() - 8.0 / 3.0).abs() < 1e-9);
    }

    #[test]
    fn ellipse_contains_and_distance() {
        let e = Ellipse::new(0.0, 0.0, 10.0, 20.0);
        assert!(e.contains(Point::new(0.0, 0.0)));
        assert!(e.contains(Point::new(9.0, 0.0)));
        assert!(!e.contains(Point::new(11.0, 0.0)));
        assert!((e.distance(Point::new(0.0, 10.0)) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn initial_hit_geometry_is_the_visual_baseline() {
        let (ag, visual, _) = three_key_geometry();
        for (i, v) in visual.iter().enumerate() {
            let c = v.center();
            let h = ag.hit(i);
            assert!((h.cx - c.x).abs() < 1e-9);
            assert!((h.cy - c.y).abs() < 1e-9);
            assert!((h.rx - v.w / 2.0).abs() < 1e-9);
            assert!((h.ry - v.h / 2.0).abs() < 1e-9);
        }
    }

    #[test]
    fn hit_test_assigns_visual_centers() {
        let (ag, visual, _) = three_key_geometry();
        for (i, v) in visual.iter().enumerate() {
            assert_eq!(ag.hit_test(v.center()), Some(i));
        }
    }

    #[test]
    fn record_hit_then_optimize_shifts_toward_bias() {
        let (mut ag, visual, _) = three_key_geometry();
        // Consistent left bias on key 1 (center x = 156).
        let c = visual[1].center();
        for k in 0..24 {
            ag.record_hit(1, Point::new(c.x - 20.0, c.y));
            let _ = k;
        }
        assert!(ag.optimize_due() || ag.samples_since_optimize() > 0);
        ag.optimize();
        assert!(ag.hit(1).cx < c.x, "hit center must move left");
        // The shift is bounded by the constraints.
        assert!(ag.hit(1).cx >= c.x - 12.0 - 1e-9);
    }

    #[test]
    fn freeze_blocks_geometry_mutation() {
        let (mut ag, visual, _) = three_key_geometry();
        let c = visual[1].center();
        ag.set_frozen(true);
        for _ in 0..24 {
            ag.record_hit(1, Point::new(c.x - 20.0, c.y));
        }
        ag.optimize();
        let before = ag.hit(1);
        ag.optimize();
        assert_eq!(ag.hit(1), before, "frozen geometry must be immutable");
        ag.set_frozen(false);
        ag.optimize();
        assert!(ag.hit(1).cx < c.x, "unfreezing resumes adaptation");
    }

    #[test]
    fn reset_restores_baseline_exactly() {
        let (mut ag, visual, _) = three_key_geometry();
        let c = visual[1].center();
        for _ in 0..24 {
            ag.record_hit(1, Point::new(c.x - 20.0, c.y));
        }
        ag.optimize();
        assert!(ag.hit(1).cx < c.x);
        ag.reset();
        let c1 = visual[1].center();
        let h = ag.hit(1);
        assert!((h.cx - c1.x).abs() < 1e-9);
        assert!((h.cy - c1.y).abs() < 1e-9);
        assert!((h.rx - visual[1].w / 2.0).abs() < 1e-9);
        assert_eq!(ag.stats(1).samples, 0);
        assert_eq!(ag.frequency(1), 0);
    }

    #[test]
    fn constraints_never_violated_after_optimization() {
        let (mut ag, visual, neighbors) = three_key_geometry();
        // Adversarial: all keys push toward the same point.
        for i in 0..visual.len() {
            for _ in 0..32 {
                ag.record_hit(i, Point::new(visual[1].center().x, visual[0].center().y));
            }
        }
        ag.optimize();
        for i in 0..visual.len() {
            let nbs: Vec<Ellipse> = neighbors[i].iter().map(|&j| ag.hit(j)).collect();
            assert_eq!(
                ag.constraints.violated_by(visual[i], ag.hit(i), &nbs),
                None,
                "key {i} violates a constraint"
            );
        }
    }

    #[test]
    fn optimization_is_deterministic() {
        let (mut a, visual, _neighbors) = three_key_geometry();
        let (mut b, _, _) = three_key_geometry();
        let ds = synthetic_dataset(PopulationKind::LeftBias, &visual, 24, 42);
        a.feed(&ds);
        b.feed(&ds);
        for i in 0..visual.len() {
            assert_eq!(a.hit(i), b.hit(i), "key {i} differs across replays");
        }
    }
}
