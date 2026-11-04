# MJPEG test font

`misans-subset.ttf` is a small test-only subset of **Noto Sans CJK SC**
(Regular, font number 2 from `NotoSansCJK-Regular.ttc`). It contains ASCII
`0x20`–`0x7e` plus the Chinese labels used by the MJPEG layout and tests.

It was produced with `pyftsubset` from the Noto CJK package and is used only to
exercise the TTF rendering path without downloading MiSans. Noto Sans CJK is
licensed under the SIL Open Font License 1.1; see `OFL.txt`.
