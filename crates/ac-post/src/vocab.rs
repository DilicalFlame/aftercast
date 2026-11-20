//! Stage `vocab`: the words this stream is likely to use, from what was typed, run, committed
//! and written around it (`derived/vocab.json`). The transcriber gets them as context and the
//! `correct` stage may only correct towards them.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;

use ac_capture::keys::KeyRecord;
use ac_core::event::EventKind;
use serde::{Deserialize, Serialize};

use crate::ctx::write_json;
use crate::{Ctx, Result, sources};

/// Most terms kept; Whisper reads only about 220 tokens of context.
const MAX_TERMS: usize = 40;
/// A word seen only in typing or chat must appear this often (typos, passwords and
/// one-off chat words stay out).
const MIN_WEAK: u32 = 2;
/// Typing pauses longer than this end a word.
const TYPING_GAP_S: f64 = 5.0;

/// Common English words, never terms. Lowercase.
const STOPWORDS: &str = "
a about above after again against all almost also always am among an and another any anyone
anything are around as ask asked at away back be because been before being below best better
between big both but by call came can cannot case change check come could day did different
do does doing done down during each early easy else end enough even ever every example fact
far few find first for found from full get gets getting give go goes going gone good got great
had has have having he her here him his how however if important in into is it its itself just
keep kind know known large last later least left less let like line little long look looking
lot made make makes making many may maybe me might more most much must my need never new next
no not nothing now number of off often okay ok old on once one only open or order other our
out over own part people place point put quite rather really right run said same say see seem
seen set she should show side since small so some something sometimes start still such sure
take than thank thanks that the their them then there these they thing things think this those
though three through time to today together too top try trying two under until up upon us use
used using very want was way we well were what when where whether which while who whole why will
with within without work working would yeah yes yet you your yours hello hey bro guys please
good morning night stream live chat video watch subscribe like comment lol haha nice cool wow
add fix feat chore docs refactor style perf merge revert wip update remove rename move
redacted https http www com org net html txt echo exit cd ls dir cat rm mv cp mkdir pwd clear
cls init huh hmm umm uh oh ah fuck shit damn ghp
";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Term {
    pub term: String,
    pub score: f64,
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Vocab {
    pub terms: Vec<Term>,
}

#[derive(Default)]
struct Entry {
    /// Spellings seen, with counts; the most common one is used.
    forms: HashMap<String, u32>,
    score: f64,
    /// Times seen in typing, chat or shell commands.
    weak: u32,
    /// Seen in the title, chapters, notes, README or commits.
    strong: bool,
    /// Part of an identifier (`serde_json`, `HashMap`, `Cargo.toml`).
    code: bool,
    sources: BTreeSet<&'static str>,
}

#[derive(Default)]
pub(crate) struct Counter {
    entries: HashMap<String, Entry>,
    stop: BTreeSet<&'static str>,
    secrets: Vec<regex::Regex>,
    /// Words the transcriber already knows (one token in its vocabulary): no hint needed.
    known: HashSet<String>,
}

impl Counter {
    pub(crate) fn new(secrets: Vec<regex::Regex>, known: HashSet<String>) -> Self {
        Self {
            entries: HashMap::new(),
            stop: STOPWORDS.split_whitespace().collect(),
            secrets,
            known,
        }
    }

    fn is_known(&self, word: &str) -> bool {
        let lower = word.to_lowercase();
        let mut capital = lower.clone();
        if let Some(first) = capital.get_mut(..1) {
            first.make_ascii_uppercase();
        }
        [word, &lower, &capital]
            .iter()
            .any(|w| self.known.contains(*w))
    }

    /// Counts the words of `text` from `source` with `weight`.
    pub(crate) fn add(&mut self, text: &str, source: &'static str, weight: f64) {
        for raw in text.split(|c: char| {
            c.is_whitespace()
                || matches!(
                    c,
                    ',' | ';'
                        | '('
                        | ')'
                        | '['
                        | ']'
                        | '{'
                        | '}'
                        | '"'
                        | '\''
                        | '`'
                        | '<'
                        | '>'
                        | '='
                        | '!'
                        | '?'
                        | '*'
                        | '&'
                        | '|'
                        | '$'
                        | '%'
                        | '@'
                        | '^'
                        | '~'
                )
        }) {
            let raw = raw.trim_matches(|c: char| !c.is_ascii_alphanumeric());
            if raw.is_empty() || self.secrets.iter().any(|r| r.is_match(raw)) || random(raw) {
                continue;
            }
            let joined = raw.contains(['_', ':', '.', '/', '-']);
            for part in raw.split(['_', ':', '.', '/', '-']) {
                let n = part.chars().count();
                if !(3..=24).contains(&n)
                    || !part
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '#')
                    || !part.chars().any(|c| c.is_ascii_alphabetic())
                    || random(part)
                {
                    continue;
                }
                let key = part.to_ascii_lowercase();
                if self.stop.contains(key.as_str()) {
                    continue;
                }
                let camel = part.chars().skip(1).any(|c| c.is_ascii_uppercase());
                let e = self.entries.entry(key).or_default();
                *e.forms.entry(part.to_owned()).or_default() += 1;
                e.score += weight;
                e.code |= joined || camel;
                e.sources.insert(source);
                if matches!(source, "typed" | "chat" | "shell") {
                    e.weak += 1;
                } else {
                    e.strong = true;
                }
            }
        }
    }

    /// The best terms, strongest first.
    pub(crate) fn terms(self, max: usize) -> Vec<Term> {
        let known = |w: &str| self.is_known(w);
        let mut terms: Vec<Term> = self
            .entries
            .iter()
            .filter(|(_, e)| e.strong || e.weak >= MIN_WEAK)
            .filter_map(|(_, e)| {
                let mut forms: Vec<(String, u32)> =
                    e.forms.iter().map(|(f, n)| (f.clone(), *n)).collect();
                forms.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
                let term = forms[0].0.clone();
                // A word the transcriber knows needs no hint (put homonyms like "Rust" in
                // post.vocabulary); a one-off with digits is a file name, not a word.
                if known(&term) || (!e.strong && term.chars().any(|c| c.is_ascii_digit())) {
                    return None;
                }
                Some((term, e))
            })
            .map(|(term, e)| {
                let bonus =
                    if e.code { 1.5 } else { 1.0 } * (1.0 + 0.5 * (e.sources.len() - 1) as f64);
                Term {
                    term,
                    score: (e.score * bonus * 10.0).round() / 10.0,
                    sources: e.sources.iter().map(|s| (*s).to_owned()).collect(),
                }
            })
            .collect();
        terms.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.term.cmp(&b.term)));
        terms.truncate(max);
        terms
    }
}

/// A shell command without what nobody says aloud: `echo` text, `-Flags`, `Verb-Noun`
/// cmdlet names.
fn shell_words(cmd: &str) -> String {
    let first = cmd
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_lowercase();
    if matches!(
        first.as_str(),
        "echo" | "write-host" | "write-output" | "printf"
    ) {
        return String::new();
    }
    cmd.split_whitespace()
        .filter(|w| !w.starts_with('-'))
        .filter(|w| {
            let parts: Vec<&str> = w.split('-').collect();
            !(parts.len() == 2
                && parts
                    .iter()
                    .all(|p| p.chars().next().is_some_and(|c| c.is_ascii_uppercase())))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The words of the faster-whisper model's tokenizer that are one token with a leading space
/// (Whisper knows them). Empty if no model is installed.
pub(crate) fn whisper_words(models: &std::path::Path) -> HashSet<String> {
    let tokenizer = fs::read_dir(models)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|d| d.file_name().to_string_lossy().contains("whisper"))
        .flat_map(|d| {
            fs::read_dir(d.path().join("snapshots"))
                .into_iter()
                .flatten()
                .flatten()
        })
        .map(|s| s.path().join("tokenizer.json"))
        .find(|p| p.exists());
    let Some(path) = tokenizer else {
        return HashSet::new();
    };
    let Ok(json) = crate::ctx::read_json::<serde_json::Value>(&path) else {
        return HashSet::new();
    };
    json["model"]["vocab"]
        .as_object()
        .map(|v| {
            v.keys()
                .filter_map(|k| k.strip_prefix('\u{120}'))
                .filter(|w| w.chars().all(char::is_alphabetic))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Looks generated (a token, a hash, an id), not like a word anyone says.
fn random(s: &str) -> bool {
    let n = s.chars().count();
    let digits = s.chars().filter(char::is_ascii_digit).count();
    let hex = s.chars().all(|c| c.is_ascii_hexdigit());
    (n >= 12 && digits > 0 && digits < n) || (n >= 8 && hex && digits > 0)
}

/// What was typed, as text: characters with Backspace and Ctrl+Backspace applied, a space
/// for Enter, Tab, shortcuts and pauses.
pub(crate) fn typed_text(keys: &[KeyRecord]) -> String {
    let mut out: Vec<char> = Vec::new();
    let mut last = None;
    for k in keys {
        if last.is_some_and(|t| k.t.secs_since(t) > TYPING_GAP_S) {
            out.push(' ');
        }
        last = Some(k.t);
        match (k.combo().as_deref(), k.ch.as_deref()) {
            (Some("Ctrl+Backspace"), _) => {
                while out.last() == Some(&' ') {
                    out.pop();
                }
                while out.last().is_some_and(|c| *c != ' ') {
                    out.pop();
                }
            }
            (Some(_), _) => out.push(' '),
            (None, Some(ch)) => {
                out.extend(ch.chars().map(|c| if c.is_control() { ' ' } else { c }))
            }
            (None, None) if k.k == "Backspace" => {
                out.pop();
            }
            (None, None) => out.push(' '),
        }
    }
    out.into_iter().collect()
}

/// `text` without the words that also occur in `other` (case-insensitive).
fn without_words_of(text: &str, other: &str) -> String {
    let split = |s: &str| {
        s.split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect::<BTreeSet<String>>()
    };
    let known = split(other);
    text.split(|c: char| !c.is_alphanumeric() && !matches!(c, '_' | '.' | '-' | ':'))
        .filter(|w| !split(w).iter().all(|p| known.contains(p)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Template tags and links are not things anyone says.
fn readme_text(md: &str) -> String {
    let tags = regex::Regex::new(r"\{\{.*?\}\}|\{%.*?%\}|https?://\S+|\[[^\]]*\]\([^)]*\)")
        .expect("valid regex");
    tags.replace_all(md, " ").into_owned()
}

pub fn run(ctx: &mut Ctx) -> Result<()> {
    let secrets = ctx
        .config
        .capture
        .secret_patterns
        .iter()
        .map(|p| p.0.clone())
        .collect();
    let mut c = Counter::new(secrets, whisper_words(&ctx.home.models_dir()));
    if let Some(title) = &ctx.session.meta.title_at_start {
        c.add(title, "title", 3.0);
    }
    for e in &ctx.events {
        match &e.kind {
            EventKind::Checkpoint { title, .. } | EventKind::QuestSet { title, .. } => {
                c.add(title, "chapters", 3.0)
            }
            EventKind::Mark { label: Some(l), .. } => c.add(l, "notes", 2.0),
            EventKind::Note { text, .. } => c.add(text, "notes", 2.0),
            _ => {}
        }
    }
    if let Ok(md) = fs::read_to_string(ctx.session.readme()) {
        // Only what was written for this stream, not the template every README starts from.
        let seed = ctx
            .home
            .path(&ctx.config.paths.templates)
            .join(&ctx.config.readme.seed_template);
        let seed = fs::read_to_string(seed).unwrap_or_default();
        c.add(&without_words_of(&readme_text(&md), &seed), "readme", 2.0);
    }
    for commit in sources::commits(ctx) {
        c.add(&commit.subject, "commits", 3.0);
        c.add(&commit.repo, "commits", 3.0);
    }
    for line in sources::shell(ctx) {
        c.add(&shell_words(&line.cmd), "shell", 2.0);
    }
    // The channel's own messages are mostly the bot's replies (links, support), not talk.
    for m in sources::chat(ctx).into_iter().filter(|m| !m.is_owner) {
        c.add(&m.text, "chat", 1.0);
    }
    c.add(&typed_text(&sources::keys(ctx)), "typed", 1.0);
    let terms = c.terms(MAX_TERMS);
    ctx.set_message(format!("{} terms", terms.len()));
    write_json(&ctx.derived("vocab.json"), &Vocab { terms })
}

/// `post.vocabulary` first, then this stream's terms, without duplicates.
pub fn all(ctx: &Ctx) -> Vec<String> {
    let auto: Vocab = crate::ctx::read_json(&ctx.derived("vocab.json")).unwrap_or_default();
    let mut out: Vec<String> = Vec::new();
    for t in ctx
        .config
        .post
        .vocabulary
        .iter()
        .cloned()
        .chain(auto.terms.into_iter().map(|t| t.term))
    {
        if !out.iter().any(|o| o.eq_ignore_ascii_case(&t)) {
            out.push(t);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use ac_core::time::Timestamp;

    fn key(t: f64, k: &str, ch: Option<&str>, mods: &[&str]) -> KeyRecord {
        let start: Timestamp = "2026-11-20T10:00:00Z".parse().unwrap();
        KeyRecord {
            t: start.add_secs(t),
            k: k.into(),
            mods: mods.iter().map(|m| m.to_string()).collect(),
            app: "Code.exe".into(),
            ch: ch.map(String::from),
        }
    }

    #[test]
    fn typed_text_applies_edits_and_pauses() {
        let mut keys = Vec::new();
        for (i, c) in "use ratatoui".chars().enumerate() {
            keys.push(key(i as f64 * 0.1, "Key", Some(&c.to_string()), &[]));
        }
        keys.push(key(1.3, "Backspace", None, &[]));
        keys.push(key(1.4, "Backspace", None, &[]));
        keys.push(key(1.5, "KeyI", Some("i"), &[]));
        keys.push(key(10.0, "KeyX", Some("x"), &[]));
        keys.push(key(10.1, "Backspace", None, &["Ctrl"]));
        assert_eq!(typed_text(&keys), "use ratatoi ");
    }

    #[test]
    fn terms_skip_common_words_typos_and_secrets() {
        let secret = regex::Regex::new("ghp_[A-Za-z0-9]{30,}").unwrap();
        let mut c = Counter::new(vec![secret], HashSet::new());
        c.add("Parser with nom", "chapters", 3.0);
        c.add("cargo add nom serde_json", "shell", 2.0);
        c.add("cargo build", "shell", 2.0);
        c.add("feat: HashMap of tokens", "commits", 3.0);
        // Chat words count only when said twice.
        c.add("the borrow checker", "chat", 1.0);
        c.add("borrow checker again", "chat", 1.0);
        c.add("lol axlrose", "chat", 1.0);
        c.add("let x = serde_json::from_str(s); qwerty", "typed", 1.0);
        c.add("serde_json HashMap hunter2", "typed", 1.0);
        c.add(&format!("ghp_{}", "Ab3dEf6hIj".repeat(4)), "typed", 1.0);
        c.add(&format!("ghp_{}", "Ab3dEf6hIj".repeat(4)), "typed", 1.0);
        c.add("deadbeef1234 a1b2c3d4e5f6g7", "shell", 2.0);
        let terms: Vec<String> = c.terms(20).into_iter().map(|t| t.term).collect();
        // Code-shaped words seen in several places rank first.
        assert!(terms[..3].contains(&"serde".to_owned()), "{terms:?}");
        assert!(terms[..3].contains(&"HashMap".to_owned()), "{terms:?}");
        for want in [
            "nom", "HashMap", "json", "cargo", "Parser", "borrow", "checker", "tokens",
        ] {
            assert!(terms.iter().any(|t| t == want), "{want} missing: {terms:?}");
        }
        for not in [
            "the",
            "with",
            "qwerty",
            "hunter2",
            "ghp",
            "deadbeef1234",
            "axlrose",
            "from",
        ] {
            assert!(!terms.iter().any(|t| t == not), "{not} kept: {terms:?}");
        }
    }

    #[test]
    fn readme_keeps_only_what_the_template_does_not_say() {
        let seed = "## Chapters
What I'm doing today:
Questions answered";
        let readme = "## Chapters
What I'm doing today: a parser with nom and serde_json";
        assert_eq!(
            without_words_of(readme, seed),
            "a parser with nom and serde_json"
        );
    }

    #[test]
    fn known_words_shell_flags_and_file_names_are_not_terms() {
        let known: HashSet<String> = ["world", "moment", "Item", "force"]
            .map(String::from)
            .into();
        let mut c = Counter::new(vec![], known);
        c.add("hello World moment", "chapters", 3.0);
        for cmd in [
            "echo hello serde",
            "Remove-Item -Recurse -Force file1.txt",
            "rm file1.txt",
            "cargo add ratatui",
            "cargo add ratatui",
        ] {
            c.add(&shell_words(cmd), "shell", 2.0);
        }
        let terms: Vec<String> = c.terms(20).into_iter().map(|t| t.term).collect();
        // "hello" is a common word; World and moment are known to Whisper; the rest are flags,
        // cmdlets, echo text or a one-off file name.
        assert_eq!(terms, ["cargo", "ratatui"], "{terms:?}");
    }

    #[test]
    fn readme_drops_template_tags_and_links() {
        let t = readme_text("Hi {{ vars.github }} see [docs](https://x.y/z) https://a.b/c ratatui");
        assert!(
            !t.contains("github") && !t.contains("docs") && t.contains("ratatui"),
            "{t}"
        );
    }
}
