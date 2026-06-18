#!/usr/bin/env python3
"""Build a minimal but valid .xlsx (zip of OOXML parts) for testing the renderer.

No dependencies — assembles the parts by hand. Contains 繁體中文 strings and
numbers so the grid renderer + CJK text stack can be verified.

Usage: python3 make_xlsx.py out.xlsx
"""
import sys
import zipfile

# Row 1 is a title merged across A1:D1; the table follows from row 2.
TITLE = "2024 年水果銷售統計表"
ROWS = [
    [TITLE, None, None, None],  # row 1 (merged)
    ["產品", "數量", "單價", "小計"],
    ["蘋果", 10, 35, 350],
    ["香蕉", 20, 18, 360],
    ["橘子", 15, 25, 375],
    ["葡萄", 8, 60, 480],
    ["合計", 53, None, 1565],
]
MERGES = ["A1:D1"]
# (min_col, max_col, width-in-chars)
COLS = [(1, 1, 18), (2, 2, 9), (3, 3, 9), (4, 4, 11)]

CT = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/>
<Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/>
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
</Relationships>"""


def col_letter(c):  # 0-based -> A, B, ...
    s = ""
    c += 1
    while c:
        c, r = divmod(c - 1, 26)
        s = chr(65 + r) + s
    return s


def main(path):
    # Shared strings table.
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
            if val is None or val == "":
                continue
            ref = f"{col_letter(ci)}{ri}"
            if isinstance(val, (int, float)):
                cells.append(f'<c r="{ref}"><v>{val}</v></c>')
            else:
                cells.append(f'<c r="{ref}" t="s"><v>{sref(val)}</v></c>')
        rows_xml.append(f'<row r="{ri}">{"".join(cells)}</row>')

    sst_items = "".join(f"<si><t>{s}</t></si>" for s in strings)
    sst = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<sst xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" '
        f'count="{len(strings)}" uniqueCount="{len(strings)}">{sst_items}</sst>'
    )

    cols_xml = "".join(
        f'<col min="{lo}" max="{hi}" width="{w}" customWidth="1"/>' for (lo, hi, w) in COLS
    )
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
        z.writestr("xl/sharedStrings.xml", sst)
        z.writestr("xl/worksheets/sheet1.xml", sheet)
    print("wrote", path)


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "sample.xlsx")
