//! Transverse Mercator projection on a sphere (spherical transverse Mercator).
//!
//! The OviMap link's only projection (`OVIMAP_SPHERICAL_TM`, `docs/ovimap-link.md`
//! §3): longitude/latitude (WGS-84 degrees) become CAD drawing units (metres), with
//! the projection centre at the project's centre (`central_meridian` = origin
//! longitude, `k0 = 1`, false easting/northing = 0, `R = 6378137`).
//!
//! Forward (§3.2), with φ, λ, Δλ = λ − λ₀, φ₀ in radians:
//!
//! ```text
//! B = cos(φ) · sin(Δλ)
//! x = R · atanh(B)
//! y = R · ( atan2(sin(φ), cos(φ)·cos(Δλ)) − φ₀ )
//! ```
//!
//! Inverse (§3.3):
//!
//! ```text
//! φ' = y / R + φ₀
//! B  = tanh(x / R)
//! D  = sqrt(1 − B²)
//! φ  = asin( sin(φ') · D )
//! Δλ = atan2( B, cos(φ') · D )
//! λ  = λ₀ + Δλ
//! ```
//!
//! Forward-then-inverse round-trips to floating-point noise only (the unit tests
//! assert `< 1e-9` m, i.e. `< 1e-6` mm, per §3.3). The spherical model itself differs
//! from the ellipsoidal WGS-84 transverse Mercator by up to ~196 m at 50 km from the
//! centre (§3.5, verified by the T10 oracle test below): CAD plane coordinates are
//! **local engineering coordinates**, never geodetic measurements, and must not be
//! used as UTM / Gauss-Krüger coordinates or stitched to survey results.

/// Mean earth radius used by the spherical projection, in metres (WGS-84 semi-major axis,
/// used as a sphere).
pub const EARTH_RADIUS: f64 = 6_378_137.0;

/// Parameters of the spherical transverse Mercator: angles in degrees. The scale factor
/// is fixed at 1 and false easting/northing at 0 by the link spec, so they are not
/// parameters here.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TmercParams {
    /// Latitude of the projection origin (the project centre), in degrees.
    pub origin_lat: f64,
    /// Central meridian (the project centre longitude), in degrees.
    pub central_meridian: f64,
}

impl TmercParams {
    /// Whether the parameters are usable: finite angles in range.
    pub fn is_valid(&self) -> bool {
        self.origin_lat.is_finite()
            && (-90.0..=90.0).contains(&self.origin_lat)
            && self.central_meridian.is_finite()
            && (-180.0..=180.0).contains(&self.central_meridian)
    }
}

/// Project longitude/latitude (degrees) to CAD coordinates (metres, §3.2).
/// Returns `None` for invalid parameters or out-of-range coordinates.
pub fn forward(lon_deg: f64, lat_deg: f64, p: &TmercParams) -> Option<(f64, f64)> {
    if !p.is_valid() || !lon_deg.is_finite() || !lat_deg.is_finite() {
        return None;
    }
    if !(-180.0..=180.0).contains(&lon_deg) || !(-90.0..=90.0).contains(&lat_deg) {
        return None;
    }
    let phi = lat_deg.to_radians();
    let dlam = (lon_deg - p.central_meridian).to_radians();
    let phi0 = p.origin_lat.to_radians();
    let b = phi.cos() * dlam.sin();
    let x = EARTH_RADIUS * b.atanh();
    let y = EARTH_RADIUS * (phi.sin().atan2(phi.cos() * dlam.cos()) - phi0);
    if x.is_finite() && y.is_finite() { Some((x, y)) } else { None }
}

/// Inverse-project CAD coordinates (metres) back to longitude/latitude (degrees, §3.3).
/// Returns `None` for invalid parameters or a non-finite result.
pub fn inverse(x: f64, y: f64, p: &TmercParams) -> Option<(f64, f64)> {
    if !p.is_valid() || !x.is_finite() || !y.is_finite() {
        return None;
    }
    let phi0 = p.origin_lat.to_radians();
    let phi_p = y / EARTH_RADIUS + phi0;
    let b = (x / EARTH_RADIUS).tanh();
    let d = (1.0 - b * b).sqrt();
    let lat = (phi_p.sin() * d).clamp(-1.0, 1.0).asin();
    let dlon = b.atan2(phi_p.cos() * d);
    if !lat.is_finite() || !dlon.is_finite() {
        return None;
    }
    let lon = p.central_meridian + dlon.to_degrees();
    let lat_deg = lat.to_degrees();
    if (-180.0..=180.0).contains(&lon) && (-90.0..=90.0).contains(&lat_deg) { Some((lon, lat_deg)) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn xinyang() -> TmercParams {
        TmercParams { origin_lat: 32.1301, central_meridian: 114.0814 }
    }

    #[test]
    fn origin_projects_to_zero() {
        let p = xinyang();
        let (x, y) = forward(114.0814, 32.1301, &p).unwrap_or((f64::NAN, f64::NAN));
        assert!(x.abs() < 1e-9, "x = {x}");
        assert!(y.abs() < 1e-9, "y = {y}");
    }

    #[test]
    fn one_degree_latitude_is_about_111319_metres() {
        let p = xinyang();
        let (_, y) = forward(114.0814, 33.1301, &p).unwrap_or((f64::NAN, f64::NAN));
        assert!((y - 111_319.490_793_273_57).abs() < 1.0, "y = {y}");
    }

    #[test]
    fn roundtrip_within_one_millionth_of_a_millimetre() {
        let p = xinyang();
        // Points up to ~50 km from the centre, in every direction.
        let pts = [
            (114.0814, 32.1301),
            (114.5814, 32.1301),
            (113.5814, 32.1301),
            (114.0814, 32.5801),
            (114.0814, 31.6801),
            (114.4314, 32.4801),
            (113.7314, 31.7801),
            (114.0814, 32.1302),
            (114.0815, 32.1301),
            (115.0814, 32.1301),
            (114.0814, 33.1301),
        ];
        for (lon, lat) in pts {
            let Some((x, y)) = forward(lon, lat, &p) else { panic!("forward failed for {lon}, {lat}") };
            let Some((lon2, lat2)) = inverse(x, y, &p) else { panic!("inverse failed for {lon}, {lat}") };
            // Re-project and compare in metres: §3.3 promises < 1e-6 mm = 1e-9 m.
            let Some((x2, y2)) = forward(lon2, lat2, &p) else { panic!("re-forward failed") };
            assert!((x2 - x).abs() < 1e-9, "x drift {x} -> {x2}");
            assert!((y2 - y).abs() < 1e-9, "y drift {y} -> {y2}");
        }
    }

    #[test]
    fn hostile_inputs_return_none() {
        let p = xinyang();
        assert!(forward(f64::NAN, 32.0, &p).is_none());
        assert!(forward(114.0, 100.0, &p).is_none());
        assert!(forward(200.0, 32.0, &p).is_none());
        assert!(inverse(f64::NAN, 0.0, &p).is_none());
        let bad = TmercParams { origin_lat: 100.0, central_meridian: 114.0814 };
        assert!(forward(114.0, 32.0, &bad).is_none());
    }

    // ---------- T10: spherical vs ellipsoidal TM oracle (docs/ovimap-link.md §3.5) ----------

    /// WGS-84 ellipsoid transverse Mercator, Snyder's sixth-order series (test oracle
    /// only; production uses the spherical closed form above).
    fn ellipsoid_tm(lon: f64, lat: f64, lon0: f64, lat0: f64) -> (f64, f64) {
        let a = 6_378_137.0f64;
        let f = 1.0 / 298.257_223_563f64;
        let e2 = 2.0 * f - f * f;
        let ep2 = e2 / (1.0 - e2);
        let phi = lat.to_radians();
        let phi0 = lat0.to_radians();
        let dlam = (lon - lon0).to_radians();
        let m = |ph: f64| {
            a * ((1.0 - e2 / 4.0 - 3.0 * e2.powi(2) / 64.0 - 5.0 * e2.powi(3) / 256.0) * ph
                - (3.0 * e2 / 8.0 + 3.0 * e2.powi(2) / 32.0 + 45.0 * e2.powi(3) / 1024.0) * (2.0 * ph).sin()
                + (15.0 * e2.powi(2) / 256.0 + 45.0 * e2.powi(3) / 1024.0) * (4.0 * ph).sin()
                - (35.0 * e2.powi(3) / 3072.0) * (6.0 * ph).sin())
        };
        let sin_phi = phi.sin();
        let n = a / (1.0 - e2 * sin_phi * sin_phi).sqrt();
        let t = phi.tan().powi(2);
        let c = ep2 * phi.cos().powi(2);
        let a_term = dlam * phi.cos();
        let x = n * (a_term + (1.0 - t + c) * a_term.powi(3) / 6.0 + (5.0 - 18.0 * t + t * t + 72.0 * c - 58.0 * ep2) * a_term.powi(5) / 120.0);
        let y = (m(phi) - m(phi0))
            + n * phi.tan()
                * (a_term.powi(2) / 2.0
                    + (5.0 - t + 9.0 * c + 4.0 * c * c) * a_term.powi(4) / 24.0
                    + (61.0 - 58.0 * t + t * t + 600.0 * c - 330.0 * ep2) * a_term.powi(6) / 720.0);
        (x, y)
    }

    /// Web Mercator (EPSG:3857) for the §3.5 cross-check.
    fn web_mercator(lon: f64, lat: f64, lon0: f64, lat0: f64) -> (f64, f64) {
        let r = EARTH_RADIUS;
        let x = r * (lon - lon0).to_radians();
        let y = r
            * (((std::f64::consts::FRAC_PI_4 + lat.to_radians() / 2.0).tan().ln())
                - ((std::f64::consts::FRAC_PI_4 + lat0.to_radians() / 2.0).tan().ln()));
        (x, y)
    }

    /// Max spherical-vs-ellipsoid error over a circle of `radius` metres around the
    /// origin, sampled every 1° of azimuth (§3.5 method), plus the azimuth of the max.
    fn max_error_on_circle(radius: f64) -> (f64, f64) {
        let lat0 = 32.1301f64;
        let lon0 = 114.0814f64;
        let f = 1.0 / 298.257_223_563f64;
        let e2 = 2.0 * f - f * f;
        let phi0 = lat0.to_radians();
        let m0 = EARTH_RADIUS * (1.0 - e2) / (1.0 - e2 * phi0.sin().powi(2)).powf(1.5);
        let n0 = EARTH_RADIUS / (1.0 - e2 * phi0.sin().powi(2)).sqrt();
        let p = xinyang();
        let mut max_err = 0.0f64;
        let mut max_az = 0.0f64;
        for deg in 0..360 {
            let az = (deg as f64).to_radians();
            let east = radius * az.sin();
            let north = radius * az.cos();
            let lat = lat0 + (north / m0).to_degrees();
            let lon = lon0 + (east / (n0 * phi0.cos())).to_degrees();
            let (sx, sy) = forward(lon, lat, &p).unwrap_or((f64::NAN, f64::NAN));
            let (ex, ey) = ellipsoid_tm(lon, lat, lon0, lat0);
            let err = ((sx - ex).powi(2) + (sy - ey).powi(2)).sqrt();
            if err > max_err {
                max_err = err;
                max_az = deg as f64;
            }
        }
        (max_err, max_az)
    }

    #[test]
    fn t10_spherical_vs_ellipsoid_matches_doc_table() {
        // docs/ovimap-link.md §3.5: 1 km → 3.88 m, 5 km → 19.42 m, 10 km → 38.89 m,
        // 50 km → 195.85 m (due south), each within ±2 m.
        for (radius, expected) in [(1_000.0, 3.88), (5_000.0, 19.42), (10_000.0, 38.89), (50_000.0, 195.85)] {
            let (max_err, _) = max_error_on_circle(radius);
            assert!((max_err - expected).abs() <= 2.0, "radius {radius}: max error {max_err}, expected {expected} ± 2");
        }
        let (max_50k, az_50k) = max_error_on_circle(50_000.0);
        assert!((190.0..=200.0).contains(&max_50k), "50 km max {max_50k} not in [190, 200]");
        // The 50 km maximum sits due south (azimuth 180°).
        assert!((az_50k - 180.0).abs() <= 2.0, "50 km max azimuth {az_50k}, expected ~180");
    }

    #[test]
    fn t10_web_mercator_cross_check() {
        // §3.5: Web Mercator vs ellipsoid at 50 km peaks at 9417.80 m (due north).
        let lat0 = 32.1301f64;
        let lon0 = 114.0814f64;
        let f = 1.0 / 298.257_223_563f64;
        let e2 = 2.0 * f - f * f;
        let phi0 = lat0.to_radians();
        let m0 = EARTH_RADIUS * (1.0 - e2) / (1.0 - e2 * phi0.sin().powi(2)).powf(1.5);
        let n0 = EARTH_RADIUS / (1.0 - e2 * phi0.sin().powi(2)).sqrt();
        let mut max_err = 0.0f64;
        for deg in 0..360 {
            let az = (deg as f64).to_radians();
            let lat = lat0 + ((50_000.0 * az.cos()) / m0).to_degrees();
            let lon = lon0 + ((50_000.0 * az.sin()) / (n0 * phi0.cos())).to_degrees();
            let (wx, wy) = web_mercator(lon, lat, lon0, lat0);
            let (ex, ey) = ellipsoid_tm(lon, lat, lon0, lat0);
            let err = ((wx - ex).powi(2) + (wy - ey).powi(2)).sqrt();
            if err > max_err {
                max_err = err;
            }
        }
        assert!((max_err - 9417.80).abs() <= 2.0, "Web Mercator 50 km max {max_err}, expected 9417.80 ± 2");
    }
}
