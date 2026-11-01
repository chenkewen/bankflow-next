"""命令行入口，直接复用完整解析器的参数与转换逻辑。"""
import sys


def main():
    if len(sys.argv) < 2 or sys.argv[1] not in {"pdf", "ocr"}:
        print("用法：python scripts/convert.py {pdf|ocr} 文件路径 [-o 输出目录]", file=sys.stderr)
        return 2
    mode = sys.argv.pop(1)
    if mode == "pdf":
        from bankflow.parsers.pdf_to_excel import main as convert
    else:
        from bankflow.parsers.ocr_to_excel import main as convert
    return convert()
