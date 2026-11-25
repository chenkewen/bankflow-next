"""对照同页直接解析与 OCR 工作簿，输出分类计数，不输出交易内容。"""
import argparse
from collections import Counter
from contextlib import closing
from decimal import Decimal
import json
from pathlib import Path
import re
import subprocess
import tempfile
import time
import unicodedata
import sys

from openpyxl import load_workbook


GROUPS = ("amount", "account", "name", "sequence", "other")
DATE = re.compile(r"(?<!\d)(\d{4})[-/.年]?(\d{2})[-/.月]?(\d{2})日?")


def clean(cell):
    value = "" if cell is None else str(cell)
    return "".join(unicodedata.normalize("NFKC", value).split())


def category(column):
    if any(s in column for s in ("金额", "余额", "发生额")) or column in ("收入", "支出"):
        return "amount"
    if any(s in column for s in ("流水号", "序号", "编号", "交易单号", "商户单号", "日志号", "凭证号", "凭证代号")):
        return "sequence"
    # 账号与户名混合列单列归入 other，避免错误拆分一格或重复计错。
    account = any(s in column for s in ("账号", "卡号", "账户", "帐号"))
    name = any(s in column for s in ("户名", "账户名", "姓名", "对方名称")) or column == "交易对方"
    if name and "与户名" not in column:
        return "name"
    if account and not name:
        return "account"
    return "other"


def normalized(cell, kind):
    value = clean(cell)
    if kind == "amount" and value and re.fullmatch(r"[+-]?(?:\d+|\d{1,3}(?:,\d{3})+)(?:\.\d+)?", value):
        return Decimal(value.replace(",", ""))
    return value


def read_table(path):
    book = load_workbook(path, read_only=True, data_only=True)
    try:
        rows = iter(book.active)
        for row in rows:
            cells = [c for c in row if c.value is not None]
            if len(cells) >= 2 and all(c.font.bold and c.alignment.horizontal == "center" for c in cells):
                headers = [clean(c.value) for c in row]
                while headers and not headers[-1]:
                    headers.pop()
                data = [[c.value for c in r] for r in rows if any(c.value is not None for c in r)]
                return headers, data, cells[0].row
    finally:
        book.close()
    raise ValueError("工作簿没有可识别的交易表头")


def align_keys(headers, rows):
    date_cols = [i for i, h in enumerate(headers) if "日期" in h or "时间" in h]
    count = Counter()
    keys = []
    for row in rows:
        date = next((m.groups() for i in date_cols if i < len(row)
                     if (m := DATE.search(clean(row[i])))), None)
        if date:
            count[date] += 1
            keys.append((date, count[date]))
        else:
            keys.append(None)
    return keys


def compare(reference, output):
    header, ref, _ = read_table(reference)
    actual, obs, header_row = read_table(output)
    ref_keys = align_keys(header, ref)
    # 同页坐标列固定，表头识别错误单计。具体样本发生增减列时单独披露。
    obs_keys = align_keys(header, obs)
    # 同一日期笔数不同时，按出现顺序硬配会把一笔漏识扩散为整日的字段错误。
    ref_dates = Counter(k[0] for k in ref_keys if k is not None)
    obs_dates = Counter(k[0] for k in obs_keys if k is not None)
    lookup = {key: j for j, key in enumerate(obs_keys) if key is not None}
    matched = [(i, lookup[k]) for i, k in enumerate(ref_keys)
               if k is not None and ref_dates[k[0]] == obs_dates[k[0]]]
    counts = {g: {"cells": 0, "different": 0, "different_preserving_width": 0} for g in GROUPS}
    errors = set()
    for i, j in matched:
        for c, label in enumerate(header):
            kind = category(label)
            a = ref[i][c] if c < len(ref[i]) else None
            b = obs[j][c] if c < len(obs[j]) else None
            counts[kind]["cells"] += 1
            different = normalized(a, kind) != normalized(b, kind)
            if different:
                counts[kind]["different"] += 1
                errors.add((j, c))
            if kind == "amount":
                counts[kind]["different_preserving_width"] += different
            else:
                counts[kind]["different_preserving_width"] += (
                    "".join(str(a if a is not None else "").split())
                    != "".join(str(b if b is not None else "").split())
                )
    highlighted = set()
    book = load_workbook(output)
    try:
        for row in book.active.iter_rows(min_row=header_row + 1):
            for cell in row:
                if cell.comment:
                    highlighted.add((cell.row - header_row - 1, cell.column - 1))
    finally:
        book.close()
    matched_outputs = {j for _, j in matched}
    judged = {(r, c) for r, c in highlighted if r in matched_outputs and c < len(header)}
    return {
        "reference_rows": len(ref), "output_rows": len(obs), "matched_rows": len(matched),
        "undated_reference": ref_keys.count(None), "undated_output": obs_keys.count(None),
        "unmatched_reference": len(ref) - len(matched), "unmatched_output": len(obs) - len(matched),
        "different_headers": sum(a != b for a, b in zip(header, actual)) + abs(len(header) - len(actual)),
        "column_count_difference": len(actual) - len(header), "fields": counts,
        "review": {"flagged": len(highlighted), "judged": len(judged), "correctly_flagged": len(judged & errors),
                   "flagged_equal": len(judged - errors), "unflagged_errors": len(errors - judged)},
    }


def peak_working_set(process):
    import ctypes
    from ctypes import wintypes

    class Counters(ctypes.Structure):
        _fields_ = [("cb", wintypes.DWORD), ("faults", wintypes.DWORD),
                    ("peak", ctypes.c_size_t), ("working_set", ctypes.c_size_t),
                    ("quota_and_pagefile", ctypes.c_size_t * 6)]

    info = Counters()
    info.cb = ctypes.sizeof(info)
    read = ctypes.WinDLL("psapi", use_last_error=True).GetProcessMemoryInfo
    read.argtypes = [wintypes.HANDLE, ctypes.POINTER(Counters), wintypes.DWORD]
    read.restype = wintypes.BOOL
    if not read(int(process._handle), ctypes.byref(info), info.cb):
        raise ctypes.WinError(ctypes.get_last_error())
    return info.peak


def run_worker(exe, source, mode, output):
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(dir=output.parent) as directory:
        request = dict(mode=mode, input=str(source.resolve()), password="", output_dir=str(Path(directory).resolve()))
        start = time.perf_counter()
        with subprocess.Popen([exe, "--image-worker"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                              stderr=subprocess.DEVNULL, encoding="utf-8") as proc:
            proc.stdin.write(json.dumps(request) + "\n")
            proc.stdin.flush()
            # 保持输入管道打开，产品协议会在管道关闭时取消任务。
            events = [json.loads(line) for line in proc.stdout]
            status = proc.wait()
            elapsed = time.perf_counter() - start
            # Windows 在保留进程句柄时仍可读取生命周期峰值，避免轮询漏掉短峰。
            peak = peak_working_set(proc) if sys.platform == "win32" else None
        if status or not events or events[-1]["kind"] != "completed":
            raise RuntimeError(f"{source.stem} 执行失败，退出状态 {status}")
        Path(events[-1]["output"]).replace(output)
        return round(elapsed, 4), peak


def time_pairs(before, after, files, mode, output, reverse=False):
    times = {}
    peaks = {}
    versions = [("before", before), ("after", after)]
    for i, source in enumerate(files):
        if not re.fullmatch(r"S\d+", source.stem):
            raise ValueError("输入文件需要使用 S 加数字作为脱敏编号")
        times[source.stem] = {}
        peaks[source.stem] = {}
        for name, exe in (versions[::-1] if bool(i % 2) ^ reverse else versions):
            dest = output / name / source.with_suffix(".xlsx").name
            times[source.stem][name], peaks[source.stem][name] = run_worker(exe, source, mode, dest)
        (output / "times.json").write_text(json.dumps(times, indent=2), encoding="utf-8", newline="\n")
        (output / "peak_bytes.json").write_text(json.dumps(peaks, indent=2), encoding="utf-8", newline="\n")
    return times


def prepare_pages(source, output, pages):
    import pypdfium2 as pdf

    if not re.fullmatch(r"S\d+", source.stem) or not pages or min(pages) < 1:
        raise ValueError("需要使用脱敏样本编号与从 1 开始的页码")
    for folder in ("scan", "source"):
        (output / folder).mkdir(parents=True, exist_ok=True)
    with pdf.PdfDocument(source) as original, pdf.PdfDocument.new() as reference, pdf.PdfDocument.new() as scan:
        reference.import_pages(original, [page - 1 for page in pages])
        reference.save(output / "source" / source.name)
        for number in pages:
            with closing(original.get_page(number - 1)) as page:
                width, height = page.get_size()
                with closing(page.render(scale=2)) as bitmap, closing(scan.new_page(width, height)) as dest:
                    image = pdf.PdfImage.new(scan)
                    image.set_bitmap(bitmap)
                    image.set_matrix(pdf.PdfMatrix().scale(width, height))
                    dest.insert_obj(image)
                    dest.gen_content()
        scan.save(output / "scan" / source.name)
    with pdf.PdfDocument(output / "scan" / source.name) as check:
        for index in range(len(check)):
            with closing(check.get_page(index)) as page, closing(page.get_textpage()) as text:
                if text.count_chars():
                    raise ValueError("扫描样本仍含原 PDF 文字层")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    actions = parser.add_subparsers(dest="action", required=True)
    comparison = actions.add_parser("compare", help="按交易日期比较一对工作簿")
    comparison.add_argument("reference", type=Path)
    comparison.add_argument("output", type=Path)
    preparation = actions.add_parser("prepare", help="按页生成 144 DPI 纯图 PDF 与同页文字参照")
    preparation.add_argument("source", type=Path, help="使用脱敏编号命名的原始 PDF")
    preparation.add_argument("output", type=Path)
    preparation.add_argument("pages", type=int, nargs="+", help="从 1 开始的原件页码")
    timing = actions.add_parser("time", help="按文件交替运行两份发布程序，保存工作簿与耗时")
    timing.add_argument("before", type=Path)
    timing.add_argument("after", type=Path)
    timing.add_argument("mode", choices=("ocr", "pdf"))
    timing.add_argument("source", type=Path, help="包含按 S 加数字命名的 PDF 的目录")
    timing.add_argument("output", type=Path)
    timing.add_argument("--reverse", action="store_true", help="第一份文件先跑新版")
    args = parser.parse_args()
    if args.action == "compare":
        print(json.dumps(compare(args.reference, args.output), ensure_ascii=False, indent=2))
    elif args.action == "prepare":
        prepare_pages(args.source, args.output, args.pages)
    else:
        files = sorted(args.source.glob("*.pdf"))
        if not files:
            parser.error("输入目录没有 PDF")
        time_pairs(args.before, args.after, files, args.mode, args.output, args.reverse)


if __name__ == "__main__":
    main()
