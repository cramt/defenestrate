//! The launcher's matching: the query's characters must appear in order
//! (case-insensitively); runs and word starts score higher, so "ff" ranks
//! "Firefox" above "Office Fax".

/// None if `query` isn't a subsequence of `name`, else higher is better.
pub fn score(query: &str, name: &str) -> Option<i32> {
    let name: Vec<char> = name.chars().flat_map(char::to_lowercase).collect();
    let mut score = 0;
    let mut from = 0;
    let mut prev: Option<usize> = None;
    for q in query.chars().flat_map(char::to_lowercase).filter(|c| !c.is_whitespace()) {
        let at = from + name[from..].iter().position(|&c| c == q)?;
        let word_start = at == 0 || !name[at - 1].is_alphanumeric();
        score += 1;
        if word_start {
            score += 8;
        }
        if prev == Some(at.wrapping_sub(1)) {
            score += 5;
        }
        prev = Some(at);
        from = at + 1;
    }
    // Among equal matches, the shorter name is the likelier target.
    Some(score * 100 - name.len() as i32)
}

/// Indices of the names matching `query`, best first; all of them,
/// alphabetically, for an empty query.
pub fn rank<S: AsRef<str>>(query: &str, names: &[S]) -> Vec<usize> {
    let mut hits: Vec<(i32, usize)> = names.iter().enumerate().filter_map(|(i, n)| Some((score(query, n.as_ref())?, i))).collect();
    if query.trim().is_empty() {
        hits.sort_by_key(|&(_, i)| names[i].as_ref().to_lowercase());
    } else {
        hits.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| names[a.1].as_ref().cmp(names[b.1].as_ref())));
    }
    hits.into_iter().map(|(_, i)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_or_nothing() {
        assert!(score("fx", "Firefox").is_some());
        assert!(score("FIRE", "firefox").is_some());
        assert!(score("xf", "Firefox").is_none());
        assert!(score("", "anything").is_some());
    }

    #[test]
    fn word_starts_and_runs_win() {
        let names = ["Office Fax", "Firefox", "Notepad", "Command Prompt"];
        assert_eq!(names[rank("ff", &names)[0]], "Firefox");
        assert_eq!(names[rank("cp", &names)[0]], "Command Prompt");
        assert_eq!(names[rank("note", &names)[0]], "Notepad");
        assert_eq!(rank("zzz", &names), Vec::<usize>::new());
    }

    #[test]
    fn empty_query_lists_everything_alphabetically() {
        let names = ["b", "C", "a"];
        assert_eq!(rank("", &names), [2, 0, 1]);
    }
}
