#!/usr/bin/env python3
"""Build a minimal but valid styled, multi-sheet .xlsx for testing the renderer.

No dependencies. Sheet 1 ("銷售表") is a small styled table (merged title, fills,
bold, borders, number formats). Sheet 2 ("明細") has ~120 rows to exercise sheet
tabs and viewport virtualization.

Usage: python3 make_xlsx.py out.xlsx
"""
import sys
import zipfile

# ---- Sheet 1: styled summary -------------------------------------------------
TITLE = "2024 年水果銷售統計表"
S1_ROWS = [
    [TITLE, None, None, None],
    ["產品", "數量", "單價", "小計"],
    ["蘋果", 10, 35, 350],
    ["香蕉", 20, 18, 360],
    ["橘子", 15, 25, 375],
    ["葡萄", 88, 60, 5280],
    ["合計", 133, None, 6365],
]
S1_STYLE = [
    [1, None, None, None],
    [2, 2, 2, 2],
    [5, 3, 3, 4],
    [5, 3, 3, 4],
    [5, 3, 3, 4],
    [5, 3, 3, 4],
    [6, 6, None, 7],
]
S1_MERGES = ["A1:D1"]
S1_COLS = [(1, 1, 18), (2, 2, 9), (3, 3, 9), (4, 4, 12)]

# ---- Sheet 2: long detail (tabs + virtualization) ---------------------------
FRUITS = ["蘋果", "香蕉", "橘子", "葡萄", "西瓜", "鳳梨", "芒果", "草莓"]
S2_ROWS = [["編號", "品項", "數量", "單價", "小計"]]
S2_STYLE = [[2, 2, 2, 2, 2]]
for i in range(1, 121):
    fruit = FRUITS[i % len(FRUITS)]
    qty = (i * 3) % 90 + 1
    price = (i * 7) % 80 + 10
    S2_ROWS.append([i, f"{fruit} 批次 {i}", qty, price, qty * price])
    S2_STYLE.append([3, 5, 3, 4, 4])
S2_COLS = [(1, 1, 7), (2, 2, 20), (3, 3, 9), (4, 4, 9), (5, 5, 12)]

SHEETS = [
    {"name": "銷售表", "rows": S1_ROWS, "style": S1_STYLE, "merges": S1_MERGES, "cols": S1_COLS},
    {"name": "明細", "rows": S2_ROWS, "style": S2_STYLE, "merges": [], "cols": S2_COLS},
]

STYLES = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">
<numFmts count="2"><numFmt numFmtId="164" formatCode="#,##0"/><numFmt numFmtId="165" formatCode="$#,##0"/></numFmts>
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


def ct(n_sheets):
    overrides = "".join(
        f'<Override PartName="/xl/worksheets/sheet{i + 1}.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>'
        for i in range(n_sheets)
    )
    return (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">'
        '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>'
        '<Default Extension="xml" ContentType="application/xml"/>'
        '<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>'
        f'{overrides}'
        '<Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/>'
        '<Override PartName="/xl/sharedStrings.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sharedStrings+xml"/>'
        '</Types>'
    )


RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/>
</Relationships>"""


def workbook_xml(sheets):
    items = "".join(f'<sheet name="{s["name"]}" sheetId="{i + 1}" r:id="rId{i + 1}"/>' for i, s in enumerate(sheets))
    return (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
        'xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships">'
        f'<sheets>{items}</sheets></workbook>'
    )


def wb_rels(n_sheets):
    sheet_rels = "".join(
        f'<Relationship Id="rId{i + 1}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet{i + 1}.xml"/>'
        for i in range(n_sheets)
    )
    sid = n_sheets + 1
    return (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">'
        f'{sheet_rels}'
        f'<Relationship Id="rId{sid}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/sharedStrings" Target="sharedStrings.xml"/>'
        f'<Relationship Id="rId{sid + 1}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/>'
        '</Relationships>'
    )


def col_letter(c):
    s = ""
    c += 1
    while c:
        c, r = divmod(c - 1, 26)
        s = chr(65 + r) + s
    return s


def sheet_xml(sheet, sref):
    rows, style, merges, cols = sheet["rows"], sheet["style"], sheet["merges"], sheet["cols"]
    rows_xml = []
    for ri, row in enumerate(rows, start=1):
        cells = []
        for ci, val in enumerate(row):
            s = style[ri - 1][ci]
            ref = f"{col_letter(ci)}{ri}"
            if val is None or val == "":
                if s is not None:
                    cells.append(f'<c r="{ref}" s="{s}"/>')
                continue
            sattr = f' s="{s}"' if s is not None else ""
            if isinstance(val, (int, float)):
                cells.append(f'<c r="{ref}"{sattr}><v>{val}</v></c>')
            else:
                cells.append(f'<c r="{ref}"{sattr} t="s"><v>{sref(val)}</v></c>')
        rows_xml.append(f'<row r="{ri}">{"".join(cells)}</row>')
    cols_xml = "".join(f'<col min="{lo}" max="{hi}" width="{w}" customWidth="1"/>' for (lo, hi, w) in cols)
    merges_xml = (
        f'<mergeCells count="{len(merges)}">' + "".join(f'<mergeCell ref="{m}"/>' for m in merges) + "</mergeCells>"
        if merges
        else ""
    )
    last = f"{col_letter(len(rows[0]) - 1)}{len(rows)}"
    return (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        '<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main">'
        f'<dimension ref="A1:{last}"/>'
        f'<cols>{cols_xml}</cols>'
        f'<sheetData>{"".join(rows_xml)}</sheetData>'
        f'{merges_xml}</worksheet>'
    )


def main(path):
    strings, index = [], {}

    def sref(t):
        if t not in index:
            index[t] = len(strings)
            strings.append(t)
        return index[t]

    sheet_parts = [sheet_xml(s, sref) for s in SHEETS]
    sst_items = "".join(f"<si><t>{s}</t></si>" for s in strings)
    sst = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
        f'count="{len(strings)}" uniqueCount="{len(strings)}">{sst_items}</sst>'
    )

    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("[Content_Types].xml", ct(len(SHEETS)))
        z.writestr("_rels/.rels", RELS)
        z.writestr("xl/workbook.xml", workbook_xml(SHEETS))
        z.writestr("xl/_rels/workbook.xml.rels", wb_rels(len(SHEETS)))
        z.writestr("xl/styles.xml", STYLES)
        z.writestr("xl/sharedStrings.xml", sst)
        for i, part in enumerate(sheet_parts):
            z.writestr(f"xl/worksheets/sheet{i + 1}.xml", part)
    print("wrote", path, "with", len(SHEETS), "sheets")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "sample.xlsx")
