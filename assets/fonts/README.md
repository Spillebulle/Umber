# Archivo

- **Font**: Archivo (variable font)
- **Licence**: SIL Open Font License, Version 1.1 (OFL-1.1) — see `OFL.txt` in this directory.
- **Source**: Google Fonts GitHub repository,
  https://github.com/google/fonts/tree/main/ofl/archivo
  - Font file: https://raw.githubusercontent.com/google/fonts/main/ofl/archivo/Archivo%5Bwdth%2Cwght%5D.ttf
  - Licence file: https://raw.githubusercontent.com/google/fonts/main/ofl/archivo/OFL.txt
- **Downloaded**: 2026-07-31

## Files

- `Archivo[wdth,wght].ttf` — variable TTF with `wght` axis (100–900) and `wdth` axis (62–125).
  No separate static Regular/SemiBold files exist upstream for Archivo; this single variable
  file covers both required weights via named instances:
  - **Regular** (`wght=400`, `wdth=100`)
  - **SemiBold** (`wght=600`, `wdth=100`)
- `OFL.txt` — SIL Open Font License 1.1 text, required to accompany the font per its licence terms.

## Usage note

When embedding with `include_bytes!`, load this single variable font and select the
Regular/SemiBold instances at runtime via the `wght` variation axis (e.g. through a text
shaping/rendering stack that supports OpenType variable font instancing), rather than
expecting two separate static files.
