"""验证 PDF 页面顺序、旋转、冲突保护和失败清理。"""
import sys
import tempfile
import threading
import unittest
from pathlib import Path
import pypdfium2 as pdfium
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))
from bankflow.services.pdf_tools import run, page_indices

class PdfToolsTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.paths = []
        for name, widths in [("a", [200, 300]), ("b", [400])]:
            path = self.root / (name + ".pdf")
            with pdfium.PdfDocument.new() as doc:
                for width in widths:
                    doc.new_page(width, 500).close()
                doc.save(str(path))
            self.paths.append(str(path))
        self.events = []
        self.cancelled = threading.Event()

    def tearDown(self):
        self.temp.cleanup()

    def execute(self, operation, **options):
        request = dict(mode="pdf_"+operation, input=self.paths[0], inputs=self.paths if operation=="merge" else self.paths[:1], password="", pages="", angle=90)
        request.update(options)
        run(request, self.cancelled, lambda kind, **data: self.events.append(dict(kind=kind, **data)))
        return Path(self.events[-1]["output"]) if self.events[-1]["kind"] == "completed" else None

    def widths(self, path):
        with pdfium.PdfDocument(str(path)) as doc:
            return [doc.get_page_size(i)[0] for i in range(len(doc))]

    def test_merge_order_and_no_overwrite(self):
        first=self.execute("merge")
        self.assertEqual(self.widths(first), [200,300,400])
        second=self.execute("merge", inputs=self.paths[::-1])
        self.assertNotEqual(first,second)
        self.assertEqual(self.widths(second), [400,200,300])
        self.assertEqual(self.widths(self.paths[0]), [200,300])

    def test_extract_split_rotate(self):
        self.assertEqual(self.widths(self.execute("extract", pages="2,1")), [300,200])
        split=self.execute("split")
        self.assertEqual(len(list(split.glob("*.pdf"))),2)
        self.assertEqual(self.widths(split/"第2页.pdf"),[300])
        rotated=self.execute("rotate",pages="1",angle=90)
        with pdfium.PdfDocument(str(rotated)) as doc:
            self.assertEqual(len(doc),2)
            page=doc[0]
            self.assertEqual(page.get_rotation(),90)
            page.close()
            page=doc[1]
            self.assertEqual(page.get_rotation(),0)
            page.close()

    def test_bad_pages_and_cancellation_leave_no_output(self):
        for pages in ("0", "3", "2-1", "1,1", "x"):
            with self.assertRaises(ValueError):
                self.execute("extract",pages=pages)
        self.cancelled.set()
        self.assertIsNone(self.execute("merge"))
        self.assertEqual(sorted(p.name for p in self.root.iterdir()),["a.pdf","b.pdf"])
