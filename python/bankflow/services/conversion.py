"""分派转换任务，协调解析、输出与取消。"""
import contextlib
import os
import shutil
import sys
import tempfile
from pathlib import Path


def convert(request, cancelled, emit):
    if request["mode"] in {"image_convert", "images_pdf", "pdf_images"}:
        from bankflow.services.image_tools import run
        return run(request, cancelled, emit)
    if request["mode"].startswith("pdf_"):
        from bankflow.services.pdf_tools import run
        return run(request, cancelled, emit)
    source = Path(request["input"]).resolve(strict=True)
    if not source.is_file():
        raise ValueError("输入必须为文件")
    mode = request["mode"]
    allowed = {".pdf"} if mode == "pdf" else {".pdf", ".png", ".jpg", ".jpeg", ".bmp", ".tif", ".tiff"}
    if mode not in {"pdf", "ocr"} or source.suffix.lower() not in allowed:
        raise ValueError("文件类型与解析方式不匹配")
    parent = Path(request.get("output_dir") or source.parent).resolve(strict=True)
    if not parent.is_dir():
        raise ValueError("输出位置必须为目录")

    def progress(page, total):
        emit("progress", page=page, total=total)

    # 隔离依赖打印，避免污染父进程读取的协议。
    with contextlib.redirect_stdout(sys.stderr):
        from bankflow.parsers import pdf_to_excel
        if mode == "ocr":
            from bankflow.parsers.ocr_to_excel import BankOCRParser
            parser_class = BankOCRParser
        else:
            parser_class = pdf_to_excel.BankPDFParser
        if cancelled.is_set():
            emit("cancelled", message="任务已取消，未导出结果。")
            return
        parser = parser_class(str(source), password=request.get("password") or None,
                              progress_callback=progress, cancel_check=cancelled.is_set).parse()
        if cancelled.is_set():
            emit("cancelled", message="任务已取消，未导出不完整数据。")
            return
        # 仅清理本次新建的目录，不接触用户已有输出。
        task_dir = Path(tempfile.mkdtemp(prefix="bankflow-", dir=parent))
        try:
            output = task_dir / (source.stem + ".xlsx")
            pdf_to_excel.write_excel(parser, str(output))
            if cancelled.is_set():
                shutil.rmtree(task_dir)
                emit("cancelled", message="任务已取消，已移除本次未交付结果。")
                return
        except Exception:
            shutil.rmtree(task_dir)
            raise
        # 在同一磁盘上以不覆盖的方式发布完整文件，不覆盖并发生成的同名结果。
        try:
            index = 0
            while True:
                suffix = f" ({index})" if index else ""
                destination = parent / f"{source.stem}{suffix}.xlsx"
                try:
                    if os.name == "nt":
                        os.rename(output, destination)
                    else:
                        os.link(output, destination)
                    break
                except FileExistsError:
                    index += 1
        finally:
            shutil.rmtree(task_dir)
    emit("completed", output=str(destination))

