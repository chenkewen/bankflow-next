"""验证桥接层的取消、冲突隔离和失败清理，不替代银行样本回归。"""

import io
import json
import sys
import tempfile
import threading
import types
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "python"))
from bankflow.controllers import worker
from bankflow.services.conversion import convert
from bankflow.parsers import pdf_to_excel


class WorkerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.source = self.root / "流水.pdf"
        self.source.write_bytes(b"fixture")
        self.request = {"input": str(self.source), "mode": "pdf", "password": ""}
        self.cancelled = threading.Event()
        self.output = io.StringIO()
        self.protocol = patch.object(worker, "PROTOCOL", self.output)
        self.protocol.start()
        outer = self

        class Parser:
            def __init__(self, *args, **kwargs):
                self.progress = kwargs["progress_callback"]

            def parse(self):
                self.progress(1, 1)
                if outer.cancel_during_parse:
                    outer.cancelled.set()
                return self

        self.cancel_during_parse = False
        self.engine = types.SimpleNamespace(
            BankPDFParser=Parser,
            write_excel=lambda parser, path: Path(path).write_bytes(b"excel fixture"))
        self.modules = patch.multiple(pdf_to_excel, BankPDFParser=self.engine.BankPDFParser, write_excel=lambda *args: self.engine.write_excel(*args))
        self.modules.start()

    def tearDown(self):
        self.modules.stop()
        self.protocol.stop()
        self.temp.cleanup()

    def events(self):
        return [json.loads(line) for line in self.output.getvalue().splitlines()]

    def test_output_does_not_replace_existing_file(self):
        original = self.root / "流水.xlsx"
        original.write_bytes(b"existing")
        convert(self.request, self.cancelled, worker.emit)
        first = Path(self.events()[-1]["output"])
        convert(self.request, self.cancelled, worker.emit)
        second = Path(self.events()[-1]["output"])
        self.assertEqual(first.parent, self.root)
        self.assertEqual(first.name, "流水 (1).xlsx")
        self.assertEqual(second.name, "流水 (2).xlsx")
        self.assertNotEqual(first, second)
        self.assertTrue(first.is_file() and second.is_file())
        self.assertEqual(original.read_bytes(), b"existing")

    def test_cancelled_parse_does_not_export_partial_result(self):
        self.cancel_during_parse = True
        convert(self.request, self.cancelled, worker.emit)
        self.assertEqual(self.events()[-1]["kind"], "cancelled")
        self.assertEqual(list(self.root.glob("bankflow-*")), [])

    def test_export_failure_removes_only_own_directory(self):
        existing = self.root / "bankflow-existing"
        existing.mkdir()

        def fail(parser, path):
            Path(path).write_bytes(b"partial")
            raise OSError("写入失败")

        self.engine.write_excel = fail
        with self.assertRaises(OSError):
            convert(self.request, self.cancelled, worker.emit)
        self.assertEqual(list(self.root.glob("bankflow-*")), [existing])

    def test_cancel_during_export_removes_unpublished_output(self):
        def write(parser, path):
            Path(path).write_bytes(b"partial")
            self.cancelled.set()

        self.engine.write_excel = write
        convert(self.request, self.cancelled, worker.emit)
        self.assertEqual(self.events()[-1]["kind"], "cancelled")
        self.assertEqual(list(self.root.glob("bankflow-*")), [])

    def test_image_rejected_in_pdf_mode(self):
        image = self.root / "扫描.png"
        image.write_bytes(b"fixture")
        self.request["input"] = str(image)
        with self.assertRaisesRegex(ValueError, "不匹配"):
            convert(self.request, self.cancelled, worker.emit)


if __name__ == "__main__":
    unittest.main()
