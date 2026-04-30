# ngspice Syntactic-Surface Coverage Matrix

This document maps every element-parser source file in ngspice, plus the key
lexer/preprocessing rules and directives, to the tests in our
`crates/spice-parser` crate that exercise the corresponding syntactic surface.
It is a read-only audit; it does not prescribe what to implement next.

**How to read it.**  The "Covered by" column lists `file::function_name`
references; `—` means no test touches that construct.  The "Status" column is
one of: `covered` (happy-path shape exercised), `partial` (some sub-forms or
parameters not reached), `gap` (recognised by ngspice, not tested at all), or
`out-of-scope` (device/feature deliberately outside our conversion target).

ngspice source paths are relative to `/home/eugene/Projects/ngspice/`.
Our test paths are relative to `crates/spice-parser/tests/`.

---

## 1. Element-Parser Coverage

| ngspice file | Element kind / construct | Syntactic form | Covered by | Status |
|---|---|---|---|---|
| `src/spicelib/parser/inp2r.c` | Resistor (R) — basic | `Rname n+ n- val` | `elements.rs::resistor_basic` | covered |
| `src/spicelib/parser/inp2r.c` | Resistor (R) — model name | `Rname n+ n- mname [w=] [l=]` | `elements.rs::resistor_with_model`, `elements.rs::resistor_with_w_l_params` | covered |
| `src/spicelib/parser/inp2r.c` | Resistor (R) — named params (tc, ac, noise…) | `Rname n+ n- val [tc=val] [ac=val]` | `elements.rs::resistor_with_params`, `elements.rs::resistor_with_ac_param` | partial — `noise=` untested |
| `src/spicelib/parser/inp2r.c` | Resistor (R) — RKM infix form | `4k7` as value via `INPevaluateRKM_R` | `numbers.rs::infix_4k7_form` | covered |
| `src/spicelib/parser/inp2c.c` | Capacitor (C) | `Cname n+ n- [val] [mname] [IC=val]` | `elements.rs::capacitor_basic`, `elements.rs::capacitor_with_ic`, `elements.rs::capacitor_with_model_form` | covered |
| `src/spicelib/parser/inp2l.c` | Inductor (L) | `Lname n+ n- [val] [mname] [IC=val]` | `elements.rs::inductor_basic`, `elements.rs::inductor_with_ic` | covered |
| `src/spicelib/parser/inp2v.c` | Voltage source (V) — DC | `Vname n+ n- [DC] val` | `elements.rs::voltage_source_dc_only`, `elements.rs::voltage_source_dc_keyword` | covered |
| `src/spicelib/parser/inp2v.c` | Voltage source (V) — AC+DC | `Vname n+ n- DC v1 AC v2 [phase]` | `elements.rs::voltage_source_ac_dc` | covered |
| `src/spicelib/parser/inp2v.c` | Voltage source (V) — SIN waveform | `Vname n+ n- SIN(…)` | `elements.rs::voltage_source_sin` | covered |
| `src/spicelib/parser/inp2v.c` | Voltage source (V) — PULSE waveform | `Vname n+ n- PULSE(…)` | `elements.rs::voltage_source_pulse` | covered |
| `src/spicelib/parser/inp2v.c` | Voltage source (V) — EXP / PWL / SFFM waveforms | `Vname n+ n- EXP(…)` / `PWL(…)` / `SFFM(…)` | `elements.rs::voltage_source_exp`, `elements.rs::voltage_source_pwl`, `elements.rs::voltage_source_sffm` | covered |
| `src/spicelib/parser/inp2v.c` | Voltage source (V) — AM / TRRANDOM waveforms | `Vname n+ n- AM(…)` / `TRRANDOM(…)` | `elements.rs::voltage_source_am`, `elements.rs::voltage_source_trrandom` | covered |
| `src/spicelib/parser/inp2i.c` | Current source (I) | `Iname n+ n- [DC] val [AC …] [<transient fn>]` | `elements.rs::current_source_basic`, `elements.rs::current_source_ac_dc` | covered |
| `src/spicelib/parser/inp2i.c` | Current source (I) — SIN / PWL waveforms | `Iname n+ n- SIN(…)` / `PWL(…)` | `elements.rs::current_source_sin`, `elements.rs::current_source_pwl` | covered |
| `src/spicelib/parser/inp2d.c` | Diode (D) | `Dname anode cathode [temp] model [val] [OFF] [IC=val]` | `elements.rs::diode_basic`, `elements.rs::diode_off`, `elements.rs::diode_with_ic_param` | partial — 3-node temp form untested |
| `src/spicelib/parser/inp2q.c` | BJT (Q) — 3 terminal | `Qname c b e model [val] [OFF] [IC=…]` | `elements.rs::bjt_3_terminal`, `elements.rs::bjt_with_ic` | covered |
| `src/spicelib/parser/inp2q.c` | BJT (Q) — 4 terminal (substrate) | `Qname c b e s model` | `elements.rs::bjt_4_terminal_ngspice_correct`, `elements.rs::bjt_4_terminal_substrate` | covered |
| `src/spicelib/parser/inp2m.c` | MOSFET (M) — standard 4-node | `Mname d g s b model [L=] [W=] [AD=] [AS=] …` | `elements.rs::mosfet_with_params`, `fixtures.rs::mosfet_continuation` | partial — extended SOI (5–7 node) and VDMOS (3-node) forms untested |
| `src/spicelib/parser/inp2n.c` | MOSFET alt (N) — same shape as M but dispatched separately | `Nname d g s b model [params]` | `elements.rs::n_mosfet_designator_and_params_preserved`, `elements.rs::n_mosfet_falls_into_other` | partial — preserved as `ElementKind::Other`; no dedicated kind |
| `src/spicelib/parser/inp2j.c` | JFET (J) | `Jname d g s model [val] [OFF] [IC=…]` | `elements.rs::jfet_basic` | partial — `OFF`, `IC=` untested |
| `src/spicelib/parser/inp2e.c` | VCVS (E) — `ElementKind::Vcvs` | `Ename out+ out- ctrl+ ctrl- gain` | `elements.rs::vcvs_e_basic`, `spice-resolve/tests/resolve.rs::vcvs_default_resolves_to_esource` | partial — poly(n) and `VALUE=` extended forms untested |
| `src/spicelib/parser/inp2f.c` | CCCS (F) — `ElementKind::Cccs` | `Fname out+ out- vname gain` (controlling Vname stored on typed `control` field) | `elements.rs::cccs_f_basic_ngspice_correct` | covered |
| `src/spicelib/parser/inp2g.c` | VCCS (G) — `ElementKind::Vccs` | `Gname out+ out- ctrl+ ctrl- transconductance` | `elements.rs::vccs_g_basic`, `spice-resolve/tests/resolve.rs::vccs_default_resolves_to_gsource` | covered — basic 4-node+value form; poly(n) and `VALUE=` extended forms untested |
| `src/spicelib/parser/inp2h.c` | CCVS (H) — `ElementKind::Ccvs` | `Hname out+ out- vname transresistance` (controlling Vname stored on typed `control` field) | `elements.rs::ccvs_h_basic` | covered |
| `src/spicelib/parser/inp2k.c` | Mutual inductance (K) — `ElementKind::MutualInductance` | `Kname Lname Lname coupling` (coupled L-refs stored on typed `coupled` field; `nodes` left empty) | `elements.rs::mutual_inductance_k_ngspice_correct`, `elements.rs::mutual_k_with_decimal_coupling` | covered |
| `src/spicelib/parser/inp2x.c` | Subcircuit instance (X) — dispatched from `inpcom.c:2884` | `Xname n1 … nN subcktname [KEY=val …]` | `elements.rs::subckt_instance_variable_ports`, `elements.rs::subckt_instance_with_params` | covered |
| `src/spicelib/parser/inp2b.c` | Arbitrary source (B) | `Bname n+ n- [V=expr] [I=expr]` | `elements.rs::b_source_v_expression_fragmented`, `elements.rs::b_source_i_expression_fragmented`, `elements.rs::b_source_braced_expression` (`#[ignore]`) | partial — fragmented `V=`/`I=` covered; braced `{…}` numparam form still ignored |
| `src/spicelib/parser/inp2t.c` | Lossless transmission line (T) | `Tname n1 n2 n3 n4 [TD=val] [F=val [NL=val]] [IC=…]` | — | out-of-scope |
| `src/spicelib/parser/inp2o.c` | Lossy transmission line (O) | `Oname n1 n2 n3 n4 [IC=…] model` | — | out-of-scope |
| `src/spicelib/parser/inp2s.c` | Voltage-controlled switch (S) | `Sname n1 n2 ctrl+ ctrl- [model] [IC]` | — | out-of-scope |
| `src/spicelib/parser/inp2w.c` | Current-controlled switch (W) | `Wname n1 n2 vctrl [model] [IC]` | — | out-of-scope |
| `src/spicelib/parser/inp2u.c` | Uniform RC line (U) | `Uname n1 n2 model [l=] [n=]` | — | out-of-scope |
| `src/spicelib/parser/inp2p.c` | Coupled transmission lines (P) | `Pname n1 gnd n2 gnd … model [length=]` | — | out-of-scope |
| `src/spicelib/parser/inp2y.c` | Single-conductor TXL (Y) | `Yname n1 gnd n2 gnd model` | — | out-of-scope |
| `src/spicelib/parser/inp2z.c` | MESFET / HFET (Z) | `Zname d g s model [val] [OFF] [IC=…]` | — | out-of-scope |

---

## 2. Lexer / Preprocessing Coverage

Source references are to `src/frontend/inpcom.c` unless otherwise noted.

| Rule | ngspice source ref | Covered by | Status |
|---|---|---|---|
| Title is the first physical line (even if it looks like a comment or element) | `src/spicelib/parser/inpgtitl.c:19`; `INPgetTitle` skips the first card unconditionally | `lex_edges.rs::title_is_first_line_comment`, `lex_edges.rs::title_even_when_element_shaped`, `lexer.rs::title_is_first_physical_line` | covered |
| `+` continuation — merges next logical line into previous | `inpcom.c:674` `inp_stitch_continuation_lines`, case `'+'` at line 704 | `lex_edges.rs::continuation_basic`, `lex_edges.rs::multiple_continuations`, `lex_edges.rs::tab_indented_continuation`, `lexer.rs::continuation_appends_words_and_tags` | covered |
| `+` continuation — blank or `*` comment lines between continuation lines are silently skipped | `inpcom.c:704`–720 (switch skips `'*'`/`'\0'` and keeps `prev` pointer) | `lex_edges.rs::continuation_after_blank_line`, `lex_edges.rs::continuation_after_comment_line` | covered |
| `;` inline comment — truncates remainder of line | `inpcom.c:3613` (`*d == ';'` breaks the loop in `inp_stripcomments_line`) | `lex_edges.rs::prose_semicolon_comment_no_tags` | covered |
| `$` inline comment (outside `.control`, non-PS mode) — truncates if preceded by space or comma | `inpcom.c:3619`–3627 (`c == '$'` branch in `inp_stripcomments_line`) | `lex_edges.rs::dollar_inline_comment`, `lex_edges.rs::dollar_with_leading_space_required`, `lex_edges.rs::dollar_after_comma`, `lex_edges.rs::dollar_does_not_carry_annotation` | covered |
| `//` inline comment — strips rest of line | `inpcom.c:3633`–3635 (`c == '/'` and `*d == '/'` branch) | `lex_edges.rs::double_slash_comment` — `#[ignore]`; our lexer intentionally does not strip `//` (ngspice-only, `$` and `;` are the portable forms) | out-of-scope |
| `*` line comment — line starting with `*` treated as full comment | `inpcom.c:3582` (`*s == '*'` early return in `inp_stripcomments_line`) | `lex_edges.rs::pure_comment_dropped` | covered |
| CRLF line endings — `\r` stripped before processing | `inpcom.c:1864`–1868 (`s[-1] == '\r'` zap) | `lex_edges.rs::crlf_line_endings`, `lex_edges.rs::mixed_lf_and_crlf` | covered |
| Bare `\r` line endings — single physical line; matches `inpcom.c` which only zaps `\r` before `\n` | `inpcom.c:1864`–1868 | `edge_inputs.rs::bare_cr_line_endings` | covered |
| Dangling `+` continuation at file start — no preceding line; produces a bogus element | `inpcom.c:704` (continuation logic with no `prev`) | `edge_inputs.rs::continuation_at_start_of_file` | covered |
| `.control … .endc` block — all content skipped | `inpcom.c:948`, `inpcom.c:3541`–3545 (`inp_stripcomments_deck` sets `found_control`) | `lex_edges.rs::control_block_skipped`, `lex_edges.rs::control_block_case_insensitive`, `directives.rs::control_block_skipped`, `lexer.rs::control_block_skipped` | covered |
| `.include "file"` — resolved and spliced at load time | `inpcom.c:1486` (`ciprefix(".include", buffer)`) | `directives.rs::include_preserved` — we preserve as directive, do not follow | partial — scope limit: we do not follow includes; test confirms preservation |
| `.lib "file" name` — library section selected | `inpcom.c:1798` (`ciprefix(".lib", buffer)`) | `directives.rs::lib_preserved` — we preserve as directive | partial — same scope limit as `.include` |
| `.if` / `.elseif` / `.else` / `.endif` conditional preprocessing | `inpcom.c:7941`, `inpcom.c:9099` | `directives.rs::if_endif_preserved_with_body`, `directives.rs::if_else_endif_preserves_both_branches` | covered — preservation only; conditional evaluation N/A (not parser scope) |
| Engineering suffixes T/G/Meg/K/M/U/N/P/F/Mil (case-insensitive) | `src/spicelib/parser/inpeval.c:143`–193 (`INPevaluate` switch) | `numbers.rs::eng_suffixes_lowercase`, `numbers.rs::eng_suffixes_case_insensitive`, `numbers.rs::m_is_milli_not_mega`, `numbers.rs::meg_precedence_over_m`, `parser.rs::parses_engineering_suffixes` | covered |
| `4k7` RKM infix form (multiplier between integer and fractional digits) | `src/spicelib/parser/inpeval.c:206`–448 (`INPevaluateRKM_R`); documented at line 206 | `numbers.rs::infix_4k7_form` | covered |
| Atto suffix `a`/`A` = 1e-18 | `src/spicelib/parser/inpeval.c:172`–175 (case `'a'`/`'A'` in main `INPevaluate`) | `numbers.rs::atto_suffix` | covered |
| Scientific notation `1e-3`, `1E+6` | `src/spicelib/parser/inpeval.c:120` (`'E'`/`'e'` branch) | `numbers.rs::scientific_notation` | covered |
| Number overflow (`1e500` → `Value::Number(inf)`) — matches ngspice's `INPevaluate` silent overflow | `src/spicelib/parser/inpeval.c:120` (no overflow check) | `edge_inputs.rs::number_overflow_input` | covered |
| D/d Fortran-style exponent (`1d3`, `1D-9`, `1.5d3k`) | `src/spicelib/parser/inpeval.c:120` (`'D'`/`'d'` same branch as `'E'`/`'e'`) | `numbers.rs::d_exponent_lowercase`, `numbers.rs::d_exponent_uppercase`, `numbers.rs::d_exponent_with_eng_suffix` | covered |
| Trailing unit letters dropped (e.g. `1kOhm` → 1000) | `inpcom.c:3104`–3115 (strips `ohms`, `farad`, `henry`) | `numbers.rs::trailing_unit_letters_dropped` | covered |

---

## 3. Directive Coverage

Directive dispatch is in `src/spicelib/parser/inp2dot.c:837`–955.
Pre-processing (`.subckt`, `.include`, `.lib`) is in `src/frontend/inpcom.c`.

| Directive | `inp2dot.c` ref | Covered by | Status |
|---|---|---|---|
| `.subckt` / `.ends` — definition parsed, nested OK | `inpcom.c:2884`; `inp2dot.c:917` | `directives.rs::subckt_basic`, `directives.rs::subckt_ports_with_kv_params`, `directives.rs::subckt_params_keyword`, `directives.rs::subckt_nested`, `directives.rs::subckt_unterminated_yields_warning_not_error`, `directives.rs::ends_without_subckt_is_error` | covered |
| `.model name type (params…)` — paren and paren-less forms | `inp2dot.c:837` | `directives.rs::model_npn_parenless`, `directives.rs::model_npn_paren_wrapped`, `directives.rs::model_continuation`, `directives.rs::model_case_insensitive_name` | covered |
| `.include "file"` | `inpcom.c:1486` | `directives.rs::include_preserved` | partial — preserved, not followed |
| `.lib "file" name` | `inpcom.c:1798` | `directives.rs::lib_preserved` | partial — preserved, not followed |
| `.param key=val` | `inp2dot.c:841` | `directives.rs::param_preserved` | covered |
| `.global net …` | `inp2dot.c:947` | `directives.rs::global_preserved` | covered |
| `.tran tstep tstop [tstart [tmax]] [UIC]` | `inp2dot.c:893` | `directives.rs::tran_preserved` | covered |
| `.ac dec/oct/lin np fstart fstop` | `inp2dot.c:878` | `directives.rs::ac_preserved` | covered |
| `.dc src start stop incr [src2 …]` | `inp2dot.c:884` | `directives.rs::dc_preserved` | covered |
| `.op` | `inp2dot.c:859` | `directives.rs::op_preserved` | covered |
| `.print type vars…` / `.plot` | `inp2dot.c:846` | `directives.rs::print_preserved` | covered |
| `.probe` | `inp2dot.c:939` (falls through to `.print`/`.plot` branch; treated as obs.) | `directives.rs::probe_preserved` | covered |
| `.ic node=val …` | `inp2dot.c:876` | `directives.rs::ic_preserved` | covered |
| `.nodeset node=val …` | `inp2dot.c:863` (`.nodeset` → goto quit) | `directives.rs::nodeset_preserved` | covered |
| `.save` | not in `inp2dot.c` switch; handled upstream | `directives.rs::save_preserved` | covered |
| `.measure` / `.meas` | `inp2dot.c:955` | `directives.rs::measure_preserved` | covered |
| `.func name(args) expr` | not in `inp2dot.c` switch; evaluated in numparam layer | `directives.rs::func_preserved` | covered |
| `.option` / `.options` / `.opt` | `inp2dot.c:940` | `directives.rs::option_preserved` | covered |
| `.temp` | `inp2dot.c:852` (warns and ignores) | `directives.rs::temp_preserved` | covered |
| `.end` | `inp2dot.c:922` | `directives.rs::end_does_not_crash` | covered |
| `.control` / `.endc` | `inpcom.c:948` | `directives.rs::control_block_skipped`, `lex_edges.rs::control_block_skipped` | covered |
| `.if` / `.elseif` / `.else` / `.endif` | `inpcom.c:7941`, `inpcom.c:9099` | `directives.rs::if_endif_preserved_with_body`, `directives.rs::if_else_endif_preserves_both_branches` | covered — preservation only; conditional evaluation N/A |
| Unknown directive | (falls off switch; preserved by our parser) | `directives.rs::unknown_directive_preserved` | covered |
| Directive name case-insensitivity | (ngspice uses `ciprefix`/`strcmp` on lowercased tokens throughout) | `directives.rs::directive_names_case_insensitive` | covered |
