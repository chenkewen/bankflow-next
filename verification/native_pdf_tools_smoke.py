"""通过桌面 worker 协议核对 PDF 工具的页序、内容、注释和失败清理。"""
import ctypes
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

import pypdfium2 as pdf
import pypdfium2.raw as raw

ROOT = Path(__file__).resolve().parents[1]
EXE = ROOT / "src-tauri/target/debug/bankflow-workbench.exe"


def fixture(path, widths):
    with pdf.PdfDocument.new() as doc:
        for i, width in enumerate(widths):
            page = doc.new_page(width, 350)
            page.set_rotation(90 if i == 0 else 0)
            rect = raw.FPDFPageObj_CreateNewRect(10, 15, width / 2, 100)
            assert raw.FPDFPageObj_SetFillColor(rect, 25, 50 + i, 100, 255)
            assert raw.FPDFPath_SetDrawMode(rect, raw.FPDF_FILLMODE_ALTERNATE, 0)
            raw.FPDFPage_InsertObject(page, rect)
            assert raw.FPDFPage_GenerateContent(page)
            annot = raw.FPDFPage_CreateAnnot(page, raw.FPDF_ANNOT_TEXT)
            assert annot
            bounds = raw.FS_RECTF(0, 20, 20, 0)
            assert raw.FPDFAnnot_SetRect(annot, ctypes.byref(bounds))
            raw.FPDFPage_CloseAnnot(annot)
            page.close()
        doc.save(str(path))


def signatures(path):
    pages = []
    with pdf.PdfDocument(str(path)) as doc:
        for page in doc:
            rotation = page.get_rotation()
            annotations = raw.FPDFPage_GetAnnotCount(page)
            bitmap = page.render(scale=0.5, rotation=(360 - rotation) % 360)
            digest = hashlib.sha256(bitmap.to_pil().tobytes()).digest()
            bitmap.close()
            pages.append((rotation, annotations, digest))
            page.close()
    return pages


class NativePdfTools(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.folder = Path(self.temp.name)
        self.a = self.folder / "输入甲.pdf"
        self.b = self.folder / "输入乙.pdf"
        fixture(self.a, [160, 220])
        fixture(self.b, [180])

    def call(self, mode, inputs, pages="", angle=90, cancel=False):
        request = dict(mode="pdf_" + mode, input=str(inputs[0]), password="",
                       inputs=list(map(str, inputs)), pages=pages, angle=angle)
        with subprocess.Popen([EXE, "--image-worker"], stdin=subprocess.PIPE,
                              stdout=subprocess.PIPE, encoding="utf-8") as process:
            process.stdin.write(json.dumps(request) + "\n")
            if cancel:
                process.stdin.write('{"cancel":true}\n')
            process.stdin.flush()
            events = list(map(json.loads, process.stdout))
        self.assertEqual(process.returncode, 0)
        return events

    def completed(self, mode, inputs, **options):
        events = self.call(mode, inputs, **options)
        self.assertEqual(events[-1]["kind"], "completed", events[-1])
        progress = events[:-1]
        self.assertEqual([e["page"] for e in progress], list(range(1, len(progress) + 1)))
        self.assertTrue(all(e["total"] == len(progress) for e in progress))
        return Path(events[-1]["output"])

    def test_merge_extract_split_and_collision(self):
        a, b = signatures(self.a), signatures(self.b)
        self.assertTrue(all(page[1] == 1 for page in a + b))
        self.assertEqual(signatures(self.completed("merge", [self.b, self.a])), b + a)
        first = self.completed("extract", [self.a], pages="2，1")
        second = self.completed("extract", [self.a], pages="1")
        self.assertNotEqual(first, second)
        self.assertEqual(signatures(first), a[::-1])
        self.assertEqual(signatures(second), a[:1])
        split = self.completed("split", [self.a], pages="2")
        self.assertEqual([p.name for p in split.iterdir()], ["第2页.pdf"])
        self.assertEqual(signatures(split / "第2页.pdf"), a[1:])

    def test_relative_rotation_and_unchanged_sources(self):
        before = signatures(self.a)
        output = self.completed("rotate", [self.a], pages="1", angle=270)
        self.assertEqual(signatures(output), [(0, *before[0][1:]), before[1]])
        self.assertEqual(signatures(self.a), before)

    def test_cancel_invalid_input_cleanup(self):
        before = set(self.folder.iterdir())
        result = self.call("merge", [self.a] * 10, cancel=True)
        self.assertEqual(result[-1]["kind"], "cancelled")
        self.assertEqual(self.call("extract", [self.a], pages="1-2,1")[-1]["kind"], "error")
        self.assertEqual(before, set(self.folder.iterdir()))


class NativeStatementEmptyResult(unittest.TestCase):
    def run_pdf(self, source):
        env = dict(os.environ, BANKFLOW_PYTHON=str(source.parent / "无此解释器.exe"))
        with subprocess.Popen([EXE, "--image-worker"], stdin=subprocess.PIPE,
                              stdout=subprocess.PIPE, encoding="utf-8", env=env) as proc:
            proc.stdin.write(json.dumps(dict(mode="pdf", input=str(source), password="")) + "\n")
            proc.stdin.flush()
            return list(map(json.loads, proc.stdout))[-1]

    def test_empty_page_and_empty_grid_have_distinct_results(self):
        from openpyxl import load_workbook

        with tempfile.TemporaryDirectory() as tmp, pdf.PdfDocument.new() as doc:
            source = Path(tmp) / "无正文.pdf"
            page = doc.new_page(360, 400)
            doc.save(source)
            result = self.run_pdf(source)
            self.assertEqual(result["kind"], "error")
            self.assertIn("流水表头或记录", result["message"])
            self.assertEqual(list(source.parent.glob("*.xlsx")), [])

            grid = Path(tmp) / "空网格.pdf"
            segments = [(x, 200, x, 300) for x in (20, 120, 220, 320)]
            segments += [(20, y, 320, y) for y in (200, 225, 250, 275, 300)]
            for x0, y0, x1, y1 in segments:
                path = raw.FPDFPageObj_CreateNewPath(x0, y0)
                self.assertTrue(raw.FPDFPath_LineTo(path, x1, y1))
                self.assertTrue(raw.FPDFPath_SetDrawMode(path, 0, 1))
                raw.FPDFPage_InsertObject(page, path)
            self.assertTrue(raw.FPDFPage_GenerateContent(page))
            doc.save(grid)
            page.close()
            result = self.run_pdf(grid)
            self.assertEqual(result["kind"], "completed", result)
            workbook = load_workbook(result["output"])
            try:
                self.assertEqual(workbook.active.dimensions, "A1:A1")
                self.assertIsNone(workbook.active["A1"].value)
            finally:
                workbook.close()


if __name__ == "__main__":
    unittest.main()
