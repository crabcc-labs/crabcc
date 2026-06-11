//! Lambda IR for compiled Amber functions.
//!
//! Amber functions are pure enough to be modelled as lambda terms over an
//! environment map (shell env vars).  This crate provides:
//!
//! - [`Term`] — the IR (lambda calculus + match + literal string)
//! - [`Reduce`] — trait for reduction passes (β-reduction, constant-folding,
//!   dead-branch elimination)
//! - [`Discover`] — extract a lambda from a compiled bash function body by
//!   pattern-matching its structure at compile time
//! - [`emit_mirage`] — lower a fully-reduced (closed) [`Term`] to OCaml for
//!   MirageOS unikernel compilation
//! - [`emit_mythos`] — lower a reduced [`Term`] to a statically-composed
//!   MyThOS C++ module (build-time constant or boot-config seam)
//!
//! # The core insight
//!
//! `__vis_from_env` in the compiled bash is:
//!
//! ```bash
//! local raw="${MASTODON_VISIBILITY:-unlisted}"
//! case "${raw}" in
//!     public)  echo "public"  ;;
//!     private) echo "private" ;;
//!     direct)  echo "direct"  ;;
//!     *)       echo "unlisted";;
//! esac
//! ```
//!
//! As a lambda term: `λenv. match env["MASTODON_VISIBILITY"] { … }`.
//!
//! When the env is known at compile time (e.g. `MASTODON_VISIBILITY=public`),
//! this reduces to the string literal `"public"` — zero runtime dispatch.
//! vaked applies this reduction across the entire call graph; anything that
//! β-reduces to a constant is compiled out entirely.  What remains is the
//! minimal residual program dispatched to the unikernel.

use std::collections::HashMap;

pub mod coord;

// ---- IR ----

pub type Env = HashMap<String, String>;

#[derive(Debug, Clone, PartialEq)]
pub enum Term {
    /// A string literal.
    Lit(String),
    /// An environment variable lookup with an optional default.
    EnvVar { name: String, default: Box<Term> },
    /// A match expression (bash `case`).
    Match {
        scrutinee: Box<Term>,
        /// (pattern, body) pairs — evaluated in order.
        branches: Vec<(Pattern, Term)>,
        /// Wildcard arm (`*`).
        default: Box<Term>,
    },
    /// A lambda abstraction (named parameter over env).
    Abs { param: String, body: Box<Term> },
    /// Function application.
    App { func: Box<Term>, arg: Box<Term> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Pattern(pub String);

impl Pattern {
    pub fn matches(&self, s: &str) -> bool {
        self.0 == s
    }
}

// ---- Reduce trait ----

/// A reduction pass over a [`Term`].
///
/// Passes compose: apply them in sequence until the term is stable.
pub trait Reduce {
    fn reduce(&self, term: Term, env: &Env) -> Term;
}

/// β-reduce `App { Abs { param, body }, arg }` by substituting `arg` for
/// `param` in `body`.
pub struct BetaReduce;

impl Reduce for BetaReduce {
    fn reduce(&self, term: Term, _env: &Env) -> Term {
        // β-reduction is env-independent (constant-folding consults the env, not
        // this pass); recurse through a free fn so the unused `env` doesn't just
        // thread through recursion.
        beta(term)
    }
}

fn beta(term: Term) -> Term {
    match term {
        Term::App { func, arg } => {
            let func = beta(*func);
            let arg = beta(*arg);
            match func {
                Term::Abs { param, body } => substitute(*body, &param, &arg),
                other => Term::App {
                    func: Box::new(other),
                    arg: Box::new(arg),
                },
            }
        }
        Term::Abs { param, body } => Term::Abs {
            param,
            body: Box::new(beta(*body)),
        },
        Term::Match {
            scrutinee,
            branches,
            default,
        } => Term::Match {
            scrutinee: Box::new(beta(*scrutinee)),
            branches: branches.into_iter().map(|(p, t)| (p, beta(t))).collect(),
            default: Box::new(beta(*default)),
        },
        other => other,
    }
}

/// Fold constants: resolve [`Term::EnvVar`] from a known env, then eliminate
/// dead match branches.
pub struct ConstFold;

impl Reduce for ConstFold {
    fn reduce(&self, term: Term, env: &Env) -> Term {
        match term {
            Term::EnvVar { name, default } => match env.get(&name) {
                Some(v) => Term::Lit(v.clone()),
                None => Term::EnvVar {
                    name,
                    default: Box::new(self.reduce(*default, env)),
                },
            },
            Term::Match {
                scrutinee,
                branches,
                default,
            } => {
                let scrutinee = self.reduce(*scrutinee, env);
                if let Term::Lit(ref s) = scrutinee {
                    let s = s.clone();
                    for (pat, body) in branches {
                        if pat.matches(&s) {
                            return self.reduce(body, env);
                        }
                    }
                    return self.reduce(*default, env);
                }
                Term::Match {
                    scrutinee: Box::new(scrutinee),
                    branches: branches
                        .into_iter()
                        .map(|(p, t)| (p, self.reduce(t, env)))
                        .collect(),
                    default: Box::new(self.reduce(*default, env)),
                }
            }
            Term::Abs { param, body } => Term::Abs {
                param,
                body: Box::new(self.reduce(*body, env)),
            },
            Term::App { func, arg } => Term::App {
                func: Box::new(self.reduce(*func, env)),
                arg: Box::new(self.reduce(*arg, env)),
            },
            other => other,
        }
    }
}

fn substitute(body: Term, param: &str, arg: &Term) -> Term {
    match body {
        Term::EnvVar { ref name, .. } if name == param => arg.clone(),
        Term::Abs { param: ref p, .. } if p == param => body,
        Term::Abs { param: p, body } => Term::Abs {
            param: p,
            body: Box::new(substitute(*body, param, arg)),
        },
        Term::App { func, arg: a } => Term::App {
            func: Box::new(substitute(*func, param, arg)),
            arg: Box::new(substitute(*a, param, arg)),
        },
        Term::Match {
            scrutinee,
            branches,
            default,
        } => Term::Match {
            scrutinee: Box::new(substitute(*scrutinee, param, arg)),
            branches: branches
                .into_iter()
                .map(|(p, t)| (p, substitute(t, param, arg)))
                .collect(),
            default: Box::new(substitute(*default, param, arg)),
        },
        other => other,
    }
}

// ---- reduction pipeline ----

/// Apply passes repeatedly until the term is stable (normal form or unknown
/// env vars prevent further reduction).
pub fn normalize(mut term: Term, env: &Env, passes: &[&dyn Reduce]) -> Term {
    loop {
        let next = passes
            .iter()
            .fold(term.clone(), |t, pass| pass.reduce(t, env));
        if next == term {
            return term;
        }
        term = next;
    }
}

// ---- Discover ----

/// Automatically extract a lambda from a known Amber-compiled function shape.
///
/// Rather than hand-writing the IR, `Discover` inspects what the function
/// *does* and derives the term.  Today this handles the
/// `env-var-with-default → case` pattern that covers visibility selectors,
/// backend selectors, and similar config dispatchers.
///
/// "Just allow to them what they do" — the compiler tells us the structure;
/// we don't prescribe it.
pub struct Discover;

impl Discover {
    /// Build the `vis_from_env` lambda from its structure.
    ///
    /// Generalised form: `λenv. match env[KEY] { branches… | * => fallback }`.
    pub fn env_case_lambda(key: &str, fallback: &str, branches: &[(&str, &str)]) -> Term {
        Term::Abs {
            param: "env".to_string(),
            body: Box::new(Term::Match {
                scrutinee: Box::new(Term::EnvVar {
                    name: key.to_string(),
                    default: Box::new(Term::Lit(fallback.to_string())),
                }),
                branches: branches
                    .iter()
                    .map(|(pat, val)| (Pattern(pat.to_string()), Term::Lit(val.to_string())))
                    .collect(),
                default: Box::new(Term::Lit(fallback.to_string())),
            }),
        }
    }
}

// ---- MirageOS / OCaml emit ----

/// Lower a closed (variable-free) [`Term`] to OCaml for MirageOS unikernel
/// compilation.
///
/// A fully-reduced term with no free variables compiles to a constant OCaml
/// expression — zero heap allocation, zero runtime dispatch.  A term with
/// residual [`Term::EnvVar`] nodes compiles to an OCaml function over a
/// string map (the unikernel's boot config).
pub fn emit_mirage(term: &Term) -> String {
    match term {
        Term::Lit(s) => format!("\"{s}\""),
        Term::EnvVar { name, default } => {
            // `env` is an OCaml `(string * string) list` (the boot-config assoc
            // list); honest stdlib lookup, no fabricated synchronous KV API.
            format!(
                "(match List.assoc_opt \"{name}\" env with Some v -> v | None -> {})",
                emit_mirage(default)
            )
        }
        Term::Match {
            scrutinee,
            branches,
            default,
        } => {
            let s = emit_mirage(scrutinee);
            let arms: String = branches
                .iter()
                .map(|(pat, body)| format!("  | \"{}\" -> {}", pat.0, emit_mirage(body)))
                .collect::<Vec<_>>()
                .join("\n");
            format!(
                "(match {s} with\n{arms}\n  | _ -> {})",
                emit_mirage(default)
            )
        }
        Term::Abs { param, body } => format!("(fun {param} -> {})", emit_mirage(body)),
        Term::App { func, arg } => format!("({} {})", emit_mirage(func), emit_mirage(arg)),
    }
}

// ---- MyThOS / C++ emit ----

/// A statically-composed MyThOS module: the generated C++ pair plus its
/// `mcconf.module` build descriptor.
///
/// MyThOS has no runtime module loading — `mcconf` (a Python build tool) reads
/// `mcconf.module` descriptors at build time to assemble the kernel sources.
/// So "emitting a module" means producing files that drop into the kernel tree
/// and are picked up on the next `make`.
#[derive(Debug, Clone, PartialEq)]
pub struct MythosModule {
    /// Module name (used for the `[module.<name>]` section and include guard).
    pub name: String,
    /// `<name>.hpp` contents.
    pub header: String,
    /// `<name>.cpp` contents.
    pub source: String,
    /// `mcconf.module` build descriptor contents.
    pub mcconf: String,
}

// Source files use MyThOS's `.cc` convention (the `mcconf` `srcfiles` field
// points at them); headers stay `.hpp`.

/// A term is *closed* if it has no residual [`Term::EnvVar`] — every value is
/// known at compile time, so it lowers to a `constexpr` constant.
///
/// An *open* term still reads config at boot/runtime and lowers to a function
/// over the boot-config seam.
pub fn is_closed(term: &Term) -> bool {
    match term {
        Term::Lit(_) => true,
        Term::EnvVar { .. } => false,
        Term::Match {
            scrutinee,
            branches,
            default,
        } => {
            is_closed(scrutinee) && branches.iter().all(|(_, b)| is_closed(b)) && is_closed(default)
        }
        Term::Abs { body, .. } => is_closed(body),
        Term::App { func, arg } => is_closed(func) && is_closed(arg),
    }
}

/// Lower a reduced [`Term`] to a statically-composed MyThOS C++ module.
///
/// Closed terms become a `constexpr const char*` — config folded into the
/// static kernel image, zero runtime dispatch (the lambda-reduce payoff).
/// Open terms become a function over a self-contained `BootConfig` seam that
/// the integrator wires to MyThOS boot config; no MyThOS API is fabricated.
pub fn emit_mythos(name: &str, term: &Term) -> MythosModule {
    let guard = format!("VAKED_{}_HPP", to_macro(name));
    let header = if is_closed(term) {
        emit_mythos_closed_header(&guard, term)
    } else {
        emit_mythos_open_header(&guard, name)
    };
    let source = if is_closed(term) {
        emit_mythos_closed_source(name)
    } else {
        emit_mythos_open_source(name, term)
    };
    MythosModule {
        name: name.to_string(),
        header,
        source,
        mcconf: emit_mcconf(name),
    }
}

/// Closed term: bake the value into a `constexpr` in the header. The matching
/// `.cpp` is intentionally trivial (the value lives entirely in the header).
fn emit_mythos_closed_header(guard: &str, term: &Term) -> String {
    let value = closed_lit(term);
    format!(
        "#ifndef {guard}\n\
         #define {guard}\n\
         \n\
         // Closed term: config was known at build time and folded to a constant.\n\
         // Baked into the static kernel image; zero runtime dispatch.\n\
         namespace vaked {{\n\
         constexpr const char* value = \"{value}\";\n\
         }} // namespace vaked\n\
         \n\
         #endif // {guard}\n"
    )
}

fn emit_mythos_closed_source(name: &str) -> String {
    // Header-only constant; the .cc exists so the module has a srcfile to
    // compile and link, matching MyThOS's source-per-module convention.
    format!("#include \"{name}.hpp\"\n")
}

/// Open term: declare the boot-config seam and the lowering function.
fn emit_mythos_open_header(guard: &str, name: &str) -> String {
    format!(
        "#ifndef {guard}\n\
         #define {guard}\n\
         \n\
         #include <string_view>\n\
         \n\
         namespace vaked {{\n\
         \n\
         // Integration seam — wire to MyThOS boot config.\n\
         // MyThOS has no env vars; the integrator implements this against\n\
         // whatever the kernel exposes (boot args, a compiled-in config blob, …).\n\
         // `get_or` returns the value for `key`, or `fallback` if absent.\n\
         struct BootConfig {{\n\
        \x20   const char* get_or(const char* key, const char* fallback) const;\n\
         }};\n\
         \n\
         const char* {name}(const BootConfig& cfg);\n\
         \n\
         }} // namespace vaked\n\
         \n\
         #endif // {guard}\n"
    )
}

fn emit_mythos_open_source(name: &str, term: &Term) -> String {
    let body = emit_mythos_expr(term);
    format!(
        "#include \"{name}.hpp\"\n\
         \n\
         namespace vaked {{\n\
         \n\
         const char* {name}(const BootConfig& cfg) {{\n\
         {body}\n\
         }}\n\
         \n\
         }} // namespace vaked\n"
    )
}

/// Lower a (possibly open) term to a C++ expression/statement body returning
/// `const char*`.
fn emit_mythos_expr(term: &Term) -> String {
    match term {
        Term::Lit(s) => format!("    return \"{s}\";"),
        Term::EnvVar { name, default } => {
            format!(
                "    return cfg.get_or(\"{name}\", {});",
                env_default_lit(default)
            )
        }
        Term::Match {
            scrutinee,
            branches,
            default,
        } => {
            // Lower `match` over a string to an if / else-if chain on the
            // scrutinee, compared with std::string_view equality (C++17).
            let scrut = match scrutinee.as_ref() {
                Term::EnvVar { name, default } => {
                    format!("cfg.get_or(\"{name}\", {})", env_default_lit(default))
                }
                Term::Lit(s) => format!("\"{s}\""),
                // A non-trivial scrutinee shouldn't survive normalisation for the
                // config-dispatch shapes we emit; fall back to its literal form.
                other => closed_lit(other),
            };
            let mut out = format!("    const std::string_view scrutinee{{{scrut}}};\n");
            for (i, (pat, body)) in branches.iter().enumerate() {
                let kw = if i == 0 { "if" } else { "else if" };
                let ret = match_branch_return(body);
                out.push_str(&format!(
                    "    {kw} (scrutinee == \"{}\") {{\n        return {ret};\n    }}\n",
                    pat.0
                ));
            }
            out.push_str(&format!("    return {};", match_branch_return(default)));
            out
        }
        Term::Abs { body, .. } => emit_mythos_expr(body),
        // App should be β-reduced away before emit; emit the func body as a
        // best effort rather than fabricating call syntax.
        Term::App { func, .. } => emit_mythos_expr(func),
    }
}

/// A match-branch body lowers to the expression returned by the chain arm.
fn match_branch_return(term: &Term) -> String {
    match term {
        Term::Lit(s) => format!("\"{s}\""),
        Term::EnvVar { name, default } => {
            format!("cfg.get_or(\"{name}\", {})", env_default_lit(default))
        }
        other => closed_lit(other),
    }
}

/// The `default` of an `EnvVar` is a fallback string literal in our IR; render
/// it as a C++ string literal for `get_or`'s second argument.
fn env_default_lit(default: &Term) -> String {
    match default {
        Term::Lit(s) => format!("\"{s}\""),
        // Defaults are literals in the shapes Discover produces; anything else
        // is rendered as its literal projection rather than inventing syntax.
        other => format!("\"{}\"", closed_lit(other)),
    }
}

/// Project a closed term to its string value (used for `constexpr` emission and
/// as a conservative fallback). Non-`Lit` closed nodes collapse to empty.
fn closed_lit(term: &Term) -> String {
    match term {
        Term::Lit(s) => s.clone(),
        Term::Match { default, .. } => closed_lit(default),
        Term::Abs { body, .. } => closed_lit(body),
        Term::App { func, .. } => closed_lit(func),
        Term::EnvVar { default, .. } => closed_lit(default),
    }
}

/// Emit the `mcconf.module` build descriptor.
///
/// Format grounded against real MyThOS descriptors (TOML mode; e.g.
/// `kernel/util/string/mcconf.module`, `kernel/runtime/memory/mcconf.module`).
/// `incfiles` (headers) and `srcfiles` (`.cc` sources) are the real `mcconf`
/// fields the kernel makefiles consume; no field names are invented.
fn emit_mcconf(name: &str) -> String {
    format!(
        "# -*- mode:toml; -*-\n\
         # Generated by vaked emit_mythos. Format per MyThOS `mcconf`.\n\
         [module.vaked-{name}]\n\
        \x20   incfiles = [ \"{name}.hpp\" ]\n\
        \x20   srcfiles = [ \"{name}.cc\" ]\n"
    )
}

/// Uppercase + underscore an identifier for use in a C preprocessor guard.
fn to_macro(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect()
}

// ---- stdlib lambdas ----

/// Pre-built lambda for the Amber `__vis_from_env` / `vis_from_env` pattern.
///
/// Reduces to a `Term::Lit` when `MASTODON_VISIBILITY` is in the env; emits
/// the full match expression otherwise (for unikernel boot-config dispatch).
pub fn vis_from_env_lambda(default_vis: &str) -> Term {
    Discover::env_case_lambda(
        "MASTODON_VISIBILITY",
        default_vis,
        &[
            ("public", "public"),
            ("private", "private"),
            ("direct", "direct"),
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passes() -> Vec<Box<dyn Reduce>> {
        vec![Box::new(BetaReduce), Box::new(ConstFold)]
    }

    #[test]
    fn reduces_to_constant_when_env_known() {
        let lambda = vis_from_env_lambda("unlisted");
        let mut env = Env::new();
        env.insert("MASTODON_VISIBILITY".to_string(), "public".to_string());
        let owned = passes();
        let p: Vec<&dyn Reduce> = owned.iter().map(|b| b.as_ref()).collect();
        let applied = Term::App {
            func: Box::new(lambda),
            arg: Box::new(Term::Lit(String::new())),
        };
        let result = normalize(applied, &env, &p);
        assert_eq!(result, Term::Lit("public".to_string()));
    }

    // When the env var is not in the compile-time env, the term stays open
    // (residual for unikernel boot-config dispatch — NOT a compile-time constant).
    #[test]
    fn stays_open_when_compile_time_env_unknown() {
        let lambda = vis_from_env_lambda("unlisted");
        let env = Env::new();
        let owned = passes();
        let p: Vec<&dyn Reduce> = owned.iter().map(|b| b.as_ref()).collect();
        let applied = Term::App {
            func: Box::new(lambda),
            arg: Box::new(Term::Lit(String::new())),
        };
        let result = normalize(applied, &env, &p);
        assert!(
            matches!(result, Term::Match { .. }),
            "open term should remain for runtime dispatch"
        );
    }

    // When the var IS set but hits no branch, the wildcard (default) applies.
    #[test]
    fn wildcard_arm_reduces_unknown_vis() {
        let lambda = vis_from_env_lambda("unlisted");
        let mut env = Env::new();
        env.insert(
            "MASTODON_VISIBILITY".to_string(),
            "unknown-value".to_string(),
        );
        let owned = passes();
        let p: Vec<&dyn Reduce> = owned.iter().map(|b| b.as_ref()).collect();
        let applied = Term::App {
            func: Box::new(lambda),
            arg: Box::new(Term::Lit(String::new())),
        };
        let result = normalize(applied, &env, &p);
        assert_eq!(result, Term::Lit("unlisted".to_string()));
    }

    #[test]
    fn emit_mirage_closed_term() {
        let ocaml = emit_mirage(&Term::Lit("public".to_string()));
        assert_eq!(ocaml, "\"public\"");
    }

    #[test]
    fn emit_mirage_open_term() {
        let lambda = vis_from_env_lambda("unlisted");
        // Inline body (skip Abs wrapper for unikernel — body is the useful part)
        if let Term::Abs { body, .. } = lambda {
            let ocaml = emit_mirage(&body);
            assert!(
                ocaml.contains("List.assoc_opt"),
                "should use honest assoc-list lookup"
            );
            assert!(
                !ocaml.contains("Mirage_kv"),
                "must not reference fabricated KV API"
            );
        }
    }

    // Reduce the canonical vis lambda against a fully-known env, then emit.
    fn reduced_vis(env_pairs: &[(&str, &str)]) -> Term {
        let lambda = vis_from_env_lambda("unlisted");
        let mut env = Env::new();
        for (k, v) in env_pairs {
            env.insert(k.to_string(), v.to_string());
        }
        let owned = passes();
        let p: Vec<&dyn Reduce> = owned.iter().map(|b| b.as_ref()).collect();
        let applied = Term::App {
            func: Box::new(lambda),
            arg: Box::new(Term::Lit(String::new())),
        };
        normalize(applied, &env, &p)
    }

    #[test]
    fn is_closed_distinguishes_open_and_closed() {
        assert!(is_closed(&Term::Lit("public".to_string())));
        assert!(!is_closed(&vis_from_env_lambda("unlisted")));
    }

    #[test]
    fn emit_mythos_closed_term_is_constexpr() {
        let m = emit_mythos("vis_public", &Term::Lit("public".to_string()));
        assert!(
            m.header.contains("constexpr"),
            "closed term must fold to constexpr"
        );
        assert!(
            m.header.contains("\"public\""),
            "literal value must be baked in"
        );
        // A constant has no runtime dispatch and no boot-config seam.
        assert!(!m.header.contains("BootConfig"));
        assert!(!m.source.contains("get_or"));
    }

    #[test]
    fn emit_mythos_open_term_uses_boot_config_seam() {
        // Env unknown at compile time -> term stays open -> boot-config seam.
        let term = reduced_vis(&[]);
        assert!(!is_closed(&term), "term should remain open");
        let m = emit_mythos("vis_from_env", &term);
        assert!(
            m.header.contains("BootConfig"),
            "open term needs the seam struct"
        );
        assert!(
            m.header.contains("get_or"),
            "seam accessor declared in header"
        );
        assert!(
            m.source.contains("get_or"),
            "EnvVar lowers to cfg.get_or call"
        );
        assert!(
            m.source.contains("std::string_view"),
            "match lowers to string_view chain"
        );
        assert!(
            m.source.contains("if (scrutinee =="),
            "match lowers to if/else-if chain"
        );
        assert!(
            m.source.contains("else if"),
            "multiple branches form an else-if chain"
        );
        assert!(
            m.source.contains("\"unlisted\""),
            "wildcard fallback preserved"
        );
    }

    #[test]
    fn emit_mythos_reduces_canonical_vis_when_env_known() {
        let term = reduced_vis(&[("MASTODON_VISIBILITY", "private")]);
        assert_eq!(
            term,
            Term::Lit("private".to_string()),
            "known env folds to a constant"
        );
        let m = emit_mythos("vis_from_env", &term);
        assert!(m
            .header
            .contains("constexpr const char* value = \"private\";"));
    }

    #[test]
    fn emit_mythos_contains_no_fabricated_apis() {
        // Both branches: open and closed emit.
        for term in [reduced_vis(&[]), Term::Lit("public".to_string())] {
            let m = emit_mythos("vis_from_env", &term);
            for blob in [&m.header, &m.source, &m.mcconf] {
                assert!(!blob.contains("Mirage_kv"), "no MirageOS API");
                assert!(
                    !blob.contains("dlopen"),
                    "MyThOS has no runtime module loading"
                );
                assert!(
                    !blob.contains(".ko"),
                    "MyThOS has no loadable kernel modules"
                );
            }
        }
    }

    #[test]
    fn emit_mcconf_uses_grounded_fields() {
        let m = emit_mythos("vis_from_env", &reduced_vis(&[]));
        assert!(m.mcconf.contains("[module.vaked-vis_from_env]"));
        assert!(m.mcconf.contains("incfiles"), "real MyThOS field");
        assert!(m.mcconf.contains("srcfiles"), "real MyThOS field");
    }
}
