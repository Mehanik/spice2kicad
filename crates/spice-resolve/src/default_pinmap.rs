//! Synthesize a default `pinmap` for an element when the user has
//! supplied none. The point is V11-correctness: we cannot just zip
//! SPICE terminals to library pins by parsed order, because library
//! symbols are not guaranteed to declare pins in SPICE-terminal order
//! (e.g. KiCad's `Device:Q_NPN_BCE` numbers pins B=1, C=2, E=3 while
//! SPICE BJT terminal order is C, B, E). We map by canonical pin
//! *name* for kinds that have one, and fall back to positional
//! mapping for kinds that don't.

use kicad_symbols::Symbol;
use spice_parser::ast::{ElementKind, PinRef, PinmapEntry};

#[derive(Debug)]
pub(crate) enum DefaultPinmapError {
    // Fields carry diagnostic detail; the resolver formats its E002
    // message off the element/symbol rather than the variant payload,
    // so prod code only matches the variant. Tests inspect the fields.
    #[allow(dead_code)]
    ArityMismatch { arity: usize, pin_count: usize },
    MissingNamedPin {
        expected: &'static str,
        lib_id: String,
    },
}

/// Canonical KiCad pin names for SPICE terminal order, by kind. `None`
/// means the kind has no canonical name table — fall through to
/// positional mapping (preserves today's behaviour).
fn canonical_pin_names(kind: ElementKind) -> Option<&'static [&'static str]> {
    Some(match kind {
        ElementKind::Diode => &["A", "K"],
        ElementKind::Bjt => &["C", "B", "E", "S"],
        ElementKind::Mosfet => &["D", "G", "S", "B"],
        ElementKind::Jfet => &["D", "G", "S"],
        // TODO: kind-specific table once we have a fixture
        ElementKind::Resistor
        | ElementKind::Capacitor
        | ElementKind::Inductor
        | ElementKind::VoltageSrc
        | ElementKind::CurrentSrc
        | ElementKind::Vcvs
        | ElementKind::Vccs
        | ElementKind::Cccs
        | ElementKind::Ccvs
        | ElementKind::MutualInductance
        | ElementKind::Subckt
        | ElementKind::Other => return None,
    })
}

pub(crate) fn synthesize(
    kind: ElementKind,
    symbol: &Symbol,
    arity: usize,
) -> Result<Vec<PinmapEntry>, DefaultPinmapError> {
    let pin_count = symbol.pin_count();
    if arity != pin_count {
        return Err(DefaultPinmapError::ArityMismatch { arity, pin_count });
    }

    if let Some(names) = canonical_pin_names(kind) {
        let take = arity.min(names.len());
        let mut entries = Vec::with_capacity(arity);
        for (i, name) in names.iter().take(take).enumerate() {
            if symbol.pin_by_name(name).is_none() {
                return Err(DefaultPinmapError::MissingNamedPin {
                    expected: name,
                    lib_id: symbol.lib_id.clone(),
                });
            }
            entries.push(PinmapEntry {
                spice_index: i + 1,
                kicad_pin: PinRef::Name((*name).to_owned()),
            });
        }
        // If arity exceeds the canonical table (shouldn't happen for
        // well-formed inputs), fall back to positional for the tail.
        for i in take..arity {
            entries.push(PinmapEntry {
                spice_index: i + 1,
                kicad_pin: PinRef::Number(symbol.pins[i].number.clone()),
            });
        }
        return Ok(entries);
    }

    // Positional fallback: SPICE term i → KiCad pin in declared order.
    Ok((0..arity)
        .map(|i| PinmapEntry {
            spice_index: i + 1,
            kicad_pin: PinRef::Number(symbol.pins[i].number.clone()),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use kicad_symbols::{Pin, PinElectrical, RawSexpr, Symbol};

    fn pin(number: &str, name: &str) -> Pin {
        Pin {
            number: number.to_owned(),
            name: name.to_owned(),
            x: 0.0,
            y: 0.0,
            angle: 0,
            length: 0.0,
            electrical: PinElectrical::Passive,
        }
    }

    fn symbol_with(lib_id: &str, pins: Vec<Pin>) -> Symbol {
        Symbol {
            lib_id: lib_id.to_owned(),
            name: lib_id.to_owned(),
            pins,
            show_pin_names: true,
            show_pin_numbers: true,
            pin_name_offset: 0.0,
            body: RawSexpr::List(Vec::new()),
        }
    }

    #[test]
    fn bjt_maps_by_pin_name_not_position() {
        // Symbol declared in (B, C, E) order with numbers 1, 2, 3 —
        // matches KiCad's Device:Q_NPN_BCE. SPICE BJT order is (C, B, E).
        let sym = symbol_with(
            "Device:Q_NPN_BCE",
            vec![pin("1", "B"), pin("2", "C"), pin("3", "E")],
        );
        let entries = synthesize(ElementKind::Bjt, &sym, 3).expect("ok");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].spice_index, 1);
        assert!(matches!(&entries[0].kicad_pin, PinRef::Name(n) if n == "C"));
        assert_eq!(entries[1].spice_index, 2);
        assert!(matches!(&entries[1].kicad_pin, PinRef::Name(n) if n == "B"));
        assert_eq!(entries[2].spice_index, 3);
        assert!(matches!(&entries[2].kicad_pin, PinRef::Name(n) if n == "E"));
    }

    #[test]
    fn resistor_falls_back_to_positional() {
        let sym = symbol_with("Device:R", vec![pin("1", "~"), pin("2", "~")]);
        let entries = synthesize(ElementKind::Resistor, &sym, 2).expect("ok");
        assert_eq!(entries.len(), 2);
        assert!(matches!(&entries[0].kicad_pin, PinRef::Number(n) if n == "1"));
        assert!(matches!(&entries[1].kicad_pin, PinRef::Number(n) if n == "2"));
    }

    #[test]
    fn bjt_missing_named_pin_errors() {
        // Pin "B" is missing — only C and E are present. The synthesizer
        // walks the canonical (C, B, E) table and surfaces the first
        // unresolved name.
        let sym = symbol_with(
            "Foo:NoBase",
            vec![pin("1", "C"), pin("2", "X"), pin("3", "E")],
        );
        let err = synthesize(ElementKind::Bjt, &sym, 3).expect_err("should fail");
        match err {
            DefaultPinmapError::MissingNamedPin { expected, lib_id } => {
                assert_eq!(expected, "B");
                assert_eq!(lib_id, "Foo:NoBase");
            }
            DefaultPinmapError::ArityMismatch { .. } => panic!("wrong error variant"),
        }
    }

    #[test]
    fn arity_mismatch_errors() {
        let sym = symbol_with("Device:R", vec![pin("1", "~"), pin("2", "~")]);
        let err = synthesize(ElementKind::Resistor, &sym, 3).expect_err("should fail");
        match err {
            DefaultPinmapError::ArityMismatch { arity, pin_count } => {
                assert_eq!(arity, 3);
                assert_eq!(pin_count, 2);
            }
            DefaultPinmapError::MissingNamedPin { .. } => panic!("wrong error variant"),
        }
    }

    #[test]
    fn bjt_synthetic_xyz_symbol_reports_first_canonical_missing() {
        // E008 unit test (Step 7). Synthetic 3-pin BJT-target symbol
        // whose names X/Y/Z bear no resemblance to canonical C/B/E —
        // the synthesizer reports the first unresolved canonical name.
        let sym = symbol_with(
            "Foo:Generic",
            vec![pin("1", "X"), pin("2", "Y"), pin("3", "Z")],
        );
        match synthesize(ElementKind::Bjt, &sym, 3) {
            Err(DefaultPinmapError::MissingNamedPin { expected, .. }) => {
                // "C" is first in the canonical table, so it's the
                // earliest miss.
                assert_eq!(expected, "C");
            }
            _ => panic!("expected MissingNamedPin"),
        }
    }
}
