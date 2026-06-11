//! Generate sample MyThOS modules from the canonical `vis_from_env` lambda.
//!
//! Run with `cargo run -p vaked-lambda --example gen_mythos_sample`. Writes:
//! - `mythos/vis_from_env/` — OPEN case (env unknown at build time -> boot seam)
//! - `mythos/vis_public/`   — CLOSED case (MASTODON_VISIBILITY=public folded)
//!
//! The output is committed so the scaffold is real, not theoretical.

use std::fs;
use std::path::Path;

use vaked_lambda::{
    emit_mythos, normalize, vis_from_env_lambda, BetaReduce, ConstFold, Env, MythosModule, Reduce,
    Term,
};

fn write_module(dir: &Path, m: &MythosModule) {
    fs::create_dir_all(dir).expect("create module dir");
    fs::write(dir.join(format!("{}.hpp", m.name)), &m.header).expect("write hpp");
    fs::write(dir.join(format!("{}.cpp", m.name)), &m.source).expect("write cpp");
    fs::write(dir.join("mcconf.module"), &m.mcconf).expect("write mcconf.module");
}

fn reduce(lambda: Term, env: &Env) -> Term {
    let passes: Vec<&dyn Reduce> = vec![&BetaReduce, &ConstFold];
    let applied = Term::App {
        func: Box::new(lambda),
        arg: Box::new(Term::Lit(String::new())),
    };
    normalize(applied, env, &passes)
}

fn main() {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("mythos");

    // OPEN: env unknown at build time -> residual EnvVar -> boot-config seam.
    let open = reduce(vis_from_env_lambda("unlisted"), &Env::new());
    write_module(
        &base.join("vis_from_env"),
        &emit_mythos("vis_from_env", &open),
    );

    // CLOSED: MASTODON_VISIBILITY=public folds to a constexpr constant.
    let mut env = Env::new();
    env.insert("MASTODON_VISIBILITY".to_string(), "public".to_string());
    let closed = reduce(vis_from_env_lambda("unlisted"), &env);
    write_module(
        &base.join("vis_public"),
        &emit_mythos("vis_public", &closed),
    );

    println!("wrote samples under {}", base.display());
}
