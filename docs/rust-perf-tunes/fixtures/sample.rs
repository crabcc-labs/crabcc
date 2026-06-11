fn build() -> Vec<i32> {
    // a comment mentioning Vec::new() should NOT match (regex would)
    let s = "Vec::new() in a string literal";
    let mut v = Vec::new();
    for i in 0..1000 {
        v.push(i);
    }
    println!("{}", s);
    v
}

fn already_good() -> Vec<i32> {
    let mut v = Vec::with_capacity(1000);
    for i in 0..1000 { v.push(i); }
    v
}
