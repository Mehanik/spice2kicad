//! Net classification (Power/Ground/Signal). See spec §3.
//!
//! Pure function: takes a `CheckedNetlist`, returns a class per net.
//! Used downstream by `bands.rs` (Y-banding) and `layers.rs` (which
//! prunes Power/Ground edges from the signal-flow DAG so feedback
//! through rails doesn't create false cycles).

use std::collections::HashMap;

use spice_policy::CheckedNetlist;
use spice_resolve::ElementRole;

/// Functional class of a SPICE net for layout purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NetClass {
    Power,
    Ground,
    Signal,
}

/// A map from net name to [`NetClass`].
pub type NetClassMap = HashMap<String, NetClass>;

/// Preferred *screen-vertical* side for a rail net, used by the V14
/// power-glyph-orientation hard constraint in the placer.
///
/// A positive supply rail (VCC / VDD / V+) conventionally runs along
/// the top of the sheet, so any element pin sitting on it should face
/// screen-**up**. Ground and *negative* supply rails (GND / VEE / VSS /
/// V- and any `*@power=-…` source) run along the bottom, so their pins
/// face screen-**down**. Signal nets carry no preference.
///
/// Note this is finer-grained than [`NetClass`]: a `*@power=-15V`
/// source's net is `NetClass::Power` (it is a *@power-tagged supply)
/// but its [`VertPref`] is [`VertPref::Down`], because a negative rail
/// belongs at the bottom of the sheet next to ground — not at the top
/// with VCC. V14 orientation keys off `VertPref`, never off `NetClass`
/// directly, so split ±rails orient correctly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VertPref {
    /// Pin on this net should face screen-up (positive supply rail).
    Up,
    /// Pin on this net should face screen-down (ground / negative rail).
    Down,
}

/// Map every net to its [`VertPref`], or absent for signal nets.
///
/// Rules (applied after [`classify_nets`]):
/// * `NetClass::Ground` → [`VertPref::Down`].
/// * `NetClass::Power` → [`VertPref::Down`] when the net is a negative
///   rail (lowercased name matches `vee`/`vss`/`v-`/`vminus`, OR a
///   `*@power`-tagged source on the net carries a rail string that
///   begins with `-`); otherwise [`VertPref::Up`].
/// * `NetClass::Signal` → no entry.
#[must_use]
pub fn vertical_prefs(checked: &CheckedNetlist) -> HashMap<String, VertPref> {
    let classes = classify_nets(checked);

    // Nets whose *@power source carries a negative rail string.
    let mut negative_power: std::collections::HashSet<String> = std::collections::HashSet::new();
    for el in &checked.elements {
        if let ElementRole::Power(rail) = &el.role
            && rail.trim_start().starts_with('-')
        {
            for n in &el.nodes {
                negative_power.insert(n.clone());
            }
        }
    }

    let mut out = HashMap::new();
    for (net, class) in classes {
        let pref = match class {
            NetClass::Ground => VertPref::Down,
            NetClass::Power => {
                let lower = net.to_ascii_lowercase();
                if matches_ground_name(&lower) || negative_power.contains(&net) {
                    VertPref::Down
                } else {
                    VertPref::Up
                }
            }
            NetClass::Signal => continue,
        };
        out.insert(net, pref);
    }
    out
}

/// Classify every net referenced by the netlist.
///
/// Rules (spec §3), applied in priority order via `entry().or_insert`:
///
/// 1. Net `"0"` → `Ground`.
/// 2. Positive terminal (`nodes[0]`) of any `*@power`-tagged voltage
///    source → `Power`.
/// 3. Any net whose lowercased name matches a canonical supply/ground
///    pattern (`vcc`, `vdd`, `v+`, `vplus` → `Power`; `gnd`, `vee`,
///    `vss`, `v-`, `vminus` → `Ground`).
/// 4. Any other net touched by ≥1 `*@power`-tagged source → `Power`
///    (handles split rails like ±15 V).
/// 5. Bypass-cap reclassification — skipped (fixtures don't need it).
/// 6. All remaining nets → `Signal`.
pub fn classify_nets(checked: &CheckedNetlist) -> NetClassMap {
    let mut map: NetClassMap = HashMap::new();

    // Rule 1: ground = "0".
    map.insert("0".to_string(), NetClass::Ground);

    // Rule 2: positive terminal of any *@power-tagged source.
    for el in &checked.elements {
        if matches!(el.role, ElementRole::Power(_)) {
            if let Some(node) = el.nodes.first() {
                map.insert(node.clone(), NetClass::Power);
            }
        }
    }

    // Rule 3: canonical supply/ground names (case-insensitive).
    // Iterate all net names visible in element nodes. `or_insert` means
    // Rules 1 and 2 already win for "0" and Power-tagged positive terminals.
    for el in &checked.elements {
        for n in &el.nodes {
            let lower = n.to_ascii_lowercase();
            if matches_power_name(&lower) {
                map.entry(n.clone()).or_insert(NetClass::Power);
            } else if matches_ground_name(&lower) {
                map.entry(n.clone()).or_insert(NetClass::Ground);
            }
        }
    }

    // Rule 4: any net touched by ≥1 *@power source → Power.
    for el in &checked.elements {
        if matches!(el.role, ElementRole::Power(_)) {
            for n in &el.nodes {
                map.entry(n.clone()).or_insert(NetClass::Power);
            }
        }
    }

    // Rule 5: bypass-cap reclassification — skipped for v0.1.

    // Rule 6: everything else → Signal.
    for el in &checked.elements {
        for n in &el.nodes {
            map.entry(n.clone()).or_insert(NetClass::Signal);
        }
    }

    map
}

fn matches_power_name(lower: &str) -> bool {
    matches!(lower, "vcc" | "vdd" | "v+" | "vplus")
}

fn matches_ground_name(lower: &str) -> bool {
    matches!(lower, "gnd" | "vee" | "vss" | "v-" | "vminus")
}

#[cfg(test)]
mod tests {
    use super::*;
    use spice_diagnostics::FileId;
    use spice_policy::check;

    use kicad_symbols::Library;
    use std::path::PathBuf;
    use std::sync::OnceLock;

    fn fixture_library() -> &'static Library {
        static LIB: OnceLock<Library> = OnceLock::new();
        LIB.get_or_init(|| {
            let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            let fixture_dir = manifest
                .parent()
                .and_then(std::path::Path::parent)
                .expect("workspace root")
                .join("crates/kicad-symbols/tests/fixtures");
            let device = Library::from_file(fixture_dir.join("Device.kicad_sym"))
                .expect("load Device fixture library");
            let spice = Library::from_file(fixture_dir.join("Simulation_SPICE.kicad_sym"))
                .expect("load Simulation_SPICE fixture library");
            device.merge(spice)
        })
    }

    /// Parse a SPICE source string, resolve, check, then classify nets.
    fn classify_str(src: &str) -> NetClassMap {
        let file_id = FileId(0);
        let parsed = spice_parser::parse(src, file_id)
            .expect("parse failed")
            .netlist;
        let resolved = spice_resolve::resolve(&parsed, fixture_library()).expect("resolve failed");
        let (checked, _warns) = check(resolved).expect("policy check failed");
        classify_nets(&checked)
    }

    #[test]
    fn ground_net_zero_classifies_as_ground() {
        let m = classify_str("test\nR1 a 0 1k\n.end\n");
        assert_eq!(m.get("0"), Some(&NetClass::Ground));
    }

    #[test]
    fn power_tagged_source_positive_terminal_is_power() {
        let m = classify_str("test\nV1 vcc 0 12 ;@ power=vcc\nR1 vcc out 1k\n.end\n");
        assert_eq!(m.get("vcc"), Some(&NetClass::Power));
        assert_eq!(m.get("out"), Some(&NetClass::Signal));
    }

    #[test]
    fn untagged_source_does_not_create_power() {
        let m = classify_str("test\nV1 in 0 AC 1\nR1 in out 1k\n.end\n");
        assert_eq!(m.get("in"), Some(&NetClass::Signal));
        assert_eq!(m.get("out"), Some(&NetClass::Signal));
    }

    #[test]
    fn signal_net_default() {
        let m = classify_str("test\nV1 in 0 AC 1\nR1 in mid 1k\nC1 mid 0 1u\n.end\n");
        assert_eq!(m.get("mid"), Some(&NetClass::Signal));
    }

    fn prefs_str(src: &str) -> HashMap<String, VertPref> {
        let file_id = FileId(0);
        let parsed = spice_parser::parse(src, file_id)
            .expect("parse failed")
            .netlist;
        let resolved = spice_resolve::resolve(&parsed, fixture_library()).expect("resolve failed");
        let (checked, _warns) = check(resolved).expect("policy check failed");
        vertical_prefs(&checked)
    }

    #[test]
    fn positive_rail_prefers_up_ground_prefers_down() {
        let p = prefs_str("test\nV1 vcc 0 12 ;@ power=vcc\nR1 vcc out 1k\n.end\n");
        assert_eq!(p.get("vcc"), Some(&VertPref::Up));
        assert_eq!(p.get("0"), Some(&VertPref::Down));
        assert_eq!(p.get("out"), None); // signal → no preference
    }

    #[test]
    fn negative_power_rail_prefers_down() {
        // A *@power source with a negative rail string is Power-class
        // but belongs at the bottom (VertPref::Down).
        let p = prefs_str("test\nVEE vee 0 DC -15 ;@ power=-15V\nR1 vee out 1k\n.end\n");
        assert_eq!(p.get("vee"), Some(&VertPref::Down));
    }

    #[test]
    fn name_based_negative_rail_prefers_down() {
        // `vee` matches the ground-name pattern even when it is power.
        let p = prefs_str("test\nVEE vee 0 DC 15 ;@ power=vee\nR1 vee out 1k\n.end\n");
        assert_eq!(p.get("vee"), Some(&VertPref::Down));
    }
}
