---
name: Unsupported format / file won't render
about: A file fails to open, throws "detected …", or renders blank/garbled
title: "[file] "
labels: ["bug", "unsupported"]
---

<!--
The fastest way to get this fixed is a MINIMAL reproducing file. Please don't upload
confidential documents — shrink the problem to a synthetic sample (one paragraph /
cell / slide that shows it) or redact the content, keeping only the structure that
breaks. A tiny file is usually all that's needed to add support or fix the bug.
-->

## The file

- Format / extension: <!-- e.g. .docx, .pptx, .pdf, .rtf, .ods, .png -->
- How it was produced: <!-- e.g. Word 2021 / LibreOffice 7 / exported from X -->
- [ ] I've attached the **smallest file that reproduces it** (synthetic or redacted)

<!-- Drag the file into this issue. If GitHub blocks the extension, zip it. -->

## What happens

<!-- Pick what applies + describe: -->
- [ ] Throws an error (paste it below)
- [ ] Renders **blank / partially blank**
- [ ] Renders **garbled / wrong** (attach a screenshot)
- [ ] Says the format is unsupported (`detected "…"`)

### Error message / console output

```
<!-- the message from mount(), and anything in the devtools console -->
```

### Expected vs actual

<!-- What it should look like vs what it does. A screenshot of the wrong output,
     and the source app's rendering for comparison, help a lot. -->

## Environment

- ShrimpDocViewer version:
- Browser / OS:
- `fontUrl` / `fonts` map used (if any):
- Mounted with `options.format` set? <!-- if you forced a format -->
