//! CTC decoding of the recogniser's output (ADR 0006): per time step a probability for every
//! class (0 = blank, 1..=n = the pack's characters, n + 1 = space).

use std::collections::HashMap;

/// A decoded line: text and per-character confidences (the class probability at the step
/// that produced the character).
#[derive(Clone, Debug, PartialEq)]
pub struct Decoded {
    pub text: String,
    pub conf: Vec<f32>,
}

impl Decoded {
    /// Mean character confidence (0 for empty text).
    pub fn mean_conf(&self) -> f32 {
        if self.conf.is_empty() {
            0.0
        } else {
            #[allow(clippy::cast_precision_loss)] // character counts
            let n = self.conf.len() as f32;
            self.conf.iter().sum::<f32>() / n
        }
    }
}

/// Beam decoder weights (pack manifest): every completed word (a token with letters) adds
/// `alpha · score + beta`, where the score is its lexicon score or `oov` if it is not listed.
#[derive(Clone, Copy, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct BeamParams {
    pub alpha: f32,
    pub beta: f32,
    pub oov: f32,
    pub beam: usize,
}

impl BeamParams {
    /// Most beams kept per step (hostile manifests).
    pub const MAX_BEAM: usize = 64;

    pub fn valid(&self) -> bool {
        [self.alpha, self.beta, self.oov]
            .iter()
            .all(|v| v.is_finite() && v.abs() <= 1e3)
            && (1..=Self::MAX_BEAM).contains(&self.beam)
    }
}

/// Word scores (log-probability-like, ≤ 0 for common words) for beam decoding.
#[derive(Clone, Debug, Default)]
pub struct Lexicon {
    words: HashMap<String, f32>,
}

impl Lexicon {
    pub fn insert(&mut self, word: &str, score: f32) {
        let e = self.words.entry(word.to_string()).or_insert(score);
        *e = e.max(score);
    }

    pub fn len(&self) -> usize {
        self.words.len()
    }

    pub fn is_empty(&self) -> bool {
        self.words.is_empty()
    }

    /// Score of a written word: punctuation around it is ignored, case-insensitive fallback
    /// (as written, lower case, capitalised). Tokens without letters (numbers, symbols) are not
    /// words: `None`, so they earn no word bonus either.
    pub fn score(&self, word: &str, oov: f32) -> Option<f32> {
        let core = word.trim_matches(|c: char| !c.is_alphanumeric());
        if !core.chars().any(char::is_alphabetic) {
            return None;
        }
        if let Some(v) = self.words.get(core) {
            return Some(*v);
        }
        let lower = core.to_lowercase();
        if let Some(v) = self.words.get(&lower) {
            return Some(*v);
        }
        let mut cs = lower.chars();
        let cap: String = cs.next().into_iter().flat_map(char::to_uppercase).chain(cs).collect();
        Some(self.words.get(&cap).copied().unwrap_or(oov))
    }
}

/// Classes kept per step: probability above this, at most `MAX_CANDIDATES` of them.
const MIN_PROB: f32 = 1e-3;
const MAX_CANDIDATES: usize = 32;

fn lse(a: f64, b: f64) -> f64 {
    if a == f64::NEG_INFINITY {
        return b;
    }
    if b == f64::NEG_INFINITY {
        return a;
    }
    let m = a.max(b);
    m + ((a - m).exp() + (b - m).exp()).ln()
}

#[derive(Clone, Debug)]
struct Beam {
    /// Emitted classes (1..=n characters, n + 1 space) and their probabilities.
    labels: Vec<usize>,
    conf: Vec<f32>,
    /// Log-probabilities of the prefix ending in blank / in its last label.
    pb: f64,
    pnb: f64,
    /// Lexicon score of the completed words.
    lm: f64,
}

impl Beam {
    fn total(&self) -> f64 {
        lse(self.pb, self.pnb) + self.lm
    }
}

/// The last word of `labels` (after the last space), as text.
fn last_word(labels: &[usize], space: usize, chars: &[String]) -> String {
    let start = labels
        .iter()
        .rposition(|l| *l == space)
        .map_or(0, |i| i.saturating_add(1));
    labels
        .get(start..)
        .unwrap_or(&[])
        .iter()
        .filter_map(|l| chars.get(l.saturating_sub(1)))
        .map(String::as_str)
        .collect()
}

/// CTC prefix beam search with word-lexicon scores (ADR 0006 step (a)); falls back to greedy
/// decoding with an empty lexicon or invalid parameters.
pub fn beam(probs: &[f32], classes: usize, chars: &[String], lex: &Lexicon, p: &BeamParams) -> Decoded {
    if lex.is_empty() || !p.valid() || classes < 2 {
        return greedy(probs, classes, chars);
    }
    let space = classes.saturating_sub(1);
    let (alpha, beta, oov) = (f64::from(p.alpha), f64::from(p.beta), p.oov);
    let word_score = |labels: &[usize]| {
        lex.score(&last_word(labels, space, chars), oov)
            .map_or(0.0, |v| alpha * f64::from(v) + beta)
    };
    let mut beams = vec![Beam {
        labels: Vec::new(),
        conf: Vec::new(),
        pb: 0.0,
        pnb: f64::NEG_INFINITY,
        lm: 0.0,
    }];
    let mut cands: Vec<(usize, f32)> = Vec::with_capacity(MAX_CANDIDATES);
    for step in probs.chunks_exact(classes) {
        cands.clear();
        cands.extend(step.iter().copied().enumerate().skip(1).filter(|(_, v)| *v > MIN_PROB));
        cands.sort_by(|a, b| b.1.total_cmp(&a.1));
        cands.truncate(MAX_CANDIDATES);
        let blank = f64::from(step.first().copied().unwrap_or(0.0).max(1e-30)).ln();
        let mut next: HashMap<Vec<usize>, Beam> = HashMap::new();
        for b in &beams {
            let tot = lse(b.pb, b.pnb);
            {
                let e = next.entry(b.labels.clone()).or_insert_with(|| Beam {
                    labels: b.labels.clone(),
                    conf: b.conf.clone(),
                    pb: f64::NEG_INFINITY,
                    pnb: f64::NEG_INFINITY,
                    lm: b.lm,
                });
                e.pb = lse(e.pb, tot + blank);
            }
            let last = b.labels.last().copied();
            for (k, v) in &cands {
                let lp = f64::from(v.max(1e-30)).ln();
                if Some(*k) == last {
                    // Repeat without a blank between: the same prefix.
                    if let Some(e) = next.get_mut(&b.labels) {
                        e.pnb = lse(e.pnb, b.pnb + lp);
                        if let Some(c) = e.conf.last_mut() {
                            *c = c.max(*v);
                        }
                    }
                }
                let mut labels = b.labels.clone();
                labels.push(*k);
                let from = if Some(*k) == last { b.pb } else { tot };
                let lm = if *k == space && last.is_some_and(|l| l != space) {
                    b.lm + word_score(&b.labels)
                } else {
                    b.lm
                };
                let e = next.entry(labels).or_insert_with_key(|key| {
                    let mut conf = b.conf.clone();
                    conf.push(*v);
                    Beam {
                        labels: key.clone(),
                        conf,
                        pb: f64::NEG_INFINITY,
                        pnb: f64::NEG_INFINITY,
                        lm,
                    }
                });
                e.pnb = lse(e.pnb, from + lp);
            }
        }
        let mut v: Vec<Beam> = next.into_values().collect();
        // Ties broken by the labels: the kept beams never depend on hash order.
        v.sort_by(|a, b| b.total().total_cmp(&a.total()).then_with(|| a.labels.cmp(&b.labels)));
        v.truncate(p.beam);
        beams = v;
    }
    let fin = |b: &Beam| {
        let open = b.labels.last().is_some_and(|l| *l != space);
        b.total() + if open { word_score(&b.labels) } else { 0.0 }
    };
    let Some(best) = beams
        .iter()
        .max_by(|a, b| fin(a).total_cmp(&fin(b)).then_with(|| b.labels.cmp(&a.labels)))
    else {
        return greedy(probs, classes, chars);
    };
    // Text with single spaces, trimmed (as greedy decoding returns it).
    let mut text = String::new();
    let mut conf = Vec::new();
    let mut pending_space: Option<f32> = None;
    for (l, c) in best.labels.iter().zip(&best.conf) {
        if *l == space {
            if !text.is_empty() {
                pending_space = Some(pending_space.map_or(*c, |p| p.max(*c)));
            }
            continue;
        }
        let Some(ch) = chars.get(l.saturating_sub(1)) else {
            continue;
        };
        if let Some(sc) = pending_space.take() {
            text.push(' ');
            conf.push(sc);
        }
        text.push_str(ch);
        conf.push(*c);
    }
    Decoded { text, conf }
}

/// Greedy decoding: best class per step, repeats collapsed, blanks dropped.
pub fn greedy(probs: &[f32], classes: usize, chars: &[String]) -> Decoded {
    let mut text = String::new();
    let mut conf = Vec::new();
    let mut prev = 0usize;
    if classes == 0 {
        return Decoded { text, conf };
    }
    for step in probs.chunks_exact(classes) {
        let (best, p) =
            step.iter().enumerate().fold(
                (0usize, f32::NEG_INFINITY),
                |acc, (k, v)| if *v > acc.1 { (k, *v) } else { acc },
            );
        if best != 0 && best != prev {
            let ch = if best == classes.saturating_sub(1) {
                Some(" ")
            } else {
                chars.get(best.saturating_sub(1)).map(String::as_str)
            };
            if let Some(c) = ch {
                text.push_str(c);
                conf.push(p);
            }
        }
        prev = best;
    }
    let trimmed = text.trim().to_string();
    if trimmed.len() != text.len() {
        // Drop the confidences of trimmed spaces.
        let lead = text.chars().take_while(|c| c.is_whitespace()).count();
        let keep = trimmed.chars().count();
        conf = conf.into_iter().skip(lead).take(keep).collect();
    }
    Decoded { text: trimmed, conf }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    #[test]
    fn greedy_collapses_repeats_and_blanks() {
        let chars: Vec<String> = ["a", "b"].iter().map(|s| (*s).to_string()).collect();
        // classes: blank, a, b, space
        let p = |k: usize| {
            let mut v = vec![0.0f32; 4];
            v[k] = 0.9;
            v
        };
        let probs: Vec<f32> = [1, 1, 0, 1, 2, 3, 2, 0].iter().flat_map(|k| p(*k)).collect();
        let d = greedy(&probs, 4, &chars);
        assert_eq!(d.text, "aab b");
        assert_eq!(d.conf.len(), 5);
        assert!((d.mean_conf() - 0.9).abs() < 1e-6);
    }

    #[test]
    fn beam_prefers_lexicon_words_and_keeps_spaces() {
        let chars: Vec<String> = ["a", "c", "o", "t"].iter().map(|s| (*s).to_string()).collect();
        // classes: blank, a, c, o, t, space; "c?t c?t" with "o" slightly ahead of "a".
        let step = |pairs: &[(usize, f32)]| {
            let mut v = vec![0.001f32; 6];
            for (k, p) in pairs {
                v[*k] = *p;
            }
            v
        };
        let word = [step(&[(2, 0.95)]), step(&[(3, 0.5), (1, 0.45)]), step(&[(4, 0.95)])];
        let mut probs: Vec<f32> = word.iter().flatten().copied().collect();
        probs.extend(step(&[(5, 0.95)]));
        probs.extend(word.iter().flatten().copied());
        assert_eq!(greedy(&probs, 6, &chars).text, "cot cot");
        let mut lex = Lexicon::default();
        lex.insert("cat", -1.0);
        let p = BeamParams {
            alpha: 1.0,
            beta: 2.0,
            oov: -3.0,
            beam: 8,
        };
        let d = beam(&probs, 6, &chars, &lex, &p);
        assert_eq!(d.text, "cat cat");
        assert_eq!(d.conf.len(), 7);
        // No lexicon or bad parameters: greedy.
        assert_eq!(beam(&probs, 6, &chars, &Lexicon::default(), &p).text, "cot cot");
        let bad = BeamParams { beam: 0, ..p };
        assert_eq!(beam(&probs, 6, &chars, &lex, &bad).text, "cot cot");
        assert_eq!(lex.score("(Cat!", -3.0), Some(-1.0));
        assert_eq!(lex.score("42", -3.0), None);
        assert_eq!(lex.score("—", -3.0), None);
        assert_eq!(lex.score("dog", -3.0), Some(-3.0));
    }
}
