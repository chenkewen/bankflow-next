"""离线 PDF 页面工具，复用 PDFium，不引入额外转换引擎。"""
import os
import re
import shutil
import tempfile
from contextlib import ExitStack
from pathlib import Path

import pypdfium2 as pdfium


def page_indices(text, count):
    if not text.strip():
        return list(range(count))
    result = []
    for item in text.replace("，", ",").split(","):
        if not re.fullmatch(r"\s*\d+\s*(?:-\s*\d+\s*)?", item):
            raise ValueError("页码格式应为 1-3,5")
        ends = [int(x.strip()) for x in item.split("-")]
        start, end = ends[0], ends[-1]
        if not 1 <= start <= end <= count:
            raise ValueError(f"页码超出范围或顺序错误，文档共 {count} 页")
        for index in range(start - 1, end):
            if index in result:
                raise ValueError("页码不能重复")
            result.append(index)
    return result


def run(request, cancelled, emit):
    operation = request["mode"].removeprefix("pdf_")
    paths = request.get("inputs") or [request["input"]]
    if operation not in {"merge", "split", "extract", "rotate"}:
        raise ValueError("不支持的 PDF 操作")
    if (operation == "merge" and len(paths) < 2) or (operation != "merge" and len(paths) != 1):
        raise ValueError("合并至少选择两个文件，其他操作请选择一个文件")
    sources = [Path(p).resolve(strict=True) for p in paths]
    if any(not p.is_file() or p.suffix.lower() != ".pdf" for p in sources):
        raise ValueError("请选择 PDF 文件")
    parent = Path(request.get("output_dir") or sources[0].parent).resolve(strict=True)
    angle = request.get("angle", 90)
    if operation == "rotate" and angle not in (90, 180, 270):
        raise ValueError("旋转角度必须为 90、180 或 270")
    labels = {"merge": "合并", "split": "拆分", "extract": "提取", "rotate": "旋转"}
    base = sources[0].stem + "_" + labels[operation]
    staging = Path(tempfile.mkdtemp(prefix="bankflow-", dir=parent))
    try:
        with ExitStack() as stack:
            docs = [stack.enter_context(pdfium.PdfDocument(str(p), password=request.get("password") or None)) for p in sources]
            selected = [list(range(len(d))) if operation == "merge" else page_indices(request.get("pages", ""), len(d)) for d in docs]
            rotation_pages = set(selected[0]) if operation == "rotate" else set()
            if operation == "rotate":
                selected = [list(range(len(docs[0])))]
            total = sum(map(len, selected))
            if not total:
                raise ValueError("没有可处理的页面")
            combined = stack.enter_context(pdfium.PdfDocument.new())
            done = 0
            for doc, indices in zip(docs, selected):
                for index in indices:
                    if cancelled.is_set():
                        emit("cancelled", message="任务已取消，未保存结果。")
                        return
                    target = pdfium.PdfDocument.new() if operation == "split" else combined
                    try:
                        target.import_pages(doc, [index])
                        if operation == "rotate" and index in rotation_pages:
                            page = target[len(target)-1]
                            try:
                                page.set_rotation((page.get_rotation() + angle) % 360)
                            finally:
                                page.close()
                        if operation == "split":
                            target.save(str(staging / f"第{index+1}页.pdf"))
                    finally:
                        if operation == "split":
                            target.close()
                    done += 1
                    emit("progress", page=done, total=total)
            if operation != "split":
                combined.save(str(staging / "result.pdf"))
        if cancelled.is_set():
            emit("cancelled", message="任务已取消，未保存结果。")
            return
        suffix = "" if operation == "split" else ".pdf"
        index = 0
        while True:
            output = parent / (base + (f" ({index})" if index else "") + suffix)
            try:
                # Windows rename 不覆盖已有目标，支持普通磁盘和移动磁盘。
                if output.exists():
                    raise FileExistsError()
                os.rename(staging if operation == "split" else staging / "result.pdf", output)
                break
            except FileExistsError:
                index += 1
        emit("completed", output=str(output))
    finally:
        if staging.exists():
            shutil.rmtree(staging)
