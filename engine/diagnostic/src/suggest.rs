//! Structured compiler error-code taxonomy and identifier similarity suggestions.

/// Compiler-wide structured diagnostic error-code ranges:
/// - `1000..=1999`: Lexical analysis (`lexer`)
/// - `2000..=2999`: Syntax & heuristic grammar analysis (`parser`)
/// - `3000..=3999`: Name, scope, visibility, import & package resolution (`resolution` / `driver`)
/// - `4000..=4999`: Type, trait, effect, error-set & pattern checking (`resolution`)
/// - `5000..=5999`: NIR lowering & capture/control-flow validation (`nir`)
/// - `6000..=6999`: Bytecode encoding & artifact verification (`codegen` / `driver`)
pub mod codes {
    // ── 3xxx: Name, Scope, Import & Visibility ──────────────────────
    pub const UNDEFINED_NAME: u32 = 3001;
    pub const DUPLICATE_DEFINITION: u32 = 3002;
    pub const VISIBILITY_DENIED: u32 = 3003;
    pub const INVALID_IMPORT: u32 = 3004;
    pub const IMMUTABLE_ASSIGNMENT: u32 = 3005;
    pub const INVALID_ASSIGNMENT_TARGET: u32 = 3006;
    pub const INVALID_CONTROL_FLOW: u32 = 3007;
    pub const CYCLIC_DEPENDENCY: u32 = 3008;

    // ── 4xxx: Types, Calls, Structs, Enums, Traits, Effects & Errors ─
    pub const TYPE_MISMATCH: u32 = 4001;
    pub const ARITY_MISMATCH: u32 = 4002;
    pub const UNKNOWN_MEMBER: u32 = 4003;
    pub const MISSING_FIELD: u32 = 4004;
    pub const DUPLICATE_FIELD: u32 = 4005;
    pub const INVALID_CAST: u32 = 4006;
    pub const INVALID_OPERATOR: u32 = 4007;
    pub const TRAIT_OBLIGATION: u32 = 4008;
    pub const MISSING_TRAIT_METHOD: u32 = 4009;
    pub const UNHANDLED_ERROR_OR_EFFECT: u32 = 4010;
    pub const NON_EXHAUSTIVE_MATCH: u32 = 4011;
    pub const INVALID_PATTERN: u32 = 4012;
    pub const INVALID_AWAIT_OR_ASYNC: u32 = 4013;
    pub const UNSUPPORTED_FEATURE: u32 = 4014;
    pub const UNKNOWN_PARAMETER: u32 = 4015;
    pub const INVALID_TYPE_EXPR: u32 = 4016;

    // ── 5xxx: NIR Lowering ──────────────────────────────────────────
    pub const NIR_LOWERING_ERROR: u32 = 5001;
    pub const NAMED_FN_CAPTURE: u32 = 5002;

    // ── 6xxx: Codegen & Artifact ────────────────────────────────────
    pub const CODEGEN_LIMIT_OR_ENCODING: u32 = 6001;
}

/// Compute the optimal string alignment (restricted Damerau-Levenshtein) distance
/// between two UTF-8 strings in character units, treating adjacent transpositions as cost 1.
pub fn edit_distance(a: &str, b: &str) -> usize {
    if a == b {
        return 0;
    }
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }

    let mut prev2 = vec![0usize; n + 1];
    let mut prev1: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0usize; n + 1];

    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = usize::from(a_chars[i - 1] != b_chars[j - 1]);
            let mut best = (prev1[j] + 1).min(curr[j - 1] + 1).min(prev1[j - 1] + cost);
            if i > 1
                && j > 1
                && a_chars[i - 1] == b_chars[j - 2]
                && a_chars[i - 2] == b_chars[j - 1]
            {
                best = best.min(prev2[j - 2] + 1);
            }
            curr[j] = best;
        }
        std::mem::swap(&mut prev2, &mut prev1);
        std::mem::swap(&mut prev1, &mut curr);
    }
    prev1[n]
}

/// Find the closest candidate identifier to `query` from `candidates`.
///
/// Maximum allowed edit distance scales conservatively with identifier length
/// so short names never produce noisy false-positive suggestions:
/// - length `0..=1`: exact case-insensitive match only
/// - length `2..=3`: max distance `1`
/// - length `4..=7`: max distance `2`
/// - length `8+`: max distance `3`
pub fn suggest_similar<'a>(
    query: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    if query.is_empty() || query == "_" {
        return None;
    }
    let q_len = query.chars().count();
    let max_dist = match q_len {
        0..=1 => 0,
        2..=3 => 1,
        4..=7 => 2,
        _ => 3,
    };
    let query_lower = query.to_lowercase();

    let mut best: Option<(&'a str, usize, bool, usize)> = None;
    for candidate in candidates {
        if candidate.is_empty() || candidate == query || candidate.starts_with("__") {
            continue;
        }
        let c_len = candidate.chars().count();
        if c_len.abs_diff(q_len) > max_dist.max(1) {
            continue;
        }
        let case_exact = candidate.to_lowercase() == query_lower;
        let dist = if case_exact {
            1
        } else {
            let raw = edit_distance(query, candidate);
            let folded = edit_distance(&query_lower, &candidate.to_lowercase());
            raw.min(folded)
        };
        if !case_exact && dist > max_dist {
            continue;
        }
        let len_diff = c_len.abs_diff(q_len);
        let replace = match best {
            None => true,
            Some((best_str, best_dist, best_case, best_len_diff)) => {
                (case_exact && !best_case)
                    || (case_exact == best_case
                        && (dist, len_diff, candidate) < (best_dist, best_len_diff, best_str))
            }
        };
        if replace {
            best = Some((candidate, dist, case_exact, len_diff));
        }
    }
    best.map(|(s, _, _, _)| s)
}

/// Format a standard `"did you mean \`...\`?"` help message if a close match exists.
pub fn did_you_mean<'a>(
    query: &str,
    candidates: impl IntoIterator<Item = &'a str>,
) -> Option<String> {
    suggest_similar(query, candidates).map(|found| format!("did you mean `{found}`?"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn damerau_levenshtein_handles_transpositions_case_and_length_caps() {
        assert_eq!(edit_distance("println", "pirntln"), 1);
        assert_eq!(edit_distance("length", "len"), 3);
        assert_eq!(
            suggest_similar("pirntln", ["print", "println", "eprintln"]),
            Some("println")
        );
        assert_eq!(suggest_similar("myval", ["myVal", "my_var"]), Some("myVal"));
        // Short 2-char query does not match unrelated 2-char name at distance 2.
        assert_eq!(suggest_similar("ab", ["xy", "zw"]), None);
        // Internal `__` names are never suggested to users.
        assert_eq!(suggest_similar("__list_len", ["__list_init"]), None);
    }
}
