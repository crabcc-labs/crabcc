// Phase-0 entrypoint for the future GPUI-based crabcc desktop app.
//
// Right now this is a no-op binary that prints a banner — its job is
// to lock in the crate name, manifest, and module layout so future
// phases (see docs/RESEARCH-native-desktop-and-rich-notifications.md)
// can grow into it without renames. Real GPUI rendering arrives in
// phase A.1; until then `cargo run -p crabcc-desktop` proves the
// scaffolding builds.

fn main() -> anyhow::Result<()> {
    println!("crabcc-desktop {} — phase 0 stub", env!("CARGO_PKG_VERSION"));
    println!("Real GPUI rendering lands in phase A.1. See");
    println!("  docs/RESEARCH-native-desktop-and-rich-notifications.md");
    Ok(())
}
