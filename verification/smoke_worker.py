"""用合成流水测试桌面原生 worker 及其 JSON 行协议。"""
import ctypes
import json
import os
import subprocess
import sys
import tempfile
import threading
from pathlib import Path

import pypdfium2 as pdf
import pypdfium2.raw as raw
from openpyxl import load_workbook


ROOT = Path(__file__).resolve().parents[1]


def create_statement(path, pages=1):
    rows = [
        ("Date", "Memo", "Credit", "Debit", "Balance"),
        ("2026-01-01", "Transfer", "100.00", "", "1100.00"),
        ("2026-01-02", "Payment", "", "20.00", "1080.00"),
    ]
    with pdf.PdfDocument.new() as doc:
        for _ in range(pages):
            page = doc.new_page(540, 320)
            for i, values in enumerate(rows):
                for j, value in enumerate(values):
                    if not value:
                        continue
                    obj = raw.FPDFPageObj_NewTextObj(doc, b"Helvetica", 14)
                    buf = ctypes.create_string_buffer((value + "\0").encode("utf-16-le"))
                    assert raw.FPDFText_SetText(obj, ctypes.cast(buf, ctypes.POINTER(ctypes.c_ushort)))
                    raw.FPDFPageObj_Transform(obj, 1, 0, 0, 1, 30 + 100*j, 245 - 45*i)
                    raw.FPDFPage_InsertObject(page, obj)
            segments = [(x, 145, x, 280) for x in range(20, 530, 100)]
            segments += [(20, y, 520, y) for y in range(145, 281, 45)]
            for x0, y0, x1, y1 in segments:
                line = raw.FPDFPageObj_CreateNewPath(x0, y0)
                assert raw.FPDFPath_LineTo(line, x1, y1)
                assert raw.FPDFPath_SetDrawMode(line, 0, 1)
                raw.FPDFPage_InsertObject(page, line)
            assert raw.FPDFPage_GenerateContent(page)
            page.close()
        doc.save(path)


def run_worker(request, *, cancel_on_progress=False):
    exe = os.environ.get("BANKFLOW_TEST_WORKER") or ROOT / "src-tauri/target/debug/bankflow-workbench.exe"
    payload = {"password": "", **request}
    with tempfile.TemporaryFile("w+", encoding="utf-8") as errors, subprocess.Popen(
        [exe, "--image-worker"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=errors,
        text=True, encoding="utf-8",
    ) as process:
        timeout = threading.Timer(90, process.kill)
        timeout.start()
        try:
            # worker 将标准输入断开视为取消，需保持管道直到结束事件。
            process.stdin.write(json.dumps(payload) + "\n")
            process.stdin.flush()
            events = []
            for line in process.stdout:
                event = json.loads(line)
                events.append(event)
                if cancel_on_progress and event["kind"] == "progress":
                    process.stdin.write('{"cancel":true}\n')
                    process.stdin.flush()
                    cancel_on_progress = False
            status = process.wait()
            errors.seek(0)
            if status or not events:
                raise AssertionError(f"worker 退出码 {status}；{errors.read()}")
            return events
        finally:
            timeout.cancel()
            if process.poll() is None:
                process.kill()


def main():
    sys.stdout.reconfigure(encoding="utf-8")
    with tempfile.TemporaryDirectory() as temp:
        source = Path(temp) / "合成流水.pdf"
        create_statement(source)
        mode = "ocr" if "--ocr" in sys.argv else "pdf"
        if mode == "ocr":
            with pdf.PdfDocument(source) as document:
                page = document[0]
                bitmap = page.render(scale=2)
                with bitmap.to_pil() as image:
                    source = Path(temp) / "合成扫描.png"
                    image.save(source)
                bitmap.close()
                page.close()
        events = run_worker({"input": str(source), "mode": mode})
        assert events[-1]["kind"] == "completed", events
        assert any(e["kind"] == "progress" for e in events), events
        workbook = load_workbook(events[-1]["output"])
        try:
            cells = [str(value) for row in workbook.active.values for value in row]
            assert "2026-01-01" in cells and "2026-01-02" in cells, cells
        finally:
            workbook.close()
        print(f"{mode} 子进程进度、完成事件和导出日期均通过验证。")


if __name__ == "__main__":
    main()
