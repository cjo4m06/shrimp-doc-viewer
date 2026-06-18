#!/usr/bin/env python3
"""Build a minimal valid .pptx for testing the slide renderer.

No dependencies. Two slides with positioned text boxes (DrawingML a:xfrm
off/ext in EMU), run formatting (size/bold/colour) and paragraph alignment, plus
a coloured rectangle shape. Mixes 繁體中文 and English.

Usage: python3 make_pptx.py out.pptx
"""
import sys
import zipfile
from xml.sax.saxutils import escape

A = "http://schemas.openxmlformats.org/drawingml/2006/main"
P = "http://schemas.openxmlformats.org/presentationml/2006/main"
R = "http://schemas.openxmlformats.org/officeDocument/2006/relationships"
EMU = 914400  # per inch

# Slide size 10in x 7.5in (4:3).
SLIDE_CX = 10 * EMU
SLIDE_CY = int(7.5 * EMU)


def textbox(idx, x_in, y_in, w_in, h_in, paras, fill=None):
    """paras: list of (align, [(text, size_pt, bold, color_hex|None)])."""
    off = f'<a:off x="{int(x_in * EMU)}" y="{int(y_in * EMU)}"/>'
    ext = f'<a:ext cx="{int(w_in * EMU)}" cy="{int(h_in * EMU)}"/>'
    fill_xml = f'<a:solidFill><a:srgbClr val="{fill}"/></a:solidFill>' if fill else "<a:noFill/>"
    ps = []
    for align, runs in paras:
        algn = f' algn="{align}"' if align else ""
        rs = []
        for (text, size_pt, bold, color) in runs:
            rpr = f'<a:rPr lang="zh-Hant" sz="{int(size_pt * 100)}" b="{1 if bold else 0}">'
            rpr += f'<a:solidFill><a:srgbClr val="{color}"/></a:solidFill>' if color else ""
            rpr += "</a:rPr>"
            rs.append(f"<a:r>{rpr}<a:t>{escape(text)}</a:t></a:r>")
        ps.append(f"<a:p><a:pPr{algn}/>{''.join(rs)}</a:p>")
    return (
        f'<p:sp><p:nvSpPr><p:cNvPr id="{idx}" name="tb{idx}"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>'
        f'<p:spPr><a:xfrm>{off}{ext}</a:xfrm>'
        f'<a:prstGeom prst="rect"><a:avLst/></a:prstGeom>{fill_xml}</p:spPr>'
        f'<p:txBody><a:bodyPr/><a:lstStyle/>{"".join(ps)}</p:txBody></p:sp>'
    )


def slide(shapes):
    return (
        '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
        f'<p:sld xmlns:a="{A}" xmlns:r="{R}" xmlns:p="{P}">'
        f'<p:cSld><p:spTree>'
        '<p:nvGrpSpPr><p:cNvPr id="1" name=""/><p:cNvGrpSpPr/><p:nvPr/></p:nvGrpSpPr>'
        '<p:grpSpPr/>'
        f'{"".join(shapes)}'
        '</p:spTree></p:cSld></p:sld>'
    )


def pic(idx, rid, x_in, y_in, w_in, h_in):
    off = f'<a:off x="{int(x_in * EMU)}" y="{int(y_in * EMU)}"/>'
    ext = f'<a:ext cx="{int(w_in * EMU)}" cy="{int(h_in * EMU)}"/>'
    return (
        f'<p:pic><p:nvPicPr><p:cNvPr id="{idx}" name="pic{idx}"/><p:cNvPicPr/><p:nvPr/></p:nvPicPr>'
        f'<p:blipFill><a:blip r:embed="{rid}"/><a:stretch><a:fillRect/></a:stretch></p:blipFill>'
        f'<p:spPr><a:xfrm>{off}{ext}</a:xfrm><a:prstGeom prst="rect"><a:avLst/></a:prstGeom></p:spPr></p:pic>'
    )


SLIDE1 = slide([
    textbox(2, 1.0, 0.5, 8.0, 1.2, [("center", [("doc-viewer 簡報檢視器", 40, True, "FFFFFF")])], fill="1F6FEB"),
    textbox(3, 1.0, 2.2, 8.0, 0.8, [("left", [("第一張投影片 — 繁體中文 PPTX 測試", 24, True, "1F2937")])]),
    textbox(4, 1.0, 3.2, 4.2, 2.5, [
        ("left", [("• 自寫 Rust 解析 DrawingML", 18, False, "374151")]),
        ("left", [("• 定位文字框、run 格式", 18, False, "374151")]),
        ("left", [("• 內嵌 PNG/JPEG 圖片 →", 18, False, "C0504D")]),
    ]),
    pic(9, "rId1", 5.5, 3.3, 3.2, 2.0),  # embedded raster image (160x100, aspect 1.6)
])

def shape(idx, prst, x, y, w, h, fill=None, line=None, text=None, tcolor="FFFFFF"):
    off = f'<a:off x="{int(x * EMU)}" y="{int(y * EMU)}"/>'
    ext = f'<a:ext cx="{int(w * EMU)}" cy="{int(h * EMU)}"/>'
    spfill = f'<a:solidFill><a:srgbClr val="{fill}"/></a:solidFill>' if fill else "<a:noFill/>"
    ln = f'<a:ln w="25400"><a:solidFill><a:srgbClr val="{line}"/></a:solidFill></a:ln>' if line else ""
    if text:
        tb = (
            f'<p:txBody><a:bodyPr/><a:lstStyle/><a:p><a:pPr algn="ctr"/>'
            f'<a:r><a:rPr sz="1800" b="1"><a:solidFill><a:srgbClr val="{tcolor}"/></a:solidFill></a:rPr>'
            f'<a:t>{escape(text)}</a:t></a:r></a:p></p:txBody>'
        )
    else:
        tb = "<p:txBody><a:bodyPr/><a:lstStyle/><a:p/></p:txBody>"
    return (
        f'<p:sp><p:nvSpPr><p:cNvPr id="{idx}" name="sp{idx}"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>'
        f'<p:spPr><a:xfrm>{off}{ext}</a:xfrm><a:prstGeom prst="{prst}"><a:avLst/></a:prstGeom>{spfill}{ln}</p:spPr>{tb}</p:sp>'
    )


def custgeom(idx, x, y, w, h, pts, fill=None, line=None):
    off = f'<a:off x="{int(x * EMU)}" y="{int(y * EMU)}"/>'
    ext = f'<a:ext cx="{int(w * EMU)}" cy="{int(h * EMU)}"/>'
    spfill = f'<a:solidFill><a:srgbClr val="{fill}"/></a:solidFill>' if fill else "<a:noFill/>"
    ln = f'<a:ln w="25400"><a:solidFill><a:srgbClr val="{line}"/></a:solidFill></a:ln>' if line else ""
    cmds = f'<a:moveTo><a:pt x="{pts[0][0]}" y="{pts[0][1]}"/></a:moveTo>'
    for px, py in pts[1:]:
        cmds += f'<a:lnTo><a:pt x="{px}" y="{py}"/></a:lnTo>'
    cmds += "<a:close/>"
    geom = f'<a:custGeom><a:avLst/><a:gdLst/><a:pathLst><a:path w="100000" h="100000">{cmds}</a:path></a:pathLst></a:custGeom>'
    return (
        f'<p:sp><p:nvSpPr><p:cNvPr id="{idx}" name="cg{idx}"/><p:cNvSpPr/><p:nvPr/></p:nvSpPr>'
        f'<p:spPr><a:xfrm>{off}{ext}</a:xfrm>{geom}{spfill}{ln}</p:spPr>'
        f'<p:txBody><a:bodyPr/><a:lstStyle/><a:p/></p:txBody></p:sp>'
    )


SLIDE3 = slide([
    textbox(2, 0.5, 0.2, 9.0, 0.8, [("left", [("第三張投影片 — 形狀幾何 (preset + custGeom + 外框)", 24, True, "1F2937")])]),
    shape(3, "roundRect", 0.5, 1.2, 2.8, 1.4, fill="4F81BD", line="1F3864", text="圓角矩形"),
    shape(4, "ellipse", 3.6, 1.2, 2.6, 1.4, fill="C0504D", line="7F1D1D", text="橢圓"),
    shape(5, "rightArrow", 6.5, 1.35, 3.0, 1.1, fill="9BBB59", line="4F6228"),
    shape(6, "triangle", 0.5, 3.0, 2.4, 2.2, fill="F79646", line="974806"),
    shape(7, "pentagon", 3.3, 3.0, 2.4, 2.2, fill="4BACC6", line="215868"),
    custgeom(8, 6.2, 3.0, 2.6, 2.2, [(50000, 0), (100000, 38000), (82000, 100000), (18000, 100000), (0, 38000)], fill="8064A2", line="3F3151"),
])

SLIDE2 = slide([
    textbox(2, 1.0, 0.5, 8.0, 1.0, [("center", [("第二張投影片", 36, True, "1F2937")])]),
    textbox(3, 1.0, 2.0, 3.6, 1.2, [("center", [("左方塊", 24, True, "FFFFFF")])], fill="4F81BD"),
    textbox(4, 5.4, 2.0, 3.6, 1.2, [("center", [("右方塊", 24, True, "FFFFFF")])], fill="C0504D"),
    textbox(5, 1.0, 4.0, 8.0, 1.0, [("right", [("— 簡報結束 —", 20, False, "888888")])]),
])

CT = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">
<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>
<Default Extension="xml" ContentType="application/xml"/>
<Default Extension="png" ContentType="image/png"/>
<Override PartName="/ppt/presentation.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml"/>
<Override PartName="/ppt/slides/slide1.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
<Override PartName="/ppt/slides/slide2.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
<Override PartName="/ppt/slides/slide3.xml" ContentType="application/vnd.openxmlformats-officedocument.presentationml.slide+xml"/>
</Types>"""

RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="ppt/presentation.xml"/>
</Relationships>"""

PRES = (
    '<?xml version="1.0" encoding="UTF-8" standalone="yes"?>'
    f'<p:presentation xmlns:a="{A}" xmlns:r="{R}" xmlns:p="{P}">'
    '<p:sldIdLst><p:sldId id="256" r:id="rId1"/><p:sldId id="257" r:id="rId2"/><p:sldId id="258" r:id="rId3"/></p:sldIdLst>'
    f'<p:sldSz cx="{SLIDE_CX}" cy="{SLIDE_CY}"/></p:presentation>'
)

PRES_RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide1.xml"/>
<Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide2.xml"/>
<Relationship Id="rId3" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide" Target="slides/slide3.xml"/>
</Relationships>"""


SLIDE1_RELS = """<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
<Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/image" Target="../media/image1.png"/>
</Relationships>"""


def main(path):
    import os
    img_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "test-image.png")
    with open(img_path, "rb") as f:
        img_bytes = f.read()
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("[Content_Types].xml", CT)
        z.writestr("_rels/.rels", RELS)
        z.writestr("ppt/presentation.xml", PRES)
        z.writestr("ppt/_rels/presentation.xml.rels", PRES_RELS)
        z.writestr("ppt/slides/slide1.xml", SLIDE1)
        z.writestr("ppt/slides/_rels/slide1.xml.rels", SLIDE1_RELS)
        z.writestr("ppt/slides/slide2.xml", SLIDE2)
        z.writestr("ppt/slides/slide3.xml", SLIDE3)
        z.writestr("ppt/media/image1.png", img_bytes)
    print("wrote", path)


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "sample.pptx")
