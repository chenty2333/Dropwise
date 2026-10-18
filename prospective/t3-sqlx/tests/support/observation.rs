//! Experiment-local comparisons, not a core oracle or automatic contract inference.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Match,
    Mismatch,
    Unknown,
}

pub fn compare<T: Eq>(baselines: [&T; 2], actual: &T, completed: bool) -> Verdict {
    if !completed || baselines[0] != baselines[1] {
        Verdict::Unknown
    } else if actual == baselines[0] {
        Verdict::Match
    } else {
        Verdict::Mismatch
    }
}
