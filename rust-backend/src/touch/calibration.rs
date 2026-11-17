// SPDX-License-Identifier: GPL-3.0-or-later
//! Five-point touch calibration solve, ported from agent/src/core/Calibration.cpp.
//!
//! Deliberately free of X and of any device I/O: it takes numbers and returns
//! numbers, so the maths can be pinned down by tests and the UI that collects
//! the taps does not have to know any of it.
//!
//! The problem X presents: a "Coordinate Transformation Matrix" is applied to
//! the digitizer before anything sees a touch, so a tap reports where the
//! pointer LANDED, not where the finger was. Correcting the panel therefore
//! means first undoing the matrix that was in effect while measuring, then
//! fitting target against the recovered raw device coordinate.

/// A point in Edge-local pixels (target) or the raw device coordinate (during
/// the fit). The unit is documented by the caller; the maths is unit-blind.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CalPoint {
    pub x: f64,
    pub y: f64,
}

/// A 3x3 row-major affine, the same shape xinput's Coordinate Transformation
/// Matrix takes. Maps device-normalised coordinates to screen-normalised ones.
pub type Matrix3 = [f64; 9];

pub const K_IDENTITY: Matrix3 = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0];

#[derive(Clone, Debug, Default)]
pub struct CalibrationInput {
    /// Target and measured positions, in pixels local to the Edge screen, in
    /// matching order. Five pairs is the intended count; three is the minimum
    /// for an affine fit and more than five is accepted.
    pub targets: Vec<CalPoint>,
    pub measured: Vec<CalPoint>,
    /// The virtual desktop the matrix maps into, and where the Edge sits in it.
    /// The matrix spans every monitor, not just this one, which is the most
    /// common way to get this calculation wrong.
    pub virtual_width: f64,
    pub virtual_height: f64,
    pub origin_x: f64,
    pub origin_y: f64,
    /// The matrix that was in effect while the taps were measured.
    pub base: Matrix3,
}

#[derive(Clone, Debug)]
pub struct CalibrationResult {
    pub ok: bool,
    pub error: String,
    pub matrix: Matrix3,
    /// How far each corrected point still lands from its target, in Edge
    /// pixels, plus the RMS and worst across all points. Reported so a bad run
    /// can be rejected before anything is written to the device.
    pub residuals_px: Vec<f64>,
    pub rms_px: f64,
    pub worst_px: f64,
}

impl Default for CalibrationResult {
    fn default() -> Self {
        Self {
            ok: false,
            error: String::new(),
            matrix: K_IDENTITY,
            residuals_px: Vec::new(),
            rms_px: 0.0,
            worst_px: 0.0,
        }
    }
}

// Solve A x = b for a 3x3 system by Gauss-Jordan with partial pivoting.
// Returns false if singular.
fn solve3(a: &mut [[f64; 3]; 3], b: &mut [f64; 3], x: &mut [f64; 3]) -> bool {
    for col in 0..3 {
        let mut piv = col;
        for r in col + 1..3 {
            if a[r][col].abs() > a[piv][col].abs() {
                piv = r;
            }
        }
        if a[piv][col].abs() < 1e-12 {
            return false;
        }
        if piv != col {
            let tmp = a[piv];
            a[piv] = a[col];
            a[col] = tmp;
            b.swap(piv, col);
        }
        for r in 0..3 {
            if r == col {
                continue;
            }
            let f = a[r][col] / a[col][col];
            for c in 0..3 {
                a[r][c] -= f * a[col][c];
            }
            b[r] -= f * b[col];
        }
    }
    for i in 0..3 {
        x[i] = b[i] / a[i][i];
    }
    true
}

// Least-squares affine row: find (a,b,c) minimising sum (a*px + b*py + c - q)^2.
// Built as normal equations over the basis [px, py, 1].
fn fit_affine_row(src: &[CalPoint], q: &[f64], out: &mut [f64; 3]) -> bool {
    let mut a = [[0.0; 3]; 3];
    let mut rhs = [0.0; 3];
    for (i, p) in src.iter().enumerate() {
        let basis = [p.x, p.y, 1.0];
        for r in 0..3 {
            for c in 0..3 {
                a[r][c] += basis[r] * basis[c];
            }
            rhs[r] += basis[r] * q[i];
        }
    }
    solve3(&mut a, &mut rhs, out)
}

/// Pure. Writes nothing, touches no device.
pub fn solve_calibration(inp: &CalibrationInput) -> CalibrationResult {
    let mut res = CalibrationResult::default();

    let n = inp.targets.len();
    if n < 3 || inp.measured.len() != n {
        res.error = "need at least three matching target and measured points".to_string();
        return res;
    }
    if inp.virtual_width <= 0.0 || inp.virtual_height <= 0.0 {
        res.error = "virtual desktop size is unknown".to_string();
        return res;
    }

    // Invert the base affine so a measured landing point can be turned back
    // into the raw device coordinate that produced it.
    let (a, b, c) = (inp.base[0], inp.base[1], inp.base[2]);
    let (d, e, f) = (inp.base[3], inp.base[4], inp.base[5]);
    let det = a * e - b * d;
    if det.abs() < 1e-12 {
        res.error = "the matrix in effect during measurement is degenerate".to_string();
        return res;
    }

    let w = inp.virtual_width;
    let h = inp.virtual_height;

    let mut raw: Vec<CalPoint> = Vec::with_capacity(n);
    let mut tx: Vec<f64> = Vec::with_capacity(n);
    let mut ty: Vec<f64> = Vec::with_capacity(n);

    for i in 0..n {
        let pnx = (inp.measured[i].x + inp.origin_x) / w;
        let pny = (inp.measured[i].y + inp.origin_y) / h;
        let dev = CalPoint {
            x: (e * (pnx - c) - b * (pny - f)) / det,
            y: (-d * (pnx - c) + a * (pny - f)) / det,
        };
        raw.push(dev);
        tx.push((inp.targets[i].x + inp.origin_x) / w);
        ty.push((inp.targets[i].y + inp.origin_y) / h);
    }

    let mut row_x = [0.0; 3];
    let mut row_y = [0.0; 3];
    if !fit_affine_row(&raw, &tx, &mut row_x) || !fit_affine_row(&raw, &ty, &mut row_y) {
        res.error = "could not solve: the tapped points are collinear or coincident".to_string();
        return res;
    }

    res.matrix = [
        row_x[0], row_x[1], row_x[2], row_y[0], row_y[1], row_y[2], 0.0, 0.0, 1.0,
    ];

    // Residual: push each raw point back through the solved matrix and measure
    // how far it lands from its target, in Edge pixels rather than normalised
    // units so the number means something to a person.
    let mut sum_sq = 0.0;
    res.residuals_px.reserve(n);
    for i in 0..n {
        let fx = row_x[0] * raw[i].x + row_x[1] * raw[i].y + row_x[2];
        let fy = row_y[0] * raw[i].x + row_y[1] * raw[i].y + row_y[2];
        let dx_px = (fx - tx[i]) * w;
        let dy_px = (fy - ty[i]) * h;
        let dist_px = (dx_px * dx_px + dy_px * dy_px).sqrt();
        res.residuals_px.push(dist_px);
        sum_sq += dist_px * dist_px;
        if dist_px > res.worst_px {
            res.worst_px = dist_px;
        }
    }
    res.rms_px = (sum_sq / n as f64).sqrt();
    res.ok = true;
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    fn near(a: f64, b: f64, eps: f64) -> bool {
        (a - b).abs() < eps
    }

    // The Edge sits below a 3440x1440 main monitor, which is the real topology
    // and the case the virtual-desktop normalisation exists for.
    fn base_case() -> CalibrationInput {
        let mut in_ = CalibrationInput {
            virtual_width: 3440.0,
            virtual_height: 2160.0,
            origin_x: 0.0,
            origin_y: 1440.0,
            base: K_IDENTITY,
            ..Default::default()
        };
        in_.targets = vec![
            CalPoint { x: 307.2, y: 129.6 },
            CalPoint { x: 2252.8, y: 129.6 },
            CalPoint { x: 1280.0, y: 360.0 },
            CalPoint { x: 307.2, y: 590.4 },
            CalPoint { x: 2252.8, y: 590.4 },
        ];
        in_
    }

    #[test]
    fn perfect_run_is_identity_with_no_residual() {
        let mut in_ = base_case();
        in_.measured = in_.targets.clone();
        let r = solve_calibration(&in_);
        assert!(r.ok);
        assert!(r.rms_px < 1e-6);
        assert!(r.worst_px < 1e-6);
        assert_eq!(r.residuals_px.len(), in_.targets.len());
        assert!(near(r.matrix[0], 1.0, 1e-9));
        assert!(near(r.matrix[1], 0.0, 1e-9));
        assert!(near(r.matrix[2], 0.0, 1e-9));
        assert!(near(r.matrix[4], 1.0, 1e-9));
        assert!(near(r.matrix[8], 1.0, 1e-9));
    }

    #[test]
    fn constant_offset_is_fit_exactly() {
        let mut in_ = base_case();
        for t in &in_.targets {
            in_.measured.push(CalPoint { x: t.x + 40.0, y: t.y + 12.0 });
        }
        let r = solve_calibration(&in_);
        assert!(r.ok);
        assert!(r.rms_px < 1e-6);
        // Correcting a +40px x error means translating back by 40 Edge pixels,
        // expressed in the virtual desktop's normalised units.
        assert!(near(r.matrix[2] * in_.virtual_width, -40.0, 1e-6));
        assert!(near(r.matrix[5] * in_.virtual_height, -12.0, 1e-6));
    }

    #[test]
    fn scale_error_is_corrected() {
        let mut in_ = base_case();
        for t in &in_.targets {
            in_.measured.push(CalPoint { x: t.x * 1.05, y: t.y * 0.97 });
        }
        let r = solve_calibration(&in_);
        assert!(r.ok);
        assert!(r.rms_px < 1e-6);
        assert!(near(r.matrix[0], 1.0 / 1.05, 1e-6));
        assert!(near(r.matrix[4], 1.0 / 0.97, 1e-6));
    }

    #[test]
    fn non_identity_base_is_undone_before_fitting() {
        let mut in_ = base_case();
        in_.base = [0.5, 0.0, 0.25, 0.0, 0.5, 0.25, 0.0, 0.0, 1.0];
        in_.measured = in_.targets.clone();
        let r = solve_calibration(&in_);
        assert!(r.ok);
        assert!(r.rms_px < 1e-6);
        // Taps already landing on target means the base matrix was already
        // correct, so the solve must reproduce it rather than return identity.
        assert!(near(r.matrix[0], 0.5, 1e-9));
        assert!(near(r.matrix[2], 0.25, 1e-9));
    }

    #[test]
    fn noisy_run_reports_honest_residual() {
        let mut in_ = base_case();
        let jitter: [[f64; 2]; 5] = [[3.0, -2.0], [-4.0, 1.0], [2.0, 2.0], [-1.0, -3.0], [5.0, 4.0]];
        for (i, t) in in_.targets.iter().enumerate() {
            in_.measured.push(CalPoint {
                x: t.x + jitter[i][0],
                y: t.y + jitter[i][1],
            });
        }
        let r = solve_calibration(&in_);
        assert!(r.ok);
        assert!(r.rms_px > 0.5); // it did not pretend to be perfect
        assert!(r.rms_px < 10.0); // but it did fit
        assert!(r.worst_px >= r.rms_px);
    }

    #[test]
    fn degenerate_inputs_fail_loudly() {
        // Too few points.
        let mut in_ = base_case();
        in_.targets.truncate(2);
        in_.measured = in_.targets.clone();
        assert!(!solve_calibration(&in_).ok);

        // Mismatched counts.
        let mut in_ = base_case();
        in_.measured = in_.targets.clone();
        in_.measured.pop();
        assert!(!solve_calibration(&in_).ok);

        // Singular base matrix.
        let mut in_ = base_case();
        in_.measured = in_.targets.clone();
        in_.base = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0];
        let r = solve_calibration(&in_);
        assert!(!r.ok);
        assert!(!r.error.is_empty());

        // All five taps on one line: an affine fit is underdetermined.
        let mut in_ = base_case();
        in_.targets = vec![
            CalPoint { x: 100.0, y: 360.0 },
            CalPoint { x: 600.0, y: 360.0 },
            CalPoint { x: 1100.0, y: 360.0 },
            CalPoint { x: 1600.0, y: 360.0 },
            CalPoint { x: 2100.0, y: 360.0 },
        ];
        in_.measured = in_.targets.clone();
        assert!(!solve_calibration(&in_).ok);

        // Unknown virtual desktop size.
        let mut in_ = base_case();
        in_.measured = in_.targets.clone();
        in_.virtual_width = 0.0;
        assert!(!solve_calibration(&in_).ok);
    }
}
