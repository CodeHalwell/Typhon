//! The `tyc::contains_secret_literal` heuristics, shared by the plain-literal
//! lint (`tyc check` and the LSP, via [`crate::analyse_secret_literal_bindings`])
//! and the `tyc build` comptime scan ([`comptime_secret_bindings`]).
//!
//! A binding is reported when its **name** names a credential and its
//! **value** is a string that could be one:
//!
//! 1. The name is split into words (`_`, digits, camelCase and
//!    `ACRONYMWord` junctions) and squashed words are segmented against the
//!    vocabulary below (`APIKEYS` → `API` `KEYS`, `dbPASSWORD` → `DB`
//!    `PASSWORD`; `MONKEY` and `PASSPORT` do not segment).
//! 2. A **credential noun** names the secret itself. [`SECRET_NAME_KEYWORDS`]
//!    are credentials on their own (`PASSWORD`, `TOKEN`); a
//!    [`SECRET_KEY_NOUNS`] word (`KEY`, `PASS`, `PWD`) is one only right
//!    after a [`SECRET_NAME_QUALIFIERS`] word (`API_KEY`, `DB_PASS`) and is
//!    ambiguous otherwise (`KEY`, `STRIPE_KEY`), as are the
//!    [`SECRET_VALUE_GATED_NOUNS`] (`DSN`, `COOKIE`). A
//!    [`NON_SECRET_QUALIFIERS`] word (`PRIMARY_KEY`, `PUBLIC_KEY`) rules a
//!    key noun out.
//! 3. A **metadata word** after the noun means the name describes the
//!    secret rather than holding it — `TOKEN_LIMIT`, `PASSWORD_MIN_LENGTH`,
//!    `CREDENTIALS_PATH`, `AUTHORIZATION_URL` ([`SECRET_NAME_METADATA_WORDS`]);
//!    a counting word anywhere does the same (`MAX_TOKENS`,
//!    [`SECRET_NAME_COUNT_WORDS`]).
//! 4. The value must be a string. A clear credential name fires on any
//!    value but a placeholder (empty, one repeated character, a
//!    `<template>`); an ambiguous one only on a credential-shaped value — a
//!    known token prefix (`ghp_`, `sk-`, `AKIA`), a PEM private key, a URL
//!    with an embedded password, or a long, high-entropy, letters-and-digits
//!    run.
//!
//! The `tyc build` scan additionally checks the key of every `env("KEY")`
//! the binding reads, so `comptime let DEPLOY_CFG: str =
//! env("AWS_SECRET_ACCESS_KEY")` is reported even though `DEPLOY_CFG` is
//! not secret-shaped.

use std::collections::{HashMap, HashSet};

use ruff_python_ast::{Expr, ModModule, Stmt};

use crate::ComptimeValue;

/// Credential nouns that name a secret on their own (`PASSWORD`,
/// `GITHUB_TOKEN`, `AWS_SECRET_ACCESS_KEY`). A trailing plural `S` is
/// accepted. This and the other word sets below are the single source both
/// consumers of the heuristic share.
pub const SECRET_NAME_KEYWORDS: &[&str] = &[
    "PASSWORD",
    "PASSWD",
    "PASSPHRASE",
    "SECRET",
    "TOKEN",
    "CREDENTIAL",
    "PRIVKEY",
    "APIKEY",
];

/// Nouns that name a credential only when a [`SECRET_NAME_QUALIFIERS`] word
/// directly precedes them (`API_KEY`, `DB_PASS`, `ADMIN_PWD`); on their own
/// (`KEY`, `SORT_KEY`, `PWD`) they are ambiguous, so the value decides.
pub const SECRET_KEY_NOUNS: &[&str] = &["KEY", "PASS", "PWD", "PIN"];

/// Nouns whose values are credentials only sometimes (`DSN` may or may not
/// embed a password; `COOKIE` may be a cookie's name): always ambiguous, so
/// the value decides.
pub const SECRET_VALUE_GATED_NOUNS: &[&str] = &["DSN", "COOKIE", "AUTHORIZATION", "WEBHOOK"];

/// Words that make a following [`SECRET_KEY_NOUNS`] word a credential.
pub const SECRET_NAME_QUALIFIERS: &[&str] = &[
    "API",
    "ACCESS",
    "AUTH",
    "OAUTH",
    "BEARER",
    "CLIENT",
    "APP",
    "APPLICATION",
    "DB",
    "DATABASE",
    "JWT",
    "CSRF",
    "SECRET",
    "PRIVATE",
    "PRIV",
    "SSH",
    "GPG",
    "PGP",
    "ENCRYPTION",
    "DECRYPTION",
    "CRYPTO",
    "CIPHER",
    "MASTER",
    "SIGNING",
    "HMAC",
    "SESSION",
    "REFRESH",
    "ADMIN",
    "ROOT",
    "SERVICE",
    "ACCOUNT",
    "LICENSE",
    "LICENCE",
    "LOGIN",
    "SMTP",
    "WEBHOOK",
    "GITHUB",
    "GH",
    "PERSONAL",
    "AWS",
];

/// Words that make a following key noun something other than a credential
/// (`PRIMARY_KEY`, `PUBLIC_KEY`, `CACHE_KEY`), and demote a following
/// credential noun to ambiguous (`PUBLIC_TOKEN`).
pub const NON_SECRET_QUALIFIERS: &[&str] = &[
    "PUBLIC",
    "PUB",
    "PRIMARY",
    "FOREIGN",
    "SORT",
    "PARTITION",
    "CACHE",
    "LOOKUP",
    "UNIQUE",
    "COMPOSITE",
    "COMPOUND",
    "SURROGATE",
    "NATURAL",
    "CANDIDATE",
    "IDEMPOTENCY",
    "SHORTCUT",
    "HOT",
    "ROUTING",
    "OBJECT",
    "GROUP",
    "SHARD",
    "DICT",
    "MAP",
    "ROW",
    "COLUMN",
    "SEARCH",
    "SECTION",
    "TRANSLATION",
    "MESSAGE",
    "CONFIG",
];

/// Words that, after the credential noun, make the name describe the secret
/// instead of holding it (`TOKEN_LIMIT`, `API_KEY_HEADER`,
/// `PASSWORD_MIN_LENGTH`, `AWS_ACCESS_KEY_ID`).
pub const SECRET_NAME_METADATA_WORDS: &[&str] = &[
    "COUNT",
    "COUNTS",
    "NUM",
    "NUMBER",
    "LIMIT",
    "LIMITS",
    "LEN",
    "LENGTH",
    "MIN",
    "MAX",
    "SIZE",
    "BITS",
    "TTL",
    "TIMEOUT",
    "EXPIRY",
    "EXPIRE",
    "EXPIRES",
    "EXPIRATION",
    "LIFETIME",
    "LIFESPAN",
    "AGE",
    "DURATION",
    "INTERVAL",
    "WINDOW",
    "DAYS",
    "HOURS",
    "MINUTES",
    "SECONDS",
    "PATH",
    "PATHS",
    "FILE",
    "FILES",
    "FILENAME",
    "DIR",
    "DIRECTORY",
    "FOLDER",
    "LOCATION",
    "URL",
    "URLS",
    "URI",
    "ENDPOINT",
    "HOST",
    "PORT",
    "NAME",
    "NAMES",
    "TYPE",
    "TYPES",
    "KIND",
    "HEADER",
    "HEADERS",
    "PREFIX",
    "SUFFIX",
    "SEPARATOR",
    "SEP",
    "DELIMITER",
    "RATE",
    "THRESHOLD",
    "JAR",
    "ALGORITHM",
    "ALGO",
    "ALG",
    "FORMAT",
    "FIELD",
    "FIELDS",
    "PARAM",
    "PARAMS",
    "ENV",
    "VAR",
    "ID",
    "IDS",
    "INDEX",
    "IDX",
    "POLICY",
    "PATTERN",
    "REGEX",
    "RULE",
    "RULES",
    "LABEL",
    "PROMPT",
    "HINT",
    "MESSAGE",
    "MSG",
    "ERROR",
    "ERR",
    "ROTATION",
    "VERSION",
    "MODE",
    "STRATEGY",
    "PROVIDER",
    "MANAGER",
    "STORE",
    "STORAGE",
    "HASH",
    "HASHER",
    "DIGEST",
    "CHECK",
    "VALIDATOR",
    "VALIDATION",
    "REQUIRED",
    "ENABLED",
    "DISABLED",
    "FLAG",
    "METHOD",
    "SCOPE",
    "SCOPES",
    "AUDIENCE",
    "ISSUER",
    "QUEUE",
    "LOG",
    "COLUMN",
    "TABLE",
    "CLASS",
    "SCHEMA",
];

/// Counting words that make the whole name a quantity wherever they appear
/// (`MAX_TOKENS`, `NUM_KEYS`, `TOTAL_TOKENS`).
pub const SECRET_NAME_COUNT_WORDS: &[&str] = &["MAX", "MIN", "NUM", "N", "TOTAL", "COUNT"];

/// Token prefixes that mark a value as a credential whatever the name
/// (GitHub, GitLab, OpenAI / Anthropic-style `sk-`, Stripe, Slack, AWS,
/// Google, SendGrid, Hugging Face).
const CREDENTIAL_VALUE_PREFIXES: &[&str] = &[
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "ghr_",
    "github_pat_",
    "glpat-",
    "sk-",
    "sk_live_",
    "sk_test_",
    "rk_live_",
    "rk_test_",
    "xoxb-",
    "xoxp-",
    "xoxa-",
    "xoxs-",
    "AKIA",
    "ASIA",
    "AIza",
    "SG.",
    "hf_",
];

/// How strongly a binding name names a credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SecretNameStrength {
    /// Could name a credential (`KEY`, `STRIPE_KEY`, `DATABASE_DSN`): the
    /// value decides.
    Ambiguous,
    /// Names a credential (`API_KEY`, `DB_PASSWORD`, `GITHUB_TOKEN`).
    Clear,
}

fn in_set(set: &[&str], word: &str) -> bool {
    set.contains(&word)
}

/// `word` (upper-case) as a noun of `set`, accepting a trailing plural `S`.
fn noun_in(set: &[&str], word: &str) -> bool {
    in_set(set, word)
        || word
            .strip_suffix('S')
            .is_some_and(|stem| !stem.is_empty() && in_set(set, stem))
}

fn is_credential_noun(word: &str) -> bool {
    noun_in(SECRET_NAME_KEYWORDS, word)
        || noun_in(SECRET_KEY_NOUNS, word)
        || noun_in(SECRET_VALUE_GATED_NOUNS, word)
}

/// Every word a squashed identifier word may be segmented into.
fn is_vocabulary_word(word: &str) -> bool {
    is_credential_noun(word) || in_set(SECRET_NAME_QUALIFIERS, word)
}

/// Split an identifier into upper-cased words at `_`, digits, other
/// non-letters, `lowerUpper` junctions and the `ACRONYMWord` junction.
fn identifier_words(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut words = Vec::new();
    let mut current = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphabetic() {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }
        if !current.is_empty() {
            let prev = chars[i - 1];
            let next_is_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            // `ACRONYMWord` splits before the capital that starts the next
            // word (`APIKey`), but an acronym's plural `s` is not a word
            // (`TOKENs`, `KEYs`).
            let plural_s = chars.get(i + 1) == Some(&'s')
                && chars.get(i + 2).is_none_or(|n| !n.is_lowercase());
            let boundary = (prev.is_lowercase() && c.is_uppercase())
                || (prev.is_uppercase() && c.is_uppercase() && next_is_lower && !plural_s);
            if boundary {
                words.push(std::mem::take(&mut current));
            }
        }
        current.push(c);
    }
    if !current.is_empty() {
        words.push(current);
    }
    words.into_iter().map(|w| w.to_uppercase()).collect()
}

/// Segment a squashed word (`APIKEYS`, `DBPASSWORD`) into vocabulary words,
/// preferring the fewest pieces. `None` when no full segmentation exists.
fn segment(word: &str) -> Option<Vec<String>> {
    let n = word.len();
    if !word.is_ascii() {
        return None;
    }
    // best[i] = fewest pieces covering word[..i], with the split point.
    let mut best: Vec<Option<(usize, usize)>> = vec![None; n + 1];
    best[0] = Some((0, 0));
    for end in 1..=n {
        for start in 0..end {
            let Some((pieces, _)) = best[start] else {
                continue;
            };
            if is_vocabulary_word(&word[start..end])
                && best[end].is_none_or(|(p, _)| pieces + 1 < p)
            {
                best[end] = Some((pieces + 1, start));
            }
        }
    }
    best[n]?;
    let mut out = Vec::new();
    let mut end = n;
    while end > 0 {
        let (_, start) = best[end]?;
        out.push(word[start..end].to_owned());
        end = start;
    }
    out.reverse();
    Some(out)
}

/// The words of `name` with squashed words segmented.
fn name_tokens(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    for word in identifier_words(name) {
        if is_vocabulary_word(&word) {
            out.push(word);
            continue;
        }
        if let Some(pieces) = segment(&word) {
            out.extend(pieces);
            continue;
        }
        // A clear noun closing a squashed word with an unknown head
        // (`GHTOKEN`, `MYPASSWORD`) — no English word ends in these nouns,
        // unlike `MONKEY` / `BYPASS` for the key nouns.
        let suffix = SECRET_NAME_KEYWORDS.iter().find_map(|noun| {
            [noun.to_string(), format!("{noun}S")]
                .into_iter()
                .find(|form| word.len() >= form.len() + 2 && word.ends_with(form.as_str()))
        });
        match suffix {
            Some(form) => {
                out.push(word[..word.len() - form.len()].to_owned());
                out.push(form);
            }
            None => out.push(word),
        }
    }
    out
}

/// How strongly `name` names a credential, or `None` when it does not.
pub fn secret_name_strength(name: &str) -> Option<SecretNameStrength> {
    let tokens = name_tokens(name);
    if tokens.iter().any(|t| in_set(SECRET_NAME_COUNT_WORDS, t)) {
        return None;
    }
    // Only nouns after the last metadata word count: `TOKEN_LIMIT`'s noun
    // is described by `LIMIT`, `SECRET_KEY_BASE`'s is not.
    let first = tokens
        .iter()
        .rposition(|t| in_set(SECRET_NAME_METADATA_WORDS, t))
        .map_or(0, |i| i + 1);
    let mut best = None;
    for i in first..tokens.len() {
        let word = tokens[i].as_str();
        let before = i.checked_sub(1).map(|j| tokens[j].as_str());
        let negated = before.is_some_and(|b| in_set(NON_SECRET_QUALIFIERS, b));
        let qualified = before.is_some_and(|b| in_set(SECRET_NAME_QUALIFIERS, b));
        let strength = if noun_in(SECRET_NAME_KEYWORDS, word) {
            Some(if negated {
                SecretNameStrength::Ambiguous
            } else {
                SecretNameStrength::Clear
            })
        } else if noun_in(SECRET_KEY_NOUNS, word) {
            if negated {
                None
            } else if qualified {
                Some(SecretNameStrength::Clear)
            } else {
                Some(SecretNameStrength::Ambiguous)
            }
        } else if noun_in(SECRET_VALUE_GATED_NOUNS, word) {
            Some(SecretNameStrength::Ambiguous)
        } else {
            None
        };
        best = best.max(strength);
    }
    best
}

/// A value that is no credential whatever the name: empty or whitespace,
/// one repeated character (`x`, `xxxx`, `****`), or a `<placeholder>` /
/// `${TEMPLATE}` / `{{x}}`.
fn is_placeholder_value(value: &str) -> bool {
    let v = value.trim();
    let Some(first) = v.chars().next() else {
        return true;
    };
    if v.chars().all(|c| c == first) {
        return true;
    }
    (v.starts_with('<') && v.ends_with('>'))
        || (v.starts_with("${") && v.ends_with('}'))
        || (v.starts_with("{{") && v.ends_with("}}"))
}

/// Shannon entropy of `value` in bits per character.
fn shannon_entropy(value: &str) -> f64 {
    let mut counts: HashMap<char, usize> = HashMap::new();
    let mut total = 0usize;
    for c in value.chars() {
        *counts.entry(c).or_default() += 1;
        total += 1;
    }
    if total == 0 {
        return 0.0;
    }
    counts
        .values()
        .map(|&n| {
            let p = n as f64 / total as f64;
            -p * p.log2()
        })
        .sum()
}

/// True when `value` looks like a credential on its own: a known token
/// prefix, a PEM private key, a URL with an embedded password, or a run of
/// at least 20 letters-and-digits characters with at least 3.5 bits of
/// entropy per character (hex digests, base64 keys, random tokens — not
/// words, paths or identifiers).
pub fn secret_value_is_credential_shaped(value: &str) -> bool {
    let v = value.trim();
    if CREDENTIAL_VALUE_PREFIXES
        .iter()
        .any(|p| v.starts_with(p) && v.len() >= p.len() + 12)
    {
        return true;
    }
    if v.contains("-----BEGIN") && v.contains("PRIVATE KEY") {
        return true;
    }
    if let Some((_, rest)) = v.split_once("://") {
        let authority = rest.split('/').next().unwrap_or("");
        if let Some((userinfo, _)) = authority.rsplit_once('@') {
            if userinfo
                .split_once(':')
                .is_some_and(|(_, pw)| !pw.is_empty())
            {
                return true;
            }
        }
        return false;
    }
    v.chars().count() >= 20
        && v.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '=' | '.'))
        && v.chars().any(|c| c.is_ascii_digit())
        && v.chars().any(|c| c.is_ascii_alphabetic())
        && shannon_entropy(v) >= 3.5
}

/// Whether a binding of this name-strength holding the string `value` is
/// reported: a clear credential name on any non-placeholder value, an
/// ambiguous one on a credential-shaped value only.
pub fn secret_value_warrants_warning(strength: SecretNameStrength, value: &str) -> bool {
    if is_placeholder_value(value) {
        return false;
    }
    match strength {
        SecretNameStrength::Clear => true,
        SecretNameStrength::Ambiguous => secret_value_is_credential_shaped(value),
    }
}

/// Whether a binding named `name` holding the string `value` is reported.
pub fn secret_binding_warrants_warning(name: &str, value: &str) -> bool {
    secret_name_strength(name).is_some_and(|s| secret_value_warrants_warning(s, value))
}

/// `(binding, env_key)` for every `comptime let` that inlines a secret: its
/// evaluated value is a string, it reads at least one `env("KEY")`
/// (directly or through the `comptime def` functions it calls), and the
/// binding name or one of those keys names a credential — weighted by the
/// value as in [`secret_value_warrants_warning`]. `env_key` is the most
/// secret-shaped key read. A comptime binding initialised from a literal is
/// left to the plain-literal lint. Bindings come back in source order.
pub fn comptime_secret_bindings(
    module: &ModModule,
    values: &HashMap<String, ComptimeValue>,
) -> Vec<(String, String)> {
    let functions: HashMap<&str, &[Stmt]> = module
        .body
        .iter()
        .filter_map(|s| match s {
            Stmt::FunctionDef(f) => Some((f.name.as_str(), f.body.as_slice())),
            _ => None,
        })
        .collect();
    let mut out = Vec::new();
    for stmt in &module.body {
        let Stmt::AnnAssign(a) = stmt else { continue };
        let (Expr::Name(target), Some(rhs)) = (a.target.as_ref(), a.value.as_deref()) else {
            continue;
        };
        let name = target.id.as_str();
        let Some(ComptimeValue::Str(value)) = values.get(name) else {
            continue;
        };
        let mut keys = Vec::new();
        let mut seen_fns = HashSet::new();
        collect_env_keys_expr(rhs, &functions, &mut seen_fns, &mut keys);
        if keys.is_empty() {
            continue;
        }
        let key_strength = |k: &String| secret_name_strength(k);
        let best_key = keys
            .iter()
            .max_by_key(|k| key_strength(k))
            .cloned()
            .unwrap_or_default();
        let strength = secret_name_strength(name).max(key_strength(&best_key));
        if strength.is_some_and(|s| secret_value_warrants_warning(s, value)) {
            out.push((name.to_owned(), best_key));
        }
    }
    out
}

fn collect_env_keys_expr<'a>(
    expr: &'a Expr,
    functions: &HashMap<&'a str, &'a [Stmt]>,
    seen_fns: &mut HashSet<&'a str>,
    keys: &mut Vec<String>,
) {
    use ruff_python_ast::visitor::{walk_expr, Visitor};
    struct V<'a, 'b> {
        functions: &'b HashMap<&'a str, &'a [Stmt]>,
        seen_fns: &'b mut HashSet<&'a str>,
        keys: &'b mut Vec<String>,
    }
    impl<'a> Visitor<'a> for V<'a, '_> {
        fn visit_expr(&mut self, expr: &'a Expr) {
            if let Expr::Call(call) = expr {
                if let Expr::Name(f) = call.func.as_ref() {
                    let callee = f.id.as_str();
                    if callee == "env" {
                        if let Some(Expr::StringLiteral(s)) = call.arguments.args.first() {
                            let key = s.value.to_str().to_owned();
                            if !self.keys.contains(&key) {
                                self.keys.push(key);
                            }
                        }
                    } else if let Some(body) = self.functions.get(callee) {
                        if self.seen_fns.insert(callee) {
                            for stmt in *body {
                                self.visit_stmt(stmt);
                            }
                        }
                    }
                }
            }
            walk_expr(self, expr);
        }
    }
    V {
        functions,
        seen_fns,
        keys,
    }
    .visit_expr(expr);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clear(name: &str) -> bool {
        secret_name_strength(name) == Some(SecretNameStrength::Clear)
    }

    fn ambiguous(name: &str) -> bool {
        secret_name_strength(name) == Some(SecretNameStrength::Ambiguous)
    }

    #[test]
    fn credential_names_are_clear() {
        for name in [
            "API_KEY",
            "OPENAI_API_KEY",
            "APIKEY",
            "APIKEYS",
            "api_keys",
            "apiKey",
            "KEY_APIKEY",
            "API_KEY_FOO",
            "FOO_API_KEY_BAR",
            "DB_PASSWORD",
            "DBPASSWORD",
            "dbPASSWORDString",
            "DB_PASS",
            "DBPASS",
            "DB_PWD",
            "DBPWD",
            "ADMIN_PWD",
            "client_secret",
            "CLIENTSECRET",
            "MY_SECRET",
            "SECRETS",
            "SECRET_KEY",
            "SECRET_KEY_BASE",
            "SECRETKEY",
            "AWS_SECRET_ACCESS_KEY",
            "PRIVATE_KEY",
            "PRIVKEY",
            "SSH_PRIVKEY",
            "PRIVKEY_PEM",
            "SSH_KEY",
            "SSHKEY",
            "SIGNING_KEY",
            "ENCRYPTION_KEY",
            "MASTER_KEY",
            "APP_KEY",
            "APPKEY",
            "TOKEN",
            "TOKENS",
            "TOKEN123",
            "123TOKEN",
            "my123TOKEN",
            "TOKENString",
            "TOKENs",
            "MyToken",
            "myTokenValue",
            "GITHUB_TOKEN",
            "GHTOKEN",
            "GH_TOKEN",
            "ACCESSTOKEN",
            "AUTHTOKEN",
            "OAUTH_TOKEN",
            "OAUTHTOKEN",
            "SlackOAuthSecret",
            "PERSONAL_ACCESS_TOKEN",
            "PERSONALACCESSTOKEN",
            "JWTTOKEN",
            "SESSION_TOKEN",
            "REFRESH_TOKEN",
            "PASSWORD",
            "PASSWD",
            "PASSPHRASE",
            "MYPASSWORD",
            "AWS_CREDENTIALS",
            "AWS_CREDENTIAL",
            "PASSWORD_RESET_TOKEN",
        ] {
            assert!(
                clear(name),
                "`{name}` should name a credential: {:?}",
                name_tokens(name)
            );
        }
    }

    #[test]
    fn descriptive_and_unrelated_names_are_not_credentials() {
        // The review's false-positive list (2026-10-03 §7.8) plus neighbours.
        for name in [
            "PASS_THRESHOLD",
            "PASS_RATE",
            "KEY_COUNT",
            "KEY_SEPARATOR",
            "CACHE_KEY_PREFIX",
            "PRIMARY_KEY",
            "SORT_KEY",
            "PARTITION_KEY",
            "PUBLIC_KEY",
            "PUBLICKEY",
            "DSN_TIMEOUT",
            "COOKIE_JAR",
            "SESSION_COOKIE_NAME",
            "SIGNING_ALGORITHM",
            "PWD_DIR",
            "TOKEN_LIMIT",
            "TOKEN_TYPE",
            "MAX_TOKEN_COUNT",
            "MAX_TOKENS",
            "tokenCount",
            "AUTHORIZATION_URL",
            "AUTHORIZATION_HEADER",
            "CREDENTIALS_PATH",
            "PASSWORD_MIN_LENGTH",
            "MIN_PASSWORD_LENGTH",
            "SESSION_TOKEN_TTL",
            "API_KEY_HEADER",
            "AWS_ACCESS_KEY_ID",
            "GITHUB_TOKEN_PATH",
            "MONKEY",
            "PASSPORT",
            "SECRETARY",
            "TOKENIZER",
            "PASSWORDLESS",
            "BYPASS",
            "KEYSTORE",
            "username",
            "PORT",
            "MAX_RETRIES",
            "USER",
        ] {
            assert_eq!(
                secret_name_strength(name),
                None,
                "`{name}` should not name a credential: {:?}",
                name_tokens(name)
            );
        }
    }

    #[test]
    fn ambiguous_names_defer_to_the_value() {
        for name in [
            "KEY",
            "PWD",
            "PASS",
            "STRIPE_KEY",
            "DATABASE_DSN",
            "SESSION_COOKIE",
            "AUTHORIZATION",
            "WEBHOOK",
            "PUBLIC_TOKEN",
        ] {
            assert!(
                ambiguous(name),
                "`{name}`: {:?}",
                secret_name_strength(name)
            );
        }
        assert!(!secret_binding_warrants_warning("KEY", "name"));
        assert!(!secret_binding_warrants_warning(
            "PWD",
            "/home/user/project"
        ));
        assert!(secret_binding_warrants_warning(
            "STRIPE_KEY",
            concat!("sk_", "live_4eC39HqLyjWDarjtT1zdp7dc")
        ));
        assert!(secret_binding_warrants_warning(
            "DATABASE_DSN",
            "postgres://app:hunter2@db.internal:5432/prod"
        ));
        assert!(!secret_binding_warrants_warning(
            "DATABASE_DSN",
            "postgres://app@db.internal:5432/prod"
        ));
        assert!(!secret_binding_warrants_warning(
            "DATABASE_DSN",
            "sqlite:///app.db"
        ));
        assert!(secret_binding_warrants_warning(
            "KEY",
            "a8f5f167f44f4964e6c998dee827110c"
        ));
    }

    #[test]
    fn clear_names_fire_on_any_real_string_but_not_placeholders() {
        assert!(secret_binding_warrants_warning("DB_PASSWORD", "hunter2"));
        assert!(secret_binding_warrants_warning("API_TOKEN", "abcd"));
        assert!(secret_binding_warrants_warning("API_KEY", "abc"));
        for value in [
            "",
            "   ",
            "x",
            "xxxxxxxx",
            "********",
            "<your token>",
            "${TOKEN}",
            "{{ token }}",
        ] {
            assert!(
                !secret_binding_warrants_warning("API_TOKEN", value),
                "{value:?} is a placeholder"
            );
        }
    }

    #[test]
    fn credential_shaped_values() {
        for v in [
            "ghp_16C7e42F292c6912E7710c838347Ae178B4a",
            "sk-proj-abcdefghijklmnop",
            "AKIAIOSFODNN7EXAMPLE",
            "xoxb-123456789012-abcdefghij",
            "-----BEGIN RSA PRIVATE KEY-----\nMIIE...",
            "https://user:s3cret@example.com/x",
            "a8f5f167f44f4964e6c998dee827110c",
            "dGhpcyBpcyBhIHNlY3JldCBrZXkgMTIzNDU2",
        ] {
            assert!(secret_value_is_credential_shaped(v), "{v:?}");
        }
        for v in [
            "hello world",
            "/usr/local/bin/python3",
            "https://example.com/callback",
            "user_profile_cache_key",
            "application/json",
            "sk-short",
            "ABCDEFGHIJKLMNOPQRSTUVWXYZ",
        ] {
            assert!(!secret_value_is_credential_shaped(v), "{v:?}");
        }
    }

    #[test]
    fn segmentation_prefers_vocabulary_words() {
        assert_eq!(name_tokens("DBPASSWORDS"), ["DB", "PASSWORDS"]);
        assert_eq!(name_tokens("SSHKEYS"), ["SSH", "KEYS"]);
        assert_eq!(name_tokens("TOKENs"), ["TOKENS"]);
        assert_eq!(
            name_tokens("dbPASSWORDString"),
            ["DB", "PASSWORD", "STRING"]
        );
        assert_eq!(name_tokens("OAUTHTOKEN"), ["OAUTH", "TOKEN"]);
        assert_eq!(name_tokens("MONKEY"), ["MONKEY"]);
        assert_eq!(name_tokens("PASSPORT"), ["PASSPORT"]);
        assert_eq!(name_tokens("myTokenValue"), ["MY", "TOKEN", "VALUE"]);
        assert_eq!(name_tokens("APIKey"), ["API", "KEY"]);
    }

    fn comptime_findings(src: &str) -> Vec<(String, String)> {
        let prep = tyc_syntax::preprocess::preprocess(src);
        let module = tyc_syntax::parse_module(&prep.python_source)
            .expect("parse failed")
            .into_syntax();
        let (values, diags) = crate::evaluate_comptime_in_source(
            &module,
            &prep.python_source,
            &prep.comptime_bindings,
            &prep.comptime_functions,
        );
        assert!(!diags.has_errors(), "{:?}", diags.errors());
        comptime_secret_bindings(&module, &values)
    }

    #[test]
    fn comptime_scan_checks_the_env_key_and_the_value() {
        // Unique variable names: other tests run in parallel and touch env.
        std::env::set_var("TYPHON_W306_AWS_SECRET_ACCESS_KEY", "wJalrXUtnFEMI/K7MDENG");
        std::env::set_var("TYPHON_W306_REGION", "eu-west-2");
        std::env::set_var("TYPHON_W306_KEY", "a8f5f167f44f4964e6c998dee827110c");
        let src = "\
comptime def secret_cfg() -> str:
    return env(\"TYPHON_W306_AWS_SECRET_ACCESS_KEY\")

comptime let DEPLOY_CFG: str = env(\"TYPHON_W306_AWS_SECRET_ACCESS_KEY\")
comptime let VIA_FN: str = secret_cfg()
comptime let API_TOKEN: str = env(\"TYPHON_W306_REGION\")
comptime let REGION: str = env(\"TYPHON_W306_REGION\")
comptime let TOKEN_LIMIT: int = int(env(\"TYPHON_W306_UNSET\", \"5\"))
comptime let EMPTY_PASSWORD: str = env(\"TYPHON_W306_UNSET\", \"\")
comptime let SIGNER: str = env(\"TYPHON_W306_KEY\")
comptime let LITERAL_TOKEN: str = \"abcd1234\"
";
        let found = comptime_findings(src);
        let names: Vec<&str> = found.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            ["DEPLOY_CFG", "VIA_FN", "API_TOKEN", "SIGNER"],
            "{found:?}"
        );
        assert_eq!(found[0].1, "TYPHON_W306_AWS_SECRET_ACCESS_KEY");
        assert_eq!(found[2].1, "TYPHON_W306_REGION");
    }

    #[test]
    fn vocabulary_sets_have_no_duplicates() {
        for set in [
            SECRET_NAME_KEYWORDS,
            SECRET_KEY_NOUNS,
            SECRET_VALUE_GATED_NOUNS,
            SECRET_NAME_QUALIFIERS,
            NON_SECRET_QUALIFIERS,
            SECRET_NAME_METADATA_WORDS,
            SECRET_NAME_COUNT_WORDS,
        ] {
            let mut seen = HashSet::new();
            for w in set {
                assert!(seen.insert(w), "duplicate `{w}`");
                assert_eq!(*w, w.to_uppercase(), "`{w}` must be upper-case");
            }
        }
    }
}
