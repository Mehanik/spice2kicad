//! V16 ink counts for arbitrary emitted sheets (ADR-31 instrument).
//!
//! `#[ignore]`d — an *instrument*, not a gate. The V16 ratchets grade the
//! fixtures the suite converts for itself; this one grades sheets a
//! **hand conversion** produced, which is what a placer A/B or a seed
//! sweep leaves behind. It reads the same `common::ink::measure` the
//! ratchets do, so its B / J are the project's own numbers rather than a
//! re-derivation.
//!
//! ```sh
//! S2K_INK_PATHS=/tmp/a/x.kicad_sch:/tmp/b/x.kicad_sch \
//!   cargo test -p spice2kicad --test ink_dump -- --ignored --nocapture
//! ```
//!
//! Used to establish ADR-31: the same fixture converted under one placer
//! at twenty SA seeds spans B = 0..6, so a single-seed B difference
//! between two arms is not on its own evidence of an effect.

mod common;

#[test]
#[ignore = "instrument, not a gate"]
fn ink_dump() {
    let paths = std::env::var("S2K_INK_PATHS")
        .expect("set S2K_INK_PATHS to a `:`-separated list of .kicad_sch paths");
    for p in paths.split(':').filter(|p| !p.is_empty()) {
        let src = std::fs::read_to_string(p).expect("read sheet");
        let root: lexpr::Value = lexpr::from_str(&src).expect("parse sheet as s-expressions");
        match common::ink::measure(&root) {
            Ok(c) => println!(
                "{p}: B={} J={} X={} segs={} runs={}",
                c.bends, c.branches, c.inter_net_crossings, c.raw_segments, c.runs
            ),
            Err(e) => println!("{p}: ERROR {e}"),
        }
    }
}
