# Desktop build size

The macOS Nightly inspected on November 21, 2026 was
`Galaxy_Nightly_11212026_105949_CT_r12-1_aarch64.dmg` (source `600d7970c`).
Its SHA-256 was
`0ec2c56617213b99cf9e769a5c1de5bb38a3347fd94e588ac7c3328e7b86636d`.

| Component | Bytes | Decimal MB |
| --- | ---: | ---: |
| Compressed DMG | 265,486,101 | 265.5 |
| App file contents | 492,286,932 | 492.3 |
| Main executable | 374,868,768 | 374.9 |
| Linux Wormhole helper archives, both architectures | 110,515,788 | 110.5 |
| Other app resources and plug-in | 6,902,376 | 6.9 |

App size above sums regular file contents after extraction; it is not native
APFS allocation. Finder, disk allocation, and compressed-download sizes can differ.

## Changes to packaging

- Release app copies are stripped of debug and local symbol tables before
  signing. Exported symbols and executable UUIDs are retained. When generated,
  Cargo's separate debug artifacts remain outside the app; the original Cargo
  executable is also untouched. Unsymbolicated traces lose local function names,
  so offline symbolication needs that matching original or dSYM. A disposable copy of the published
  executable shrank from 374,868,768 to 315,595,296 bytes with LLVM's Mach-O
  `strip -S -x`: a measured saving of **59,273,472 bytes**. Apple's strip runs in
  macOS packaging; the final signed bundle and compressed DMG need a native build
  for exact measurements. See [Apple's strip documentation](https://github.com/apple-oss-distributions/cctools/blob/main/man/strip.1)
  for the retained global symbols and removed local/debug symbols.
- Desktop packaging no longer enables the obsolete ONNX input classifier and
  its heuristic feature. Galaxy's active input detector uses deterministic shell
  inventory and history lookups. This avoids enabling the unused Candle/ONNX and
  tokenizer dependencies: the macOS normal/build dependency graph drops from
  1,208 to 1,170 packages. Research classifier features remain available explicitly.
- Seventeen unused app dependency declarations were removed: fifteen direct
  dependencies and two build dependencies (`prost-build` and `tempfile`). Dependencies
  still required by another crate remain in the graph; declaration removal alone
  does not imply the entire dependency disappears or a measurable binary saving.
- Six orphaned promotion images were removed from the embedded async asset set:
  `Trial-Image.png`, the four `agents_3_*.png` images, and
  `code_launch_swe_bench.png`. Their file names/stems have no references in tracked
  source or textual data; the asset macros use literal paths. This removes another
  **3,611,357 bytes** of source image payloads, without pruning referenced theme
  or onboarding images. The unused previous DMG background was also removed.
- macOS CI writes `macos-bundle-size.json` next to its build outputs and adds
  total size and the largest files to the build summary, before signing.
- DMGs use a Galaxy background, fixed app/Applications positions, and level-9
  UDZO compression. Installer artwork and Python packaging tools are not bundled
  inside the app. The app is signed, notarized, and stapled before image creation;
  the completed DMG then receives its own signature, notarization, and staple.

Stripping alone would put the inspected app near **433 MB**; excluding the orphaned
image payloads puts the estimate near **429 MB**, before differences from
recompilation and signing. Do not treat those estimates as newly published artifact
measurements or predict the compressed DMG from them.

## Remaining opportunities

Both committed Wormhole helpers are already stripped static Linux executables.
Their archives remain included for installing remote helpers without rebuilding.
Any further reduction requires a separately validated helper rebuild or a change
to offline availability, rather than deleting the archives during desktop packaging.

The inspected baseline executable contains at least 67.9 MB of known image/font assets,
including 60.5 MB of asynchronous imagery. Some are inherited onboarding and
promotion graphics; this pass removes the six orphaned images listed above.
A future cleanup should establish which screens and dynamic
asset paths are still reachable before pruning these bytes. Larger build-profile
changes such as ThinLTO also need native measurements of size, memory, and compile
time.
