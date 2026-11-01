"""使用合成 PDF 验证实际子进程协议，不包含真实流水数据。"""

import json
import os
import subprocess
import sys
import tempfile
import threading
from pathlib import Path

from openpyxl import load_workbook
from reportlab.pdfbase import pdfmetrics
from reportlab.pdfbase.cidfonts import UnicodeCIDFont
from reportlab.pdfgen.canvas import Canvas


def main():
    sys.stdout.reconfigure(encoding="utf-8")
    root = Path(__file__).resolve().parents[1]
    with tempfile.TemporaryDirectory() as temp:
        source = Path(temp) / "合成流水.pdf"
        pdfmetrics.registerFont(UnicodeCIDFont("STSong-Light"))
        canvas = Canvas(str(source))
        canvas.setFont("STSong-Light", 12)
        canvas.drawString(50, 790, "银行交易流水")
        rows = [
            ["交易日期", "摘要", "收入", "支出", "余额"],
            ["2026-01-01", "转入", "100.00", "", "1100.00"],
            ["2026-01-02", "消费", "", "20.00", "1080.00"],
        ]
        for i, row in enumerate(rows):
            for x, value in zip([50, 180, 290, 390, 490], row):
                canvas.drawString(x, 740 - i * 30, value)
        canvas.save()
        mode = "ocr" if "--ocr" in sys.argv else "pdf"
        if mode == "ocr":
            import pypdfium2
            document = pypdfium2.PdfDocument(str(source))
            page = document[0]
            bitmap = page.render(scale=2)
            image = bitmap.to_pil()
            source = Path(temp) / "合成扫描流水.png"
            image.save(source)
            image.close()
            bitmap.close()
            page.close()
            document.close()
        process = subprocess.Popen(
            ([os.environ["BANKFLOW_TEST_WORKER"]] if os.environ.get("BANKFLOW_TEST_WORKER")
             else [sys.executable, "-u", str(root / "scripts/run_worker.py")]),
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            text=True, encoding="utf-8")
        deadline = threading.Timer(90, process.kill)
        deadline.start()
        try:
            # 保持管道打开，避免把父进程断开误作正常提交完成。
            process.stdin.write(json.dumps({"input": str(source), "mode": mode}) + "\n")
            process.stdin.flush()
            events = [json.loads(line) for line in process.stdout]
            status = process.wait(timeout=30)
            errors = process.stderr.read()
            assert status == 0, errors + str(events)
            assert events[-1]["kind"] == "completed", events
            assert any(event["kind"] == "progress" for event in events), events
            workbook = load_workbook(events[-1]["output"])
            cells = [str(cell.value) for row in workbook.active for cell in row if cell.value is not None]
            assert "2026-01-01" in cells and "2026-01-02" in cells, cells
            workbook.close()
            print(f"{mode} 子进程转换通过：进度事件、完成事件、Excel 日期数据均已验证。")
        finally:
            deadline.cancel()
            if process.poll() is None:
                process.kill()
                process.wait()
            for stream in (process.stdin, process.stdout, process.stderr):
                stream.close()


if __name__ == "__main__":
    main()
