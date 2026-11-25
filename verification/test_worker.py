"""通过真实 Rust 子进程验证冲突、取消及失败后的文件状态。"""
import os
import subprocess
import tempfile
import unittest
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

from PIL import Image
from openpyxl import load_workbook

if __package__:
    from .smoke_worker import create_statement, run_worker
else:
    from smoke_worker import create_statement, run_worker


class WorkerTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / "流水.pdf"
        self.out = self.root / "输出"
        self.out.mkdir()
        self.request = {"input": str(self.source), "output_dir": str(self.out), "mode": "pdf"}

    def test_cli_handles_duplicate_names_and_single_file_replacement(self):
        sources = self.root / "输入"
        for directory, pages in (("a", 1), ("b", 2)):
            folder = sources / directory
            folder.mkdir(parents=True)
            create_statement(folder / "相同.pdf", pages)
        original = self.out / "相同.xlsx"
        original.write_bytes(b"existing output")
        exe = os.environ.get("BANKFLOW_TEST_WORKER") or (
            Path(__file__).resolve().parents[1] / "src-tauri/target/debug/bankflow-workbench.exe"
        )

        def export(path, *options):
            result = subprocess.run(
                [exe, "--convert", "pdf", path, "-o", self.out, *options],
                capture_output=True, encoding="utf-8", timeout=90,
            )
            self.assertEqual(result.returncode, 0, result.stderr)
            return [Path(line) for line in result.stdout.splitlines()]

        outputs = export(sources, "-r")
        self.assertEqual(len(set(outputs)), 2)
        self.assertEqual(original.read_bytes(), b"existing output")
        counts = []
        for output in outputs:
            book = load_workbook(output, read_only=True)
            try:
                counts.append(sum(row[0] == "2026-01-01" for row in book.active.values))
            finally:
                book.close()
        self.assertEqual(sorted(counts), [1, 2])
        self.assertEqual(export(sources / "a" / "相同.pdf"), [original])
        with original.open("rb") as stream:
            self.assertEqual(stream.read(2), b"PK")

    def test_concurrent_exports_preserve_previous_file(self):
        create_statement(self.source)
        original = self.out / "流水.xlsx"
        original.write_bytes(b"existing")
        with ThreadPoolExecutor(max_workers=2) as pool:
            jobs = [pool.submit(run_worker, self.request) for _ in range(2)]
            outputs = [job.result()[-1] for job in jobs]
        self.assertTrue(all(e["kind"] == "completed" for e in outputs), outputs)
        paths = {Path(e["output"]) for e in outputs}
        self.assertEqual(paths, {self.out / "流水 (1).xlsx", self.out / "流水 (2).xlsx"})
        for result in paths:
            book = load_workbook(result)
            try:
                dates = [row[0] for row in book.active.values]
                self.assertIn("2026-01-01", dates)
                self.assertIn("2026-01-02", dates)
            finally:
                book.close()
        self.assertEqual(original.read_bytes(), b"existing")
        self.assertEqual(set(self.out.iterdir()), paths | {original})

    def test_cancel_during_pdf_parse(self):
        create_statement(self.source, pages=40)
        events = run_worker(self.request, cancel_on_progress=True)
        self.assertEqual(events[0]["kind"], "progress", events)
        self.assertEqual(events[-1]["kind"], "cancelled", events)
        self.assertEqual(list(self.out.iterdir()), [])

    def test_partial_batch_failure_preserves_other_work(self):
        sentinel = self.out / "bankflow-existing"
        sentinel.mkdir()
        (sentinel / "原文件").write_bytes(b"kept")
        source = self.root / "已转换.png"
        broken = self.root / "损坏.png"
        Image.new("RGB", (32, 32)).save(source)
        broken.write_bytes(source.read_bytes()[:30])
        events = run_worker({
            "input": str(source), "inputs": [str(source), str(broken)],
            "output_dir": str(self.out), "mode": "image_convert",
        })
        self.assertEqual(events[0], {"kind": "progress", "page": 1, "total": 2}, events)
        self.assertEqual(events[-1]["kind"], "error", events)
        self.assertEqual(list(self.out.iterdir()), [sentinel])
        self.assertEqual((sentinel / "原文件").read_bytes(), b"kept")

    def test_cancel_after_first_image_written(self):
        source = self.root / "图片.png"
        Image.new("RGB", (384, 384)).save(source)
        events = run_worker({
            "input": str(source), "inputs": [str(source)] * 24,
            "output_dir": str(self.out), "mode": "image_convert",
        }, cancel_on_progress=True)
        self.assertEqual(events[0], {"kind": "progress", "page": 1, "total": 24}, events)
        self.assertEqual(events[-1]["kind"], "cancelled", events)
        self.assertEqual(list(self.out.iterdir()), [])

    def test_image_rejected_in_pdf_mode(self):
        image = self.root / "扫描.png"
        Image.new("RGB", (8, 8)).save(image)
        events = run_worker({**self.request, "input": str(image)})
        self.assertEqual(events[-1]["kind"], "error", events)
        self.assertIn("PDF", events[-1]["message"])
        self.assertEqual(list(self.out.iterdir()), [])


if __name__ == "__main__":
    unittest.main()
