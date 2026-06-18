#!/usr/bin/env python3
"""Build a minimal valid .docx for testing the flow-layout renderer.

No dependencies. Direct run/paragraph formatting (bold, size, colour, alignment)
so no styles.xml is needed. Mixes 繁體中文 and English to exercise wrapping.

Usage: python3 make_docx.py out.docx
"""
import sys
import zipfile
from xml.sax.saxutils import escape

W = "http://schemas.openxmlformats.org/wordprocessingml/2006/main"

# Each paragraph: (align, [ (text, bold, size_halfpoints, color_hex_or_None) ]).
PARAS = [
    ("center", [("doc-viewer 文件檢視器", True, 48, "1F6FEB")]),
    # pStyle inheritance test: runs carry NO direct rPr, so bold/colour/size come
    # entirely from the styles.xml Heading1/Heading2 definitions (H2 basedOn H1).
    ("left", [("樣式繼承 Heading1(粗體+藍+大字,皆繼承自樣式)", False, None, None)], "Heading1"),
    ("left", [("樣式繼承 Heading2(basedOn H1:繼承粗體+藍,字級改小)", False, None, None)], "Heading2"),
    ("left", [("一、繁體中文流式排版測試", True, 32, None)]),
    ("both", [
        ("這是一段較長的內文,用來測試自動換行(line wrapping)。", False, 24, None),
        ("doc-viewer", True, 24, "C0504D"),
        (" 以 Rust 自寫排版引擎,將段落與文字 run 降階成共用顯示列表,再由 tiny-skia 光柵化。"
         "中文可在任意字元間斷行,English words break at spaces。", False, 24, None),
    ]),
    ("left", [("二、字元格式", True, 32, None)]),
    ("left", [
        ("支援 ", False, 24, None),
        ("粗體", True, 24, None),
        ("、字級大小、", False, 24, None),
        ("顏色", False, 24, "1F6FEB"),
        (" 與段落對齊(左/中/右/兩端)。", False, 24, None),
    ]),
    ("right", [("— 完 —", False, 24, "888888")]),
]

# Extra body paragraphs so the document spans several pages (pagination +
# virtualization test).
for _i in range(1, 30):
    PARAS.append(("both", [(
        f"第 {_i} 段:這是用來測試分頁與頁面虛擬化的內文。doc-viewer 將流式內容切成固定大小的頁面,"
        f"只渲染視窗附近的頁,捲離很遠的頁會釋放以維持恆定記憶體。Paragraph {_i}: pagination and "
        f"viewport virtualization with mixed 中文 and English so lines wrap across the page width.",
        False, 24, None,
    )]))

CT = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/>
<Override PartName="/word/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml"/>
</Types>"""

RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/>
</Relationships>"""


def run_xml(text, bold, size_hp, color):
    rpr = "<w:rPr>"
    if bold:
        rpr += "<w:b/>"
    if size_hp:
        rpr += f'<w:sz w:val="{size_hp}"/>'
    if color:
        rpr += f'<w:color w:val="{color}"/>'
    rpr += "</w:rPr>"
    return f'<w:r>{rpr}<w:t xml:space="preserve">{escape(text)}</w:t></w:r>'


STYLES = f"""<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:styles xmlns:w="{W}">
<w:docDefaults><w:rPrDefault><w:rPr><w:sz w:val="24"/></w:rPr></w:rPrDefault></w:docDefaults>
<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/><w:rPr><w:b/><w:color w:val="1F6FEB"/><w:sz w:val="40"/></w:rPr></w:style>
<w:style w:type="paragraph" w:styleId="Heading2"><w:name w:val="heading 2"/><w:basedOn w:val="Heading1"/><w:rPr><w:sz w:val="28"/></w:rPr></w:style>
</w:styles>"""


def para_xml(align, runs, pstyle=None):
    ppr = "<w:pPr>"
    if pstyle:
        ppr += f'<w:pStyle w:val="{pstyle}"/>'
    ppr += f'<w:jc w:val="{align}"/></w:pPr>'
    body = "".join(run_xml(*r) for r in runs)
    return f"<w:p>{ppr}{body}</w:p>"


def main(path):
    body = "".join(para_xml(*p) for p in PARAS)
    sect = (
        '<w:sectPr><w:pgSz w:w="11906" w:h="16838"/>'
        '<w:pgMar w:top="1440" w:right="1440" w:bottom="1440" w:left="1440"/></w:sectPr>'
    )
    document = (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<w:document xmlns:w="{W}"><w:body>{body}{sect}</w:body></w:document>'
    )
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("[Content_Types].xml", CT)
        z.writestr("_rels/.rels", RELS)
        z.writestr("word/document.xml", document)
        z.writestr("word/styles.xml", STYLES)
    print("wrote", path)


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "sample.docx")
