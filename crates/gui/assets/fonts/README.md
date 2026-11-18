# Fonts of the window

The window bundles two font families with `include_bytes!`
(`crates/gui/src/theme/fonts.rs`, ADR 0021, `docs/design/tokens.md`). Both are
under the SIL Open Font License 1.1. Each licence text sits next to its font,
copied unchanged from the source. The files themselves are unchanged too.

| File | Font | Version | Licence |
| --- | --- | --- | --- |
| `ReadexPro[HEXP,wght].ttf` | Readex Pro, the variable font (axes `wght` 160 to 700, default 400; `HEXP` 0 to 100, default 0) | 1.205 | `ReadexPro-OFL.txt`, OFL 1.1 |
| `JetBrainsMono-Regular.ttf` | JetBrains Mono Regular | 2.304 | `JetBrainsMono-OFL.txt`, OFL 1.1 |

The window takes the weights 300, 400, 500, and 600 of Readex Pro from the
one variable file with `VariationCoords` on the `wght` axis (spike S-14).
400 is the file's default, so the regular face sets no coordinates.

## Sources

Readex Pro, from the Google Fonts repository at commit
`8b0a1d0f5983c89bc2b93f1b5fb55f9e252744b5`, folder `ofl/readexpro`
(upstream: https://github.com/ThomasJockin/readexpro, commit
`563dfbb36ae45e52ec50829b016ce724ac2fca70`, named in that folder's
`METADATA.pb`):

- https://raw.githubusercontent.com/google/fonts/8b0a1d0f5983c89bc2b93f1b5fb55f9e252744b5/ofl/readexpro/ReadexPro%5BHEXP%2Cwght%5D.ttf
- https://raw.githubusercontent.com/google/fonts/8b0a1d0f5983c89bc2b93f1b5fb55f9e252744b5/ofl/readexpro/OFL.txt (saved as `ReadexPro-OFL.txt`)

JetBrains Mono, from the release `v2.304` of
https://github.com/JetBrains/JetBrainsMono, the archive
https://github.com/JetBrains/JetBrainsMono/releases/download/v2.304/JetBrainsMono-2.304.zip
(SHA-256 `6f6376c6ed2960ea8a963cd7387ec9d76e3f629125bc33d1fdcd7eb7012f7bbf`):

- `fonts/ttf/JetBrainsMono-Regular.ttf`
- `OFL.txt` (saved as `JetBrainsMono-OFL.txt`)

## SHA-256

```
268bba7e1e8f3b14d798b3fb0e40ebaa3fc39308c9ac0020e2faf6df181cc30e  ReadexPro[HEXP,wght].ttf
b41267a3903a45955f77b44596e080480c577fb7221df59672d0bc8cd8cd4b6f  ReadexPro-OFL.txt
a0bf60ef0f83c5ed4d7a75d45838548b1f6873372dfac88f71804491898d138f  JetBrainsMono-Regular.ttf
30f0c136e3c88e422d0791acd97238870f9054a9729bc34cf2ff0d4ed8cac4ad  JetBrainsMono-OFL.txt
```

## Licence notes

The OFL allows bundling the fonts with software, sold or not, as long as the
copyright notice and the licence go with them and the fonts are not sold on
their own. Readex Pro reserves the font name "RevReading Lexend": a modified
version must not use it. These files are not modified.

The release archives must carry both licence texts in their licence notes
(the release files belong to the release session).
