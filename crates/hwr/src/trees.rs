//! Gradient-boosted trees (ADR 0007 decision 3: "hand-ported trees in Rust"): the format
//! `tools/hwrtrain/train_classifier.py` exports from scikit-learn's
//! `HistGradientBoostingClassifier`, and its evaluation. A model file is input like any other:
//! sizes and node indices are checked when it loads.

use serde::Deserialize;

/// Most trees and nodes accepted (hostile model files).
const MAX_TREES: usize = 10_000;
const MAX_NODES: usize = 1_000_000;

#[derive(Clone, Debug, Deserialize)]
struct Node {
    /// Feature index (inner nodes).
    #[serde(default)]
    f: usize,
    /// Go left if the value is ≤ this.
    #[serde(default)]
    t: f64,
    #[serde(default)]
    l: usize,
    #[serde(default)]
    r: usize,
    /// NaN goes left.
    #[serde(default)]
    nl: bool,
    leaf: bool,
    /// Leaf value (raw score).
    #[serde(default)]
    v: f64,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Model {
    pub features: usize,
    baseline: f64,
    trees: Vec<Vec<Node>>,
    /// Probability at or above which a block counts as writing (0 drawing false positives on
    /// validation).
    pub threshold: f64,
    #[serde(default)]
    pub about: serde_json::Value,
}

impl Model {
    pub fn parse(json: &str) -> Result<Model, String> {
        let m: Model = serde_json::from_str(json).map_err(|e| format!("classifier model: {e}"))?;
        if m.trees.len() > MAX_TREES || m.trees.iter().map(Vec::len).sum::<usize>() > MAX_NODES {
            return Err("classifier model too large".into());
        }
        if !(m.baseline.is_finite() && m.threshold.is_finite() && (0.0..=1.0).contains(&m.threshold)) {
            return Err("classifier model: bad baseline or threshold".into());
        }
        for t in &m.trees {
            if t.is_empty() {
                return Err("classifier model: empty tree".into());
            }
            for n in t {
                let ok = if n.leaf {
                    n.v.is_finite()
                } else {
                    n.f < m.features && n.l < t.len() && n.r < t.len() && !n.t.is_nan()
                };
                if !ok {
                    return Err("classifier model: bad node".into());
                }
            }
        }
        Ok(m)
    }

    /// Probability that `x` is writing.
    pub fn predict(&self, x: &[f64]) -> f64 {
        let mut raw = self.baseline;
        for t in &self.trees {
            let mut i = 0usize;
            // Bounded walk: a tree with a cycle cannot loop forever.
            for _ in 0..t.len() {
                let Some(n) = t.get(i) else { break };
                if n.leaf {
                    raw += n.v;
                    break;
                }
                let v = x.get(n.f).copied().unwrap_or(f64::NAN);
                i = if v.is_nan() {
                    if n.nl { n.l } else { n.r }
                } else if v <= n.t {
                    n.l
                } else {
                    n.r
                };
            }
        }
        1.0 / (1.0 + (-raw).exp())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    #[test]
    fn evaluates_and_rejects_bad_models() {
        let m = Model::parse(
            r#"{"features":1,"baseline":0.0,"threshold":0.5,"trees":[[{"f":0,"t":1.0,"l":1,"r":2,"leaf":false,"nl":true},{"leaf":true,"v":-2.0},{"leaf":true,"v":2.0}]]}"#,
        )
        .unwrap();
        assert!(m.predict(&[0.5]) < 0.2);
        assert!(m.predict(&[3.0]) > 0.8);
        assert!(m.predict(&[f64::NAN]) < 0.2, "NaN goes left");
        for bad in [
            r#"{"features":1,"baseline":0.0,"threshold":0.5,"trees":[[{"f":3,"t":1.0,"l":1,"r":2,"leaf":false}]]}"#,
            r#"{"features":1,"baseline":0.0,"threshold":2.0,"trees":[]}"#,
            r#"{"features":1,"baseline":0.0,"threshold":0.5,"trees":[[{"f":0,"t":1.0,"l":0,"r":9,"leaf":false}]]}"#,
        ] {
            assert!(Model::parse(bad).is_err(), "{bad}");
        }
        // A cyclic tree terminates.
        let c = Model::parse(
            r#"{"features":1,"baseline":0.0,"threshold":0.5,"trees":[[{"f":0,"t":1.0,"l":0,"r":0,"leaf":false}]]}"#,
        )
        .unwrap();
        assert!(c.predict(&[0.0]).is_finite());
    }
}
