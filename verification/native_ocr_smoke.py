"""验证 Rust 整页识别、表格线、识别文字和置信度。"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import struct
import tempfile
import unittest

from PIL import Image, ImageDraw, ImageFont
import pypdfium2 as pdf
from openpyxl import load_workbook

ROOT = Path(__file__).resolve().parents[1]
DEBUG_EXE = ROOT / "src-tauri/target/debug/bankflow-workbench.exe"
DIAGNOSTICS = os.environ.get("BANKFLOW_DIAGNOSTICS_ENGINE") or os.environ.get("BANKFLOW_ROWS_ENGINE") or DEBUG_EXE
WORKER = os.environ.get("BANKFLOW_TEST_WORKER") or os.environ.get("BANKFLOW_ROWS_ENGINE") or DEBUG_EXE


def operation(command, payload):
    result = subprocess.run([DIAGNOSTICS, command], input=json.dumps(payload).encode(),
                            stdout=subprocess.PIPE, check=True)
    return json.loads(result.stdout)


class NativeOcrSmoke(unittest.TestCase):
    def test_watermark_and_unresolved_amounts_in_exported_cells(self):
        image = Image.new("RGB", (1050, 450), "white")
        draw = ImageDraw.Draw(image)
        font = ImageFont.truetype("msyh.ttc", 30)
        xs, ys = [45, 335, 550, 770, 990], [65, 140, 215, 290, 365]
        for x in xs:
            draw.line((x, ys[0], x, ys[-1]), fill="black", width=2)
        for y in ys:
            draw.line((xs[0], y, xs[-1], y), fill="black", width=2)
        draw.text((477, 177), "电子回单", font=ImageFont.truetype("msyh.ttc", 44), fill=(186,) * 3)
        rows = [["交易日期", "交易金额", "账户余额", "摘要"],
                ["2026-04-01", "123.45", "1123.45", "转入"],
                ["2026-04-02", "12或345", "待核", "消费"],
                ["2026-04-03", "80.00", "1183.45", "转入"]]
        for y, row in zip(ys, rows):
            for x, value in zip(xs, row):
                draw.text((x + 10, y + 8), value, font=font, fill="black")
        with tempfile.TemporaryDirectory() as tmp:
            source = Path(tmp) / "watermark.png"
            image.save(source)
            subprocess.run([WORKER, "--convert", "ocr", source, "-o", tmp], check=True,
                           stdout=subprocess.PIPE)
            workbook = load_workbook(source.with_suffix(".xlsx"))
            try:
                first, uncertain, last = list(workbook.active)[-3:]
                self.assertEqual([c.value for c in first[1:3]], [123.45, 1123.45])
                self.assertEqual([c.value for c in last[1:3]], [80, 1183.45])
                self.assertEqual([c.value for c in uncertain[1:3]], ["12或345", "待核"])
                for cell in uncertain[1:3]:
                    self.assertIn("核对", cell.comment.text)
                    self.assertEqual(cell.fill.fgColor.rgb[-6:], "FFF2CC")
            finally:
                workbook.close()

    def test_cover_and_blank_pages_do_not_hide_later_transactions(self):
        blank = Image.new("RGB", (800, 400), "white")
        cover, table = blank.copy(), blank.copy()
        font = ImageFont.truetype("msyh.ttc", 23)
        ImageDraw.Draw(cover).text((80, 60), "账户查询封面", font=font, fill="black")
        draw = ImageDraw.Draw(table)
        columns, lines = [12, 235, 403, 569, 782], [100, 148, 196, 244]
        for x in columns:
            draw.line((x, lines[0], x, lines[-1]), fill="black", width=2)
        for y in lines:
            draw.line((columns[0], y, columns[-1], y), fill="black", width=2)
        values = [["交易日期", "交易金额", "账号余额", "摘要"],
                  ["2026-01-01", "120.00", "250.00", "转入"],
                  ["2026-01-02", "-10.00", "240.00", "消费"]]
        for y, row in zip(lines, values):
            for x, text in zip(columns, row):
                draw.text((x + 6, y + 5), text, font=font, fill="black")
        with tempfile.TemporaryDirectory() as tmp:
            src = Path(tmp) / "ocr.pdf"
            with pdf.PdfDocument.new() as document:
                for image in (blank, cover, table, blank):
                    page = document.new_page(400, 200)
                    bitmap = pdf.PdfBitmap.from_pil(image)
                    obj = pdf.PdfImage.new(document)
                    obj.set_bitmap(bitmap)
                    obj.set_matrix(pdf.PdfMatrix().scale(400, 200))
                    page.insert_obj(obj)
                    page.gen_content()
                    page.close()
                    bitmap.close()
                document.save(src)
            env = dict(os.environ, BANKFLOW_PYTHON=str(Path(tmp) / "missing.exe"))
            with subprocess.Popen([WORKER, "--image-worker"], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, env=env) as proc:
                proc.stdin.write(json.dumps({"mode": "ocr", "input": str(src), "password": ""}).encode() + b"\n")
                proc.stdin.flush()
                events = [json.loads(line) for line in proc.stdout]
            self.assertEqual([e["page"] for e in events if e["kind"] == "progress"], [1, 2, 3, 4])
            self.assertEqual(events[-1]["kind"], "completed", events[-1])
            workbook = load_workbook(events[-1]["output"], read_only=True)
            try:
                rows = list(workbook.active.values)
            finally:
                workbook.close()
            self.assertEqual(rows[0][0], "账户查询封面")
            self.assertEqual([r[0] for r in rows[-3:]], [r[0] for r in values])
            self.assertEqual([r[1] for r in rows[-2:]], [120, -10])

    def test_ocr_geometry(self):
        texts = ["交易日期借方金额贷方金额余额", "收入/支出金额", "数值 A01 33.45",
                 "2025-12-28 18:28:05", "750,000.0020,933,362.14",
                 "123456789123452026-01-30转账", "0123456机构", "户名12345678",
                 "7", "支", "A", "①", "三", "０", "2026‑12‑03"]
        items = [([[0, 30*i], [170, 30*i+4], [170, 30*i+15], [0, 30*i+11]],
                  text, 0.6 if len(text) == 1 else 0.95) for i, text in enumerate(texts)]
        # 基线由旧解析器生成，逐项固定词、字、顺序及四个坐标，保留七位小数。
        expected = {
            (True, ()): "8fcd88b2d766c66e8856431b7e098a53e028001bf6034e34badd4e5a4900d7de",
            (True, (40,)): "f07f23b81139bb65988463a4bf45396b65614577f9d6a08b7499b083f5180976",
            (True, (40, 60)): "db887b74626f6a651cfae205d8cab363651ee7934da3848f037b3044022ac14d",
            (False, ()): "55fe1ab03b4b5771ee68d6339cd0ed23cc77d9ba6c17a316025ea068f9843743",
            (False, (40,)): "8546ed44763dce5c9bfcbad5499cc5ba4dbf14377403c4c5fd14529b4ac0a7f7",
            (False, (40, 60)): "63ed17bf05a25345e525be8d765c3b036b3c61fd840879a1a09568228a77d466",
        }
        for (deskew, lines), digest in expected.items():
            with self.subTest(deskew=deskew, lines=lines):
                result = operation("--ocr-geometry", {
                    "items": [dict(points=b,text=t,score=s) for b,t,s in items],
                    "width": 200, "height": 680, "scale_to_pt": 0.5,
                    "allow_deskew": deskew, "vlines": lines,
                })
                glyphs = [[(w["text"], *[round(w[k], 7) for k in ("x0", "x1", "top", "bottom")])
                           for w in group] for group in result]
                serialized = json.dumps(glyphs, ensure_ascii=False, separators=(",", ":")).encode()
                self.assertEqual(hashlib.sha256(serialized).hexdigest(), digest)

    def test_metadata_grid_and_plain(self):
        def word(text, x0, x1, top):
            return dict(text=text, x0=x0, x1=x1, top=top, bottom=top + 4)

        words = [word("日期：", 7, 20, 6), word("Print", 7, 18, 12),
                 word("Date:", 21, 33, 12), word("2025-2-2", 35, 63, 12),
                 word("Page 2", 100, 125, 12), word("时间：14:00", 140, 180, 6),
                 word("本次查询时间：", 1, 48, 40), word("01:45", 50, 63, 40),
                 word("户", 70, 72, 40), word("名：合成", 75, 98, 40),
                 word("温馨提示", 2, 36, 55), word("Currency", 8, 33, 65)]
        actual = operation("--ocr-metadata", dict(words=words, lines=[0, 24], width=200, hdr_top=90))
        self.assertEqual(actual, [
            [["日期", "2025-02-02"], ["时间", "14:00"]],
            [["本次查询时间", "01:45"], ["户名", "合成"]],
        ])

    def test_page_outputs(self):
        image = Image.new("RGB", (500, 120), "white")
        draw = ImageDraw.Draw(image)
        draw.text((15, 12), "1234.56   2026/11/24", font_size=27, fill="black")
        draw.text((15, 70), "Balance 789.01", font_size=27, fill="black")
        draw.line((10, 56, 490, 56), fill="black", width=2)
        draw.ellipse((58, 75, 84, 99), outline=(221, 35, 45), width=2)
        frame = struct.pack("<IId", *image.size, 0.5) + image.tobytes()
        with subprocess.Popen([DIAGNOSTICS, "--ocr-page"], stdin=subprocess.PIPE, stdout=subprocess.PIPE) as proc:
            self.assertEqual(proc.stdout.readline(), b"OK\n")
            pages = []
            for _ in range(2):
                proc.stdin.write(frame)
                proc.stdin.flush()
                pages.append(json.loads(proc.stdout.readline()))
            proc.stdin.close()
        result = pages[0]
        self.assertEqual(result, pages[1])
        self.assertEqual((result["width"], result["height"]), image.size)
        self.assertEqual((result["h"], result["v"]), ([28.2], []))
        expected = [
            ([[16, 16], [277, 16], [277, 41], [16, 41]], "1234.562026/11/24", 0.9990489167325637),
            ([[15, 74], [203, 74], [203, 100], [15, 100]], "Balance 789.01", 0.9620644961084638),
        ]
        self.assertEqual(len(result["items"]), len(expected))
        for item, (points, text, score) in zip(result["items"], expected):
            self.assertEqual((item["points"], item["text"]), (points, text))
            self.assertAlmostEqual(item["score"], score, delta=1e-6)


if __name__ == "__main__":
    unittest.main()
