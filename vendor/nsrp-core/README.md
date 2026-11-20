# NSRP native file processing engine — embedded MIT source

Original project: `D:\NSW.dev\NSRP\rust_port/src`, https://github.com/sinjunyoung/RomForge.
License: [MIT](LICENSE.txt), copyright (c) 2026 sinjunyoung.

This is a vendored **build-time source dependency** included so an NSL binary
does not need an installed NSRP program. File processing modules under
`src/ops/` and `src/formats/` and `src/{crypto,keys,process}.rs` were copied
from NSRP's local rust_port reference. `src/lib.rs`, `src/workspace.rs` and
`Cargo.toml` were adapted to omit the NSRP Slint GUI and all external helper
tool/resource extraction. The original source remains untouched.

Some methods in this engine still contain optional external hactool/hacbuild
fallback. NSL calls should not silently require external tools: paths are
provided explicitly and unsupported work must fail closed while preserving
all original files. Output must be staged, verified and published separately.
