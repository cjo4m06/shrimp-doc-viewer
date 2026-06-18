#!/usr/bin/env python3
"""Build a minimal but valid styled .xlsx for testing the renderer.

No dependencies. Contains 繁體中文, numbers, a merged title, varied column
widths, and a styles.xml exercising fills, bold/coloured fonts, borders,
alignment, and number formats (thousands + currency).

Usage: python3 make_xlsx.py out.xlsx
"""
import sys
import zipfile

TITLE = "2024 年水果銷售統計表"
# (value) per cell, row-major. None = empty.
ROWS = [
    [TITLE, None, None, None],
    ["產品", "數量", "單價", "小計"],
    ["蘋果", 10, 35, 350],
    ["香蕉", 20, 18, 360],
    ["橘子", 15, 25, 375],
    ["葡萄", 88, 60, 5280],
    ["合計", 133, None, 6365],
]
# Parallel style indices into cellXfs (None = no explicit style).
STYLE = [
    [1, None, None, None],   # title (merged)
    [2, 2, 2, 2],            # header
    [5, 3, 3, 4],            # data
    [5, 3, 3, 4],
    [5, 3, 3, 4],
    [5, 3, 3, 4],
    [6, 6, None, 7],         # totals (bold)
]
MERGES = ["A1:D1"]
COLS = [(1, 1, 18), (2, 2, 9), (3, 3, 9), (4, 4, 12)]

CT = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
<Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>
<Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>
</Types>"""

RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"""

WORKBOOK = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">
<sheets><sheet name="銷售表" sheetId="1" r:id="rId1"/></sheets>
</workbook>"""

WB_RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/>
<Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>
</Relationships>"""

STYLES = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<numFmts count="2">
<numFmt numFmtId="164" formatCode="#,##0"/>
<numFmt numFmtId="165" formatCode="$#,##0"/>
</numFmts>
<fonts count="3">
<font><sz val="11"/><name val="Calibri"/></font>
<font><b/><sz val="13"/><color rgb="FFFFFFFF"/></font>
<font><b/><sz val="11"/></font>
</fonts>
<fills count="4">
<fill><patternFill patternType="none"/></fill>
<fill><patternFill patternType="gray125"/></fill>
<fill><patternFill patternType="solid"><fgColor rgb="FF4F81BD"/></patternFill></fill>
<fill><patternFill patternType="solid"><fgColor rgb="FFD9E1F2"/></patternFill></fill>
</fills>
<borders count="3">
<border><left/><right/><top/><bottom/><diagonal/></border>
<border><left style="thin"/><right style="thin"/><top style="thin"/><bottom style="thin"/><diagonal/></border>
<border><left/><right/><top/><bottom style="thin"/><diagonal/></border>
</borders>
<cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs>
<cellXfs count="8">
<xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/>
<xf numFmtId="0" fontId="1" fillId="2" borderId="0" xfId="0" applyFont="1" applyFill="1" applyAlignment="1"><alignment horizontal="center" vertical="center"/></xf>
<xf numFmtId="0" fontId="2" fillId="3" borderId="2" xfId="0" applyFont="1" applyFill="1" applyBorder="1" applyAlignment="1"><alignment horizontal="center"/></xf>
<xf numFmtId="164" fontId="0" fillId="0" borderId="1" xfId="0" applyNumberFormat="1" applyBorder="1"/>
<xf numFmtId="165" fontId="0" fillId="0" borderId="1" xfId="0" applyNumberFormat="1" applyBorder="1"/>
<xf numFmtId="0" fontId="0" fillId="0" borderId="1" xfId="0" applyBorder="1" applyAlignment="1"><alignment horizontal="left"/></xf>
<xf numFmtId="164" fontId="2" fillId="3" borderId="1" xfId="0" applyNumberFormat="1" applyFont="1" applyFill="1" applyBorder="1"/>
<xf numFmtId="165" fontId="2" fillId="3" borderId="1" xfId="0" applyNumberFormat="1" applyFont="1" applyFill="1" applyBorder="1"/>
</cellXfs>
<cellStyles count="1"><cellStyle name="Normal" xfId="0" builtinId="0"/></cellStyles>
</styleSheet>"""


def col_letter(c):
    s = ""
    c += 1
    while c:
        c, r = divmod(c - 1, 26)
        s = chr(65 + r) + s
    return s


def main(path):
    strings = []
    index = {}

    def sref(t):
        if t not in index:
            index[t] = len(strings)
            strings.append(t)
        return index[t]

    rows_xml = []
    for ri, row in enumerate(ROWS, start=1):
        cells = []
        for ci, val in enumerate(row):
            s = STYLE[ri - 1][ci]
            if val is None or val == "":
                if s is not None:
                    cells.append(f'<c r="{col_letter(ci)}{ri}" s="{s}"/>')
                continue
            ref = f"{col_letter(ci)}{ri}"
            sattr = f' s="{s}"' if s is not None else ""
            if isinstance(val, (int, float)):
                cells.append(f'<c r="{ref}"{sattr}><v>{val}</v></c>')
            else:
                cells.append(f'<c r="{ref}"{sattr} t="s"><v>{sref(val)}</v></c>')
        rows_xml.append(f'<row r="{ri}">{"".join(cells)}</row>')

    sst_items = "".join(f"<si><t>{s}</t></si>" for s in strings)
    sst = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
        f'count="{len(strings)}" uniqueCount="{len(strings)}">{sst_items}</sst>'
    )

    cols_xml = "".join(f'<col min="{lo}" max="{hi}" width="{w}" customWidth="1"/>' for (lo, hi, w) in COLS)
    merges_xml = "".join(f'<mergeCell ref="{m}"/>' for m in MERGES)
    sheet = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
        f'<dimension ref="A1:{col_letter(len(ROWS[0]) - 1)}{len(ROWS)}"/>'
        f'<cols>{cols_xml}</cols>'
        f'<sheetData>{"".join(rows_xml)}</sheetData>'
        f'<mergeCells count="{len(MERGES)}">{merges_xml}</mergeCells>'
        '</worksheet>'
    )

    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("[Content_Types].xml", CT)
        z.writestr("_rels/.rels", RELS)
        z.writestr("xl/workbook.xml", WORKBOOK)
        z.writestr("xl/_rels/workbook.xml.rels", WB_RELS)
        z.writestr("xl/styles.xml", STYLES)
        z.writestr("xl/sharedStrings.xml", sst)
        z.writestr("xl/worksheets/sheet1.xml", sheet)
    print("wrote", path)


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "sample.xlsx")
