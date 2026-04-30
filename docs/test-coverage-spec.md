# Test Coverage vs Annotation Spec

This document maps every requirement in `docs/annotation-spec.md` to the
integration and unit tests that exercise it. It was produced by reading the spec
end-to-end and cross-referencing every test function in
`crates/spice-parser/tests/`, the `#[cfg(test)]` blocks inside
`crates/spice-parser/src/lexer.rs` and `crates/spice-parser/src/parser.rs`,
and the cross-crate integration tests in `crates/spice-resolve/tests/resolve.rs`
and `crates/spice-policy/tests/policy.rs`.
All parser test references are relative to `crates/spice-parser/`.
Cross-crate test references use the full `crate-name/tests/file.rs::test_name`
form.

Verified against `crates/spice-parser/tests/*.rs`,
`crates/spice-parser/src/{lexer,parser}.rs`,
`crates/spice-resolve/tests/resolve.rs`, and
`crates/spice-policy/tests/policy.rs` as of the current `master` HEAD.

---

## 1. Spec-Section Coverage Matrix

| Spec ref | Requirement | Covered by | Status |
|----------|-------------|------------|--------|
| §2 | Block comment (`*@`) and trailing tag (`;@`) are the two annotation carriers | `tests/lex_edges.rs::block_annotation_top_level`, `tests/lex_edges.rs::prose_semicolon_comment_no_tags`, `src/lexer.rs::block_annotation_emits_block_line` | covered |
| §2 | Whitespace between marker and directive name is optional (`;@symbol=` = `;@ symbol=`) | `tests/lex_edges.rs::tag_no_space_after_marker` | covered |
| §2 (lexer) | Bare `\r` not treated as a line separator (matches ngspice `inpcom.c` `\r\n`-only zap) | `tests/edge_inputs.rs::bare_cr_line_endings` | covered |
| §2.2 | Dangling `+` continuation at start of file becomes a bogus `Other` element (documented quirk) | `tests/edge_inputs.rs::continuation_at_start_of_file` | covered |
| §3.1 | Number overflow (`1e500`) parses to `Value::Number(inf)` (matches ngspice `INPevaluate`) | `tests/edge_inputs.rs::number_overflow_input` | covered |
| §2.3 | Tab-separated `=` in tag bodies parsed correctly | `tests/edge_inputs.rs::tab_inside_tag_body` | covered |
| §2 | Directive names and bare keys are case-insensitive ASCII (dotted-directive case: covered; tag-directive keyword case: see §3 gap) | `tests/elements.rs::case_insensitive_refdes`, `tests/directives.rs::directive_names_case_insensitive`, `tests/numbers.rs::eng_suffixes_case_insensitive` | partial [^2] |
| §2.1 | Refdes accepts bare SPICE refdes (`R1`, `Q3`, `XU2`) | `tests/fixtures.rs::pinmap_tag_parses`, `tests/fixtures.rs::diff_pair_align_and_place` | covered |
| §2.1 | Refdes accepts dotted subcircuit path (`XU2.R5`) | `tests/lex_edges.rs::place_dotted_anchor` | covered |
| §2.2 | Trailing `;@` tag binds to the logical element (any physical `+` continuation line) | `tests/lex_edges.rs::continuation_carries_tag`, `tests/lex_edges.rs::tag_on_continuation_line`, `src/lexer.rs::continuation_appends_words_and_tags`, `tests/fixtures.rs::mosfet_continuation` | covered |
| §2.3 | One directive per annotation line (comma-separated directives not supported) | — | gap [^3] |
| §2.3 | Multiple trailing tags on the same element via adjacent `;@` lines or continuation lines | `tests/lex_edges.rs::multiple_tags_on_element_line`, `tests/lex_edges.rs::standalone_tag_attaches_to_previous`, `src/lexer.rs::multiple_tags_on_one_line`, `src/lexer.rs::standalone_tag_line_attaches_to_previous` | covered |
| §3 | `.subckt`/`.ends` maps to hierarchical sheet; `.include` maps to visual cluster | `tests/directives.rs::subckt_basic`, `tests/directives.rs::include_preserved`, `tests/fixtures.rs::opamp_subckt_definition` | partial [^4] |
| §3 | `.subckt` that is defined but never instantiated produces no schematic output | — | gap [^5] |
| §3 | `.include` with only non-placeable content produces no cluster | — | gap [^5] |
| §3.1 | Placeable elements (R, C, L, V, I, D, Q, M, J, K, E, F, G, H, X, T) parsed as such | `tests/elements.rs::resistor_basic`, `tests/elements.rs::capacitor_basic`, `tests/elements.rs::inductor_basic`, `tests/elements.rs::voltage_source_dc_only`, `tests/elements.rs::current_source_basic`, `tests/elements.rs::diode_basic`, `tests/elements.rs::bjt_3_terminal`, `tests/elements.rs::mosfet_with_params`, `tests/elements.rs::jfet_basic`, `tests/elements.rs::subckt_instance_variable_ports`, `tests/elements.rs::vcvs_e_basic` (typed `Vcvs`), `tests/elements.rs::vccs_g_basic` (typed `Vccs`), `tests/elements.rs::cccs_f_basic_ngspice_correct`, `tests/elements.rs::ccvs_h_basic`, `tests/elements.rs::mutual_inductance_k_ngspice_correct` | partial [^6] |
| §3.1 | Structural statements (`.subckt`, `.include`, `.global`) parsed correctly | `tests/directives.rs::subckt_basic`, `tests/directives.rs::include_preserved`, `tests/directives.rs::global_preserved` | covered |
| §3.1 | Simulation-only statements (`.model`, `.param`, `.tran`, etc.) passed through; do not appear on schematic | `tests/directives.rs::model_npn_parenless`, `tests/directives.rs::param_preserved`, `tests/directives.rs::tran_preserved`, `tests/directives.rs::ac_preserved`, `tests/directives.rs::dc_preserved`, `tests/directives.rs::op_preserved`, `tests/directives.rs::print_preserved`, `tests/directives.rs::ic_preserved`, `tests/directives.rs::measure_preserved`, `tests/directives.rs::option_preserved` | covered |
| §3.1 | Net `0` and `.global` nets auto-render as ground symbol | — | gap [^5] |
| §4.1 | `symbol=Lib:Name` trailing tag maps element to KiCad library symbol | `tests/lex_edges.rs::continuation_carries_tag`, `tests/fixtures.rs::rc_lowpass` | covered |
| §4.1 | Block `*@symbol Lib:Name for=GLOB` sets default for matching elements | `tests/directives.rs::block_annotation_symbol_default`, `tests/lex_edges.rs::block_annotation_top_level`, `tests/fixtures.rs::rc_lowpass`, `tests/fixtures.rs::common_emitter_subckt_and_align_and_power` | covered |
| §4.1 | Glob syntax: `*` matches any run of characters, case-insensitive | `spice-resolve/tests/resolve.rs::block_symbol_default_with_glob` | covered |
| §4.1 | Resolution order: trailing tag > last matching `for=` > built-in default | `spice-resolve/tests/resolve.rs::later_block_annotation_wins_for_matches`, `spice-resolve/tests/resolve.rs::trailing_tag_beats_block_annotation` | covered |
| §4.2 | `pinmap=N:pin[,…]` remap by 1-based SPICE terminal index to KiCad pin number or name | `tests/fixtures.rs::pinmap_tag_parses` | covered |
| §4.2 | KiCad pin referenced by number or by name (`A`, `K`, `+`, `-`, etc.) | `tests/fixtures.rs::pinmap_tag_parses` (name form), `tests/elements.rs::pinmap_numeric_pin` (number form), `tests/elements.rs::pinmap_mixed_number_and_name` | covered |
| §4.3 | `place=<relation> <anchor>` accepted; relation is one of `right-of`, `left-of`, `above`, `below` | `tests/lex_edges.rs::standalone_tag_attaches_to_previous`, `tests/lex_edges.rs::place_relation_left_of`, `tests/lex_edges.rs::place_relation_above`, `tests/lex_edges.rs::place_relation_below`, `tests/fixtures.rs::diff_pair_align_and_place`, `tests/fixtures.rs::multivibrator_parses_cleanly` | covered |
| §4.3 | Anchor is a reference identifier per §2.1 | `tests/fixtures.rs::diff_pair_align_and_place`, `tests/fixtures.rs::multivibrator_parses_cleanly`, `tests/lex_edges.rs::place_dotted_anchor` | covered |
| §4.3 | Spacing chosen by layout engine; spec does not expose numeric gaps | N/A — spec design choice, not testable at parser level | not parser-scope |
| §4.3 | Geometric effect is on connecting pins, not symbol centers | N/A — layout phase | not parser-scope |
| §4.4 | `*@align horizontal R1 R2 ...` block annotation parsed | `tests/directives.rs::block_annotation_align_horizontal`, `tests/fixtures.rs::diff_pair_align_and_place` | covered |
| §4.4 | `*@align vertical C1 C2 ...` block annotation parsed | `tests/directives.rs::block_annotation_inside_subckt_lands_in_subckt`, `tests/fixtures.rs::common_emitter_subckt_and_align_and_power` | covered |
| §4.4 | `align` references must be within the same parent sheet (cross-boundary → E004) | — | gap [^5] |
| §4.4 | Equal Y/X applies to connecting pins, not symbol centers | N/A — layout phase | not parser-scope |
| §4.5 | `power=<rail>` marks a voltage source as a power rail | `tests/fixtures.rs::common_emitter_subckt_and_align_and_power` | covered |
| §4.5 | Power-flagged net renders as KiCad power flag; source itself not drawn | N/A — emitter/layout phase | not parser-scope |
| §4.6 | `ignore` hides element from schematic | `tests/fixtures.rs::rc_lowpass` (V1 has ignore tag), `tests/fixtures.rs::common_emitter_subckt_and_align_and_power` (RL) | covered |
| §5 | Phase 1: structural — `.subckt`/`.include` boundaries established first | N/A — layout phase | not parser-scope |
| §5 | Phase 2: `align` directives fixed before `place` | N/A — layout phase | not parser-scope |
| §5 | Phase 3: `place` directives applied; source order wins on conflict (W101) | `spice-policy/tests/policy.rs::w101_duplicate_place_keeps_first` | covered |
| §5 | Phase 4: unconstrained elements auto-filled by heuristic | N/A — layout phase | not parser-scope |
| §5 | Unknown refdes in constraint → E001 (hard error) | `spice-policy/tests/policy.rs::e001_align_unknown_refdes`, `spice-policy/tests/policy.rs::e001_place_unknown_refdes`, `spice-policy/tests/policy.rs::e001_place_unknown_anchor`, `spice-policy/tests/policy.rs::e001_collects_multiple` | covered |
| §5 | `place` on `align`-fixed element → W104 | `spice-policy/tests/policy.rs::w104_place_on_align_fixed_element`, `spice-policy/tests/policy.rs::w104_alone_when_place_overlaps_align` | covered |
| §7 E001 | Unknown refdes in directive → hard error | `spice-policy/tests/policy.rs::e001_align_unknown_refdes`, `spice-policy/tests/policy.rs::e001_place_unknown_refdes`, `spice-policy/tests/policy.rs::e001_place_unknown_anchor`, `spice-policy/tests/policy.rs::e001_collects_multiple` | covered |
| §7 E002 | Symbol pin count mismatch → hard error | `spice-resolve/tests/resolve.rs::pin_count_mismatch_no_pinmap_is_e002` | covered |
| §7 E003 | Unknown library symbol or unmapped X instance → hard error. F/H/K still require annotation (no canonical KiCad symbol); E and G default to `Simulation_SPICE:ESOURCE`/`GSOURCE` and do not raise E003. | `spice-resolve/tests/resolve.rs::unknown_lib_id_is_e003`, `spice-resolve/tests/resolve.rs::subckt_instance_without_symbol_is_error`, `spice-resolve/tests/resolve.rs::vcvs_default_resolves_to_esource`, `spice-resolve/tests/resolve.rs::vccs_default_resolves_to_gsource` | covered |
| §7 E004 | `align` references cross a sheet boundary → hard error | — | gap |
| §7 E005 | Invalid `pinmap` (unknown pin, out-of-range index, repeated entry) → hard error | Parser: `tests/diagnostics.rs::e005_invalid_pinmap_no_colon`, `tests/diagnostics.rs::e005_invalid_pinmap_non_numeric_index`, `tests/diagnostics.rs::e005_invalid_pinmap_empty`, `tests/diagnostics.rs::pinmap_with_repeated_spice_index`, `tests/diagnostics.rs::pinmap_with_repeated_kicad_pin`. Resolver: `spice-resolve/tests/resolve.rs::pinmap_with_unknown_pin_is_e005`, `spice-resolve/tests/resolve.rs::pinmap_duplicate_spice_index_is_e005`, `spice-resolve/tests/resolve.rs::pinmap_duplicate_kicad_pin_is_e005`, `spice-resolve/tests/resolve.rs::pinmap_out_of_range_index_is_e005` | covered |
| §7 E006 | Directional cycle in `place` graph → hard error | `spice-policy/tests/policy.rs::e006_two_cycle_same_axis`, `spice-policy/tests/policy.rs::e006_three_cycle_same_axis`, `spice-policy/tests/policy.rs::e006_disjoint_cycles_each_reported`, `spice-policy/tests/policy.rs::cyclic_inputs_emit_e006` | covered |
| §7 E007 | Internal: layout stall after policy pass → hard error | — | gap |
| §7 W101 | Conflicting `place` constraints → warning (first wins) | `spice-policy/tests/policy.rs::w101_duplicate_place_keeps_first` | covered |
| §7 W102 | `align` cluster has fewer than two members → warning | `spice-policy/tests/policy.rs::w102_single_member_cluster`, `spice-policy/tests/policy.rs::w102_duplicates_collapse_to_single` | covered |
| §7 W103 | Annotation on unrecognised line → warning | `tests/diagnostics.rs::w103_unknown_block_directive`, `tests/diagnostics.rs::w103_unknown_tag_directive` | covered |
| §7 W104 | `place` on `align`-fixed element → dropped with warning | `spice-policy/tests/policy.rs::w104_place_on_align_fixed_element`, `spice-policy/tests/policy.rs::w104_alone_when_place_overlaps_align`, `spice-policy/tests/policy.rs::errors_carry_warnings_too` | covered |
| §8 | `*@` lines are invisible to SPICE simulators (leading `*` makes them comments) | `tests/directives.rs::control_star_lines_skipped` (negative case), `tests/lex_edges.rs::pure_comment_dropped` | partial [^10] |
| §8 | `;@` lines are invisible to SPICE simulators (`;` is inline comment in ngspice/LTspice) | `tests/lex_edges.rs::prose_semicolon_comment_no_tags` | partial [^10] |
| §8 caveat 1 | Inline `;` is an extension, not base SPICE3 | `tests/lex_edges.rs::prose_semicolon_comment_no_tags` (documents behaviour, not portability) | partial [^10] |
| §8 | Inline `$` is an ngspice extension; comment-introducer rules | `tests/lex_edges.rs::dollar_inline_comment`, `tests/lex_edges.rs::dollar_with_leading_space_required`, `tests/lex_edges.rs::dollar_after_comma`, `tests/lex_edges.rs::dollar_does_not_carry_annotation`, `tests/lex_edges.rs::semicolon_before_dollar`, `tests/lex_edges.rs::dollar_before_semicolon`, `tests/lex_edges.rs::dollar_at_start_of_line` (`#[ignore]`) | partial |
| §7 W900 | Unterminated `.subckt` produces warning, parse still succeeds | `tests/diagnostics.rs::w900_unterminated_subckt_returns_ok`, `tests/directives.rs::subckt_unterminated_yields_warning_not_error` | covered |
| §8 caveat 2 | Annotations MUST NOT appear inside `.control` blocks | `tests/directives.rs::control_star_lines_skipped`, `tests/lex_edges.rs::block_annotation_inside_control_not_processed` | covered |
| §9 | Spec versioning (`*@spec version=…`) | — | deferred |
| §9 | Net cosmetics (`*@net style=… label=…`) | — | deferred |
| §9 | Absolute / corner anchoring (`*@anchor`) | — | deferred |
| §9 | Per-instance overrides for `.subckt` instances | — | deferred |
| §9 | Multi-unit symbols (`unit=` story) | — | deferred |
| §9 | Wire routing hints (`*@route via=…`) | — | deferred |
| §9 | Bus / vector notation alignment | — | deferred |
| §9 | `align` under mixed orientation (under-specified) | — | deferred |
| §9 | Round-trip from KiCad back to annotations | — | deferred |

[^2]: Case-insensitivity of directive names themselves (the `symbol`, `place`, `align`, `pinmap`, `power`, `ignore` keywords) has no dedicated test; covered incidentally by `model_case_insensitive_name` for dotted directives and `eng_suffixes_case_insensitive` for number suffixes.
[^3]: Comma-separated multi-directive in one tag is explicitly rejected by the spec. There is no negative test verifying that `R1 a b 1k ;@ symbol=D:R, place=right-of V1` is treated as a single malformed directive rather than two.
[^4]: The parser preserves `.include` as a directive entry but does not resolve, read, or cluster it. The visual-cluster / hierarchical-sheet distinction is an emitter/layout concern not yet exercised.
[^5]: These are emitter or layout/policy requirements, not parser requirements. No tests exist anywhere in the repo for these (confirmed by grep).
[^6]: T (transmission line) element kind is not individually tested; it would be classified as `ElementKind::Other` by the current parser. All other listed element prefixes now have at least one test.
[^10]: The tests verify parser behaviour; they do not spin up a SPICE simulator to confirm invisibility. Spec §8 is a design invariant, not a testable parser property.

---

## 2. Test-by-Test Inventory

| Test | Spec refs | Description |
|------|-----------|-------------|
| `tests/fixtures.rs::rc_lowpass` | §3.1, §4.1, §4.6 | End-to-end parse of RC low-pass fixture; checks title, elements, block symbol defaults, ignore tag on V1, numeric values |
| `tests/fixtures.rs::common_emitter_subckt_and_align_and_power` | §3.1, §4.1, §4.4, §4.5, §4.6 | Common-emitter fixture: symbol defaults, power tag, BJT kind, ignore tag, `.model` capture |
| `tests/fixtures.rs::diff_pair_align_and_place` | §4.3, §4.4 | Diff-pair fixture: two horizontal align directives, `right-of` place tag on RC2 |
| `tests/fixtures.rs::opamp_subckt_definition` | §3, §3.1 | Opamp inverting fixture: `.subckt` with port list, VCVS (E1) body, X instance nodes and value |
| `tests/fixtures.rs::multivibrator_parses_cleanly` | §4.3 | Multivibrator fixture: Q1/Q2 present, Q2 has `place` tag anchored to Q1 |
| `tests/fixtures.rs::pinmap_tag_parses` | §2.1, §4.2 | Inline pinmap: `pinmap=1:A,2:K` parsed to spice_index/kicad_pin pairs with name references |
| `tests/fixtures.rs::mosfet_continuation` | §2.2, §3.1 | MOSFET with `+` continuation: nodes, model name, and params (L, W) all collected |
| `tests/numbers.rs::plain_integers_and_decimals` | §3.1 (value parsing) | Integer and decimal SPICE number tokens parse to correct f64 |
| `tests/numbers.rs::scientific_notation` | §3.1 | Scientific-notation number tokens parse correctly |
| `tests/numbers.rs::scientific_no_exponent_digits_is_string` | §3.1 | (ignored) `1e` without exponent digits: both ngspice and our parser return 1.0; assertion documents wrong expectation |
| `tests/numbers.rs::d_exponent_lowercase` | §3.1 | Fortran-style `d` exponent marker (lowercase): `1d3` = 1000 |
| `tests/numbers.rs::d_exponent_uppercase` | §3.1 | Fortran-style `D` exponent marker (uppercase): `1D3` = 1000 |
| `tests/numbers.rs::d_exponent_with_eng_suffix` | §3.1 | Fortran exponent combined with engineering suffix: `1.5d3k` = 1.5e6 |
| `tests/numbers.rs::eng_suffixes_lowercase` | §3.1 | All lowercase engineering suffixes (t, g, meg, k, m, u, n, p, f, mil) |
| `tests/numbers.rs::eng_suffixes_case_insensitive` | §2, §3.1 | Case-insensitive suffix variants (MEG, K, U, etc.) |
| `tests/numbers.rs::m_is_milli_not_mega` | §3.1 | `m`/`M` resolves to 1e-3 (milli), not 1e6 |
| `tests/numbers.rs::meg_precedence_over_m` | §3.1 | `Meg`/`MEG` resolves to 1e6, taking precedence over bare `m` rule |
| `tests/numbers.rs::trailing_unit_letters_dropped` | §3.1 | Unit-letter tails (Hz, F, Ohm) silently dropped after suffix |
| `tests/numbers.rs::infix_4k7_form` | §3.1 | LTspice/PSpice RKM infix form (4k7 = 4700) parsed |
| `tests/numbers.rs::signed_with_suffix` | §3.1 | Sign combined with engineering suffix parses correctly |
| `tests/numbers.rs::atto_suffix` | §3.1 | `a`/`A` atto (1e-18) suffix: ngspice-compatible; our parser returns 1e-18 |
| `tests/numbers.rs::rejections_non_numeric` | §3.1 | Pure alphabetic tokens do not produce Value::Number |
| `tests/numbers.rs::rejections_malformed_numbers` | §3.1 | Double decimal point and double sign do not produce Value::Number |
| `tests/elements.rs::resistor_basic` | §3.1 | R element: kind, nodes, numeric value |
| `tests/elements.rs::resistor_with_model` | §3.1 | R element with string model name as value |
| `tests/elements.rs::resistor_with_params` | §3.1 | R element with named param `tc` |
| `tests/elements.rs::resistor_with_ac_param` | §3.1 | R element with `ac=` named param |
| `tests/elements.rs::resistor_with_w_l_params` | §3.1 | R element with model name and W/L params (semiconductor resistor form) |
| `tests/elements.rs::capacitor_basic` | §3.1 | C element: kind, nodes, value |
| `tests/elements.rs::capacitor_with_ic` | §3.1 | C element with IC= initial condition in params |
| `tests/elements.rs::capacitor_with_model_form` | §3.1 | C element with non-numeric model name as value |
| `tests/elements.rs::inductor_basic` | §3.1 | L element: kind, nodes, value |
| `tests/elements.rs::inductor_with_ic` | §3.1 | L element with IC= initial condition in params |
| `tests/elements.rs::voltage_source_dc_only` | §3.1 | V element: plain numeric value |
| `tests/elements.rs::voltage_source_dc_keyword` | §3.1 | V element: `DC 12` preserved as String |
| `tests/elements.rs::voltage_source_ac_dc` | §3.1 | V element: `DC 0 AC 1` multi-token spec preserved |
| `tests/elements.rs::voltage_source_sin` | §3.1 | V element: `SIN(…)` spec preserved as String |
| `tests/elements.rs::voltage_source_pulse` | §3.1 | V element: `PULSE(…)` spec preserved as String |
| `tests/elements.rs::voltage_source_pwl` | §3.1 | V element: `PWL(…)` waveform spec preserved as String |
| `tests/elements.rs::voltage_source_exp` | §3.1 | V element: `EXP(…)` waveform spec preserved as String |
| `tests/elements.rs::voltage_source_sffm` | §3.1 | V element: `SFFM(…)` waveform spec preserved as String |
| `tests/elements.rs::current_source_basic` | §3.1 | I element: kind, nodes, value |
| `tests/elements.rs::current_source_ac_dc` | §3.1 | I element: `DC … AC …` multi-token spec preserved |
| `tests/elements.rs::diode_basic` | §3.1 | D element: kind, nodes, model string |
| `tests/elements.rs::diode_off` | §3.1 | D element with `OFF` positional keyword after model |
| `tests/elements.rs::diode_with_ic_param` | §3.1 | D element with IC= initial condition |
| `tests/elements.rs::bjt_3_terminal` | §3.1 | Q element: 3-node form, kind, model |
| `tests/elements.rs::bjt_4_terminal_ngspice_correct` | §3.1 | Q element: 4-terminal form with substrate node, ngspice-correct shape |
| `tests/elements.rs::bjt_4_terminal_substrate` | §3.1 | Q element: 4-node form with named substrate |
| `tests/elements.rs::bjt_with_ic` | §3.1 | Q element with IC= (comma-separated values); documents parser behaviour |
| `tests/elements.rs::mosfet_with_params` | §3.1 | M element: 4 nodes, model, L/W params |
| `tests/elements.rs::jfet_basic` | §3.1 | J element: 3 nodes, model |
| `tests/elements.rs::subckt_instance_variable_ports` | §3, §3.1 | X element: variable port count, model name as last token |
| `tests/elements.rs::subckt_instance_with_params` | §3, §3.1 | X element: ports, model name, key=value params |
| `tests/elements.rs::vcvs_e_basic_current_behaviour` | §3.1 | E element (VCVS, ElementKind::Other): 4 nodes, numeric gain as value |
| `tests/elements.rs::cccs_f_basic_ngspice_correct` | §3.1 | F element (CCCS): 2 output nodes, control vsrc refdes in params, numeric gain |
| `tests/elements.rs::ccvs_h_basic` | §3.1 | H element (CCVS): same syntax as F; 2 nodes, control param, numeric value |
| `tests/elements.rs::mutual_inductance_k_ngspice_correct` | §3.1 | K element: inductor refdes stored as nodes, coupling factor as value |
| `tests/elements.rs::mutual_k_with_decimal_coupling` | §3.1 | K element: decimal coupling factor parsed as Number |
| `tests/elements.rs::vccs_g_basic` | §3.1 | G element (VCCS, ElementKind::Other): 4 nodes, numeric transconductance as value |
| `tests/elements.rs::case_insensitive_refdes` | §2, §3.1 | Lower-case element prefix (`r1`) still maps to Resistor kind |
| `tests/elements.rs::pinmap_numeric_pin` | §4.2 | `pinmap=1:1,2:2` produces PinRef::Number entries |
| `tests/elements.rs::pinmap_mixed_number_and_name` | §4.2 | `pinmap=1:A,2:2` produces mixed PinRef::Name / PinRef::Number entries |
| `tests/directives.rs::subckt_basic` | §3, §3.1 | `.subckt`/`.ends` with ports and body element |
| `tests/directives.rs::subckt_ports_with_kv_params` | §3 | `.subckt` with key=value params in port list |
| `tests/directives.rs::subckt_params_keyword` | §3 | (ignored) `.subckt params:` ngspice extension not yet parsed |
| `tests/directives.rs::subckt_nested` | §3 | Nested `.subckt` definitions both land in `nl.subckts` |
| `tests/directives.rs::subckt_unterminated_yields_warning_not_error` | §3 | Missing `.ends` yields a warning, not an error; subckt still lands |
| `tests/directives.rs::ends_without_subckt_is_error` | §3 | Stray `.ends` emits E900 |
| `tests/directives.rs::model_npn_parenless` | §3.1 | `.model` without parens: type and params parsed |
| `tests/directives.rs::model_npn_paren_wrapped` | §3.1 | `.model` with `(…)` parens: type and params parsed |
| `tests/directives.rs::model_continuation` | §3.1 | `.model` with `+` continuation: multi-line params merged |
| `tests/directives.rs::model_case_insensitive_name` | §2 | `.MODEL` and `.Model` both recognised |
| `tests/directives.rs::include_preserved` | §3, §3.1 | `.include` preserved as directive with path in args |
| `tests/directives.rs::lib_preserved` | §3.1 | `.lib` preserved as directive |
| `tests/directives.rs::param_preserved` | §3.1 | `.param` preserved as directive |
| `tests/directives.rs::global_preserved` | §3.1 | `.global` preserved as directive with net names |
| `tests/directives.rs::tran_preserved` | §3.1 | `.tran` preserved as directive |
| `tests/directives.rs::ac_preserved` | §3.1 | `.ac` preserved as directive with args |
| `tests/directives.rs::dc_preserved` | §3.1 | `.dc` preserved as directive |
| `tests/directives.rs::op_preserved` | §3.1 | `.op` preserved as directive |
| `tests/directives.rs::print_preserved` | §3.1 | `.print` preserved as directive |
| `tests/directives.rs::ic_preserved` | §3.1 | `.ic` preserved as directive |
| `tests/directives.rs::measure_preserved` | §3.1 | `.measure` preserved as directive |
| `tests/directives.rs::option_preserved` | §3.1 | `.option` preserved as directive |
| `tests/directives.rs::end_does_not_crash` | §3.1 | `.end` stops parsing without panic; R1 before it is present |
| `tests/directives.rs::control_block_skipped` | §8 caveat 2 | `.control`/`.endc` block content not parsed as elements |
| `tests/directives.rs::control_star_lines_skipped` | §8 caveat 2 | `*@align` inside `.control` not processed as annotation |
| `tests/directives.rs::unknown_directive_preserved` | §3.1 | Unrecognised `.frobnitz` lands in `nl.directives` unchanged |
| `tests/directives.rs::directive_names_case_insensitive` | §2 | `.SUBCKT`/`.SubCkt`/`.subckt` all recognised |
| `tests/directives.rs::block_annotation_symbol_default` | §4.1 | `*@symbol Lib:Name for=GLOB` produces SymbolDefault in nl.annotations |
| `tests/directives.rs::block_annotation_align_horizontal` | §4.4 | `*@align horizontal R1 R2` produces Align with Horizontal axis |
| `tests/directives.rs::block_annotation_inside_subckt_lands_in_subckt` | §3, §4.4 | `*@align vertical` inside `.subckt` lands in subckt.annotations, not top-level |
| `tests/lex_edges.rs::title_is_first_line_comment` | §2 (lexer) | First line (even a `*` comment) becomes the title |
| `tests/lex_edges.rs::title_even_when_element_shaped` | §2 (lexer) | First line shaped like an element is still the title, not a parsed element |
| `tests/lex_edges.rs::crlf_line_endings` | §2 (lexer) | CRLF endings stripped; R1 parses normally |
| `tests/lex_edges.rs::mixed_lf_and_crlf` | §2 (lexer) | Mixed LF and CRLF in same file; both elements parse |
| `tests/lex_edges.rs::tab_separated_tokens` | §2 (lexer) | Tabs between tokens treated as whitespace |
| `tests/lex_edges.rs::empty_file` | §2 (lexer) | Empty source does not panic |
| `tests/lex_edges.rs::title_only` | §2 (lexer) | Source with only a title line has no elements |
| `tests/lex_edges.rs::only_dot_end` | §3.1 | `.end`-only file has no elements |
| `tests/lex_edges.rs::blank_lines_between_elements` | §2 (lexer) | Blank lines between elements are transparent |
| `tests/lex_edges.rs::continuation_basic` | §2.2 | `+` continuation merges params into preceding element |
| `tests/lex_edges.rs::continuation_carries_tag` | §2.2 | `;@` tag on `+` continuation line binds to the logical element |
| `tests/lex_edges.rs::multiple_continuations` | §2.2 | Multiple `+` lines: all params collected |
| `tests/lex_edges.rs::continuation_after_blank_line` | §2.2 | `+` after a blank line still continues; one logical code line |
| `tests/lex_edges.rs::continuation_after_comment_line` | §2.2 | `+` after a pure `*` comment still continues |
| `tests/lex_edges.rs::tab_indented_continuation` | §2.2 | Tab-indented `+` continuation still attaches |
| `tests/lex_edges.rs::standalone_tag_attaches_to_previous` | §2.3 | Standalone `;@` line (no element prefix) attaches to previous element |
| `tests/lex_edges.rs::multiple_tags_on_element_line` | §2.3 | Multiple `;@` tags on one element line: both collected |
| `tests/lex_edges.rs::tag_on_continuation_line` | §2.2, §2.3 | `;@ ignore` on a continuation line binds to the element |
| `tests/lex_edges.rs::block_annotation_top_level` | §4.1 | Top-level `*@symbol` lands in `nl.annotations` |
| `tests/lex_edges.rs::block_annotation_inside_subckt` | §3, §4.1 | `*@symbol` inside `.subckt` lands in subckt.annotations |
| `tests/lex_edges.rs::control_block_skipped` | §8 caveat 2 | R99 inside `.control` not parsed as element |
| `tests/lex_edges.rs::control_block_case_insensitive` | §8 caveat 2 | `.CONTROL`/`.ENDC` (uppercase) recognised |
| `tests/lex_edges.rs::control_block_mixed_case` | §8 caveat 2 | `.Control`/`.endc` (mixed case) recognised |
| `tests/lex_edges.rs::block_annotation_inside_control_not_processed` | §8 caveat 2 | `*@symbol` inside `.control` produces no annotations |
| `tests/lex_edges.rs::pure_comment_dropped` | §2 | Pure `*` comment lines dropped from code line list |
| `tests/lex_edges.rs::prose_semicolon_comment_no_tags` | §2, §8 | `;` without `@` is prose comment; element has no tags |
| `tests/lex_edges.rs::dollar_inline_comment` | §8 | `$` preceded by space is a comment introducer; tokens after stripped |
| `tests/lex_edges.rs::dollar_with_leading_space_required` | §8 | `$` not preceded by whitespace/comma is NOT a comment introducer |
| `tests/lex_edges.rs::dollar_after_comma` | §8 | `$` after a comma is a comment introducer |
| `tests/lex_edges.rs::dollar_does_not_carry_annotation` | §8 | `$@` is not an annotation marker; treated as prose comment |
| `tests/lex_edges.rs::semicolon_before_dollar` | §8 | `;@` wins over later `$` prose |
| `tests/lex_edges.rs::dollar_before_semicolon` | §8 | `$` wins; `;@` inside `$` comment is not harvested |
| `tests/lex_edges.rs::double_slash_comment` | §8 | (ignored) `//` comment not stripped by current lexer; ngspice does not support it either |
| `tests/lex_edges.rs::place_relation_left_of` | §4.3 | `left-of` relation parses to Relation::LeftOf with correct anchor |
| `tests/lex_edges.rs::place_relation_above` | §4.3 | `above` relation parses to Relation::Above with correct anchor |
| `tests/lex_edges.rs::place_relation_below` | §4.3 | `below` relation parses to Relation::Below with correct anchor |
| `tests/lex_edges.rs::place_dotted_anchor` | §2.1, §4.3 | Dotted subcircuit path (`XU2.R5`) preserved verbatim as anchor |
| `tests/lex_edges.rs::tag_no_space_after_marker` | §2 | No-space form `;@symbol=…` produces Symbol tag |
| `tests/lex_edges.rs::continuation_with_dollar_comment` | §2.2, §8 | `$` comment on continuation line: param survives, tokens after `$` stripped |
| `tests/lex_edges.rs::continuation_immediately_after_block_annotation` | §2.2 | `+` after a `*@` block line attaches to the preceding Code line, not the annotation |
| `tests/lex_edges.rs::subckt_with_block_annotations_inside` | §3, §4.1, §4.4 | Both `*@symbol` and `*@align` inside a subckt land in subckt.annotations |
| `tests/lex_edges.rs::nested_subckt_block_annotation_scope` | §3, §4.1 | Annotation inside inner subckt scoped to inner only; outer and top-level are empty |
| `tests/corpus.rs::ngspice_corpus_parses` | §3.1, §8 | (env-gated) Parses all `.cir` under `$NGSPICE_SRC/tests/` |
| `src/lexer.rs::title_is_first_physical_line` | §2 (lexer) | Scanner: first physical line becomes title, one code line remains |
| `src/lexer.rs::continuation_appends_words_and_tags` | §2.2 | Scanner: `+` merges words and tag from continuation into preceding line |
| `src/lexer.rs::block_annotation_emits_block_line` | §2 | Scanner: `*@` line gets `LineKind::BlockAnnotation` and correct word split |
| `src/lexer.rs::pure_comment_dropped` | §2 | Scanner: pure `*` comment produces no code line |
| `src/lexer.rs::control_block_skipped` | §8 caveat 2 | Scanner: `.control`/`.endc` block absent from output lines |
| `src/lexer.rs::multiple_tags_on_one_line` | §2.3 | Scanner: two `;@` tags on one line yields two tag entries |
| `src/lexer.rs::standalone_tag_line_attaches_to_previous` | §2.3 | Scanner: standalone `;@` line merges into previous line's tags |
| `src/lexer.rs::equals_and_parens_are_separators` | §2 (lexer) | Scanner: `=` and `(` / `)` are token separators |
| `src/parser.rs::parses_engineering_suffixes` | §3.1 | Parser unit test: engineering suffix table spot-checks |
| `src/parser.rs::rejects_non_numbers` | §3.1 | Parser unit test: pure alphabetic tokens not numbers |
| `spice-resolve/tests/resolve.rs::resistor_default_resolution` | §4.1 | Resolver: R element maps to `Device:R` by built-in default; pin mapping and role correct |
| `spice-resolve/tests/resolve.rs::trailing_symbol_tag_overrides_default` | §4.1 | Resolver: trailing `symbol=` tag wins over built-in default |
| `spice-resolve/tests/resolve.rs::block_symbol_default_with_glob` | §4.1 | Resolver: `*@symbol … for=R*` matches R10/R20 but not C1; glob works |
| `spice-resolve/tests/resolve.rs::later_block_annotation_wins_for_matches` | §4.1 | Resolver: later block annotation beats earlier one on same refdes |
| `spice-resolve/tests/resolve.rs::trailing_tag_beats_block_annotation` | §4.1 | Resolver: trailing tag beats all block annotations |
| `spice-resolve/tests/resolve.rs::pinmap_swaps_terminals` | §4.2 | Resolver: `pinmap=1:2,2:1` swaps the resolved pin mapping |
| `spice-resolve/tests/resolve.rs::pinmap_can_reference_pin_by_name` | §4.2 | Resolver: `pinmap=1:B,2:C,3:E` pin-by-name resolves to correct KiCad pin numbers |
| `spice-resolve/tests/resolve.rs::pinmap_with_unknown_pin_is_e005` | §7 E005 | Resolver: pinmap referencing non-existent KiCad pin number emits E005 |
| `spice-resolve/tests/resolve.rs::pinmap_duplicate_spice_index_is_e005` | §7 E005 | Resolver: duplicate SPICE index in pinmap emits E005 |
| `spice-resolve/tests/resolve.rs::pinmap_duplicate_kicad_pin_is_e005` | §7 E005 | Resolver: duplicate KiCad pin target in pinmap emits E005 |
| `spice-resolve/tests/resolve.rs::pinmap_out_of_range_index_is_e005` | §7 E005 | Resolver: out-of-range SPICE index in pinmap emits E005 |
| `spice-resolve/tests/resolve.rs::pin_count_mismatch_no_pinmap_is_e002` | §7 E002 | Resolver: element with wrong terminal count (no pinmap) emits E002 |
| `spice-resolve/tests/resolve.rs::unknown_lib_id_is_e003` | §7 E003 | Resolver: symbol tag pointing to non-existent library ID emits E003 |
| `spice-resolve/tests/resolve.rs::subckt_instance_without_symbol_is_error` | §7 E003 | Resolver: X instance with no explicit symbol mapping emits E003 |
| `spice-resolve/tests/resolve.rs::power_tag_marks_role` | §4.5 | Resolver: `power=vcc` tag sets ElementRole::Power on the resolved element |
| `spice-resolve/tests/resolve.rs::ignore_tag_drops_element` | §4.6 | Resolver: `ignore` tag removes element from resolved output |
| `spice-resolve/tests/resolve.rs::place_tag_passes_through` | §4.3 | Resolver: `place` tag passes through to PlaceSpec in resolved output |
| `spice-resolve/tests/resolve.rs::align_annotation_passes_through_unvalidated` | §4.4 | Resolver: `align` annotation (even with unknown refdes) passes to AlignSpec; refdes validation is the policy pass's job |
| `spice-resolve/tests/resolve.rs::subckt_body_resolves` | §3 | Resolver: element inside a `.subckt` body resolves correctly using subckt-scoped annotations |
| `spice-policy/tests/policy.rs::all_clean_yields_ok_with_no_warnings` | §5 | Policy: clean input (valid align + place, no conflicts) produces no warnings |
| `spice-policy/tests/policy.rs::e001_align_unknown_refdes` | §7 E001 | Policy: unknown refdes in align cluster emits E001 |
| `spice-policy/tests/policy.rs::e001_place_unknown_refdes` | §7 E001 | Policy: unknown subject refdes in place directive emits E001 |
| `spice-policy/tests/policy.rs::e001_place_unknown_anchor` | §7 E001 | Policy: unknown anchor refdes in place directive emits E001 |
| `spice-policy/tests/policy.rs::e001_collects_multiple` | §7 E001 | Policy: all unknown-refdes errors (align + place) collected together |
| `spice-policy/tests/policy.rs::w102_single_member_cluster` | §7 W102 | Policy: single-member align cluster emits W102 and is removed |
| `spice-policy/tests/policy.rs::w102_duplicates_collapse_to_single` | §7 W102 | Policy: duplicates in align cluster collapse to single member → W102 |
| `spice-policy/tests/policy.rs::w104_place_on_align_fixed_element` | §5, §7 W104 | Policy: place on align-fixed element emits W104 and drops the place entry |
| `spice-policy/tests/policy.rs::w101_duplicate_place_keeps_first` | §5, §7 W101 | Policy: two place directives for same element → W101, first wins |
| `spice-policy/tests/policy.rs::w104_alone_when_place_overlaps_align` | §7 W104 | Policy: align-fixed element with one place emits only W104 (no spurious W101) |
| `spice-policy/tests/policy.rs::e006_two_cycle_same_axis` | §7 E006 | Policy: two-element same-axis cycle in place graph emits E006 |
| `spice-policy/tests/policy.rs::e006_three_cycle_same_axis` | §7 E006 | Policy: three-element same-axis cycle emits exactly one E006 |
| `spice-policy/tests/policy.rs::cross_axis_loop_is_not_a_cycle` | §7 E006 | Policy: cross-axis loop (right-of + above) is not a cycle; no E006 |
| `spice-policy/tests/policy.rs::e006_disjoint_cycles_each_reported` | §7 E006 | Policy: two disjoint cycles each produce their own E006 |
| `spice-policy/tests/policy.rs::errors_carry_warnings_too` | §7 W104, §7 E006 | Policy: when fatal errors exist, non-fatal warnings (W104) are still reported |
| `spice-policy/tests/policy.rs::idempotence_after_cleanup` | §5 | Policy: checked output is stable under re-check (no spurious warnings) |
| `spice-policy/tests/policy.rs::acyclic_inputs_check_ok` (proptest) | §5, §7 E006 | Property: arbitrarily constructed acyclic inputs never produce errors |
| `spice-policy/tests/policy.rs::cyclic_inputs_emit_e006` (proptest) | §7 E006 | Property: inputs with a guaranteed X-axis cycle always produce E006 |
| `spice-policy/tests/policy.rs::idempotent` (proptest) | §5 | Property: re-checking any clean output produces no new warnings |

---

## 3. Gap Summary

Ordered by correctness impact on the round-trip pipeline:

- **§7 E004** — `align` cross-sheet-boundary check not yet implemented or tested. The resolver passes `align` through unvalidated (`align_annotation_passes_through_unvalidated` explicitly notes this). No test exists anywhere in the repo.
  Proposed test: `spice-policy/tests/policy.rs::e004_align_crosses_sheet_boundary` — build a `ResolvedNetlist` with subckt scope metadata and an align cluster whose members belong to different sheets; assert E004.

- **§7 E007** — Internal layout-stall diagnostic. The code path exists in `spice-layout/src/lib.rs` but is not reachable by normal inputs (it fires only when the placement loop fails to make progress). No test exercises it.
  Proposed test: `spice-layout/tests/cases.rs::e007_layout_stall` — construct a degenerate input that causes the placer to stall; assert E007 in the returned diagnostics.

- **§4.1 case-insensitive glob matching** — `block_symbol_default_with_glob` uses `R*` matching uppercase `R10`/`R20`, but no test exercises a lowercase `for=r*` glob matching an uppercase refdes (cross-case).
  Proposed test: `spice-resolve/tests/resolve.rs::symbol_glob_case_insensitive` — `for_glob: "r*"` should match `R1`.

- **§2 directive keyword case-insensitivity** — no dedicated test for the `symbol`, `place`, `align`, `pinmap`, `power`, `ignore` keywords in mixed case.
  Proposed test: `tests/directives.rs::tag_directive_keywords_case_insensitive` — `R1 a b 1k ;@ IGNORE`, assert `Tag::Ignore` is produced.

- **§2.3 comma-separated multi-directive rejection** — no negative test that a single tag containing a comma is treated as one malformed directive.
  Proposed test: `tests/lex_edges.rs::tag_no_comma_multi_directive` — `R1 a b 1k ;@ symbol=D:R, place=right-of V1`, assert only one tag is parsed (not two).

- **§3 T (transmission line) element kind** — no test for the `T` element prefix; classified as `ElementKind::Other` by the current parser.
  Proposed test: `tests/elements.rs::transmission_line_basic` — parse `T1 in 0 out 0 Z0=50` and assert `ElementKind::Other`.

- **§3.1 K (mutual inductance) schematic representation** — no canonical KiCad library symbol matches the SPICE concept of `K L1 L2 coupling` (a relationship, not a 2-pin component). The resolver therefore raises E003 unless the user supplies `;@ symbol=`. Choice between (a) require user-supplied symbol, (b) render as a layout-pass annotation between coupled L symbols, or (c) emit nothing with a warning is still open (see `/tmp/phase3-decisions.md` "Open questions"). No test exists for any of these paths.

- **§3.1 F/H (current-controlled sources) schematic representation** — KiCad's stock `Simulation_SPICE` library ships no CCCS/CCVS symbol (only voltage-controlled E/G). F and H therefore require user-supplied `;@ symbol=`; the resolver E003 message is sharpened to call this out, but no positive test exercises a user-supplied F/H symbol mapping end-to-end.

- **§3 auto-grounding of net `0` and `.global` nets** — parser preserves `.global` but no test asserts downstream ground-symbol rendering.
  Proposed test: integration test in `kicad-emitter` — emit a netlist with `V1 vcc 0 5`, assert ground power symbol in output.

- **`tests/directives.rs::subckt_params_keyword`** — remains `#[ignore]`: the ngspice `params:` keyword extension is not yet parsed; `params:` token lands in the ports list.

- **`tests/lex_edges.rs::double_slash_comment`** — remains `#[ignore]`: `//` is not a comment introducer in ngspice or our lexer; kept to document the intentional non-support.
