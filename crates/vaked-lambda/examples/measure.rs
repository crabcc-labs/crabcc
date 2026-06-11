//! Real reduction + emit metrics for the session write-up.
//! Run: cargo run -q -p vaked-lambda --example measure --release

use std::time::Instant;

use vaked_lambda::{
    emit_mirage, emit_mythos, is_closed, normalize, vis_from_env_lambda, BetaReduce, ConstFold,
    Env, Reduce, Term,
};

/// Count IR nodes in a term (the size we reduce away).
fn count_nodes(t: &Term) -> usize {
    1 + match t {
        Term::Lit(_) => 0,
        Term::EnvVar { default, .. } => count_nodes(default),
        Term::Match {
            scrutinee,
            branches,
            default,
        } => {
            count_nodes(scrutinee)
                + branches.iter().map(|(_, b)| count_nodes(b)).sum::<usize>()
                + count_nodes(default)
        }
        Term::Abs { body, .. } => count_nodes(body),
        Term::App { func, arg } => count_nodes(func) + count_nodes(arg),
        Term::Seq(a, b) | Term::Par(a, b) | Term::Dep(a, b) => count_nodes(a) + count_nodes(b),
    }
}

fn applied() -> Term {
    Term::App {
        func: Box::new(vis_from_env_lambda("unlisted")),
        arg: Box::new(Term::Lit(String::new())),
    }
}

fn reduce(env: &Env) -> Term {
    let passes: Vec<&dyn Reduce> = vec![&BetaReduce, &ConstFold];
    normalize(applied(), env, &passes)
}

fn main() {
    let before = count_nodes(&applied());

    let open = reduce(&Env::new());
    let mut env = Env::new();
    env.insert("MASTODON_VISIBILITY".into(), "public".into());
    let closed = reduce(&env);

    println!("== IR nodes (vis_from_env: env-case over 3 visibilities) ==");
    println!("applied term, before reduce : {before}");
    println!(
        "open   (compile-time env unknown): {} nodes (closed={})",
        count_nodes(&open),
        is_closed(&open)
    );
    println!(
        "closed (MASTODON_VISIBILITY=public): {} node  (closed={})",
        count_nodes(&closed),
        is_closed(&closed)
    );

    let m_open = emit_mythos("vis_from_env", &open);
    let m_closed = emit_mythos("vis_public", &closed);
    println!("\n== emit size (bytes) ==");
    println!(
        "mythos open   (header+source): {} B",
        m_open.header.len() + m_open.source.len()
    );
    println!(
        "mythos closed (header+source): {} B",
        m_closed.header.len() + m_closed.source.len()
    );
    println!("mirage open  : {} B", emit_mirage(&open).len());
    println!("mirage closed: {} B", emit_mirage(&closed).len());

    println!("\n== OCaml (MirageOS) emit ==");
    println!("open:\n{}", emit_mirage(&open));
    println!("closed: {}", emit_mirage(&closed));

    // Build+reduce throughput (honest: includes lambda construction per op).
    let n = 1_000_000u32;
    let t0 = Instant::now();
    for _ in 0..n {
        std::hint::black_box(reduce(&env));
    }
    let dt = t0.elapsed();
    println!("\n== build+reduce throughput (closed path) ==");
    println!(
        "{n} ops in {dt:?}  ->  {:.0} ns/op",
        dt.as_nanos() as f64 / n as f64
    );
}
