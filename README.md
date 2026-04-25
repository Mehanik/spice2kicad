# spice2kicad

Convert SPICE netlists into KiCad schematics (`.kicad_sch`) and netlists (`.net`).

## Status

Early scaffolding. Parser and emitter are stubbed; the CLI plumbing and
project structure are in place.

## Install

```sh
cargo install --path crates/spice2kicad
```

## Usage

```sh
spice2kicad input.cir --target schematic --output input.kicad_sch
spice2kicad input.cir --target netlist   --output input.net
```

Supported SPICE dialects: generic Berkeley SPICE3 (LTspice / ngspice / PSpice
extensions are on the roadmap).

## Project layout

```
crates/
  spice-parser/    SPICE source -> typed AST
  kicad-emitter/   AST -> KiCad S-expressions
  spice2kicad/     CLI binary
```

## Development

```sh
just check         # fmt + clippy + test
just test
just hooks         # install git pre-commit hooks
```

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[MIT](LICENSE).
