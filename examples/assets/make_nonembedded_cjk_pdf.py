#!/usr/bin/env python3
"""Emit a PDF that uses a NON-embedded CJK CID font.

This is the classic "blank 繁中" case: a Type0/CIDFontType2 font with
BaseFont /MingLiU, the predefined CMap /UniCNS-UCS2-H, CIDSystemInfo Adobe-CNS1,
and NO FontFile2 (the font is referenced by name, not embedded). A viewer with
no MingLiU and no CJK fallback renders these glyphs blank; a viewer that installs
a CJK fallback via FPDF_SetSystemFontInfo renders them correctly.

Usage: python3 make_nonembedded_cjk_pdf.py out.pdf
"""
import sys

# UniCNS-UCS2-H codes == UTF-16BE of the characters.
#   繁7E41 體9AD4 中4E2D 文6587   字5B57 型578B 測6E2C 試8A66
LINE1 = b"<7E419AD44E2D6587>"  # 繁體中文
LINE2 = b"<5B57578B6E2C8A66>"  # 字型測試


def main(path):
    objects = {}
    objects[1] = b"<< /Type /Catalog /Pages 2 0 R >>"
    objects[2] = b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"
    objects[3] = (
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] "
        b"/Resources << /Font << /F1 5 0 R /F2 8 0 R >> >> /Contents 4 0 R >>"
    )

    content = (
        b"BT\n"
        b"/F2 14 Tf 1 0 0 1 72 790 Tm (Non-embedded CID font: MingLiU / Adobe-CNS1 / UniCNS-UCS2-H) Tj\n"
        b"/F2 12 Tf 1 0 0 1 72 766 Tm (Latin via base-14 Helvetica should always render.) Tj\n"
        b"/F1 32 Tf 1 0 0 1 72 700 Tm " + LINE1 + b" Tj\n"
        b"/F1 32 Tf 1 0 0 1 72 655 Tm " + LINE2 + b" Tj\n"
        b"ET\n"
    )
    objects[4] = b"<< /Length %d >>\nstream\n" % len(content) + content + b"endstream"
    objects[5] = (
        b"<< /Type /Font /Subtype /Type0 /BaseFont /MingLiU "
        b"/Encoding /UniCNS-UCS2-H /DescendantFonts [6 0 R] >>"
    )
    objects[6] = (
        b"<< /Type /Font /Subtype /CIDFontType2 /BaseFont /MingLiU "
        b"/CIDSystemInfo << /Registry (Adobe) /Ordering (CNS1) /Supplement 0 >> "
        b"/FontDescriptor 7 0 R /DW 1000 >>"
    )
    objects[7] = (
        b"<< /Type /FontDescriptor /FontName /MingLiU /Flags 4 "
        b"/FontBBox [0 -120 1000 880] /ItalicAngle 0 /Ascent 880 /Descent -120 "
        b"/CapHeight 880 /StemV 80 >>"
    )
    objects[8] = b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"

    out = bytearray(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
    offsets = {}
    n = len(objects)
    for i in range(1, n + 1):
        offsets[i] = len(out)
        out += b"%d 0 obj\n" % i + objects[i] + b"\nendobj\n"

    xref_pos = len(out)
    out += b"xref\n0 %d\n" % (n + 1)
    out += b"0000000000 65535 f \n"
    for i in range(1, n + 1):
        out += b"%010d 00000 n \n" % offsets[i]
    out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n" % (n + 1, xref_pos)

    with open(path, "wb") as f:
        f.write(out)
    print("wrote", path, len(out), "bytes")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "sample-zhTW-nonembedded.pdf")
